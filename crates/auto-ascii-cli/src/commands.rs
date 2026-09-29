//! The everyday library commands: list, info, cut, compose, stream, home,
//! agent-guide and doctor, plus the clip-recording helpers import shares.

use std::path::{Path, PathBuf};

use auto_ascii::compose::ExportOptions;
use auto_ascii::timecode;
use auto_ascii::tools::Tool;

use crate::args::StreamArgs;
use crate::home::{Home, cut_name, kebab_case, library_name, rfc3339_utc, stem_of};
use crate::library::{self, AssetInfo, Sidecar, Source, absolute, clip_ref};
use crate::output::{fps_text, human_bytes};
use crate::{BoxErr, Cli, composition, deps, stream};

const AGENT_GUIDE: &str = include_str!("../../../docs/AGENT-GUIDE.md");

const STREAM_IS_INTERACTIVE: &str =
    "stream is interactive; run it without --json (or add --sim for one JSON stats line)";

pub fn clip_name(
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

pub fn refuse_existing(name: &str, asset: &Path, force: bool) -> Result<(), BoxErr> {
    if asset.exists() && !force {
        return Err(format!(
            "clip {name:?} already exists at {} — pass --force to replace it",
            asset.display()
        )
        .into());
    }
    Ok(())
}

pub fn record(
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

pub fn orphaned(e: &BoxErr, asset: &Path, cmd: &str) -> BoxErr {
    format!(
        "{e} (the clip landed at {} but has no sidecar; re-run the {cmd} with --force)",
        asset.display()
    )
    .into()
}

pub fn finish_clip(
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

pub fn export_options() -> Result<ExportOptions, BoxErr> {
    let params = auto_ascii_factory::effective_params(None, None, None)?;
    let keyframe_ivl = u8::try_from(params.build.keyframe_ivl).map_err(|_| {
        format!(
            "params build.keyframe_ivl {} does not fit the .ascii header",
            params.build.keyframe_ivl
        )
    })?;
    Ok(ExportOptions { keyframe_ivl, zstd_level: params.build.zstd_level })
}

pub struct CutArgs<'a> {
    pub clip: &'a str,
    pub in_spec: &'a str,
    pub out_spec: &'a str,
    pub name: Option<&'a str>,
    pub force: bool,
}

pub fn cut(cli: &Cli, home: &Home, args: &CutArgs<'_>) -> Result<(), BoxErr> {
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
    comp.resolve()?;
    let opts = export_options()?;
    auto_ascii::compose::export(&comp, &asset, &opts)?;

    library::remove_sidecar(&asset)?;
    let from = clip_ref(home, &from_path);
    let (sidecar, sidecar_path) = record(
        &name,
        Source::cut(from.clone(), in_secs, out_secs),
        library::asset_info(&asset)?,
        &asset,
    )
    .map_err(|e| orphaned(&e, &asset, "cut"))?;
    finish_clip(cli, &format!("cut {name} from {from}"), &sidecar, &sidecar_path)
}

pub fn list(cli: &Cli, home: &Home) -> Result<(), BoxErr> {
    let clips = library::list(home)?;
    if cli.json {
        outln!("{}", serde_json::to_string(&clips)?);
        return Ok(());
    }
    if clips.is_empty() {
        outln!(
            "no clips in {} (add one: auto-ascii add <link-or-file>)",
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

pub fn info(cli: &Cli, home: &Home, clip: &str) -> Result<(), BoxErr> {
    let path = home.resolve_clip(clip)?;
    let sidecar = library::describe(&library::clip_name(home, &path), &path)?;
    if cli.json {
        outln!("{}", serde_json::to_string(&sidecar)?);
    } else {
        outln!("{}", sidecar.name);
        print_clip_body(&sidecar);
    }
    Ok(())
}

pub fn compose_new(cli: &Cli, home: &Home, name: &str) -> Result<(), BoxErr> {
    home.create()?;
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

pub struct AddArgs<'a> {
    pub name: &'a str,
    pub clip: &'a str,
    pub in_spec: Option<&'a str>,
    pub out_spec: Option<&'a str>,
    pub at_spec: Option<&'a str>,
}

pub fn compose_add(cli: &Cli, home: &Home, args: &AddArgs<'_>) -> Result<(), BoxErr> {
    let path = home.resolve_composition(args.name)?;
    let clip = home.resolve_clip(args.clip)?;
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

pub fn compose_show(cli: &Cli, home: &Home, name: &str) -> Result<(), BoxErr> {
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

pub struct ExportArgs<'a> {
    pub name: &'a str,
    pub out: Option<&'a Path>,
    pub force: bool,
}

pub fn compose_export(cli: &Cli, home: &Home, args: &ExportArgs<'_>) -> Result<(), BoxErr> {
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

fn open_composition(home: &Home, path: &Path) -> Result<auto_ascii::Composition, BoxErr> {
    let mut comp = auto_ascii::Composition::from_toml_file(path, Some(&home.library()))?;
    comp.resolve()?;
    Ok(comp)
}

pub fn stream(cli: &Cli, args: &StreamArgs) -> Result<(), BoxErr> {
    if cli.json && args.sim.is_none() {
        return Err(STREAM_IS_INTERACTIVE.into());
    }
    let sim = args.sim.as_deref().map(stream::parse_sim).transpose()?;
    let input = args.input.join(" ");
    if input.trim().is_empty() {
        return Err("stream needs a URL or search terms".into());
    }
    let (ctx, found) = crate::ensure_tools(cli, &[Tool::YtDlp, Tool::Ffmpeg])?;
    let programs = stream::Programs {
        ffmpeg: found.path(Tool::Ffmpeg),
        ffmpeg_ca_file: deps::ca_file(&found),
        ytdlp: found.path(Tool::YtDlp),
        ytdlp_update: deps::ytdlp_updater(&ctx, &found),
    };
    let args = stream::StreamArgs {
        input,
        style: args.style,
        palette: args.palette.into(),
        max_height: args.max_height,
        no_audio: args.no_audio,
        sim,
        sim_dump: args.sim_dump.clone(),
        cookies_from_browser: args.cookies_from_browser.clone(),
    };
    let ran_with = deps::cached_version(&ctx);
    let Err(e) = stream::run(&programs, &args) else { return Ok(()) };
    if args.sim.is_none() {
        stream::default_signals();
        if deps::offer_update(&ctx, &found, ran_with.as_deref(), &e.to_string(), &mut ctx.live()).is_some() {
            return stream::run(&programs, &args);
        }
    }
    Err(e)
}

pub fn doctor(cli: &Cli, fetch: bool) -> Result<(), BoxErr> {
    let ctx = deps::Ctx::new(cli.yes, cli.json);
    if fetch {
        deps::provide(&ctx, &Tool::ALL, true, &mut ctx.live())?;
    }
    let report = deps::report(&ctx);
    if cli.json {
        outln!("{}", serde_json::to_string(&report)?);
        return Ok(());
    }
    let unset = || "(none)".to_string();
    outln!("{:<11}{}", "platform", report.platform);
    outln!("{:<11}{}", "cache", report.bin_dir.clone().unwrap_or_else(unset));
    outln!("{:<11}{}", "downloads", report.downloads);
    for tool in &report.tools {
        let detail = match (&tool.path, &tool.version, &tool.error) {
            (None, _, _) => "not found".to_string(),
            (Some(path), Some(version), _) => format!("{version}  {path}"),
            (Some(path), None, error) => {
                format!("{path} (does not run: {})", error.as_deref().unwrap_or("no version"))
            }
        };
        outln!("{:<11}{:<9}{detail}", tool.name, tool.source);
        if let Some(c) = &tool.cached {
            outln!(
                "{:<11}{:<9}{}, {}, checked {} day{} ago{}",
                "",
                "cached",
                c.version,
                human_bytes(c.bytes),
                c.age_days,
                if c.age_days == 1 { "" } else { "s" },
                if c.stale { " (stale: `auto-ascii doctor --fetch` refreshes it)" } else { "" }
            );
        }
    }
    if report.tools.iter().any(|t| t.path.is_none()) {
        outln!("missing tools download on first use, or now with `{}`", deps::FETCH_COMMAND);
    }
    Ok(())
}

pub fn agent_guide(cli: &Cli) -> Result<(), BoxErr> {
    if cli.json {
        outln!("{}", serde_json::json!({ "guide": AGENT_GUIDE }));
    } else {
        out!("{AGENT_GUIDE}");
    }
    Ok(())
}

pub fn home(cli: &Cli, home: &Home) -> Result<(), BoxErr> {
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

pub fn parse_time(spec: Option<&str>, flag: &str) -> Result<Option<f64>, BoxErr> {
    spec.map(|s| parse_one_time(s, flag)).transpose()
}

pub fn parse_one_time(spec: &str, flag: &str) -> Result<f64, BoxErr> {
    timecode::parse(spec).map_err(|e| format!("{flag} {spec:?}: {e}").into())
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

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
            composition::Row::Gap(gap) => outln!(
                "  {:>3}  {:<w$}  {:>7.2}  {:>7.2}",
                "-", "GAP", gap.start_secs, gap.end_secs
            ),
        }
    }
}

pub fn print_clip_body(sidecar: &Sidecar) {
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
            match bytes {
                Some(n) => outln!("  {:<14}{n} ({})", "source bytes:", human_bytes(*n)),
                None => outln!("  {:<14}{}", "source bytes:", library::UNKNOWN),
            }
            outln!("  {:<14}{}", "source sha:", sha256.as_deref().unwrap_or(library::UNKNOWN));
        }
        Some(cut @ Source::Cut { .. }) => outln!("  {:<14}{}", "source:", cut.summary()),
        None => outln!("  {:<14}(none: no sidecar beside this asset)", "source:"),
    }
    if let Some(created) = &sidecar.created {
        outln!("  {:<14}{}", "created:", created);
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
    fn export_knobs_come_from_the_factory_params() {
        let opts = export_options().expect("the committed params load");
        let params = auto_ascii_factory::effective_params(None, None, None).unwrap();
        assert_eq!(u32::from(opts.keyframe_ivl), params.build.keyframe_ivl);
        assert_eq!(opts.zstd_level, params.build.zstd_level);
    }

    #[test]
    fn facade_export_defaults_match_the_committed_factory_params() {
        assert_eq!(ExportOptions::default(), export_options().unwrap());
    }

    #[test]
    fn agent_guide_is_embedded_and_short() {
        assert!(AGENT_GUIDE.starts_with("# auto-ascii for agents\n"), "{AGENT_GUIDE:.40}");
        assert!(AGENT_GUIDE.lines().count() < 80, "the guide must stay under 80 lines");
    }
}
