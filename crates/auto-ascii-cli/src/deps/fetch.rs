//! One standalone build onto disk: HTTPS only, streamed to a temp file beside
//! its destination while hashing, checked against the publisher's SHA-256,
//! unzipped when the source ships a zip, test-run, then renamed into place.
//! A lock file serialises concurrent runs; `manifest.json` records each
//! install for `doctor` and the yt-dlp staleness check.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use auto_ascii::tools::Tool;
use auto_ascii_factory::sha256::Sha256;
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use serde::{Deserialize, Serialize};

use super::platform::{self, Asset, FfmpegBuild, YtdlpBuild};

pub const MANIFEST: &str = "manifest.json";

pub const FOLDERS: &str = "yt-dlp";

const LOCK: &str = ".lock";

const VERSION_TIMEOUT: Duration = Duration::from_secs(120);

const BODY_TIMEOUT: Duration = Duration::from_secs(20 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Noise {
    pub quiet: bool,
}

impl Noise {
    pub fn say(self, message: &str) {
        if !self.quiet {
            eprintln!("auto-ascii: {message}");
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub tools: BTreeMap<String, Entry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
    pub installed_unix: u64,
    pub checked_unix: u64,
}

impl Manifest {
    pub fn read(bin: &Path) -> Manifest {
        fs::read(bin.join(MANIFEST))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn entry(&self, tool: Tool) -> Option<&Entry> {
        self.tools.get(tool.name())
    }

    fn write(&self, bin: &Path) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        let temp = Temp::new(bin, MANIFEST);
        write_synced(temp.path(), &json).map_err(|e| format!("writing {}: {e}", temp.path().display()))?;
        temp.persist(&bin.join(MANIFEST)).map_err(|e| format!("installing {MANIFEST}: {e}"))
    }
}

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

pub struct Temp {
    path: PathBuf,
    armed: bool,
}

fn nanos() -> u32 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos())
}

fn temp_name(dir: &Path, label: &str) -> PathBuf {
    dir.join(format!(".{label}.{}.{}.part", std::process::id(), nanos()))
}

impl Temp {
    pub fn new(dir: &Path, label: &str) -> Temp {
        Temp { path: temp_name(dir, label), armed: true }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn persist(mut self, to: &Path) -> io::Result<()> {
        fs::rename(&self.path, to)?;
        self.armed = false;
        if let Some(dir) = to.parent() {
            sync_dir(dir);
        }
        Ok(())
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

pub struct Lock {
    _file: File,
}

impl Lock {
    pub fn acquire(bin: &Path, noise: Noise) -> Result<Lock, String> {
        let path = bin.join(LOCK);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| format!("opening {}: {e}", path.display()))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                noise.say("waiting for another auto-ascii download to finish");
                file.lock().map_err(|e| format!("locking {}: {e}", path.display()))?;
            }
            Err(fs::TryLockError::Error(e)) => return Err(format!("locking {}: {e}", path.display())),
        }
        Ok(Lock { _file: file })
    }
}

pub fn save_verified(
    dir: &Path,
    label: &str,
    mut body: impl Read,
    expected_sha256: &str,
    progress: &ProgressBar,
) -> Result<(Temp, u64), String> {
    let temp = Temp::new(dir, label);
    let mut file = File::create(temp.path()).map_err(|e| format!("creating {}: {e}", temp.path().display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut bytes = 0u64;
    loop {
        let n = match body.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("downloading {label}: {e}")),
        };
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(|e| format!("writing {}: {e}", temp.path().display()))?;
        bytes += n as u64;
        progress.inc(n as u64);
    }
    file.sync_all().map_err(|e| format!("syncing {}: {e}", temp.path().display()))?;
    let got: String = hasher.finish().iter().map(|b| format!("{b:02x}")).collect();
    if !got.eq_ignore_ascii_case(expected_sha256) {
        return Err(format!(
            "checksum mismatch for {label}: expected sha256 {expected_sha256}, got {got}; nothing was installed"
        ));
    }
    Ok((temp, bytes))
}

pub fn extract(zip_path: &Path, dir: &Path, tool: Tool) -> Result<Temp, String> {
    let file = File::open(zip_path).map_err(|e| format!("opening {}: {e}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("reading the {} zip: {e}", tool.name()))?;
    let want = tool.file_name();
    let index = (0..archive.len())
        .find(|&i| {
            archive.name_for_index(i).is_some_and(|name| {
                !name.ends_with('/') && name.rsplit(['/', '\\']).next() == Some(want.as_str())
            })
        })
        .ok_or_else(|| format!("the downloaded zip has no {want}"))?;
    let mut entry = archive.by_index(index).map_err(|e| format!("reading {want} from the zip: {e}"))?;
    let temp = Temp::new(dir, tool.name());
    let mut out = File::create(temp.path()).map_err(|e| format!("creating {}: {e}", temp.path().display()))?;
    io::copy(&mut entry, &mut out).map_err(|e| format!("unzipping {want}: {e}"))?;
    out.sync_all().map_err(|e| format!("syncing {}: {e}", temp.path().display()))?;
    Ok(temp)
}

pub struct TempTree {
    path: PathBuf,
    armed: bool,
}

impl TempTree {
    fn new(dir: &Path, label: &str) -> TempTree {
        TempTree { path: temp_name(dir, label), armed: true }
    }

    fn persist(mut self, to: &Path) -> io::Result<()> {
        fs::rename(&self.path, to)?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

pub fn place_folder(
    zip_path: &Path,
    bin: &Path,
    tag: &str,
    exe: &str,
    check: &dyn Fn(&Path) -> Result<String, String>,
) -> Result<(PathBuf, String), String> {
    let folders = bin.parent().ok_or("the cache bin folder has no parent")?.join(FOLDERS);
    fs::create_dir_all(&folders).map_err(|e| format!("creating {}: {e}", folders.display()))?;
    let tree = TempTree::new(&folders, tag);
    let file = File::open(zip_path).map_err(|e| format!("opening {}: {e}", zip_path.display()))?;
    zip::ZipArchive::new(file)
        .and_then(|mut archive| archive.extract(&tree.path))
        .map_err(|e| format!("unzipping yt-dlp {tag}: {e}"))?;
    let inner = tree.path.join(exe);
    if !auto_ascii::tools::is_executable(&inner) {
        return Err(format!("the yt-dlp {tag} zip has no executable {exe}"));
    }
    let version = check(&inner).map_err(|e| format!("the downloaded yt-dlp does not run: {e}"))?;
    let name = format!("{tag}-{}", nanos());
    let dest = folders.join(&name);
    tree.persist(&dest).map_err(|e| format!("installing {}: {e}", dest.display()))?;
    let link = bin.join(Tool::YtDlp.file_name());
    let previous = fs::read_link(&link).ok().and_then(|t| t.parent().and_then(Path::file_name).map(|n| n.to_owned()));
    let temp = Temp::new(bin, Tool::YtDlp.name());
    symlink(&Path::new("..").join(FOLDERS).join(&name).join(exe), temp.path())
        .map_err(|e| format!("linking {}: {e}", temp.path().display()))?;
    temp.persist(&link).map_err(|e| format!("installing {}: {e}", link.display()))?;
    if let Ok(entries) = fs::read_dir(&folders) {
        for entry in entries.flatten() {
            let keep = entry.file_name() == name.as_str() || previous.as_deref() == Some(entry.file_name().as_os_str());
            if !keep {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
    Ok((link, version))
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn symlink(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "folder builds need symlinks"))
}

pub fn place(
    temp: Temp,
    bin: &Path,
    tool: Tool,
    check: &dyn Fn(&Path) -> Result<String, String>,
) -> Result<(PathBuf, String), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("marking {} executable: {e}", temp.path().display()))?;
    }
    let version = check(temp.path()).map_err(|e| format!("the downloaded {} does not run: {e}", tool.name()))?;
    let dest = bin.join(tool.file_name());
    temp.persist(&dest).map_err(|e| format!("installing {}: {e}", dest.display()))?;
    Ok((dest, version))
}

pub fn version_of(program: &Path, tool: Tool) -> Result<String, String> {
    let flag = if tool == Tool::YtDlp { "--version" } else { "-version" };
    let mut child = Command::new(program)
        .arg(flag)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stdout = child.stdout.take().expect("stdout was piped");
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.by_ref().take(64 * 1024).read_to_end(&mut out);
        out
    });
    let deadline = Instant::now() + VERSION_TIMEOUT;
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("`{} {flag}` took over {}s", tool.name(), VERSION_TIMEOUT.as_secs()));
            }
            None => std::thread::sleep(Duration::from_millis(25)),
        }
    };
    let out = String::from_utf8_lossy(&reader.join().unwrap_or_default()).into_owned();
    if !status.success() {
        return Err(format!("`{} {flag}` exited with {status}", tool.name()));
    }
    parse_version(&out, tool).ok_or_else(|| format!("`{} {flag}` printed no version", tool.name()))
}

pub fn parse_version(out: &str, tool: Tool) -> Option<String> {
    let line = out.lines().map(str::trim).find(|l| !l.is_empty())?;
    if tool == Tool::YtDlp {
        return Some(line.to_string());
    }
    let mut words = line.split_whitespace();
    words.find(|w| *w == "version")?;
    words.next().map(str::to_string)
}

fn agent(max_redirects: u32) -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(max_redirects)
        .user_agent(concat!("auto-ascii/", env!("CARGO_PKG_VERSION"), " (+https://github.com/andmckay01/auto-ascii)"))
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .timeout_recv_body(Some(BODY_TIMEOUT))
        .build()
        .into()
}

struct Http {
    follow: ureq::Agent,
    stay: ureq::Agent,
}

impl Http {
    fn new() -> Http {
        Http { follow: agent(10), stay: agent(0) }
    }
}

fn redirect_target(http: &Http, url: &str) -> Result<String, String> {
    let response = http.stay.get(url).call().map_err(|e| format!("GET {url}: {e}"))?;
    let location = response
        .status()
        .is_redirection()
        .then(|| response.headers().get("location").and_then(|v| v.to_str().ok()))
        .flatten()
        .ok_or_else(|| format!("GET {url}: expected a redirect to the latest build, got {}", response.status()))?;
    absolute(url, location).ok_or_else(|| format!("GET {url}: unusable redirect to {location:?}"))
}

pub fn absolute(base: &str, location: &str) -> Option<String> {
    if location.starts_with("https://") {
        return Some(location.to_string());
    }
    let rest = location.strip_prefix('/')?;
    let (scheme, after) = base.split_once("://")?;
    let host = after.split('/').next()?;
    Some(format!("{scheme}://{host}/{rest}"))
}

fn text(http: &Http, url: &str) -> Result<String, String> {
    let mut response = http.follow.get(url).call().map_err(|e| format!("GET {url}: {e}"))?;
    response.body_mut().read_to_string().map_err(|e| format!("GET {url}: {e}"))
}

fn progress(noise: Noise, total: Option<u64>, label: &str) -> ProgressBar {
    if noise.quiet {
        return ProgressBar::hidden();
    }
    let bar = ProgressBar::with_draw_target(total, ProgressDrawTarget::stderr());
    let style = ProgressStyle::with_template(
        "{prefix:>8} [{bar:30}] {binary_bytes}/{binary_total_bytes} {binary_bytes_per_sec} eta {eta}",
    )
    .map(|s| s.progress_chars("=> "))
    .unwrap_or_else(|_| ProgressStyle::default_bar());
    bar.set_style(style);
    bar.set_prefix(label.to_string());
    bar
}

struct Download {
    temp: Temp,
    url: String,
    sha256: String,
    bytes: u64,
}

fn download(http: &Http, bin: &Path, url: &str, sha256: &str, label: &str, noise: Noise) -> Result<Download, String> {
    let response = http.follow.get(url).call().map_err(|e| format!("GET {url}: {e}"))?;
    let total = response.body().content_length();
    noise.say(&format!(
        "downloading {label}{} from {url}",
        total.map_or_else(String::new, |n| format!(" ({})", crate::human_bytes(n)))
    ));
    let bar = progress(noise, total, label);
    let body = response.into_body().into_reader();
    let result = save_verified(bin, label, body, sha256, &bar);
    bar.finish_and_clear();
    let (temp, bytes) = result?;
    Ok(Download { temp, url: url.to_string(), sha256: sha256.to_string(), bytes })
}

pub fn install(bin: &Path, asset: &Asset, tools: &[Tool], noise: Noise) -> Result<Vec<Tool>, String> {
    fs::create_dir_all(bin).map_err(|e| format!("creating {}: {e}", bin.display()))?;
    let _lock = Lock::acquire(bin, noise)?;
    let http = Http::new();
    let before = Manifest::read(bin);
    let mut manifest = before.clone();
    let result = (|| {
        if tools.contains(&Tool::YtDlp) {
            let tag = latest_ytdlp_tag(&http)?;
            install_ytdlp(&http, bin, asset, &tag, noise, &mut manifest)?;
        }
        let programs: Vec<Tool> = tools.iter().copied().filter(|t| *t != Tool::YtDlp).collect();
        if !programs.is_empty() {
            install_ffmpeg(&http, bin, asset, &programs, noise, &mut manifest)?;
        }
        Ok(())
    })();
    let done: Vec<Tool> = tools.iter().copied().filter(|t| manifest.entry(*t) != before.entry(*t)).collect();
    if !done.is_empty() {
        manifest.write(bin)?;
    }
    result.map(|()| done)
}

pub fn update_ytdlp(
    bin: &Path,
    asset: &Asset,
    noise: Noise,
    failed_with: Option<&str>,
) -> Result<Option<String>, String> {
    fs::create_dir_all(bin).map_err(|e| format!("creating {}: {e}", bin.display()))?;
    let _lock = Lock::acquire(bin, noise)?;
    let mut manifest = Manifest::read(bin);
    let current = manifest.entry(Tool::YtDlp).map(|e| e.version.clone());
    if let (Some(failed), Some(now)) = (failed_with, current.as_deref())
        && failed != now
        && bin.join(Tool::YtDlp.file_name()).is_file()
    {
        return Ok(Some(now.to_string()));
    }
    let http = Http::new();
    let tag = latest_ytdlp_tag(&http)?;
    if current.as_deref() == Some(tag.as_str()) && bin.join(Tool::YtDlp.file_name()).is_file() {
        if let Some(entry) = manifest.tools.get_mut(Tool::YtDlp.name()) {
            entry.checked_unix = now_unix();
        }
        manifest.write(bin)?;
        return Ok(None);
    }
    install_ytdlp(&http, bin, asset, &tag, noise, &mut manifest)?;
    manifest.write(bin)?;
    Ok(Some(tag))
}

fn latest_ytdlp_tag(http: &Http) -> Result<String, String> {
    let resolved = redirect_target(http, platform::YTDLP_LATEST)?;
    platform::ytdlp_tag(&resolved)
        .ok_or_else(|| format!("{} redirected to {resolved}, not a release tag", platform::YTDLP_LATEST))
}

fn install_ytdlp(
    http: &Http,
    bin: &Path,
    asset: &Asset,
    tag: &str,
    noise: Noise,
    manifest: &mut Manifest,
) -> Result<(), String> {
    let base = format!("{}/{tag}", platform::YTDLP_RELEASES);
    let file = asset.ytdlp.asset();
    let sums = text(http, &format!("{base}/{}", platform::YTDLP_SUMS))?;
    let sha = platform::sums_entry(&sums, file)
        .ok_or_else(|| format!("yt-dlp {tag}'s {} lists no {file}", platform::YTDLP_SUMS))?;
    let got = download(http, bin, &format!("{base}/{file}"), &sha, "yt-dlp", noise)?;
    let check = |p: &Path| version_of(p, Tool::YtDlp);
    let (path, version) = match asset.ytdlp {
        YtdlpBuild::File(_) => place(got.temp, bin, Tool::YtDlp, &check)?,
        YtdlpBuild::Folder { exe, .. } => place_folder(got.temp.path(), bin, tag, exe, &check)?,
    };
    record(manifest, Tool::YtDlp, tag, &got.url, &got.sha256, got.bytes);
    noise.say(&format!("installed yt-dlp {version} at {} (sha256 verified)", path.display()));
    Ok(())
}

fn install_ffmpeg(
    http: &Http,
    bin: &Path,
    asset: &Asset,
    tools: &[Tool],
    noise: Noise,
    manifest: &mut Manifest,
) -> Result<(), String> {
    let latest = asset.ffmpeg.latest_url();
    let resolved = redirect_target(http, &latest)?;
    let version = asset
        .ffmpeg
        .version_of(&resolved)
        .ok_or_else(|| format!("{latest} redirected to {resolved}, not a versioned build"))?;
    match asset.ffmpeg {
        FfmpegBuild::Riedl(_) => {
            let dir = resolved.rsplit_once('/').map_or(resolved.as_str(), |(dir, _)| dir);
            for &tool in tools {
                let url = format!("{dir}/{}.zip", tool.name());
                let sha = platform::single_sum(&text(http, &format!("{url}.sha256"))?)
                    .ok_or_else(|| format!("{url}.sha256 holds no SHA-256 digest"))?;
                let got = download(http, bin, &url, &sha, tool.name(), noise)?;
                let unzipped = extract(got.temp.path(), bin, tool)?;
                finish(unzipped, bin, tool, &version, &got, noise, manifest)?;
            }
        }
        FfmpegBuild::Gyan => {
            let sha = platform::single_sum(&text(http, &format!("{resolved}.sha256"))?)
                .ok_or_else(|| format!("{resolved}.sha256 holds no SHA-256 digest"))?;
            let got = download(http, bin, &resolved, &sha, "ffmpeg", noise)?;
            for &tool in tools {
                let unzipped = extract(got.temp.path(), bin, tool)?;
                finish(unzipped, bin, tool, &version, &got, noise, manifest)?;
            }
        }
    }
    Ok(())
}

fn finish(
    unzipped: Temp,
    bin: &Path,
    tool: Tool,
    version: &str,
    got: &Download,
    noise: Noise,
    manifest: &mut Manifest,
) -> Result<(), String> {
    let (path, reported) = place(unzipped, bin, tool, &|p| version_of(p, tool))?;
    record(manifest, tool, version, &got.url, &got.sha256, got.bytes);
    noise.say(&format!("installed {} {reported} at {} (sha256 verified)", tool.name(), path.display()));
    Ok(())
}

fn record(manifest: &mut Manifest, tool: Tool, version: &str, url: &str, sha256: &str, bytes: u64) {
    let now = now_unix();
    manifest.tools.insert(
        tool.name().to_string(),
        Entry {
            version: version.to_string(),
            url: url.to_string(),
            sha256: sha256.to_string(),
            bytes,
            installed_unix: now,
            checked_unix: now,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::procs::ScratchDir;

    fn sha_hex(data: &[u8]) -> String {
        auto_ascii_factory::sha256_hex(data)
    }

    fn listing(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> =
            fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    struct Broken(usize);

    impl Read for Broken {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.0 == 0 {
                return Err(io::Error::new(io::ErrorKind::ConnectionReset, "connection reset"));
            }
            let n = self.0.min(buf.len());
            buf[..n].fill(b'x');
            self.0 -= n;
            Ok(n)
        }
    }

    fn ok_check(_: &Path) -> Result<String, String> {
        Ok("1.0".into())
    }

    #[cfg(unix)]
    #[test]
    fn a_verified_download_is_placed_executable_and_leaves_no_temp() {
        let scratch = ScratchDir::new().unwrap();
        let bin = scratch.path();
        let body = b"#!/bin/sh\necho 2026.08.19\n".to_vec();
        let (temp, bytes) = save_verified(bin, "yt-dlp", &body[..], &sha_hex(&body), &ProgressBar::hidden()).unwrap();
        assert_eq!(bytes, body.len() as u64);
        let (path, version) = place(temp, bin, Tool::YtDlp, &|p| version_of(p, Tool::YtDlp)).unwrap();
        assert_eq!(path, bin.join(Tool::YtDlp.file_name()));
        assert_eq!(fs::read(&path).unwrap(), body);
        assert_eq!(version, "2026.08.19");
        assert_eq!(listing(bin), [Tool::YtDlp.file_name()]);
    }

    #[test]
    fn a_checksum_mismatch_installs_nothing() {
        let scratch = ScratchDir::new().unwrap();
        let wrong = sha_hex(b"something else");
        let err = save_verified(scratch.path(), "ffmpeg", &b"payload"[..], &wrong, &ProgressBar::hidden())
            .err()
            .unwrap();
        assert!(err.starts_with("checksum mismatch for ffmpeg: expected sha256 "), "{err}");
        assert!(err.contains(&sha_hex(b"payload")), "{err}");
        assert!(listing(scratch.path()).is_empty(), "{:?}", listing(scratch.path()));
    }

    #[test]
    fn a_broken_connection_leaves_no_partial_file() {
        let scratch = ScratchDir::new().unwrap();
        let err = save_verified(scratch.path(), "yt-dlp", Broken(300_000), &sha_hex(b""), &ProgressBar::hidden())
            .err()
            .unwrap();
        assert_eq!(err, "downloading yt-dlp: connection reset");
        assert!(listing(scratch.path()).is_empty(), "{:?}", listing(scratch.path()));
    }

    #[test]
    fn a_program_that_fails_its_test_run_is_not_installed() {
        let scratch = ScratchDir::new().unwrap();
        let bin = scratch.path();
        let old = bin.join(Tool::Ffmpeg.file_name());
        fs::write(&old, "old").unwrap();
        let (temp, _) = save_verified(bin, "ffmpeg", &b"new"[..], &sha_hex(b"new"), &ProgressBar::hidden()).unwrap();
        let err = place(temp, bin, Tool::Ffmpeg, &|_| Err("exec format error".into())).err().unwrap();
        assert_eq!(err, "the downloaded ffmpeg does not run: exec format error");
        assert_eq!(fs::read(&old).unwrap(), b"old", "the previous install survives");
        assert_eq!(listing(bin), [Tool::Ffmpeg.file_name()]);
    }

    fn zip_with(path: &Path, entries: &[(&str, &[u8])]) {
        let mut writer = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, data) in entries {
            if name.ends_with('/') {
                writer.add_directory(*name, zip::write::SimpleFileOptions::default()).unwrap();
            } else {
                let options = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated);
                writer.start_file(*name, options).unwrap();
                writer.write_all(data).unwrap();
            }
        }
        writer.finish().unwrap();
    }

    #[test]
    fn programs_are_found_in_a_zip_by_file_name() {
        let scratch = ScratchDir::new().unwrap();
        let bin = scratch.path();
        let archive = bin.join("build.zip");
        let exe = Tool::Ffprobe.file_name();
        let nested = format!("ffmpeg-9.0.2-essentials_build/bin/{exe}");
        zip_with(&archive, &[
            ("ffmpeg-9.0.2-essentials_build/", b""),
            (&format!("ffmpeg-9.0.2-essentials_build/doc/{exe}.html"), b"<html>"),
            (&nested, b"probe bytes"),
        ]);
        let temp = extract(&archive, bin, Tool::Ffprobe).unwrap();
        assert_eq!(fs::read(temp.path()).unwrap(), b"probe bytes");
        let (path, _) = place(temp, bin, Tool::Ffprobe, &ok_check).unwrap();
        assert_eq!(path, bin.join(&exe));

        let err = extract(&archive, bin, Tool::Ffmpeg).err().unwrap();
        assert_eq!(err, format!("the downloaded zip has no {}", Tool::Ffmpeg.file_name()));
        assert_eq!(listing(bin), ["build.zip".to_string(), exe]);
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_build_is_linked_into_bin_and_old_folders_are_pruned() {
        let scratch = ScratchDir::new().unwrap();
        let bin = scratch.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let archive = scratch.path().join("yt.zip");
        let build = |version: &str| {
            let mut writer = zip::ZipWriter::new(File::create(&archive).unwrap());
            let exe = zip::write::SimpleFileOptions::default().unix_permissions(0o755);
            writer.start_file("yt-dlp_macos", exe).unwrap();
            writer.write_all(format!("#!/bin/sh\necho {version}\n").as_bytes()).unwrap();
            writer.start_file("_internal/base_library.zip", zip::write::SimpleFileOptions::default()).unwrap();
            writer.write_all(b"lib").unwrap();
            writer.finish().unwrap();
        };
        let check = |p: &Path| version_of(p, Tool::YtDlp);
        let folders = scratch.path().join(FOLDERS);
        for (i, tag) in ["2026.01.01", "2026.02.02", "2026.03.03"].iter().enumerate() {
            build(tag);
            let (link, version) = place_folder(&archive, &bin, tag, "yt-dlp_macos", &check).unwrap();
            assert_eq!(link, bin.join("yt-dlp"));
            assert_eq!(&version, tag);
            assert_eq!(version_of(&link, Tool::YtDlp).unwrap(), *tag, "the link runs the new build");
            let target = fs::read_link(&link).unwrap();
            assert!(target.starts_with("../yt-dlp/"), "{target:?}");
            assert!(link.parent().unwrap().join(&target).parent().unwrap().join("_internal/base_library.zip").is_file());
            assert_eq!(listing(&folders).len(), (i + 1).min(2), "keeps the new build and the one before");
        }
        assert_eq!(listing(&bin), ["yt-dlp"], "no temp links left");

        build("2026.04.04");
        let err = place_folder(&archive, &bin, "2026.04.04", "yt-dlp_macos", &|_| Err("killed".into())).unwrap_err();
        assert_eq!(err, "the downloaded yt-dlp does not run: killed");
        assert_eq!(version_of(&bin.join("yt-dlp"), Tool::YtDlp).unwrap(), "2026.03.03", "the old link survives");
        assert_eq!(listing(&folders).len(), 2, "no half-extracted folder left");
    }

    #[test]
    fn the_manifest_round_trips_and_tolerates_garbage() {
        let scratch = ScratchDir::new().unwrap();
        let bin = scratch.path();
        assert_eq!(Manifest::read(bin), Manifest::default());
        let mut manifest = Manifest::default();
        record(&mut manifest, Tool::YtDlp, "2026.08.19", "https://x/yt-dlp_macos", &sha_hex(b"x"), 3);
        manifest.write(bin).unwrap();
        let back = Manifest::read(bin);
        assert_eq!(back, manifest);
        let entry = back.entry(Tool::YtDlp).unwrap();
        assert_eq!((entry.version.as_str(), entry.bytes), ("2026.08.19", 3));
        assert_eq!(entry.installed_unix, entry.checked_unix);
        assert_eq!(listing(bin), [MANIFEST]);
        fs::write(bin.join(MANIFEST), "{not json").unwrap();
        assert_eq!(Manifest::read(bin), Manifest::default());
    }

    #[test]
    fn redirects_resolve_against_the_original_origin() {
        let base = "https://ffmpeg.martin-riedl.de/redirect/latest/macos/arm64/release/ffmpeg.zip";
        assert_eq!(
            absolute(base, "/download/macos/arm64/1789931890_9.0.2/ffmpeg.zip").as_deref(),
            Some("https://ffmpeg.martin-riedl.de/download/macos/arm64/1789931890_9.0.2/ffmpeg.zip")
        );
        let tag = "https://github.com/yt-dlp/yt-dlp/releases/tag/2026.08.19";
        assert_eq!(absolute(platform::YTDLP_LATEST, tag).as_deref(), Some(tag));
        assert_eq!(absolute(base, "http://evil.example/ffmpeg.zip"), None, "never downgrade to http");
        assert_eq!(absolute(base, "relative/ffmpeg.zip"), None);
    }

    #[test]
    fn a_copy_replaced_by_another_run_is_retried_without_the_network() {
        let scratch = ScratchDir::new().unwrap();
        let bin = scratch.path();
        fs::write(bin.join(Tool::YtDlp.file_name()), "new").unwrap();
        let mut manifest = Manifest::default();
        record(&mut manifest, Tool::YtDlp, "2026.08.19", "https://x", &sha_hex(b"new"), 3);
        manifest.write(bin).unwrap();
        let asset = platform::asset_for("linux", "x86_64").unwrap();
        let got = update_ytdlp(bin, &asset, Noise { quiet: true }, Some("2026.01.01")).unwrap();
        assert_eq!(got.as_deref(), Some("2026.08.19"));
    }

    #[test]
    fn versions_parse_from_the_first_line() {
        let ff = "ffmpeg version 9.0.2-https://www.martin-riedl.de Copyright (c) 2000-2026\nbuilt with clang\n";
        assert_eq!(parse_version(ff, Tool::Ffmpeg).as_deref(), Some("9.0.2-https://www.martin-riedl.de"));
        assert_eq!(parse_version("ffprobe version 8.0 Copyright", Tool::Ffprobe).as_deref(), Some("8.0"));
        assert_eq!(parse_version("\n2026.08.19\n", Tool::YtDlp).as_deref(), Some("2026.08.19"));
        assert_eq!(parse_version("usage: ffmpeg", Tool::Ffmpeg), None);
        assert_eq!(parse_version("", Tool::YtDlp), None);
    }

    #[test]
    fn a_second_lock_waits_for_the_first() {
        let scratch = ScratchDir::new().unwrap();
        let bin = scratch.path().to_path_buf();
        let first = Lock::acquire(&bin, Noise { quiet: true }).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = {
            let bin = bin.clone();
            std::thread::spawn(move || {
                let _second = Lock::acquire(&bin, Noise { quiet: true }).unwrap();
                tx.send(()).unwrap();
            })
        };
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err(), "the lock is exclusive");
        drop(first);
        assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok(), "released on drop");
        waiter.join().unwrap();
    }

    #[test]
    #[ignore = "downloads yt-dlp (~40 MB) from GitHub"]
    fn live_ytdlp_install() {
        let scratch = ScratchDir::new().unwrap();
        let asset = platform::current().expect("a supported platform");
        let done = install(scratch.path(), &asset, &[Tool::YtDlp], Noise { quiet: false }).unwrap();
        assert_eq!(done, [Tool::YtDlp]);
        assert!(Manifest::read(scratch.path()).entry(Tool::YtDlp).is_some());
        assert_eq!(update_ytdlp(scratch.path(), &asset, Noise { quiet: false }, None).unwrap(), None);
    }

    #[test]
    #[ignore = "downloads ffmpeg and ffprobe (~60 MB)"]
    fn live_ffmpeg_install() {
        let scratch = ScratchDir::new().unwrap();
        let asset = platform::current().expect("a supported platform");
        let done = install(scratch.path(), &asset, &[Tool::Ffmpeg, Tool::Ffprobe], Noise { quiet: false }).unwrap();
        assert_eq!(done, [Tool::Ffmpeg, Tool::Ffprobe]);
    }
}
