//! `auto-ascii` — the agent-first CLI (PLAN-M6-M8 §2).
//!
//! Takes a video from anywhere on the desktop, processes it through the
//! factory's ffmpeg ingest, and lands it in the folder where the user's
//! processed clips live — then lists, describes and plays what is there.
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

mod home;
mod library;

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use auto_ascii::timecode;
use auto_ascii_factory::BuildRequest;
use clap::{Parser, Subcommand};

use home::{Home, kebab_case, name_for, rfc3339_utc, stem_of};
use library::{AssetInfo, Sidecar, Source, absolute};

/// The error type the whole CLI funnels into — the factory's, so its errors
/// pass through unwrapped and read the same in both binaries.
pub type BoxErr = Box<dyn std::error::Error>;

/// The agent guide, embedded so `auto-ascii agent-guide` and the committed
/// file cannot drift (PLAN-M6-M8 §2).
const AGENT_GUIDE: &str = include_str!("../../../docs/AGENT-GUIDE.md");

#[derive(Parser)]
#[command(
    name = "auto-ascii",
    version,
    about = "Import, list and play ASCII-art video clips (PLAN-M6-M8 §2)",
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
    /// Play a clip interactively (same keys as `auto-ascii-player`).
    Play {
        /// A path if one exists, else `library/<clip>.ascii`.
        clip: String,
    },
    /// Print the embedded agent guide.
    AgentGuide,
    /// Print the resolved home folder, creating it and its subfolders.
    Home,
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
        Cmd::Play { clip } => cmd_play(cli, clip),
        Cmd::AgentGuide => cmd_agent_guide(cli),
        Cmd::Home => cmd_home(cli, &Home::resolve()?),
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
    let created_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let sidecar = Sidecar {
        name: name.to_string(),
        source: Some(Source { path: absolute(video), sha256, bytes: source_bytes }),
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
            (None, Some(src)) => src.path.clone(),
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

fn cmd_play(cli: &Cli, clip: &str) -> Result<(), BoxErr> {
    // Refused before anything else, and long before a terminal session:
    // the player OWNS stdout for the whole run, so no JSON printed around
    // it could ever be the only value there. Saying so beats emitting a
    // document an agent would have to dig out of an animation.
    if cli.json {
        return Err("play is interactive; run it without --json".into());
    }
    let home = Home::resolve()?;
    // The M8 seam: a composition is a .toml and the player cannot map one
    // onto frames yet, so say so instead of failing as "not an asset".
    if std::path::Path::new(clip).extension().is_some_and(|e| e == "toml") {
        return Err(format!(
            "{clip} looks like a composition; playing compositions lands with M8 \
             (`auto-ascii agent-guide` describes the schema). Pass a library \
             clip name or a path to a .ascii asset."
        )
        .into());
    }
    let path = home.resolve_clip(clip)?;
    // Read the header before taking over the terminal: a bad asset should
    // fail as a plain error, not as a dead alternate screen.
    library::describe(&stem_of(&path), &path)?;
    auto_ascii::Player::builder().asset(&path).build()?.run()?;
    Ok(())
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

/// Parse one `--ss`/`--t` argument through the facade's shared grammar,
/// naming the flag in the error the way clap would have.
fn parse_time(spec: Option<&str>, flag: &str) -> Result<Option<f64>, BoxErr> {
    match spec {
        None => Ok(None),
        Some(s) => match timecode::parse(s) {
            Ok(secs) => Ok(Some(secs)),
            Err(e) => Err(format!("{flag} {s:?}: {e}").into()),
        },
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
        Some(src) => {
            println!("  {:<14}{}", "source:", src.path);
            println!("  {:<14}{} ({})", "source bytes:", src.bytes, human_bytes(src.bytes));
            println!("  {:<14}{}", "source sha:", src.sha256);
        }
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
    /// thing to pin is that it is the guide and that it stayed short.
    #[test]
    fn agent_guide_is_embedded_and_short() {
        assert!(AGENT_GUIDE.starts_with("# auto-ascii for agents\n"), "{AGENT_GUIDE:.40}");
        assert!(AGENT_GUIDE.lines().count() < 60, "the guide must stay under 60 lines");
    }
}
