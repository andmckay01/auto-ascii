//! yt-dlp in one place: classify the input, walk a playlist, channel or
//! search down to its first video, then read that video's stream URLs and
//! metadata. The program path is injectable (`AUTO_ASCII_YTDLP`) so tests
//! can stand in a fake script; a failing cached copy is updated and retried.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use super::procs::Procs;

const PREFIX: [&str; 5] = ["--ignore-config", "--no-cache-dir", "--simulate", "--skip-download", "--no-warnings"];

const MAX_DEPTH: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Video(String),
    Playlist(String),
    Channel(String),
    SearchUrl(String),
    Search(String),
    Other(String),
}

impl Input {
    pub fn classify(raw: &str) -> Input {
        let raw = raw.trim();
        let Some(url) = as_url(raw) else {
            return Input::Search(raw.split_whitespace().collect::<Vec<_>>().join(" "));
        };
        let rest = url.split_once("://").map_or(url.as_str(), |(_, r)| r);
        let (host, path) = match rest.find(['/', '?', '#']) {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        let host = host.to_ascii_lowercase();
        let host = host.strip_prefix("www.").unwrap_or(&host);
        let host = host.strip_prefix("m.").unwrap_or(host);
        let host = host.strip_prefix("music.").unwrap_or(host);
        if host == "youtu.be" {
            return Input::Video(url);
        }
        if host != "youtube.com" && host != "youtube-nocookie.com" {
            return Input::Other(url);
        }
        let (route, query) = path.split_once('?').unwrap_or((path, ""));
        let has = |key: &str| query.split('&').any(|kv| kv.split('=').next() == Some(key));
        let watch = (route == "/watch" || route == "/watch/") && has("v");
        if watch || ["/shorts/", "/live/", "/embed/", "/v/"].iter().any(|p| route.starts_with(p)) {
            Input::Video(url)
        } else if route.starts_with("/playlist") && has("list") {
            Input::Playlist(url)
        } else if route.starts_with("/results") {
            Input::SearchUrl(url)
        } else if ["/@", "/channel/", "/c/", "/user/"].iter().any(|p| route.starts_with(p)) {
            Input::Channel(url)
        } else {
            Input::Other(url)
        }
    }

    fn target(&self) -> String {
        match self {
            Input::Search(terms) => format!("ytsearch1:{terms}"),
            Input::Video(u) | Input::Playlist(u) | Input::Channel(u) | Input::SearchUrl(u) | Input::Other(u) => {
                u.clone()
            }
        }
    }
}

fn as_url(raw: &str) -> Option<String> {
    if raw.contains(char::is_whitespace) || raw.is_empty() {
        return None;
    }
    let lower = raw.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        return Some(raw.to_string());
    }
    let bare = ["youtube.com/", "www.youtube.com/", "m.youtube.com/", "youtu.be/", "music.youtube.com/"];
    if bare.iter().any(|p| lower.starts_with(p)) {
        return Some(format!("https://{raw}"));
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub url: String,
    pub format_id: String,
    pub protocol: String,
    pub headers: Vec<(String, String)>,
}

impl Track {
    pub fn is_hls(&self) -> bool {
        self.protocol.starts_with("m3u8")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Media {
    pub id: String,
    pub title: String,
    pub duration: Option<f64>,
    pub fps: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub video: Vec<Track>,
    pub audio: Vec<Track>,
}

pub enum Event {
    Started,
    EntryFound(String),
}

pub struct YtDlp {
    program: PathBuf,
    procs: Procs,
    cwd: PathBuf,
    cookies_from_browser: Option<String>,
}

impl YtDlp {
    pub fn new(program: impl Into<PathBuf>, procs: Procs, cwd: &Path) -> YtDlp {
        YtDlp { program: program.into(), procs, cwd: cwd.to_path_buf(), cookies_from_browser: None }
    }

    pub fn cookies_from_browser(mut self, browser: Option<&str>) -> YtDlp {
        self.cookies_from_browser = browser.filter(|b| !b.trim().is_empty()).map(str::to_string);
        self
    }

    fn metadata_args(&self, rest: &[&str]) -> Vec<String> {
        let mut args: Vec<String> = PREFIX.map(String::from).to_vec();
        if let Some(browser) = &self.cookies_from_browser {
            args.push("--cookies-from-browser".into());
            args.push(browser.clone());
        }
        args.extend(rest.iter().map(|a| a.to_string()));
        args
    }

    pub fn selector(max_height: u32) -> String {
        format!("b[height<={max_height}][vcodec!=none][acodec!=none]/bv*[height<={max_height}]+ba/b")
    }

    pub fn resolve(
        &self,
        input: &Input,
        max_height: u32,
        on: &mut dyn FnMut(Event),
    ) -> Result<Media, String> {
        on(Event::Started);
        let video_url = match input {
            Input::Video(url) => url.clone(),
            other => self.first_entry(&other.target(), input, 0)?,
        };
        on(Event::EntryFound(video_url.clone()));
        let json = self.run_json(&["--no-playlist", "-f", &Self::selector(max_height), "-J", "--", &video_url])?;
        media_from_json(&json, max_height)
    }

    pub fn resolve_or_update(
        &self,
        input: &Input,
        max_height: u32,
        on: &mut dyn FnMut(Event),
        update: Option<&(dyn Fn() -> Result<Option<String>, String> + Send + Sync)>,
        note: &mut dyn FnMut(String),
    ) -> Result<Media, String> {
        let first = self.resolve(input, max_height, on);
        let (Err(e), Some(update)) = (&first, update) else { return first };
        if !failed_itself(e) {
            return first;
        }
        match update() {
            Ok(Some(version)) => {
                note(format!("the cached yt-dlp failed ({e}); updated it to {version} and retried"));
                self.resolve(input, max_height, on)
            }
            Ok(None) => first,
            Err(u) => {
                note(format!("the cached yt-dlp failed; updating it failed too: {u}"));
                first
            }
        }
    }

    fn first_entry(&self, target: &str, input: &Input, depth: usize) -> Result<String, String> {
        let json = self.run_json(&["--flat-playlist", "-I", "1", "-J", "--", target])?;
        let is_list = json["_type"].as_str() == Some("playlist") || json.get("entries").is_some();
        if !is_list {
            return json["webpage_url"]
                .as_str()
                .or_else(|| json["original_url"].as_str())
                .map_or_else(|| Ok(target.to_string()), |u| Ok(u.to_string()));
        }
        let Some(entry) = json["entries"].as_array().and_then(|e| e.iter().find(|v| !v.is_null()))
        else {
            return Err(no_results(input));
        };
        let nested = entry["_type"].as_str() == Some("playlist")
            || matches!(entry["ie_key"].as_str(), Some("YoutubeTab"));
        let url = entry_url(entry).ok_or_else(|| no_results(input))?;
        if nested {
            if depth + 1 >= MAX_DEPTH {
                return Err(format!("no video found within {MAX_DEPTH} levels of {}", input.target()));
            }
            return self.first_entry(&url, input, depth + 1);
        }
        Ok(url)
    }

    fn run_json(&self, args: &[&str]) -> Result<Value, String> {
        let mut cmd = Command::new(&self.program);
        cmd.args(self.metadata_args(args)).current_dir(&self.cwd);
        let spawned = self.procs.spawn(&mut cmd, "yt-dlp")?;
        let mut stderr = spawned.stderr;
        let drain = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf);
            buf
        });
        let mut stdout = spawned.stdout;
        let mut out = Vec::new();
        let read = stdout.read_to_end(&mut out);
        let status = self.procs.reap(spawned.id);
        let err = drain.join().unwrap_or_default();
        let ok = status.is_some_and(|s| s.success());
        if !ok || read.is_err() {
            return Err(clean_error(&String::from_utf8_lossy(&err), status.and_then(|s| s.code())));
        }
        serde_json::from_slice(&out).map_err(|e| format!("yt-dlp printed unparseable JSON: {e}"))
    }
}

fn no_results(input: &Input) -> String {
    match input {
        Input::Search(terms) => format!("no videos found for {terms:?}"),
        other => format!("no videos found at {}", other.target()),
    }
}

fn entry_url(entry: &Value) -> Option<String> {
    if let Some(url) = entry["url"].as_str().or_else(|| entry["webpage_url"].as_str()) {
        return Some(url.to_string());
    }
    let id = entry["id"].as_str()?;
    match entry["ie_key"].as_str() {
        Some("Youtube") | None => Some(format!("https://www.youtube.com/watch?v={id}")),
        Some(_) => None,
    }
}

pub fn failed_itself(err: &str) -> bool {
    (err.starts_with("yt-dlp") && err != "yt-dlp was stopped") || err.starts_with("failed to run yt-dlp")
}

pub fn clean_error(stderr: &str, code: Option<i32>) -> String {
    let line = stderr
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("ERROR:"))
        .or_else(|| stderr.lines().map(str::trim).rfind(|l| !l.is_empty()));
    let Some(line) = line else {
        return match code {
            Some(c) => format!("yt-dlp exited with status {c}"),
            None => "yt-dlp was stopped".to_string(),
        };
    };
    let mut msg = line.strip_prefix("ERROR:").unwrap_or(line).trim();
    if msg.starts_with('[')
        && let Some(end) = msg.find("] ")
    {
        msg = msg[end + 2..].trim_start();
        if let Some((id, rest)) = msg.split_once(": ")
            && !id.is_empty()
            && !id.contains(char::is_whitespace)
        {
            msg = rest;
        }
    }
    format!("yt-dlp: {msg}")
}

fn track_of(v: &Value) -> Option<Track> {
    let url = v["url"].as_str()?.to_string();
    let headers = v["http_headers"]
        .as_object()
        .map(|h| {
            h.iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                .collect()
        })
        .unwrap_or_default();
    Some(Track {
        url,
        format_id: v["format_id"].as_str().unwrap_or("?").to_string(),
        protocol: v["protocol"].as_str().unwrap_or("https").to_string(),
        headers,
    })
}

fn has_video(v: &Value) -> bool {
    v["vcodec"].as_str() != Some("none") && (v["vcodec"].is_string() || v["height"].is_u64())
}

fn has_audio(v: &Value) -> bool {
    v["acodec"].as_str() != Some("none") && v["acodec"].is_string()
}

fn rank(v: &Value) -> (u64, u64, String) {
    let tbr = v["tbr"].as_f64().unwrap_or(0.0).max(0.0) as u64;
    (v["height"].as_u64().unwrap_or(0), tbr, v["format_id"].as_str().unwrap_or("").to_string())
}

fn hls_fallback(formats: &[Value], max_height: u32) -> (Option<Track>, Option<Track>) {
    let hls: Vec<&Value> = formats
        .iter()
        .filter(|f| f["protocol"].as_str().is_some_and(|p| p.starts_with("m3u8")))
        .collect();
    let video = hls
        .iter()
        .filter(|f| {
            f["vcodec"].as_str() != Some("none")
                && f["height"].as_u64().is_some_and(|h| h <= u64::from(max_height))
        })
        .max_by_key(|f| rank(f))
        .and_then(|f| track_of(f));
    let audio = hls
        .iter()
        .filter(|f| f["vcodec"].as_str() == Some("none"))
        .max_by_key(|f| rank(f))
        .and_then(|f| track_of(f))
        .or_else(|| {
            hls.iter()
                .filter(|f| has_audio(f) && f["vcodec"].as_str() != Some("none"))
                .filter(|f| f["height"].as_u64().is_none_or(|h| h <= u64::from(max_height)))
                .max_by_key(|f| rank(f))
                .and_then(|f| track_of(f))
        });
    (video, audio)
}

pub fn media_from_json(json: &Value, max_height: u32) -> Result<Media, String> {
    if json["is_live"].as_bool() == Some(true) || json["live_status"].as_str() == Some("is_live") {
        return Err("live streams are not supported (only finished videos)".into());
    }
    if json["live_status"].as_str() == Some("is_upcoming") {
        return Err("this video has not premiered yet".into());
    }
    let mut video = Vec::new();
    let mut audio = Vec::new();
    match json["requested_formats"].as_array() {
        Some(parts) => {
            for part in parts {
                let Some(track) = track_of(part) else { continue };
                if has_video(part) {
                    video.push(track.clone());
                }
                if !has_video(part) || has_audio(part) {
                    audio.push(track);
                }
            }
        }
        None => {
            if let Some(track) = track_of(json) {
                if has_video(json) || !json["vcodec"].is_string() {
                    video.push(track.clone());
                }
                if has_audio(json) || !json["acodec"].is_string() {
                    audio.push(track);
                }
            }
        }
    }
    if video.is_empty() {
        return Err("yt-dlp found no video stream for this link".into());
    }
    let formats = json["formats"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let (hls_video, hls_audio) = hls_fallback(formats, max_height);
    if let Some(t) = hls_video
        && !video.iter().any(|v| v.is_hls())
    {
        video.push(t);
    }
    if let Some(t) = hls_audio
        && !audio.iter().any(|a| a.is_hls())
    {
        audio.push(t);
    }
    let id = json["id"].as_str().unwrap_or("?").to_string();
    Ok(Media {
        title: json["title"].as_str().unwrap_or(&id).to_string(),
        id,
        duration: json["duration"].as_f64().filter(|d| *d > 0.0),
        fps: json["fps"].as_f64().filter(|f| *f > 0.0),
        width: json["width"].as_u64().and_then(|w| u32::try_from(w).ok()).filter(|w| *w > 0),
        height: json["height"].as_u64().and_then(|h| u32::try_from(h).ok()).filter(|h| *h > 0),
        video,
        audio,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::procs::ScratchDir;

    #[test]
    fn inputs_classify_by_route() {
        let v = "https://www.youtube.com/watch?v=jNQXAC9IVRw";
        assert_eq!(Input::classify(v), Input::Video(v.into()));
        let vl = "https://www.youtube.com/watch?v=jNQXAC9IVRw&list=PL123";
        assert_eq!(Input::classify(vl), Input::Video(vl.into()));
        assert_eq!(
            Input::classify("youtu.be/jNQXAC9IVRw?list=PL1"),
            Input::Video("https://youtu.be/jNQXAC9IVRw?list=PL1".into())
        );
        let s = "https://youtube.com/shorts/abcdEFGhijk";
        assert_eq!(Input::classify(s), Input::Video(s.into()));
        let p = "https://www.youtube.com/playlist?list=PL123";
        assert_eq!(Input::classify(p), Input::Playlist(p.into()));
        for c in [
            "https://www.youtube.com/@jawed",
            "https://www.youtube.com/@jawed/videos",
            "https://www.youtube.com/channel/UC4QobU6STFB0P71PMvOGN5A",
            "https://m.youtube.com/c/Someone/shorts",
        ] {
            assert_eq!(Input::classify(c), Input::Channel(c.into()), "{c}");
        }
        let r = "https://www.youtube.com/results?search_query=me+at+the+zoo";
        assert_eq!(Input::classify(r), Input::SearchUrl(r.into()));
        assert_eq!(Input::classify("  me   at the zoo "), Input::Search("me at the zoo".into()));
        assert_eq!(Input::classify("zoo"), Input::Search("zoo".into()));
        let o = "https://vimeo.com/123";
        assert_eq!(Input::classify(o), Input::Other(o.into()));
        assert_eq!(Input::Search("a b".into()).target(), "ytsearch1:a b");
    }

    #[test]
    fn errors_come_out_as_one_clean_line() {
        let age = "[youtube] Extracting URL: x\nERROR: [youtube] abcDEF12345: Sign in to confirm your age. This video may be inappropriate for some users.\n";
        assert_eq!(
            clean_error(age, Some(1)),
            "yt-dlp: Sign in to confirm your age. This video may be inappropriate for some users."
        );
        assert_eq!(clean_error("ERROR: Private video\n", Some(1)), "yt-dlp: Private video");
        assert_eq!(clean_error("", Some(2)), "yt-dlp exited with status 2");
        assert_eq!(clean_error("", None), "yt-dlp was stopped");
    }

    struct Fake {
        _scratch: ScratchDir,
        dir: PathBuf,
        log: PathBuf,
    }

    impl Fake {
        fn new(script_body: &str) -> Fake {
            let scratch = ScratchDir::new().unwrap();
            let dir = scratch.path().to_path_buf();
            let log = dir.join("argv.log");
            let fake = Fake { _scratch: scratch, dir, log };
            fake.rewrite(script_body);
            fake
        }

        fn rewrite(&self, script_body: &str) {
            let program = self.dir.join("yt-dlp");
            let script = format!(
                "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> '{}'; done\nprintf 'END_CALL\\n' >> '{}'\nargs=\"$*\"\n{script_body}\n",
                self.log.display(),
                self.log.display()
            );
            let temp = self.dir.join("yt-dlp.new");
            std::fs::write(&temp, script).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            std::fs::rename(&temp, &program).unwrap();
        }

        fn calls(&self) -> usize {
            std::fs::read_to_string(&self.log).unwrap_or_default().matches("END_CALL").count()
        }

        fn resolve_updating(
            &self,
            input: &str,
            update: &(dyn Fn() -> Result<Option<String>, String> + Send + Sync),
        ) -> (Result<Media, String>, Vec<String>) {
            let procs = Procs::new();
            let ytdlp = YtDlp::new(self.dir.join("yt-dlp"), procs.clone(), &self.dir);
            let mut notes = Vec::new();
            let result = ytdlp.resolve_or_update(
                &Input::classify(input),
                480,
                &mut |_| {},
                Some(update),
                &mut |n| notes.push(n),
            );
            assert_eq!(procs.running(), 0);
            (result, notes)
        }

        fn resolve(&self, input: &str) -> (Result<Media, String>, Vec<Vec<String>>, Vec<String>) {
            self.resolve_with(input, None)
        }

        fn resolve_with(&self, input: &str, browser: Option<&str>) -> (Result<Media, String>, Vec<Vec<String>>, Vec<String>) {
            let procs = Procs::new();
            let ytdlp = YtDlp::new(self.dir.join("yt-dlp"), procs.clone(), &self.dir).cookies_from_browser(browser);
            let mut events = Vec::new();
            let result = ytdlp.resolve(&Input::classify(input), 480, &mut |e| {
                events.push(match e {
                    Event::Started => "started".to_string(),
                    Event::EntryFound(u) => format!("entry {u}"),
                })
            });
            assert_eq!(procs.running(), 0, "every yt-dlp run is reaped");
            let log = std::fs::read_to_string(&self.log).unwrap_or_default();
            let calls = log
                .split("END_CALL\n")
                .filter(|c| !c.is_empty())
                .map(|c| c.lines().map(str::to_string).collect())
                .collect();
            (result, calls, events)
        }
    }

    const VIDEO_JSON: &str = r#"{"_type":"video","id":"jNQXAC9IVRw","title":"Me at the zoo","duration":19,"fps":15,"width":320,"height":240,
"requested_formats":[
 {"format_id":"395","protocol":"https","url":"https://v.example/395","vcodec":"av01","acodec":"none","height":240,"http_headers":{"User-Agent":"UA/1","Accept":"*/*"}},
 {"format_id":"251","protocol":"https","url":"https://v.example/251","vcodec":"none","acodec":"opus","http_headers":{"User-Agent":"UA/1"}}],
"formats":[
 {"format_id":"229","protocol":"m3u8_native","url":"https://m.example/229","vcodec":"avc1","height":240,"tbr":200},
 {"format_id":"230","protocol":"m3u8_native","url":"https://m.example/230","vcodec":"avc1","height":720,"tbr":900},
 {"format_id":"233","protocol":"m3u8_native","url":"https://m.example/233","vcodec":"none","tbr":64},
 {"format_id":"234","protocol":"m3u8_native","url":"https://m.example/234","vcodec":"none","tbr":128}]}"#;

    fn video_branch() -> String {
        format!("case \"$args\" in *--no-playlist*) cat <<'JSON'\n{VIDEO_JSON}\nJSON\nexit 0;; esac")
    }

    fn flat(entries: &str) -> String {
        format!("{{\"_type\":\"playlist\",\"id\":\"x\",\"entries\":[{entries}]}}")
    }

    fn youtube_entry(id: &str) -> String {
        format!("{{\"_type\":\"url\",\"ie_key\":\"Youtube\",\"id\":\"{id}\",\"url\":\"https://www.youtube.com/watch?v={id}\"}}")
    }

    fn assert_zoo(result: Result<Media, String>) -> Media {
        let media = result.expect("resolves");
        assert_eq!(media.id, "jNQXAC9IVRw");
        assert_eq!(media.title, "Me at the zoo");
        assert_eq!((media.width, media.height, media.fps, media.duration), (Some(320), Some(240), Some(15.0), Some(19.0)));
        media
    }

    #[test]
    fn a_video_url_with_a_list_plays_the_video_and_skips_the_playlist() {
        let fake = Fake::new(&video_branch());
        let url = "https://www.youtube.com/watch?v=jNQXAC9IVRw&list=PL123";
        let (result, calls, events) = fake.resolve(url);
        let media = assert_zoo(result);
        assert_eq!(calls.len(), 1, "a video URL needs one yt-dlp run: {calls:?}");
        assert_eq!(calls[0], with_prefix(&["--no-playlist", "-f", &YtDlp::selector(480), "-J", "--", url]));
        assert_eq!(events, ["started".to_string(), format!("entry {url}")]);
        assert_eq!(media.video.iter().map(|t| t.format_id.as_str()).collect::<Vec<_>>(), ["395", "229"]);
        assert_eq!(media.audio.iter().map(|t| t.format_id.as_str()).collect::<Vec<_>>(), ["251", "234"]);
        assert!(media.video[1].is_hls() && !media.video[0].is_hls());
        assert_eq!(media.video[0].headers, [("Accept".into(), "*/*".into()), ("User-Agent".into(), "UA/1".into())]);
    }

    fn with_prefix(rest: &[&str]) -> Vec<String> {
        PREFIX.iter().chain(rest).map(|a| a.to_string()).collect()
    }

    #[test]
    fn cookies_are_passed_only_when_asked() {
        let fake = Fake::new(&video_branch());
        let url = "https://www.youtube.com/watch?v=jNQXAC9IVRw";
        let (result, calls, _) = fake.resolve_with(url, Some("firefox"));
        assert_zoo(result);
        let at = calls[0].iter().position(|a| a == "--cookies-from-browser").expect("flag passed");
        assert_eq!(calls[0][at + 1], "firefox");
        assert_eq!(calls[0][0], "--ignore-config", "config stays ignored even with cookies");
    }

    fn channel_branch() -> String {
        let tab = "{\"_type\":\"url\",\"ie_key\":\"YoutubeTab\",\"url\":\"https://www.youtube.com/@jawed/videos\"}";
        format!(
            "{}\ncase \"$args\" in *@jawed/videos*) cat <<'JSON'\n{}\nJSON\n;; *@jawed*) cat <<'JSON'\n{}\nJSON\n;; esac",
            video_branch(),
            flat(&youtube_entry("jNQXAC9IVRw")),
            flat(tab)
        )
    }

    #[test]
    fn every_call_ignores_user_config_and_never_downloads() {
        let body = channel_branch();
        let fake = Fake::new(&body);
        let (result, calls, _) = fake.resolve("https://www.youtube.com/@jawed");
        assert_zoo(result);
        assert_eq!(calls.len(), 3);
        for call in &calls {
            assert_eq!(call.first().map(String::as_str), Some("--ignore-config"), "{call:?}");
            for flag in ["--no-cache-dir", "--simulate", "--skip-download", "-J"] {
                assert!(call.iter().any(|a| a == flag), "{flag} missing from {call:?}");
            }
            assert!(!call.iter().any(|a| a.starts_with("--cookies")), "no cookies unless asked: {call:?}");
            assert_eq!(call[call.len() - 2], "--", "{call:?}");
        }
    }

    #[test]
    fn every_channel_call_passes_opt_in_cookies_once_before_the_target() {
        let fake = Fake::new(&channel_branch());
        let (result, calls, _) = fake.resolve_with("https://www.youtube.com/@jawed", Some("firefox"));
        assert_zoo(result);
        assert_eq!(calls.len(), 3);
        for call in &calls {
            assert_eq!(call[0], "--ignore-config", "{call:?}");
            assert_eq!(call.iter().filter(|arg| arg.starts_with("--cookies")).count(), 1, "{call:?}");
            let at = call.iter().position(|arg| arg == "--cookies-from-browser").unwrap();
            assert_eq!(call[at + 1], "firefox", "{call:?}");
            let end = call.iter().position(|arg| arg == "--").unwrap();
            assert!(at > 0 && at + 1 < end, "{call:?}");
            assert_eq!(end, call.len() - 2, "{call:?}");
        }
    }

    #[test]
    fn nested_targets_starting_with_a_dash_follow_the_option_terminator() {
        let tab = r#"{"_type":"url","ie_key":"YoutubeTab","url":"-nested-tab"}"#;
        let video = r#"{"_type":"url","ie_key":"Youtube","url":"-video"}"#;
        let body = format!(
            "{}\ncase \"$args\" in *-nested-tab*) cat <<'JSON'\n{}\nJSON\n;; *) cat <<'JSON'\n{}\nJSON\n;; esac",
            video_branch(), flat(video), flat(tab)
        );
        let fake = Fake::new(&body);
        let channel = "https://www.youtube.com/@jawed";
        let (result, calls, _) = fake.resolve_with(channel, Some("firefox"));
        assert_zoo(result);
        assert_eq!(calls.len(), 3);
        for (call, target) in calls.iter().zip([channel, "-nested-tab", "-video"]) {
            assert_eq!(call[call.len() - 2..], ["--", target], "{call:?}");
        }
    }

    #[test]
    fn a_playlist_takes_its_first_item() {
        let body = format!(
            "{}\ncase \"$args\" in *--flat-playlist*) cat <<'JSON'\n{}\nJSON\n;; esac",
            video_branch(),
            flat(&youtube_entry("jNQXAC9IVRw"))
        );
        let fake = Fake::new(&body);
        let url = "https://www.youtube.com/playlist?list=PL123";
        let (result, calls, _) = fake.resolve(url);
        assert_zoo(result);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], with_prefix(&["--flat-playlist", "-I", "1", "-J", "--", url]));
        assert_eq!(calls[1].last().map(String::as_str), Some("https://www.youtube.com/watch?v=jNQXAC9IVRw"));
        assert!(calls[1].contains(&"--no-playlist".to_string()));
    }

    #[test]
    fn a_channel_follows_nested_tabs_down_to_a_video() {
        let body = channel_branch();
        let fake = Fake::new(&body);
        let (result, calls, _) = fake.resolve("https://www.youtube.com/@jawed");
        assert_zoo(result);
        assert_eq!(calls.len(), 3, "{calls:?}");
        assert_eq!(calls[0].last().map(String::as_str), Some("https://www.youtube.com/@jawed"));
        assert_eq!(calls[1].last().map(String::as_str), Some("https://www.youtube.com/@jawed/videos"));
        assert!(calls[1].contains(&"-I".to_string()));
    }

    #[test]
    fn nesting_is_bounded() {
        let tab = "{\"_type\":\"playlist\",\"url\":\"https://www.youtube.com/@loop/videos\"}";
        let body = format!("cat <<'JSON'\n{}\nJSON", flat(tab));
        let fake = Fake::new(&body);
        let (result, calls, _) = fake.resolve("https://www.youtube.com/@loop");
        assert!(result.unwrap_err().starts_with("no video found within 3 levels"));
        assert_eq!(calls.len(), 3);
    }

    #[test]
    fn a_search_url_and_plain_terms_take_the_first_result() {
        let body = format!(
            "{}\ncase \"$args\" in *--flat-playlist*) cat <<'JSON'\n{}\nJSON\n;; esac",
            video_branch(),
            flat(&youtube_entry("jNQXAC9IVRw"))
        );
        let fake = Fake::new(&body);
        let url = "https://www.youtube.com/results?search_query=me+at+the+zoo";
        let (result, calls, _) = fake.resolve(url);
        assert_zoo(result);
        assert_eq!(calls[0].last().map(String::as_str), Some(url));

        let fake = Fake::new(&body);
        let (result, calls, _) = fake.resolve("me at the zoo");
        assert_zoo(result);
        assert_eq!(calls[0], with_prefix(&["--flat-playlist", "-I", "1", "-J", "--", "ytsearch1:me at the zoo"]));
    }

    #[test]
    fn an_empty_search_is_a_clean_no_results_error() {
        let fake = Fake::new(&format!("cat <<'JSON'\n{}\nJSON", flat("")));
        let (result, calls, _) = fake.resolve("zzqqxx nothing");
        assert_eq!(result.unwrap_err(), "no videos found for \"zzqqxx nothing\"");
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn an_age_restricted_video_surfaces_the_yt_dlp_reason() {
        let fake = Fake::new(
            "echo '[youtube] Extracting URL' >&2\necho 'ERROR: [youtube] abcDEF12345: Sign in to confirm your age. This video may be inappropriate for some users.' >&2\nexit 1",
        );
        let (result, _, events) = fake.resolve("https://www.youtube.com/watch?v=abcDEF12345");
        assert_eq!(
            result.unwrap_err(),
            "yt-dlp: Sign in to confirm your age. This video may be inappropriate for some users."
        );
        assert_eq!(events.len(), 2);
    }

    const EXTRACTOR_BROKE: &str =
        "echo 'ERROR: [youtube] jNQXAC9IVRw: Unable to extract player response; please report this issue' >&2\nexit 1";

    #[test]
    fn a_failing_cached_ytdlp_is_updated_once_and_retried() {
        let fake = Fake::new(EXTRACTOR_BROKE);
        let updates = std::sync::atomic::AtomicUsize::new(0);
        let url = "https://www.youtube.com/watch?v=jNQXAC9IVRw";
        let (result, notes) = fake.resolve_updating(url, &|| {
            updates.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            fake.rewrite(&video_branch());
            Ok(Some("2099.01.01".into()))
        });
        assert_zoo(result);
        assert_eq!(updates.into_inner(), 1);
        assert_eq!(fake.calls(), 2, "one failed run, one retry");
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("the cached yt-dlp failed (yt-dlp: Unable to extract player response"), "{notes:?}");
        assert!(notes[0].ends_with("updated it to 2099.01.01 and retried"), "{notes:?}");
    }

    #[test]
    fn the_retry_happens_once_even_if_the_update_did_not_help() {
        let fake = Fake::new(EXTRACTOR_BROKE);
        let updates = std::sync::atomic::AtomicUsize::new(0);
        let (result, notes) = fake.resolve_updating("https://youtu.be/jNQXAC9IVRw", &|| {
            updates.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Some("2099.01.01".into()))
        });
        assert!(result.unwrap_err().starts_with("yt-dlp: Unable to extract player response"));
        assert_eq!((updates.into_inner(), fake.calls(), notes.len()), (1, 2, 1));
    }

    #[test]
    fn no_retry_when_already_latest_or_when_the_update_fails() {
        let fake = Fake::new(EXTRACTOR_BROKE);
        let (result, notes) = fake.resolve_updating("https://youtu.be/jNQXAC9IVRw", &|| Ok(None));
        assert!(result.unwrap_err().starts_with("yt-dlp: Unable to extract"));
        assert_eq!((fake.calls(), notes.len()), (1, 0), "already the latest release: nothing to retry");

        let fake = Fake::new(EXTRACTOR_BROKE);
        let (result, notes) = fake.resolve_updating("https://youtu.be/jNQXAC9IVRw", &|| Err("offline".into()));
        assert!(result.unwrap_err().starts_with("yt-dlp: Unable to extract"));
        assert_eq!(fake.calls(), 1);
        assert_eq!(notes, ["the cached yt-dlp failed; updating it failed too: offline"]);
    }

    #[test]
    fn answers_about_the_video_are_not_a_reason_to_update() {
        let fake = Fake::new(&format!("cat <<'JSON'\n{}\nJSON", flat("")));
        let (result, notes) = fake.resolve_updating("zzqqxx nothing", &|| panic!("no update for an empty search"));
        assert_eq!(result.unwrap_err(), "no videos found for \"zzqqxx nothing\"");
        assert!(notes.is_empty());
        assert!(failed_itself("failed to run yt-dlp (is it installed and on PATH?): Exec format error"));
        assert!(failed_itself("yt-dlp printed unparseable JSON: EOF"));
        assert!(!failed_itself("yt-dlp was stopped"));
        assert!(!failed_itself("live streams are not supported (only finished videos)"));
    }

    #[test]
    fn live_and_audio_only_results_are_refused() {
        let live: Value = serde_json::from_str(r#"{"id":"x","is_live":true,"url":"https://a"}"#).unwrap();
        assert!(media_from_json(&live, 480).unwrap_err().contains("live"));
        let audio: Value =
            serde_json::from_str(r#"{"id":"x","url":"https://a","vcodec":"none","acodec":"opus"}"#).unwrap();
        assert!(media_from_json(&audio, 480).unwrap_err().contains("no video stream"));
        let muxed: Value = serde_json::from_str(
            r#"{"id":"x","url":"https://a","format_id":"18","vcodec":"avc1","acodec":"mp4a","height":360}"#,
        )
        .unwrap();
        let m = media_from_json(&muxed, 480).unwrap();
        assert_eq!((m.video.len(), m.audio.len()), (1, 1));
        assert_eq!(m.video[0].url, m.audio[0].url);
    }
}
