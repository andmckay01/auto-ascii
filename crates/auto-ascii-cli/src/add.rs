//! `add`: one step from a link or a local video to a playable clip folder,
//! `<library>/<slug>/` with `<Title>.ascii`, its `<Title>.m4a` soundtrack, a
//! sidecar, a `play.command` launcher and the logs. `source` fetches the
//! video; everything here works on local files, staged in `<slug>.partial`.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use auto_ascii::audio::source::{Tools, probe};
use auto_ascii::tools::Tool;
use auto_ascii_factory::{BuildReport, Programs};

use crate::args::AddArgs;
use crate::commands::print_clip_body;
use crate::home::{Home, kebab_case};
use crate::import::{build_with_defaults, record_import};
use crate::library::absolute;
use crate::source::{self, YtDlpDownloader};
use crate::{BoxErr, Cli};

pub const LAUNCHER: &str = "play.command";

pub const BUILD_LOG: &str = "distill.log";

pub const STAGING_SUFFIX: &str = ".partial";

pub const LENGTH_SLACK_SECS: f64 = 1.0;

pub const LENGTH_SLACK_FRACTION: f64 = 0.01;

pub const TITLE_MAX_CHARS: usize = 120;

const UNSAFE_IN_NAMES: &str = "/\\:*?\"<>|";

struct Built {
    report: BuildReport,
    soundtrack: bool,
    source_secs: Option<f64>,
}

pub fn run(cli: &Cli, args: &AddArgs) -> Result<(), BoxErr> {
    let library = match &args.library {
        Some(dir) => dir.clone(),
        None => Home::resolve()?.library(),
    };
    let downloader = YtDlpDownloader::new(cli);
    let source = source::resolve(&args.input, &downloader)?;
    let title = file_title(&args.title.clone().unwrap_or_else(|| source.title()));
    let slug = kebab_case(&title);
    if slug.is_empty() {
        return Err(format!("cannot name a folder after {title:?}: pass --title").into());
    }
    let folder = library.join(&slug);
    if folder.exists() && !args.force {
        return Err(format!("{} already exists — pass --force to replace it", folder.display()).into());
    }
    if let source::Source::File(path) = &source
        && folder.exists()
        && Path::new(&absolute(path)).starts_with(absolute(&folder))
    {
        return Err(format!(
            "{} lies inside {}, which --force would replace; move it out first",
            path.display(),
            folder.display()
        )
        .into());
    }
    let (_, found) = crate::ensure_tools(cli, &[Tool::Ffmpeg, Tool::Ffprobe])?;
    let programs = Programs { ffmpeg: found.path(Tool::Ffmpeg), ffprobe: found.path(Tool::Ffprobe) };
    let staging = library.join(format!("{slug}{STAGING_SUFFIX}"));
    if staging.exists() {
        std::fs::remove_dir_all(&staging).map_err(|e| format!("remove {}: {e}", staging.display()))?;
    }
    std::fs::create_dir_all(&staging).map_err(|e| format!("create {}: {e}", staging.display()))?;

    let video = source.fetch(&staging, &downloader)?;
    let built = build(cli, &programs, &video, &staging, &title)?;

    if folder.exists() {
        std::fs::remove_dir_all(&folder).map_err(|e| format!("remove {}: {e}", folder.display()))?;
    }
    std::fs::rename(&staging, &folder)
        .map_err(|e| format!("move {} to {}: {e}", staging.display(), folder.display()))?;
    let video = match video.strip_prefix(&staging) {
        Ok(inside) => folder.join(inside),
        Err(_) => video,
    };
    let clip = folder.join(format!("{title}.ascii"));
    let exe = std::env::current_exe().map_err(|e| format!("cannot find the auto-ascii binary: {e}"))?;
    let launcher = write_launcher(&folder, &exe, &clip)?;
    let (sidecar, _) = record_import(&slug, &video, &clip, &built.report)?;
    let soundtrack = built.soundtrack.then(|| absolute(&folder.join(format!("{title}.m4a"))));

    if cli.json {
        let obj = serde_json::json!({
            "name": slug,
            "title": title,
            "folder": absolute(&folder),
            "url": source.url(),
            "soundtrack": soundtrack,
            "launcher": absolute(&launcher),
            "source_duration_secs": built.source_secs,
            "source": sidecar.source,
            "asset": sidecar.asset,
            "created": sidecar.created,
        });
        outln!("{obj}");
        return Ok(());
    }
    outln!("added {title}");
    outln!("  {:<14}{}", "folder:", absolute(&folder));
    if let Some(url) = source.url() {
        outln!("  {:<14}{url}", "link:");
    }
    print_clip_body(&sidecar);
    outln!(
        "  {:<14}{}",
        "soundtrack:",
        soundtrack.as_deref().unwrap_or("none (the source has no audio)")
    );
    let length = match built.source_secs {
        Some(secs) => format!("length matches the source ({secs:.2}s)"),
        None => "the source's length is unknown".to_string(),
    };
    outln!("  {:<14}integrity OK, {length}", "checks:");
    outln!("  {:<14}{}", "launcher:", absolute(&launcher));
    if args.library.is_none() {
        outln!("  {:<14}auto-ascii play {slug}", "play:");
    }
    Ok(())
}

fn build(
    cli: &Cli,
    programs: &Programs,
    video: &Path,
    dir: &Path,
    title: &str,
) -> Result<Built, BoxErr> {
    let clip = dir.join(format!("{title}.ascii"));
    let log_path = dir.join(BUILD_LOG);
    let log = File::create(&log_path).map_err(|e| format!("create {}: {e}", log_path.display()))?;
    let report = build_with_defaults(cli, video, &clip, &mut Tee(log, std::io::stderr()))?;
    let soundtrack = extract_soundtrack(programs, video, &dir.join(format!("{title}.m4a")))?;
    let source_secs = verify(programs, video, &clip)?;
    Ok(Built { report, soundtrack, source_secs })
}

struct Tee<A, B>(A, B);

impl<A: Write, B: Write> Write for Tee<A, B> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write_all(buf)?;
        self.1.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()?;
        self.1.flush()
    }
}

pub fn extract_soundtrack(programs: &Programs, video: &Path, out: &Path) -> Result<bool, BoxErr> {
    if out.exists() {
        std::fs::remove_file(out).map_err(|e| format!("remove {}: {e}", out.display()))?;
    }
    let tools = Tools { ffmpeg: programs.ffmpeg.clone(), ffprobe: programs.ffprobe.clone() };
    if !probe(&tools, video)?.has_audio {
        return Ok(false);
    }
    let copy = ffmpeg_audio(&programs.ffmpeg, video, out, &["-c:a", "copy"])?;
    if copy.is_ok() {
        return Ok(true);
    }
    ffmpeg_audio(&programs.ffmpeg, video, out, &["-c:a", "aac", "-b:a", "192k"])?
        .map_err(|e| format!("ffmpeg could not write the soundtrack {}: {e}", out.display()))?;
    Ok(true)
}

fn ffmpeg_audio(
    ffmpeg: &Path,
    video: &Path,
    out: &Path,
    codec: &[&str],
) -> Result<Result<(), String>, BoxErr> {
    let output = Command::new(ffmpeg)
        .args(["-nostdin", "-v", "error", "-y", "-i"])
        .arg(video)
        .args(["-map", "0:a:0", "-vn"])
        .args(codec)
        .arg(out)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| auto_ascii_factory::ffmpeg::missing_tool("ffmpeg", &e))?;
    if output.status.success() {
        return Ok(Ok(()));
    }
    let _ = std::fs::remove_file(out);
    Ok(Err(String::from_utf8_lossy(&output.stderr).trim().to_string()))
}

fn verify(programs: &Programs, video: &Path, clip: &Path) -> Result<Option<f64>, BoxErr> {
    let report = crate::dev::inspect::collect(clip, None, &[])
        .map_err(|e| format!("{} failed its integrity check: {e}", clip.display()))?;
    let source_secs = auto_ascii_factory::ffmpeg::probe(&programs.ffprobe, video)?.duration_secs;
    if let Some(secs) = source_secs
        && !lengths_match(report.duration_secs, secs)
    {
        return Err(format!(
            "{} runs {:.2}s but the source runs {secs:.2}s",
            clip.display(),
            report.duration_secs
        )
        .into());
    }
    Ok(source_secs)
}

pub fn lengths_match(clip_secs: f64, source_secs: f64) -> bool {
    (clip_secs - source_secs).abs() <= LENGTH_SLACK_SECS.max(source_secs * LENGTH_SLACK_FRACTION)
}

pub fn file_title(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_control() || UNSAFE_IN_NAMES.contains(c) { '-' } else { c })
        .collect();
    let trimmed = cleaned.trim().trim_start_matches('.').trim_start();
    trimmed.chars().take(TITLE_MAX_CHARS).collect::<String>().trim_end().to_string()
}

pub fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

pub fn launcher(exe: &Path, clip: &Path) -> String {
    format!(
        "#!/bin/sh\nexec {} play {} --loop\n",
        sh_quote(&exe.to_string_lossy()),
        sh_quote(&clip.to_string_lossy())
    )
}

pub fn write_launcher(folder: &Path, exe: &Path, clip: &Path) -> Result<PathBuf, BoxErr> {
    let path = folder.join(LAUNCHER);
    std::fs::write(&path, launcher(exe, Path::new(&absolute(clip))))
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod {}: {e}", path.display()))?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::procs::ScratchDir;

    #[test]
    fn titles_become_safe_file_names() {
        assert_eq!(file_title("Iron Man"), "Iron Man");
        assert_eq!(file_title("Kiki's Delivery Service"), "Kiki's Delivery Service");
        assert_eq!(file_title("AC/DC: Thunderstruck | Live"), "AC-DC- Thunderstruck - Live");
        assert_eq!(file_title("  ..hidden\n"), "hidden-");
        assert_eq!(file_title(&"x".repeat(300)).chars().count(), TITLE_MAX_CHARS);
        assert_eq!(kebab_case(&file_title("Mad Max: Fury Road (2015)")), "mad-max-fury-road-2015");
    }

    #[test]
    fn lengths_match_within_a_second_or_a_percent() {
        assert!(lengths_match(420.77, 420.75));
        assert!(lengths_match(2.9, 2.0));
        assert!(!lengths_match(3.1, 2.0));
        assert!(lengths_match(3600.0 + 30.0, 3600.0));
        assert!(!lengths_match(3600.0 + 40.0, 3600.0));
    }

    #[test]
    fn quoting_survives_spaces_and_apostrophes() {
        assert_eq!(sh_quote("plain"), "'plain'");
        assert_eq!(sh_quote("Kiki's Clip"), r"'Kiki'\''s Clip'");
    }

    #[cfg(unix)]
    #[test]
    fn the_launcher_execs_the_binary_on_the_clip_and_is_executable() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = ScratchDir::new().unwrap();
        let folder = scratch.path().join("Kiki's $HOME `x` folder");
        std::fs::create_dir_all(&folder).unwrap();
        let clip = folder.join("Kiki's \"Delivery\" Service.ascii");
        std::fs::write(&clip, b"clip").unwrap();
        let exe = scratch.path().join("bin dir's").join("auto ascii");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        let argv = scratch.path().join("argv.txt");
        std::fs::write(&exe, format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n", sh_quote(&argv.to_string_lossy())))
            .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();

        let path = write_launcher(&folder, &exe, &clip).unwrap();
        assert_eq!(path, folder.join("play.command"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, launcher(&exe, Path::new(&absolute(&clip))));
        assert!(text.starts_with("#!/bin/sh\nexec '"), "{text}");
        assert!(text.ends_with(" --loop\n"), "{text}");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o111, 0o111);

        let status = Command::new(&path).env("HOME", "/elsewhere").status().unwrap();
        assert!(status.success());
        let args = std::fs::read_to_string(&argv).unwrap();
        assert_eq!(args, format!("play\n{}\n--loop\n", absolute(&clip)));
    }

    fn tools() -> Option<Programs> {
        let found = Tools::find().ok().or_else(|| {
            eprintln!("skipping: ffmpeg/ffprobe not found");
            None
        })?;
        Some(Programs { ffmpeg: found.ffmpeg, ffprobe: found.ffprobe })
    }

    fn test_video(programs: &Programs, path: &Path, with_sound: bool) {
        let mut cmd = Command::new(&programs.ffmpeg);
        cmd.args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=64x36:rate=10:duration=2"]);
        if with_sound {
            cmd.args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100:duration=2", "-c:a", "aac"]);
        }
        let status = cmd.args(["-pix_fmt", "yuv420p"]).arg(path).status().unwrap();
        assert!(status.success());
    }

    #[test]
    fn the_soundtrack_is_copied_out_beside_the_clip() {
        let Some(programs) = tools() else { return };
        let scratch = ScratchDir::new().unwrap();
        let video = scratch.path().join("with sound.mp4");
        test_video(&programs, &video, true);
        let out = scratch.path().join("Kiki's Clip.m4a");
        assert!(extract_soundtrack(&programs, &video, &out).unwrap());
        let tools = Tools { ffmpeg: programs.ffmpeg.clone(), ffprobe: programs.ffprobe.clone() };
        let track = probe(&tools, &out).unwrap();
        assert!(track.has_audio);
        assert!((track.secs.unwrap() - 2.0).abs() < 0.2, "{track:?}");
        assert!(auto_ascii::audio::source::discover(&scratch.path().join("Kiki's Clip.ascii"))
            .is_some_and(|t| t.path() == out));
    }

    #[test]
    fn a_silent_source_has_no_soundtrack() {
        let Some(programs) = tools() else { return };
        let scratch = ScratchDir::new().unwrap();
        let video = scratch.path().join("silent.mov");
        test_video(&programs, &video, false);
        let out = scratch.path().join("clip.m4a");
        std::fs::write(&out, b"stale").unwrap();
        assert!(!extract_soundtrack(&programs, &video, &out).unwrap());
        assert!(!out.exists(), "a stale soundtrack is not left behind");
    }
}
