//! Where `add` gets its video: a local file is used where it lies, a link is
//! identified and downloaded by a [`Downloader`] into the clip's folder. The
//! yt-dlp downloader reuses the stream's input classification and metadata
//! resolver; import itself only ever sees the local file this returns.

use std::cell::OnceCell;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use auto_ascii::tools::Tool;

use crate::deps::{self, Resolved};
use crate::home::stem_of;
use crate::output::terminal_safe_line;
use crate::stream::procs::{Procs, missing};
use crate::stream::ytdlp::{BASE, Event, Input, YtDlp, clean_error};
use crate::{BoxErr, Cli};

pub const DOWNLOAD_NAME: &str = "source.mp4";

pub const DOWNLOAD_LOG: &str = "download.log";

pub const HEIGHTS: [u32; 3] = [1080, 1080, 720];

pub const TRANSIENT: [&str; 5] = ["http error 403", "timed out", "stalled", "connection reset", "fragment"];

pub struct Failure {
    pub message: String,
    pub transient: bool,
}

pub fn transient(message: &str) -> bool {
    let text = message.to_lowercase();
    TRANSIENT.iter().any(|pattern| text.contains(pattern))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub url: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    File(PathBuf),
    Remote(Remote),
}

pub trait Downloader {
    fn identify(&self, url: &str) -> Result<Remote, BoxErr>;
    fn download(&self, remote: &Remote, dir: &Path) -> Result<PathBuf, BoxErr>;
}

pub fn resolve(raw: &str, downloader: &dyn Downloader) -> Result<Source, BoxErr> {
    let path = Path::new(raw);
    if path.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("ascii")) {
        return Err(format!("{raw} is already a clip: auto-ascii play {raw}").into());
    }
    if path.is_file() {
        return Ok(Source::File(path.to_path_buf()));
    }
    if path.is_dir() {
        return Err(format!("{raw} is a folder; give a video file or a link").into());
    }
    match Input::classify(raw) {
        Input::Search(_) => Err(format!("{raw:?} is neither a video file nor a link").into()),
        link => downloader.identify(&link_url(&link)).map(Source::Remote),
    }
}

fn link_url(input: &Input) -> String {
    match input {
        Input::Video(u)
        | Input::Playlist(u)
        | Input::Channel(u)
        | Input::SearchUrl(u)
        | Input::Other(u)
        | Input::Search(u) => u.clone(),
    }
}

impl Source {
    pub fn title(&self) -> String {
        match self {
            Source::File(path) => stem_of(path),
            Source::Remote(remote) => remote.title.clone(),
        }
    }

    pub fn url(&self) -> Option<&str> {
        match self {
            Source::File(_) => None,
            Source::Remote(remote) => Some(&remote.url),
        }
    }

    pub fn fetch(&self, dir: &Path, downloader: &dyn Downloader) -> Result<PathBuf, BoxErr> {
        match self {
            Source::File(path) => Ok(path.clone()),
            Source::Remote(remote) => downloader.download(remote, dir),
        }
    }
}

pub struct YtDlpDownloader<'a> {
    cli: &'a Cli,
    tools: OnceCell<(deps::Ctx, Resolved)>,
}

impl<'a> YtDlpDownloader<'a> {
    pub fn new(cli: &'a Cli) -> YtDlpDownloader<'a> {
        YtDlpDownloader { cli, tools: OnceCell::new() }
    }

    fn tools(&self) -> Result<&(deps::Ctx, Resolved), BoxErr> {
        if let Some(tools) = self.tools.get() {
            return Ok(tools);
        }
        let tools = crate::ensure_tools(self.cli, &[Tool::YtDlp, Tool::Ffmpeg, Tool::Ffprobe])?;
        Ok(self.tools.get_or_init(|| tools))
    }

    fn say(&self, message: &str) {
        if !self.cli.json {
            crate::output::emit_err(&format!("auto-ascii: {}\n", terminal_safe_line(message)));
        }
    }
}

impl Downloader for YtDlpDownloader<'_> {
    fn identify(&self, url: &str) -> Result<Remote, BoxErr> {
        let (ctx, found) = self.tools()?;
        let ytdlp = YtDlp::new(found.path(Tool::YtDlp), Procs::new(), &std::env::temp_dir());
        let input = Input::classify(url);
        let update = deps::ytdlp_updater(ctx, found);
        let guarded = update.map(|update| move || update(&deps::fetch::never));
        let mut video_url = url.to_string();
        let ran_with = deps::cached_version(ctx);
        let mut attempt = || {
            ytdlp.resolve_or_update(
                &input,
                HEIGHTS[0],
                &mut |event| {
                    if let Event::EntryFound(found) = event {
                        video_url = found;
                    }
                },
                guarded.as_ref().map(|g| g as &(dyn Fn() -> Result<Option<String>, String> + Send + Sync)),
                &mut |note| self.say(&note),
            )
        };
        let media = match attempt() {
            Ok(media) => media,
            Err(e) => {
                let retry = deps::offer_update(ctx, found, ran_with.as_deref(), &e, &mut ctx.live());
                if retry.is_none() {
                    return Err(e.into());
                }
                attempt()?
            }
        };
        Ok(Remote { url: video_url, title: media.title })
    }

    fn download(&self, remote: &Remote, dir: &Path) -> Result<PathBuf, BoxErr> {
        let (_, found) = self.tools()?;
        let log = dir.join(DOWNLOAD_LOG);
        let target = dir.join(DOWNLOAD_NAME);
        with_retries(|attempt, height| {
            let tried = if attempt == 0 { "downloading" } else { "retrying" };
            self.say(&format!("{tried} {} (up to {height}p)", remote.url));
            let args = download_args(&found.path(Tool::Ffmpeg), dir, height, &remote.url);
            run_logged(Command::new(found.path(Tool::YtDlp)).args(&args), &log, !self.cli.json)?;
            if target.is_file() {
                return Ok(target.clone());
            }
            Err(Failure {
                message: format!("yt-dlp finished but wrote no {}", target.display()),
                transient: false,
            })
        })
    }
}

pub fn with_retries(
    mut attempt: impl FnMut(usize, u32) -> Result<PathBuf, Failure>,
) -> Result<PathBuf, BoxErr> {
    let mut last = String::new();
    for (n, height) in HEIGHTS.iter().enumerate() {
        match attempt(n, *height) {
            Ok(path) => return Ok(path),
            Err(failure) if failure.transient => last = failure.message,
            Err(failure) => return Err(failure.message.into()),
        }
    }
    Err(last.into())
}

pub fn download_args(ffmpeg: &Path, dir: &Path, height: u32, url: &str) -> Vec<String> {
    let format = format!(
        "bv*[height<={height}]+ba[acodec^=mp4a]/bv*[height<={height}]+ba/b[height<={height}]"
    );
    let template = dir.join("source.%(ext)s");
    BASE.iter()
        .chain(&[
        "--no-playlist",
        "--newline",
        "--retries",
        "10",
        "--fragment-retries",
        "10",
        "--socket-timeout",
        "30",
        "--ffmpeg-location",
    ])
    .map(|a| a.to_string())
    .chain([ffmpeg.display().to_string(), "-f".into(), format])
    .chain(["--merge-output-format", "mp4", "--remux-video", "mp4", "-o"].map(String::from))
    .chain([template.display().to_string(), "--".into(), url.to_string()])
    .collect()
}

fn run_logged(cmd: &mut Command, log: &Path, echo: bool) -> Result<(), Failure> {
    let fatal = |message: String| Failure { message, transient: false };
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .map_err(|e| fatal(format!("open {}: {e}", log.display())))?;
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| fatal(missing("yt-dlp", &e)))?;
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let errors = std::thread::scope(|scope| {
        scope.spawn(|| copy_lines(stdout, &file, echo));
        copy_lines(stderr, &file, echo)
    });
    let status = child.wait().map_err(|e| fatal(format!("yt-dlp: {e}")))?;
    if status.success() {
        return Ok(());
    }
    let message = clean_error(&errors, status.code());
    Err(Failure { transient: transient(&message), message })
}

fn copy_lines(from: impl Read, mut log: &File, echo: bool) -> String {
    let mut seen = String::new();
    for line in BufReader::new(from).lines().map_while(Result::ok) {
        let _ = writeln!(log, "{line}");
        if echo {
            crate::output::emit_err(&format!("{}\n", terminal_safe_line(after_carriage_returns(&line))));
        }
        seen.push_str(&line);
        seen.push('\n');
    }
    seen
}

fn after_carriage_returns(line: &str) -> &str {
    let line = line.trim_end_matches(['\r', '\n']);
    line.rsplit_once('\r').map_or(line, |(_, last)| last)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::stream::procs::ScratchDir;

    #[derive(Default)]
    struct Fake {
        identified: RefCell<Vec<String>>,
        downloaded: RefCell<Vec<(String, PathBuf)>>,
    }

    impl Downloader for Fake {
        fn identify(&self, url: &str) -> Result<Remote, BoxErr> {
            self.identified.borrow_mut().push(url.to_string());
            Ok(Remote { url: format!("{url}#resolved"), title: "Iron Man: Suit Up".into() })
        }

        fn download(&self, remote: &Remote, dir: &Path) -> Result<PathBuf, BoxErr> {
            self.downloaded.borrow_mut().push((remote.url.clone(), dir.to_path_buf()));
            let path = dir.join(DOWNLOAD_NAME);
            std::fs::write(&path, b"video").unwrap();
            Ok(path)
        }
    }

    #[test]
    fn a_local_file_is_used_in_place_and_never_downloaded() {
        let scratch = ScratchDir::new().unwrap();
        let video = scratch.path().join("My Clip (final).mov");
        std::fs::write(&video, b"video").unwrap();
        let fake = Fake::default();
        let source = resolve(video.to_str().unwrap(), &fake).unwrap();
        assert_eq!(source, Source::File(video.clone()));
        assert_eq!(source.title(), "My Clip (final)");
        assert_eq!(source.url(), None);
        assert_eq!(source.fetch(Path::new("/nowhere"), &fake).unwrap(), video);
        assert!(fake.identified.borrow().is_empty() && fake.downloaded.borrow().is_empty());
    }

    #[test]
    fn a_link_is_identified_then_downloaded_into_the_folder() {
        let scratch = ScratchDir::new().unwrap();
        let fake = Fake::default();
        let source = resolve("youtu.be/RUSCKKd9mRw", &fake).unwrap();
        assert_eq!(*fake.identified.borrow(), ["https://youtu.be/RUSCKKd9mRw"]);
        assert_eq!(source.title(), "Iron Man: Suit Up");
        assert_eq!(source.url(), Some("https://youtu.be/RUSCKKd9mRw#resolved"));
        assert!(fake.downloaded.borrow().is_empty(), "identifying downloads nothing");
        let video = source.fetch(scratch.path(), &fake).unwrap();
        assert_eq!(video, scratch.path().join("source.mp4"));
        assert_eq!(
            *fake.downloaded.borrow(),
            [("https://youtu.be/RUSCKKd9mRw#resolved".to_string(), scratch.path().to_path_buf())]
        );
        resolve("https://vimeo.com/123", &fake).unwrap();
        assert_eq!(fake.identified.borrow()[1], "https://vimeo.com/123");
    }

    #[test]
    fn anything_else_is_refused_without_touching_the_downloader() {
        let scratch = ScratchDir::new().unwrap();
        let fake = Fake::default();
        let missing = scratch.path().join("nope.mp4");
        let err = resolve(missing.to_str().unwrap(), &fake).unwrap_err().to_string();
        assert!(err.ends_with("is neither a video file nor a link"), "{err}");
        let err = resolve("me at the zoo", &fake).unwrap_err().to_string();
        assert!(err.contains("neither a video file nor a link"), "{err}");
        let err = resolve(scratch.path().to_str().unwrap(), &fake).unwrap_err().to_string();
        assert!(err.ends_with("is a folder; give a video file or a link"), "{err}");
        assert!(fake.identified.borrow().is_empty());
    }

    #[test]
    fn an_ascii_file_is_already_a_clip() {
        let scratch = ScratchDir::new().unwrap();
        let clip = scratch.path().join("done.ASCII");
        std::fs::write(&clip, b"clip").unwrap();
        let fake = Fake::default();
        let err = resolve(clip.to_str().unwrap(), &fake).unwrap_err().to_string();
        assert_eq!(err, format!("{0} is already a clip: auto-ascii play {0}", clip.display()));
    }

    #[test]
    fn only_transient_errors_are_retried() {
        let retried = |stderr: &str| transient(&clean_error(stderr, Some(1)));
        for stderr in [
            "ERROR: unable to download video data: HTTP Error 403: Forbidden",
            "ERROR: Read timed out.",
            "ERROR: The download stalled",
            "ERROR: [Errno 54] Connection reset by peer",
            "ERROR: fragment 3 not found, unable to continue",
            "WARNING: [youtube] ab403cdEFgh: Private video\nERROR: [youtube] ab403cdEFgh: HTTP Error 403: Forbidden",
        ] {
            assert!(retried(stderr), "{stderr}");
        }
        for stderr in [
            "ERROR: [youtube] x: Private video",
            "ERROR: [youtube] x: Video unavailable",
            "ERROR: [youtube] ab403cdEFgh: Video unavailable",
            "ERROR: Unsupported URL: https://example.com/watch/403",
            "ERROR: ffmpeg not found. Please install or provide the path using --ffmpeg-location",
            "WARNING: HTTP Error 403: Forbidden, retrying\nERROR: [youtube] x: Sign in to confirm your age",
        ] {
            assert!(!retried(stderr), "{stderr}");
        }
    }

    fn fail(message: &str, transient: bool) -> Failure {
        Failure { message: message.into(), transient }
    }

    #[test]
    fn a_lasting_error_fails_fast_and_a_transient_one_falls_back_to_720p() {
        let mut heights = Vec::new();
        let err = with_retries(|_, h| {
            heights.push(h);
            Err(fail("private", false))
        })
        .unwrap_err();
        assert_eq!((err.to_string().as_str(), heights.as_slice()), ("private", [1080].as_slice()));

        let mut heights = Vec::new();
        let got = with_retries(|n, h| {
            heights.push(h);
            if n == 0 { Err(fail("403", true)) } else { Ok(PathBuf::from("ok")) }
        });
        assert_eq!((got.unwrap(), heights.as_slice()), (PathBuf::from("ok"), [1080, 1080].as_slice()));

        let mut heights = Vec::new();
        let err = with_retries(|n, h| {
            heights.push(h);
            Err(fail(&format!("stall {n}"), true))
        })
        .unwrap_err();
        assert_eq!((err.to_string().as_str(), heights.as_slice()), ("stall 2", [1080, 1080, 720].as_slice()));
    }

    #[test]
    fn the_download_asks_for_mp4_under_the_height_and_never_for_cookies() {
        let args = download_args(Path::new("/bin/ffmpeg"), Path::new("/lib/x.partial"), 720, "-u");
        assert_eq!(args[0], "--ignore-config");
        assert!(!args.iter().any(|a| a.contains("cookies")), "{args:?}");
        let at = |flag: &str| args[args.iter().position(|a| a == flag).unwrap() + 1].clone();
        assert_eq!(at("-f"), "bv*[height<=720]+ba[acodec^=mp4a]/bv*[height<=720]+ba/b[height<=720]");
        assert_eq!(at("--merge-output-format"), "mp4");
        assert_eq!(at("--remux-video"), "mp4");
        assert_eq!(at("--ffmpeg-location"), "/bin/ffmpeg");
        assert_eq!(at("-o"), "/lib/x.partial/source.%(ext)s");
        assert_eq!(args[args.len() - 2..], ["--", "-u"]);
        assert_eq!(HEIGHTS, [1080, 1080, 720], "1080p, a retry, then 720p");
    }

    #[test]
    fn an_echoed_line_keeps_only_what_its_carriage_returns_leave_on_screen() {
        let ffmpeg = "frame=    1 fps=0.0 q=-1.0\rframe=   60 fps=30 q=-1.0\rframe=  120 fps=30 q=-1.0";
        assert_eq!(after_carriage_returns(ffmpeg), "frame=  120 fps=30 q=-1.0");
        assert_eq!(after_carriage_returns("[download]   1.0% of 2.00MiB\r[download]  50.0% of 2.00MiB\r"), "[download]  50.0% of 2.00MiB");
        assert_eq!(after_carriage_returns("[download] Destination: source.mp4\r\n"), "[download] Destination: source.mp4");
        assert_eq!(after_carriage_returns("ERROR: from a Windows tool\r"), "ERROR: from a Windows tool");
        assert_eq!(after_carriage_returns("no carriage return"), "no carriage return");
        assert_eq!(after_carriage_returns("\r"), "");
    }
}
