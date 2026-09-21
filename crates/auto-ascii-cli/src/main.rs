//! `auto-ascii` — the agent-first CLI (PLAN-M6-M8 §2).
//!
//! Takes a video from anywhere on the desktop, processes it through the
//! factory's ffmpeg ingest, and lands it in the folder where the user's
//! processed clips live — then lists, describes, trims, stitches and plays
//! what is there (`cut` and `compose …` are M8's half, PLAN-M6-M8 §3).
//!
//! Two output modes, one code path: humans get aligned text on stdout,
//! agents pass `--json` and get **exactly one** JSON value on stdout and
//! nothing else (errors become `{"error": "..."}` on stderr with exit 1).
//! Everything chatty — ffmpeg progress, the factory's `input:`/`pass 1/2:`/
//! `wrote` lines — is written to stderr in both modes, which is what makes
//! that promise keepable (PLAN §5's "stdout stays clean" rule, extended).
//!
//! The crate exists because the facade cannot host it: `auto-ascii-factory`
//! already depends on `auto-ascii`, so the binary that needs BOTH has to be
//! a third crate. M7 split the factory into lib + thin bin for exactly this.

mod composition;
mod home;
mod library;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use auto_ascii::compose::ExportOptions;
use auto_ascii::timecode;
use auto_ascii_factory::BuildRequest;
use clap::error::ErrorKind;
use clap::{Parser, Subcommand};

use home::{Home, Target, cut_name, kebab_case, library_name, name_for, rfc3339_utc, stem_of};
use library::{AssetInfo, Sidecar, Source, absolute, clip_ref};

/// The error type the whole CLI funnels into — the factory's, so its errors
/// pass through unwrapped and read the same in both binaries.
pub type BoxErr = Box<dyn std::error::Error>;

/// Every byte this CLI puts on stdout goes through here.
///
/// Rust ignores SIGPIPE, so when the reader goes away — `auto-ascii list |
/// head -1`, a quit pager — the next write fails with `BrokenPipe`, and
/// `outln!` PANICS on that: a stack trace and exit 101 where the user
/// did something completely ordinary. Ending quietly with 0 is the shell
/// convention and this repo's precedent (`examples/headless-dump.rs`, M5
/// fix 7).
fn emit(text: &str) {
    let mut out = std::io::stdout().lock();
    if let Err(e) = out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            std::process::exit(0);
        }
        // A full disk, a closed descriptor: stdout IS the product here, so
        // say so where anyone can still see it and fail.
        emit_err(&format!("auto-ascii: write to stdout failed: {e}\n"));
        std::process::exit(1);
    }
}

/// The same for stderr, where a failed write has nowhere left to report
/// itself — the exit code still carries the outcome, so it is dropped.
fn emit_err(text: &str) {
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(text.as_bytes()).and_then(|()| err.flush());
}

/// `outln!` for this CLI: the same formatting, through [`emit`].
macro_rules! outln {
    ($($arg:tt)*) => {{
        let mut line = std::fmt::format(format_args!($($arg)*));
        line.push('\n');
        emit(&line);
    }};
}

/// [`outln!`] without the newline, for text that carries its own.
macro_rules! out {
    ($($arg:tt)*) => { emit(&std::fmt::format(format_args!($($arg)*))) };
}

/// The agent guide, embedded so `auto-ascii agent-guide` and the committed
/// file cannot drift (PLAN-M6-M8 §2).
const AGENT_GUIDE: &str = include_str!("../../../docs/AGENT-GUIDE.md");

/// Why `play` and `compose play` refuse `--json`. One string: the two
/// commands are the same refusal for the same reason.
const PLAY_IS_INTERACTIVE: &str = "play is interactive; run it without --json";

#[derive(Parser)]
#[command(
    name = "auto-ascii",
    version,
    about = "Import, list, cut, stitch and play ASCII-art video clips",
    after_help = "Run `auto-ascii agent-guide` for the folder layout, the JSON \
                  shapes and the composition schema."
)]
struct Cli {
    /// Print one JSON value on stdout instead of human text; errors become
    /// `{"error": "..."}` on stderr with exit 1.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Ingest a video into `library/<name>.ascii` + a `<name>.json`
    /// provenance sidecar. Needs ffmpeg on PATH.
    Import {
        /// Source video (any ffmpeg-readable container).
        video: PathBuf,
        /// Library name (kebab-cased). Default: the file stem, kebab-cased.
        #[arg(long)]
        name: Option<String>,
        /// Start offset into the source: SS[.f], MM:SS[.f] or HH:MM:SS[.f].
        #[arg(long)]
        ss: Option<String>,
        /// Duration to take from the source, same formats as --ss.
        #[arg(long = "t")]
        t: Option<String>,
        /// Output frame rate (default: the factory's params.toml).
        #[arg(long)]
        fps: Option<u16>,
        /// Stored plane resolution as WxH, even dimensions (default:
        /// the factory's params.toml).
        #[arg(long)]
        res: Option<String>,
        /// Replace an existing clip of the same name.
        #[arg(long)]
        force: bool,
    },
    /// List the library: name, duration, fps, frames, bytes, source.
    List,
    /// Header + sidecar for one clip.
    Info {
        /// A path if one exists, else `library/<clip>.ascii`.
        clip: String,
    },
    /// Slice one clip into a new library clip (an export of a one-clip
    /// composition).
    Cut {
        /// A path if one exists, else `library/<clip>.ascii`.
        clip: String,
        /// Start of the slice inside the clip: SS[.f], MM:SS[.f] or
        /// HH:MM:SS[.f].
        #[arg(long = "in")]
        in_spec: String,
        /// End of the slice inside the clip, same formats as --in.
        #[arg(long = "out")]
        out_spec: String,
        /// Library name for the slice (kebab-cased). Default:
        /// `<clip>-<in>-<out>`, e.g. `apple-1984-0m05s-0m20s`.
        #[arg(long)]
        name: Option<String>,
        /// Replace an existing clip of the same name.
        #[arg(long)]
        force: bool,
    },
    /// Compositions: the `schema = 1` TOML timelines in `compositions/`.
    Compose {
        #[command(subcommand)]
        cmd: ComposeCmd,
    },
    /// Play a clip or a composition interactively (same keys as
    /// `auto-ascii-player`).
    Play {
        /// A path if one exists, else `library/<target>.ascii`, else
        /// `compositions/<target>.toml`.
        target: String,
    },
    /// Print the embedded agent guide.
    AgentGuide,
    /// Print the resolved home folder, creating it and its subfolders.
    Home,
}

/// `compose …` (PLAN-M6-M8 §3). `<name>` is a path to a `.toml` if one
/// exists, else `compositions/<name>.toml` — the file is the source of
/// truth, and these subcommands only ever edit the same bytes an agent
/// would have written by hand.
#[derive(Subcommand)]
enum ComposeCmd {
    /// Start `compositions/<name>.toml` (fails if it is already there).
    New {
        /// Composition name (kebab-cased), which is also the file stem.
        name: String,
    },
    /// Append one `[[clip]]` table, leaving the rest of the file alone.
    Add {
        /// The composition to append to.
        name: String,
        /// A path if one exists, else `library/<clip>.ascii`.
        clip: String,
        /// Trim start inside the asset (default: its start).
        #[arg(long = "in")]
        in_spec: Option<String>,
        /// Trim end inside the asset (default: its end).
        #[arg(long = "out")]
        out_spec: Option<String>,
        /// Timeline position (default: the end of the previous clip).
        #[arg(long = "at")]
        at_spec: Option<String>,
    },
    /// The resolved timeline: each clip's place on it, plus the gaps and
    /// the overlaps.
    Show {
        /// The composition to describe.
        name: String,
    },
    /// Play a composition interactively.
    Play {
        /// The composition to play.
        name: String,
    },
    /// Flatten a composition into one `.ascii` file.
    Export {
        /// The composition to flatten.
        name: String,
        /// Where to write it (default: `exports/<name>.ascii`).
        #[arg(short = 'o', long = "out")]
        out: Option<PathBuf>,
        /// Replace an existing file at that path.
        #[arg(long)]
        force: bool,
    },
}

fn main() -> ExitCode {
    // `--json` is read out of argv, not out of the parsed `Cli`: a usage
    // error means there IS no parsed `Cli`, and an agent that asked for
    // JSON must not get clap's usage block on stderr instead of an object.
    let json = std::env::args_os().any(|arg| arg == "--json");
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => return usage_exit(&e, json),
    };
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            fail(&e.to_string(), cli.json);
            ExitCode::FAILURE
        }
    }
}

/// One error in the caller's chosen shape. Under `--json` serde_json owns
/// the escaping, so a message with a quote or a newline in it still leaves
/// exactly one value on stderr.
fn fail(message: &str, json: bool) {
    if json {
        let obj = serde_json::json!({ "error": message });
        emit_err(&format!("{obj}\n"));
    } else {
        emit_err(&format!("auto-ascii: {message}\n"));
    }
}

/// What clap produced instead of a `Cli`.
///
/// `--help` and `--version` are OUTPUT, not failures: they print where
/// clap wanted them and exit 0. Everything else is a usage error, and
/// under `--json` it goes through the same one-object contract as every
/// other error — an agent that typo'd a flag gets a document it can read
/// rather than a usage block it cannot. Without `--json` a human keeps
/// clap's own rendering and clap's own exit code (2), which stays
/// distinguishable from the exit 1 of a command that ran and failed.
fn usage_exit(e: &clap::Error, json: bool) -> ExitCode {
    if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
        let _ = e.print();
        return ExitCode::SUCCESS;
    }
    if !json {
        let _ = e.print();
        return ExitCode::from(2);
    }
    // `render()` is the unstyled text (`print` is what paints it), and its
    // leading `error: ` label is what the JSON key already says.
    let rendered = e.render().to_string();
    let message = rendered.trim();
    fail(message.strip_prefix("error: ").unwrap_or(message), true);
    ExitCode::FAILURE
}

/// The home folder is resolved by the commands that need one, not up
/// front: `agent-guide` is pure output and must work on a box with no
/// `HOME` and no `AUTO_ASCII_HOME` at all.
fn run(cli: &Cli) -> Result<(), BoxErr> {
    match &cli.cmd {
        Cmd::Import { video, name, ss, t, fps, res, force } => {
            let args = ImportArgs {
                video,
                name: name.as_deref(),
                ss: ss.as_deref(),
                t: t.as_deref(),
                fps: *fps,
                res: res.as_deref(),
                force: *force,
            };
            cmd_import(cli, &Home::resolve()?, &args)
        }
        Cmd::List => cmd_list(cli, &Home::resolve()?),
        Cmd::Info { clip } => cmd_info(cli, &Home::resolve()?, clip),
        Cmd::Cut { clip, in_spec, out_spec, name, force } => {
            let args = CutArgs {
                clip,
                in_spec,
                out_spec,
                name: name.as_deref(),
                force: *force,
            };
            cmd_cut(cli, &Home::resolve()?, &args)
        }
        Cmd::Compose { cmd } => run_compose(cli, cmd),
        Cmd::Play { target } => cmd_play(cli, target),
        Cmd::AgentGuide => cmd_agent_guide(cli),
        Cmd::Home => cmd_home(cli, &Home::resolve()?),
    }
}

/// The `compose …` half of [`run`]. `play` resolves its own home for the
/// same reason `Cmd::Play` does: the `--json` refusal comes first.
fn run_compose(cli: &Cli, cmd: &ComposeCmd) -> Result<(), BoxErr> {
    match cmd {
        ComposeCmd::New { name } => cmd_compose_new(cli, &Home::resolve()?, name),
        ComposeCmd::Add { name, clip, in_spec, out_spec, at_spec } => {
            let args = AddArgs {
                name,
                clip,
                in_spec: in_spec.as_deref(),
                out_spec: out_spec.as_deref(),
                at_spec: at_spec.as_deref(),
            };
            cmd_compose_add(cli, &Home::resolve()?, &args)
        }
        ComposeCmd::Show { name } => cmd_compose_show(cli, &Home::resolve()?, name),
        ComposeCmd::Play { name } => cmd_compose_play(cli, name),
        ComposeCmd::Export { name, out, force } => {
            let args = ExportArgs { name, out: out.as_deref(), force: *force };
            cmd_compose_export(cli, &Home::resolve()?, &args)
        }
    }
}

/// `import`'s flags, borrowed out of the clap enum so the command function
/// takes one argument instead of eight.
struct ImportArgs<'a> {
    video: &'a std::path::Path,
    name: Option<&'a str>,
    ss: Option<&'a str>,
    t: Option<&'a str>,
    fps: Option<u16>,
    res: Option<&'a str>,
    force: bool,
}

fn cmd_import(cli: &Cli, home: &Home, args: &ImportArgs<'_>) -> Result<(), BoxErr> {
    home.create()?;
    if !args.video.is_file() {
        return Err(format!("input not found: {}", args.video.display()).into());
    }
    let name = clip_name(args.name, || name_for(args.video))?;
    let asset = home.clip_path(&name);
    refuse_existing(&name, &asset, args.force)?;

    let ss = parse_time(args.ss, "--ss")?;
    let t = parse_time(args.t, "--t")?;
    let res = args.res.map(auto_ascii_factory::parse_res).transpose()?;

    // The factory's human lines go to stderr in BOTH modes: under --json
    // stdout carries the sidecar and nothing else.
    let report = auto_ascii_factory::build(
        &BuildRequest {
            input: args.video,
            output: &asset,
            params: None,
            ss,
            t,
            fps: args.fps,
            res,
        },
        &mut std::io::stderr(),
    )?;

    // Only NOW does the old provenance go (see `retire_provenance`).
    retire_provenance(&asset)?;
    let (sidecar, sidecar_path) = record_import(&name, args.video, &asset, &report)
        .map_err(|e| orphaned(&e, &asset, "import"))?;
    finish_clip(cli, &format!("imported {name}"), &sidecar, &sidecar_path)
}

/// Hash the source, describe what the factory wrote, and record it. Split
/// out so `import` can name the clip it just orphaned when any of these
/// steps fails.
fn record_import(
    name: &str,
    video: &Path,
    asset: &Path,
    report: &auto_ascii_factory::BuildReport,
) -> Result<(Sidecar, PathBuf), BoxErr> {
    let source_bytes = std::fs::metadata(video)
        .map_err(|e| format!("stat {}: {e}", video.display()))?
        .len();
    let sha256 = auto_ascii_factory::sha256_file(video)
        .map_err(|e| format!("hash {}: {e}", video.display()))?;
    record(
        name,
        Source::Video {
            path: absolute(video),
            sha256: Some(sha256),
            bytes: Some(source_bytes),
        },
        AssetInfo {
            path: absolute(asset),
            bytes: report.bytes,
            frames: report.frames,
            fps: report.fps,
            duration_secs: report.duration_secs,
            base_w: report.base_w,
            base_h: report.base_h,
        },
        asset,
    )
}

// ---------------------------------------------------------------------------
// The steps `import` and `cut` share (PLAN-M6-M8 §2, §3): both name a clip,
// refuse to overwrite one, retire the provenance the old bytes had, record
// the new bytes and print the result. Only what they PUT in the library
// differs — an ffmpeg ingest or a one-clip export.
// ---------------------------------------------------------------------------

/// The library name a writing command will use: `--name` kebab-cased (the
/// documented rule, and the reason a name can hold neither a separator nor
/// `..`), else the caller's own default.
fn clip_name(
    flag: Option<&str>,
    default: impl FnOnce() -> Result<String, BoxErr>,
) -> Result<String, BoxErr> {
    let Some(given) = flag else { return default() };
    let kebab = kebab_case(given);
    if kebab.is_empty() {
        return Err(format!("--name {given:?} has no alphanumerics to name a clip").into());
    }
    Ok(kebab)
}

/// Refuse to write over a clip that is already there, unless `--force`.
fn refuse_existing(name: &str, asset: &Path, force: bool) -> Result<(), BoxErr> {
    if asset.exists() && !force {
        return Err(format!(
            "clip {name:?} already exists at {} — pass --force to replace it",
            asset.display()
        )
        .into());
    }
    Ok(())
}

/// Drop the sidecar describing the bytes that were just replaced.
///
/// Called only once the NEW asset is in place, which is the whole point:
/// before that moment a failed rebuild must leave the old clip AND its
/// provenance exactly as they were, and after it any sidecar still on disk
/// describes the wrong file. Missing provenance is visible (`list` says
/// so); wrong provenance is not.
fn retire_provenance(asset: &Path) -> Result<(), BoxErr> {
    library::remove_sidecar(asset)
}

/// Stamp the time and write `library/<name>.json` for bytes that have
/// already landed.
fn record(
    name: &str,
    source: Source,
    info: AssetInfo,
    asset: &Path,
) -> Result<(Sidecar, PathBuf), BoxErr> {
    let created_unix = now_unix();
    let sidecar = Sidecar {
        name: name.to_string(),
        source: Some(source),
        asset: Some(info),
        created_unix: Some(created_unix),
        created: Some(rfc3339_utc(created_unix)),
        error: None,
    };
    let path = library::write_sidecar(asset, &sidecar)?;
    Ok((sidecar, path))
}

/// The failure both writers share: a clip is on disk and the provenance
/// beside it is not. Name the file — the next run would otherwise just hit
/// the name collision with no idea why.
fn orphaned(e: &BoxErr, asset: &Path, cmd: &str) -> BoxErr {
    format!(
        "{e} (the clip landed at {} but has no sidecar; re-run the {cmd} with --force)",
        asset.display()
    )
    .into()
}

/// The tail both writers share: one JSON value, or a headline over the
/// aligned body.
fn finish_clip(
    cli: &Cli,
    headline: &str,
    sidecar: &Sidecar,
    sidecar_path: &Path,
) -> Result<(), BoxErr> {
    if cli.json {
        outln!("{}", serde_json::to_string(sidecar)?);
    } else {
        outln!("{headline}");
        print_clip_body(sidecar);
        outln!("  {:<14}{}", "sidecar:", sidecar_path.display());
    }
    Ok(())
}

/// The encode knobs `cut` and `compose export` write with: the factory's
/// committed `params.toml` `[build]` section, read through the merge the
/// factory itself uses. A slice or a flattened composition is the same
/// kind of asset `import` produces, so its cadence and level come from the
/// same place rather than from a constant that can drift away from it.
fn export_options() -> Result<ExportOptions, BoxErr> {
    let params = auto_ascii_factory::effective_params(None, None, None)?;
    let keyframe_ivl = u8::try_from(params.build.keyframe_ivl).map_err(|_| {
        format!(
            "params build.keyframe_ivl {} does not fit the ASCI header",
            params.build.keyframe_ivl
        )
    })?;
    Ok(ExportOptions { keyframe_ivl, zstd_level: params.build.zstd_level })
}

/// `cut`'s flags, borrowed out of the clap enum for the same reason
/// [`ImportArgs`] is.
struct CutArgs<'a> {
    clip: &'a str,
    in_spec: &'a str,
    out_spec: &'a str,
    name: Option<&'a str>,
    force: bool,
}

/// `cut` is an export of a ONE-CLIP composition (PLAN-M6-M8 §3), so the
/// slice is the same bytes the composition would have played: planes are
/// copied out of the source, never re-derived, and no ffmpeg is involved.
fn cmd_cut(cli: &Cli, home: &Home, args: &CutArgs<'_>) -> Result<(), BoxErr> {
    home.create()?;
    let from_path = home.resolve_clip(args.clip)?;
    let in_secs = parse_one_time(args.in_spec, "--in")?;
    let out_secs = parse_one_time(args.out_spec, "--out")?;
    if out_secs <= in_secs {
        return Err(format!(
            "--out {:?} ({out_secs}s) must be later than --in {:?} ({in_secs}s)",
            args.out_spec, args.in_spec
        )
        .into());
    }
    let name = clip_name(args.name, || Ok(cut_name(&stem_of(&from_path), in_secs, out_secs)))?;
    let asset = home.clip_path(&name);
    refuse_existing(&name, &asset, args.force)?;

    let mut clip = auto_ascii::Clip::new(&from_path);
    clip.in_secs = in_secs;
    clip.out_secs = Some(out_secs);
    let mut comp = auto_ascii::Composition::from_clips(name.clone(), vec![clip]);
    // Validated BEFORE anything on disk moves: a slice past the end of the
    // asset must leave the clip it would have replaced exactly as it was,
    // provenance included.
    comp.resolve()?;
    let opts = export_options()?;
    // `export` is atomic (`<out>.part` + rename), which is what makes this
    // safe to point straight at `library/`: a failed `--force` leaves the
    // clip already there intact, and a half-written file never shows up in
    // `list`.
    auto_ascii::compose::export(&comp, &asset, &opts)?;

    // Only NOW does the old provenance go (see `retire_provenance`).
    retire_provenance(&asset)?;
    let from = clip_ref(home, &from_path);
    // The `asset` block is read back off the header, exactly like `list`
    // and `info` read it, so it describes the file that actually landed.
    let (sidecar, sidecar_path) = record(
        &name,
        Source::cut(from.clone(), in_secs, out_secs),
        library::asset_info(&asset)?,
        &asset,
    )
    .map_err(|e| orphaned(&e, &asset, "cut"))?;
    finish_clip(cli, &format!("cut {name} from {from}"), &sidecar, &sidecar_path)
}

fn cmd_list(cli: &Cli, home: &Home) -> Result<(), BoxErr> {
    let clips = library::list(home)?;
    if cli.json {
        outln!("{}", serde_json::to_string(&clips)?);
        return Ok(());
    }
    if clips.is_empty() {
        outln!(
            "no clips in {} (import one: auto-ascii import <video>)",
            home.library().display()
        );
        return Ok(());
    }
    let w = clips.iter().map(|c| c.name.len()).max().unwrap_or(4).max(4);
    outln!(
        "{:<w$}  {:>8}  {:>5}  {:>7}  {:>10}  source",
        "name", "duration", "fps", "frames", "bytes"
    );
    for c in &clips {
        // An entry we could not read keeps its row and fills the numbers
        // with `?`; the last column carries the reason instead of a source.
        let (duration, fps, frames, bytes) = match &c.asset {
            Some(a) => (
                timecode::format_mmss(a.duration_secs),
                fps_text(a.fps),
                a.frames.to_string(),
                human_bytes(a.bytes),
            ),
            None => ("?".to_string(), "?".to_string(), "?".to_string(), "?".to_string()),
        };
        let last = match (&c.error, &c.source) {
            (Some(e), _) => format!("! {e}"),
            (None, Some(src)) => src.summary(),
            (None, None) => "-".to_string(),
        };
        outln!("{:<w$}  {duration:>8}  {fps:>5}  {frames:>7}  {bytes:>10}  {last}", c.name);
    }
    Ok(())
}

fn cmd_info(cli: &Cli, home: &Home, clip: &str) -> Result<(), BoxErr> {
    let path = home.resolve_clip(clip)?;
    // The stem verbatim, so `info` and `list` name the same file the same
    // way; naming one clip is strict, unlike listing all of them.
    let sidecar = library::describe(&stem_of(&path), &path)?;
    if cli.json {
        outln!("{}", serde_json::to_string(&sidecar)?);
    } else {
        outln!("{}", sidecar.name);
        print_clip_body(&sidecar);
    }
    Ok(())
}

fn cmd_compose_new(cli: &Cli, home: &Home, name: &str) -> Result<(), BoxErr> {
    home.create()?;
    // Kebab-cased on the way in, like `import --name`: it is the naming
    // rule, and it is also why no name can write outside `compositions/`.
    // The folder's own extension comes off first, so `compose new
    // demo.toml` starts `demo.toml` rather than `demo-toml.toml` — the
    // same rule the resolvers apply on the way back.
    let name = kebab_case(library_name(name, "toml"));
    if name.is_empty() {
        return Err("a composition name needs at least one alphanumeric".into());
    }
    let path = home.composition_path(&name);
    composition::create(&path, &name)?;
    if cli.json {
        let obj = serde_json::json!({ "name": name, "path": absolute(&path) });
        outln!("{obj}");
    } else {
        outln!("created {name}");
        outln!("  {:<14}{}", "path:", absolute(&path));
        outln!("  {:<14}auto-ascii compose add {name} <clip>", "next:");
    }
    Ok(())
}

/// `compose add`'s flags, borrowed out of the clap enum.
struct AddArgs<'a> {
    name: &'a str,
    clip: &'a str,
    in_spec: Option<&'a str>,
    out_spec: Option<&'a str>,
    at_spec: Option<&'a str>,
}

fn cmd_compose_add(cli: &Cli, home: &Home, args: &AddArgs<'_>) -> Result<(), BoxErr> {
    let path = home.resolve_composition(args.name)?;
    let clip = home.resolve_clip(args.clip)?;
    // Parsed here for the error and for the JSON; WRITTEN verbatim, so the
    // file keeps the timestamp the agent typed.
    let in_secs = parse_time(args.in_spec, "--in")?;
    let out_secs = parse_time(args.out_spec, "--out")?;
    let at_secs = parse_time(args.at_spec, "--at")?;
    if let (Some(i), Some(o)) = (in_secs, out_secs)
        && o <= i
    {
        return Err(format!(
            "--out {:?} ({o}s) must be later than --in {:?} ({i}s)",
            args.out_spec.unwrap_or_default(),
            args.in_spec.unwrap_or_default()
        )
        .into());
    }
    let asset = clip_ref(home, &clip);
    // Resolve the composition this WOULD be before touching the file: a
    // trim the timeline rejects, a `<clip>` that is not an ASCI asset, or
    // a table above that no longer parses must fail with the file exactly
    // as it was. An `add` that exits 1 having appended anyway is the one
    // way this command could lie to an agent.
    let existing = auto_ascii::Composition::from_toml_file(&path, Some(&home.library()))?;
    let mut clips = existing.clips().to_vec();
    clips.push(auto_ascii::Clip {
        name: asset.clone(),
        path: clip.clone(),
        in_secs: in_secs.unwrap_or(0.0),
        out_secs,
        at_secs,
    });
    auto_ascii::Composition::from_clips(existing.name(), clips).resolve()?;

    let times: Vec<(&str, &str)> = [
        ("in", args.in_spec),
        ("out", args.out_spec),
        ("at", args.at_spec),
    ]
    .into_iter()
    .filter_map(|(key, spec)| spec.map(|spec| (key, spec)))
    .collect();
    composition::append_clip(&path, &composition::clip_table(&asset, &times))?;

    let name = stem_of(&path);
    if cli.json {
        let obj = serde_json::json!({
            "name": name,
            "path": absolute(&path),
            "clip": {
                "asset": asset,
                "path": absolute(&clip),
                "in_secs": in_secs,
                "out_secs": out_secs,
                "at_secs": at_secs,
            },
        });
        outln!("{obj}");
    } else {
        outln!("added {asset} to {name}");
        outln!("  {:<14}{}", "path:", absolute(&path));
        outln!("  {:<14}{}", "clip:", absolute(&clip));
        for (label, spec) in
            [("in:", args.in_spec), ("out:", args.out_spec), ("at:", args.at_spec)]
        {
            if let Some(spec) = spec {
                outln!("  {label:<14}{spec}");
            }
        }
    }
    Ok(())
}

fn cmd_compose_show(cli: &Cli, home: &Home, name: &str) -> Result<(), BoxErr> {
    let path = home.resolve_composition(name)?;
    let comp = open_composition(home, &path)?;
    let report = composition::report(&comp);
    if cli.json {
        outln!("{}", serde_json::to_string(&report)?);
    } else {
        print_timeline(&report);
    }
    Ok(())
}

fn cmd_compose_play(cli: &Cli, name: &str) -> Result<(), BoxErr> {
    // Refused before the home folder is resolved, exactly as `play` does.
    if cli.json {
        return Err(PLAY_IS_INTERACTIVE.into());
    }
    let home = Home::resolve()?;
    let path = home.resolve_composition(name)?;
    play_composition(&path)
}

/// `compose export`'s flags, borrowed out of the clap enum.
struct ExportArgs<'a> {
    name: &'a str,
    out: Option<&'a Path>,
    force: bool,
}

fn cmd_compose_export(cli: &Cli, home: &Home, args: &ExportArgs<'_>) -> Result<(), BoxErr> {
    home.create()?;
    let path = home.resolve_composition(args.name)?;
    let comp = open_composition(home, &path)?;
    let name = stem_of(&path);
    let out = match args.out {
        Some(out) => out.to_path_buf(),
        None => home.export_path(&name),
    };
    if out.exists() && !args.force {
        return Err(format!(
            "{} already exists — pass --force to replace it",
            out.display()
        )
        .into());
    }
    let report = auto_ascii::compose::export(&comp, &out, &export_options()?)?;
    if cli.json {
        let obj = serde_json::json!({
            "path": absolute(&out),
            "frames": report.frames,
            "fps": report.fps,
            "bytes": report.bytes,
            "shots": report.shots,
            "cuts": report.cuts,
        });
        outln!("{obj}");
    } else {
        outln!("exported {name}");
        outln!("  {:<14}{}", "path:", absolute(&out));
        outln!("  {:<14}{} ({})", "bytes:", report.bytes, human_bytes(report.bytes));
        outln!(
            "  {:<14}{} ({:.2}s @ {} fps)",
            "frames:",
            report.frames,
            f64::from(report.frames) / report.fps,
            fps_text(report.fps)
        );
        outln!(
            "  {:<14}{} ({} cut{})",
            "shots:",
            report.shots,
            report.cuts,
            if report.cuts == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

/// Parse a composition file and resolve its timeline, with the home
/// `library/` as the folder a bare `asset` name looks in — the CLI always
/// knows which home it is working in, so it never falls back to the
/// environment sniff `Composition::default_library_dir` does.
fn open_composition(home: &Home, path: &Path) -> Result<auto_ascii::Composition, BoxErr> {
    let mut comp = auto_ascii::Composition::from_toml_file(path, Some(&home.library()))?;
    comp.resolve()?;
    Ok(comp)
}

/// `PlayerBuilder::build` parses the file and reads every clip's header
/// before any terminal state changes, so a broken composition fails as a
/// plain error here — the same promise `play <clip>` keeps by describing
/// the asset first.
fn play_composition(path: &Path) -> Result<(), BoxErr> {
    auto_ascii::Player::builder().composition(path).build()?.run()?;
    Ok(())
}

fn cmd_play(cli: &Cli, target: &str) -> Result<(), BoxErr> {
    // Refused before anything else, and long before a terminal session:
    // the player OWNS stdout for the whole run, so no JSON printed around
    // it could ever be the only value there. Saying so beats emitting a
    // document an agent would have to dig out of an animation.
    if cli.json {
        return Err(PLAY_IS_INTERACTIVE.into());
    }
    let home = Home::resolve()?;
    match home.resolve_playable(target)? {
        Target::Clip(path) => {
            // Read the HEADER before taking over the terminal: a bad asset
            // should fail as a plain error, not as a dead alternate screen.
            // Only the header — a clip whose sidecar is truncated or
            // hand-written still plays, because provenance is not the film.
            library::asset_info(&path)?;
            auto_ascii::Player::builder().asset(&path).build()?.run()?;
            Ok(())
        }
        Target::Composition(path) => play_composition(&path),
    }
}

fn cmd_agent_guide(cli: &Cli) -> Result<(), BoxErr> {
    if cli.json {
        outln!("{}", serde_json::json!({ "guide": AGENT_GUIDE }));
    } else {
        // out!, not outln!: the file ends with its own newline.
        out!("{AGENT_GUIDE}");
    }
    Ok(())
}

fn cmd_home(cli: &Cli, home: &Home) -> Result<(), BoxErr> {
    home.create()?;
    if cli.json {
        let obj = serde_json::json!({
            "home": home.root().display().to_string(),
            "library": home.library().display().to_string(),
            "compositions": home.compositions().display().to_string(),
            "exports": home.exports().display().to_string(),
        });
        outln!("{obj}");
    } else {
        outln!("{}", home.root().display());
    }
    Ok(())
}

/// Parse one OPTIONAL time argument (`--ss`, `--t`, `compose add`'s
/// three) through the facade's shared grammar.
fn parse_time(spec: Option<&str>, flag: &str) -> Result<Option<f64>, BoxErr> {
    spec.map(|s| parse_one_time(s, flag)).transpose()
}

/// The same for a REQUIRED one (`cut --in`/`--out`), naming the flag in
/// the error the way clap would have.
fn parse_one_time(spec: &str, flag: &str) -> Result<f64, BoxErr> {
    timecode::parse(spec).map_err(|e| format!("{flag} {spec:?}: {e}").into())
}

/// Now, as seconds since the Unix epoch — the sidecar's `created_unix`.
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The `compose show` table: one row per clip in TIMELINE order with its
/// place on the composition, a `GAP` row wherever nothing plays, and an
/// `OVERLAP` note on a clip that covers an earlier one.
fn print_timeline(report: &composition::Report) {
    let clips = report.clips.len();
    outln!(
        "{}  {clips} clip{}  {:.2}s  {} frames @ {} fps",
        report.name,
        if clips == 1 { "" } else { "s" },
        report.duration_secs,
        report.frame_count,
        fps_text(report.fps)
    );
    let w = report.clips.iter().map(|c| c.asset.len()).max().unwrap_or(4).max(4);
    outln!(
        "  {:>3}  {:<w$}  {:>7}  {:>7}  {:>7}  {:>7}  {:>5}",
        "#", "clip", "start", "end", "in", "out", "fps"
    );
    for row in report.rows() {
        match row {
            composition::Row::Clip(c) => {
                let note: String = c
                    .marks
                    .iter()
                    .map(|mark| match mark {
                        composition::Mark::Over(idx) => format!("  OVERLAP #{idx}"),
                        composition::Mark::Under(idx) => format!("  UNDER #{idx}"),
                        composition::Mark::Hidden(idx) => format!("  HIDDEN #{idx}"),
                    })
                    .collect();
                outln!(
                    "  {:>3}  {:<w$}  {:>7.2}  {:>7.2}  {:>7.2}  {:>7.2}  {:>5}{note}",
                    c.index,
                    c.asset,
                    c.start_secs,
                    c.end_secs,
                    c.in_secs,
                    c.out_secs,
                    fps_text(c.fps)
                );
            }
            // A gap has no clip, no trim and no rate: it plays black.
            composition::Row::Gap(gap) => outln!(
                "  {:>3}  {:<w$}  {:>7.2}  {:>7.2}",
                "-", "GAP", gap.start_secs, gap.end_secs
            ),
        }
    }
}

/// The aligned body shared by `import` and `info` (the factory's `inspect`
/// column style: two spaces, a 14-wide label, the value).
fn print_clip_body(sidecar: &Sidecar) {
    if let Some(a) = &sidecar.asset {
        outln!("  {:<14}{}", "asset:", a.path);
        outln!("  {:<14}{} ({})", "bytes:", a.bytes, human_bytes(a.bytes));
        outln!(
            "  {:<14}{} ({:.2}s @ {} fps)",
            "frames:",
            a.frames,
            a.duration_secs,
            fps_text(a.fps)
        );
        outln!("  {:<14}{}x{}", "base res:", a.base_w, a.base_h);
    }
    if let Some(e) = &sidecar.error {
        outln!("  {:<14}{e}", "error:");
    }
    match &sidecar.source {
        Some(Source::Video { path, sha256, bytes }) => {
            outln!("  {:<14}{}", "source:", path);
            // A partial sidecar describes what it knows; the rest says so
            // rather than going missing.
            match bytes {
                Some(n) => outln!("  {:<14}{n} ({})", "source bytes:", human_bytes(*n)),
                None => outln!("  {:<14}{}", "source bytes:", library::UNKNOWN),
            }
            outln!("  {:<14}{}", "source sha:", sha256.as_deref().unwrap_or(library::UNKNOWN));
        }
        // A cut's provenance is the slice, so it fits on the one line.
        Some(cut @ Source::Cut { .. }) => outln!("  {:<14}{}", "source:", cut.summary()),
        None => outln!("  {:<14}(none: no sidecar beside this asset)", "source:"),
    }
    if let Some(created) = &sidecar.created {
        outln!("  {:<14}{}", "created:", created);
    }
}

/// `30`, not `30.000`, when the rate is whole — which it is for every asset
/// the factory writes (fps_den is 1).
fn fps_text(fps: f64) -> String {
    if (fps - fps.round()).abs() < 1e-9 {
        format!("{}", fps.round() as u64)
    } else {
        format!("{fps:.3}")
    }
}

/// Binary-prefix sizes, like the factory's `inspect` report.
fn human_bytes(n: u64) -> String {
    const KIB: f64 = 1024.0;
    let v = n as f64;
    if v >= KIB * KIB {
        format!("{:.1} MiB", v / (KIB * KIB))
    } else if v >= KIB {
        format!("{:.1} KiB", v / KIB)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_parse_through_the_shared_grammar() {
        assert_eq!(parse_time(None, "--ss").unwrap(), None);
        assert_eq!(parse_time(Some("1:30"), "--ss").unwrap(), Some(90.0));
        let err = parse_time(Some("nope"), "--ss").unwrap_err().to_string();
        assert!(err.starts_with("--ss \"nope\": "), "{err}");
        assert!(err.contains("bad timestamp component"), "{err}");
    }

    /// The knobs a slice is written with come from the factory's params,
    /// not from a constant beside them: `cut`, `compose export` and
    /// `import` are all supposed to produce the same kind of asset.
    #[test]
    fn export_knobs_come_from_the_factory_params() {
        let opts = export_options().expect("the committed params load");
        let params = auto_ascii_factory::effective_params(None, None, None).unwrap();
        assert_eq!(u32::from(opts.keyframe_ivl), params.build.keyframe_ivl);
        assert_eq!(opts.zstd_level, params.build.zstd_level);
    }

    #[test]
    fn number_formatting() {
        assert_eq!(fps_text(30.0), "30");
        assert_eq!(fps_text(29.97), "29.970");
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1024 * 1024 * 3 / 2), "1.5 MiB");
    }

    /// The embedded guide IS the committed file (include_str!), so the only
    /// thing to pin is that it is the guide and that it stayed short — 80
    /// lines at M8, up from M7's 60 for `cut` and the five `compose …`
    /// subcommands (PLAN-M6-M8 §3).
    #[test]
    fn agent_guide_is_embedded_and_short() {
        assert!(AGENT_GUIDE.starts_with("# auto-ascii for agents\n"), "{AGENT_GUIDE:.40}");
        assert!(AGENT_GUIDE.lines().count() < 80, "the guide must stay under 80 lines");
    }
}
