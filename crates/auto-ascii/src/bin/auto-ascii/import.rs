//! `import`: build a video into a `.ascii` asset, either into the library
//! (with a provenance sidecar) or, with `-o`, to exactly the path given.
//! Both modes share one BuildOptions -> BuildRequest adapter, which `add`
//! also drives with the defaults.

use std::io::Write;
use std::path::{Path, PathBuf};

use auto_ascii::tools::Tool;
use auto_ascii_factory::{BuildReport, BuildRequest, Programs};

use crate::args::{BuildOptions, ImportArgs};
use crate::commands::{clip_name, finish_clip, orphaned, parse_time, record, refuse_existing};
use crate::home::{Home, name_for};
use crate::library::{self, AssetInfo, Source, absolute};
use crate::output::{fps_text, human_bytes};
use crate::{BoxErr, Cli};

struct Build {
    ss: Option<f64>,
    t: Option<f64>,
    res: Option<(u16, u16)>,
}

impl Build {
    fn parse(opts: &BuildOptions) -> Result<Build, BoxErr> {
        let ss = parse_time(opts.ss.as_deref(), "--ss")?;
        let t = parse_time(opts.t.as_deref(), "--t")?;
        if let (Some(secs), Some(spec)) = (t, &opts.t)
            && secs <= 0.0
        {
            return Err(format!("--t {spec:?}: the duration must be longer than zero").into());
        }
        let res = opts.res.as_deref().map(auto_ascii_factory::parse_res).transpose()?;
        auto_ascii_factory::effective_params(opts.params.as_deref(), opts.fps, res)?;
        Ok(Build { ss, t, res })
    }

    fn run(
        &self,
        cli: &Cli,
        opts: &BuildOptions,
        video: &Path,
        output: &Path,
        info: &mut dyn Write,
    ) -> Result<BuildReport, BoxErr> {
        let (_, found) = crate::ensure_tools(cli, &[Tool::Ffmpeg, Tool::Ffprobe])?;
        let programs =
            Programs { ffmpeg: found.path(Tool::Ffmpeg), ffprobe: found.path(Tool::Ffprobe) };
        auto_ascii_factory::build(
            &BuildRequest {
                input: video,
                output,
                params: opts.params.as_deref(),
                ss: self.ss,
                t: self.t,
                fps: opts.fps,
                res: self.res,
                programs: &programs,
            },
            info,
        )
    }
}

pub fn build_with_defaults(
    cli: &Cli,
    video: &Path,
    output: &Path,
    info: &mut dyn Write,
) -> Result<BuildReport, BoxErr> {
    let opts = BuildOptions::default();
    Build::parse(&opts)?.run(cli, &opts, video, output, info)
}

pub fn run(cli: &Cli, args: &ImportArgs) -> Result<(), BoxErr> {
    match &args.output {
        Some(output) => to_path(cli, args, output),
        None => to_library(cli, &Home::resolve()?, args),
    }
}

fn to_library(cli: &Cli, home: &Home, args: &ImportArgs) -> Result<(), BoxErr> {
    let build = Build::parse(&args.build)?;
    home.create()?;
    if !args.video.is_file() {
        return Err(format!("input not found: {}", args.video.display()).into());
    }
    let name = clip_name(args.name.as_deref(), || name_for(&args.video))?;
    let asset = home.clip_path(&name);
    refuse_existing(&name, &asset, args.force)?;

    let report = build.run(cli, &args.build, &args.video, &asset, &mut std::io::stderr())?;

    library::remove_sidecar(&asset)?;
    let (sidecar, sidecar_path) = record_import(&name, &args.video, &asset, &report)
        .map_err(|e| orphaned(&e, &asset, "import"))?;
    finish_clip(cli, &format!("imported {name}"), &sidecar, &sidecar_path)
}

fn to_path(cli: &Cli, args: &ImportArgs, output: &Path) -> Result<(), BoxErr> {
    let build = Build::parse(&args.build)?;
    if !args.video.is_file() {
        return Err(format!("input not found: {}", args.video.display()).into());
    }
    let replacing = output.exists();
    if replacing && !args.force {
        return Err(
            format!("{} already exists — pass --force to replace it", output.display()).into()
        );
    }
    if output.is_dir() {
        return Err(format!("{} is a directory; -o needs a file path", output.display()).into());
    }
    let parent = output.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    if !parent.is_dir() {
        return Err(format!("output folder {} does not exist", parent.display()).into());
    }

    let report = build.run(cli, &args.build, &args.video, output, &mut std::io::stderr())?;
    if replacing {
        retire_stale_provenance(output)?;
    }

    if cli.json {
        let obj = serde_json::json!({
            "path": absolute(output),
            "frames": report.frames,
            "fps": report.fps,
            "duration_secs": report.duration_secs,
            "base_w": report.base_w,
            "base_h": report.base_h,
            "bytes": report.bytes,
        });
        outln!("{obj}");
    } else {
        outln!("wrote {}", output.display());
        outln!("  {:<14}{}", "path:", absolute(output));
        outln!("  {:<14}{} ({})", "bytes:", report.bytes, human_bytes(report.bytes));
        outln!(
            "  {:<14}{} ({:.2}s @ {} fps)",
            "frames:",
            report.frames,
            report.duration_secs,
            fps_text(report.fps)
        );
        outln!("  {:<14}{}x{}", "base res:", report.base_w, report.base_h);
    }
    Ok(())
}

fn retire_stale_provenance(asset: &Path) -> Result<(), BoxErr> {
    match library::read_provenance(asset) {
        Ok(p) if p.source.is_some() => library::remove_sidecar(asset),
        _ => Ok(()),
    }
}

pub fn record_import(
    name: &str,
    video: &Path,
    asset: &Path,
    report: &BuildReport,
) -> Result<(library::Sidecar, PathBuf), BoxErr> {
    let source_bytes = std::fs::metadata(video)
        .map_err(|e| format!("stat {}: {e}", video.display()))?
        .len();
    let sha256 = auto_ascii_factory::sha256_file(video)
        .map_err(|e| format!("hash {}: {e}", video.display()))?;
    record(
        name,
        Source::Video { path: absolute(video), sha256: Some(sha256), bytes: Some(source_bytes) },
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
