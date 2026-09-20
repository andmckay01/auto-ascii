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
use clap::{Parser, Subcommand};

use home::{Home, Target, cut_name, kebab_case, name_for, rfc3339_utc, stem_of};
use library::{AssetInfo, Sidecar, Source, absolute, clip_ref};

/// The error type the whole CLI funnels into — the factory's, so its errors
/// pass through unwrapped and read the same in both binaries.
pub type BoxErr = Box<dyn std::error::Error>;

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
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if cli.json {
                // serde_json owns the escaping; an error text with a quote
                // or a newline in it must not break the contract.
                let obj = serde_json::json!({ "error": e.to_string() });
                eprintln!("{obj}");
            } else {
                eprintln!("auto-ascii: {e}");
            }
            ExitCode::FAILURE
        }
    }
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
    let name = match args.name {
        Some(n) => {
            let kebab = kebab_case(n);
            if kebab.is_empty() {
                return Err(format!("--name {n:?} has no alphanumerics to name a clip").into());
            }
            kebab
        }
        None => name_for(args.video)?,
    };
    let asset = home.clip_path(&name);
    if asset.exists() && !args.force {
        return Err(format!(
            "clip {name:?} already exists at {} — pass --force to replace it",
            asset.display()
        )
        .into());
    }

    let ss = parse_time(args.ss, "--ss")?;
    let t = parse_time(args.t, "--t")?;
    let res = args.res.map(auto_ascii_factory::parse_res).transpose()?;

    // Drop the old provenance BEFORE building over its asset: a build that
    // fails halfway must not leave a sidecar describing bytes that are no
    // longer there. Missing provenance is visible (`list` says so); wrong
    // provenance is not.
    if args.force {
        library::remove_sidecar(&asset)?;
    }

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

    // Everything from here on happens AFTER the asset was renamed into
    // place, so a failure leaves a real clip with no provenance. Say which
    // file that is: the next import would otherwise just hit the name
    // collision with no idea why.
    let (sidecar, sidecar_path) = match record_provenance(&name, args.video, &asset, &report) {
        Ok(pair) => pair,
        Err(e) => {
            return Err(format!(
                "{e} (the asset landed at {} but has no sidecar; \
                 re-run the import with --force)",
                asset.display()
            )
            .into());
        }
    };

    if cli.json {
        println!("{}", serde_json::to_string(&sidecar)?);
    } else {
        println!("imported {name}");
        print_clip_body(&sidecar);
        println!("  {:<14}{}", "sidecar:", sidecar_path.display());
    }
    Ok(())
}

/// Hash the source, stamp the time and write `library/<name>.json`.
/// Split out so `import` can name the asset it just orphaned when any of
/// these three steps fails.
fn record_provenance(
    name: &str,
    video: &std::path::Path,
    asset: &std::path::Path,
    report: &auto_ascii_factory::BuildReport,
) -> Result<(Sidecar, std::path::PathBuf), BoxErr> {
    let source_bytes = std::fs::metadata(video)
        .map_err(|e| format!("stat {}: {e}", video.display()))?
        .len();
    let sha256 = auto_ascii_factory::sha256_file(video)
        .map_err(|e| format!("hash {}: {e}", video.display()))?;
    let created_unix = now_unix();
    let sidecar = Sidecar {
        name: name.to_string(),
        source: Some(Source::Video { path: absolute(video), sha256, bytes: source_bytes }),
        asset: Some(AssetInfo {
            path: absolute(asset),
            bytes: report.bytes,
            frames: report.frames,
            fps: report.fps,
            duration_secs: report.duration_secs,
            base_w: report.base_w,
            base_h: report.base_h,
        }),
        created_unix: Some(created_unix),
        created: Some(rfc3339_utc(created_unix)),
        error: None,
    };
    let path = library::write_sidecar(asset, &sidecar)?;
    Ok((sidecar, path))
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
    let source = home.resolve_clip(args.clip)?;
    let in_secs = parse_one_time(args.in_spec, "--in")?;
    let out_secs = parse_one_time(args.out_spec, "--out")?;
    if out_secs <= in_secs {
        return Err(format!(
            "--out {:?} ({out_secs}s) must be later than --in {:?} ({in_secs}s)",
            args.out_spec, args.in_spec
        )
        .into());
    }
    let name = match args.name {
        Some(n) => {
            let kebab = kebab_case(n);
            if kebab.is_empty() {
                return Err(format!("--name {n:?} has no alphanumerics to name a clip").into());
            }
            kebab
        }
        None => cut_name(&stem_of(&source), in_secs, out_secs),
    };
    let asset = home.clip_path(&name);
    if asset.exists() && !args.force {
        return Err(format!(
            "clip {name:?} already exists at {} — pass --force to replace it",
            asset.display()
        )
        .into());
    }
    let mut clip = auto_ascii::Clip::new(&source);
    clip.in_secs = in_secs;
    clip.out_secs = Some(out_secs);
    let mut comp = auto_ascii::Composition::from_clips(name.clone(), vec![clip]);
    // Validated BEFORE anything on disk moves: a slice past the end of the
    // asset must leave the clip it would have replaced exactly as it was,
    // provenance included.
    comp.resolve()?;

    // Then the same order `import` keeps (note 28d): the old provenance
    // goes before the new bytes land, so a build that fails leaves a clip
    // visibly unrecorded rather than one described by bytes that were
    // never written.
    if args.force {
        library::remove_sidecar(&asset)?;
    }
    // `export` is atomic (`<out>.part` + rename), which is what makes this
    // safe to point straight at `library/`: a failed `--force` leaves the
    // clip already there intact, and a half-written file never shows up in
    // `list`.
    auto_ascii::compose::export(&comp, &asset, &ExportOptions::default())?;

    let from = clip_ref(home, &source);
    let (sidecar, sidecar_path) = match record_cut(&name, &from, in_secs, out_secs, &asset) {
        Ok(pair) => pair,
        Err(e) => {
            return Err(format!(
                "{e} (the slice landed at {} but has no sidecar; \
                 re-run the cut with --force)",
                asset.display()
            )
            .into());
        }
    };

    if cli.json {
        println!("{}", serde_json::to_string(&sidecar)?);
    } else {
        println!("cut {name} from {from}");
        print_clip_body(&sidecar);
        println!("  {:<14}{}", "sidecar:", sidecar_path.display());
    }
    Ok(())
}

/// Stamp the time and write `library/<name>.json` for a cut. The `asset`
/// block is read back off the header, exactly like `list` and `info` read
/// it, so it describes the file that actually landed.
fn record_cut(
    name: &str,
    from: &str,
    in_secs: f64,
    out_secs: f64,
    asset: &Path,
) -> Result<(Sidecar, PathBuf), BoxErr> {
    let created_unix = now_unix();
    let sidecar = Sidecar {
        name: name.to_string(),
        source: Some(Source::cut(from.to_string(), in_secs, out_secs)),
        asset: Some(library::asset_info(asset)?),
        created_unix: Some(created_unix),
        created: Some(rfc3339_utc(created_unix)),
        error: None,
    };
    let path = library::write_sidecar(asset, &sidecar)?;
    Ok((sidecar, path))
}

fn cmd_list(cli: &Cli, home: &Home) -> Result<(), BoxErr> {
    let clips = library::list(home)?;
    if cli.json {
        println!("{}", serde_json::to_string(&clips)?);
        return Ok(());
    }
    if clips.is_empty() {
        println!(
            "no clips in {} (import one: auto-ascii import <video>)",
            home.library().display()
        );
        return Ok(());
    }
    let w = clips.iter().map(|c| c.name.len()).max().unwrap_or(4).max(4);
    println!(
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
        println!("{:<w$}  {duration:>8}  {fps:>5}  {frames:>7}  {bytes:>10}  {last}", c.name);
    }
    Ok(())
}

fn cmd_info(cli: &Cli, home: &Home, clip: &str) -> Result<(), BoxErr> {
    let path = home.resolve_clip(clip)?;
    // The stem verbatim, so `info` and `list` name the same file the same
    // way; naming one clip is strict, unlike listing all of them.
    let sidecar = library::describe(&stem_of(&path), &path)?;
    if cli.json {
        println!("{}", serde_json::to_string(&sidecar)?);
    } else {
        println!("{}", sidecar.name);
        print_clip_body(&sidecar);
    }
    Ok(())
}

fn cmd_compose_new(cli: &Cli, home: &Home, name: &str) -> Result<(), BoxErr> {
    home.create()?;
    // Kebab-cased on the way in, like `import --name`: it is the naming
    // rule, and it is also why no name can write outside `compositions/`.
    let name = kebab_case(name);
    if name.is_empty() {
        return Err("a composition name needs at least one alphanumeric".into());
    }
    let path = home.composition_path(&name);
    composition::create(&path, &name)?;
    if cli.json {
        let obj = serde_json::json!({ "name": name, "path": absolute(&path) });
        println!("{obj}");
    } else {
        println!("created {name}");
        println!("  {:<14}{}", "path:", absolute(&path));
        println!("  {:<14}auto-ascii compose add {name} <clip>", "next:");
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
    let times: Vec<(&str, &str)> = [
        ("in", args.in_spec),
        ("out", args.out_spec),
        ("at", args.at_spec),
    ]
    .into_iter()
    .filter_map(|(key, spec)| spec.map(|spec| (key, spec)))
    .collect();
    composition::append_clip(&path, &composition::clip_table(&asset, &times))?;
    // The append is a text edit, so the only proof it left a file anything
    // can load is loading it. Exiting 0 on a composition that no longer
    // parses is the one way this command could lie to an agent — and the
    // file it edits may have been hand-written, so the breakage is not
    // always the table we just added.
    if let Err(e) = auto_ascii::Composition::from_toml_file(&path, Some(&home.library())) {
        return Err(format!(
            "{e} (the clip WAS appended, so fix the file rather than re-running `add`)"
        )
        .into());
    }

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
        println!("{obj}");
    } else {
        println!("added {asset} to {name}");
        println!("  {:<14}{}", "path:", absolute(&path));
        println!("  {:<14}{}", "clip:", absolute(&clip));
        for (label, spec) in
            [("in:", args.in_spec), ("out:", args.out_spec), ("at:", args.at_spec)]
        {
            if let Some(spec) = spec {
                println!("  {label:<14}{spec}");
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
        println!("{}", serde_json::to_string(&report)?);
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
    let report = auto_ascii::compose::export(&comp, &out, &ExportOptions::default())?;
    if cli.json {
        let obj = serde_json::json!({
            "path": absolute(&out),
            "frames": report.frames,
            "fps": report.fps,
            "bytes": report.bytes,
            "shots": report.shots,
            "cuts": report.cuts,
        });
        println!("{obj}");
    } else {
        println!("exported {name}");
        println!("  {:<14}{}", "path:", absolute(&out));
        println!("  {:<14}{} ({})", "bytes:", report.bytes, human_bytes(report.bytes));
        println!(
            "  {:<14}{} ({:.2}s @ {} fps)",
            "frames:",
            report.frames,
            f64::from(report.frames) / report.fps,
            fps_text(report.fps)
        );
        println!(
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
            // Read the header before taking over the terminal: a bad asset
            // should fail as a plain error, not as a dead alternate screen.
            library::describe(&stem_of(&path), &path)?;
            auto_ascii::Player::builder().asset(&path).build()?.run()?;
            Ok(())
        }
        Target::Composition(path) => play_composition(&path),
    }
}

fn cmd_agent_guide(cli: &Cli) -> Result<(), BoxErr> {
    if cli.json {
        println!("{}", serde_json::json!({ "guide": AGENT_GUIDE }));
    } else {
        // print!, not println!: the file ends with its own newline.
        print!("{AGENT_GUIDE}");
        std::io::stdout().flush()?;
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
        println!("{obj}");
    } else {
        println!("{}", home.root().display());
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
    println!(
        "{}  {clips} clip{}  {:.2}s  {} frames @ {} fps",
        report.name,
        if clips == 1 { "" } else { "s" },
        report.duration_secs,
        report.frame_count,
        fps_text(report.fps)
    );
    let w = report.clips.iter().map(|c| c.asset.len()).max().unwrap_or(4).max(4);
    println!(
        "  {:>3}  {:<w$}  {:>7}  {:>7}  {:>7}  {:>7}  {:>5}",
        "#", "clip", "start", "end", "in", "out", "fps"
    );
    for row in report.rows() {
        match row {
            composition::Row::Clip(c, marks) => {
                let note: String = marks
                    .iter()
                    .map(|mark| match mark {
                        composition::Mark::Over(idx) => format!("  OVERLAP #{idx}"),
                        composition::Mark::Under(idx) => format!("  UNDER #{idx}"),
                        composition::Mark::Hidden(idx) => format!("  HIDDEN #{idx}"),
                    })
                    .collect();
                println!(
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
            composition::Row::Gap(gap) => println!(
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
        println!("  {:<14}{}", "asset:", a.path);
        println!("  {:<14}{} ({})", "bytes:", a.bytes, human_bytes(a.bytes));
        println!(
            "  {:<14}{} ({:.2}s @ {} fps)",
            "frames:",
            a.frames,
            a.duration_secs,
            fps_text(a.fps)
        );
        println!("  {:<14}{}x{}", "base res:", a.base_w, a.base_h);
    }
    if let Some(e) = &sidecar.error {
        println!("  {:<14}{e}", "error:");
    }
    match &sidecar.source {
        Some(Source::Video { path, sha256, bytes }) => {
            println!("  {:<14}{}", "source:", path);
            println!("  {:<14}{} ({})", "source bytes:", bytes, human_bytes(*bytes));
            println!("  {:<14}{}", "source sha:", sha256);
        }
        // A cut's provenance is the slice, so it fits on the one line.
        Some(cut @ Source::Cut { .. }) => println!("  {:<14}{}", "source:", cut.summary()),
        None => println!("  {:<14}(none: no sidecar beside this asset)", "source:"),
    }
    if let Some(created) = &sidecar.created {
        println!("  {:<14}{}", "created:", created);
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
