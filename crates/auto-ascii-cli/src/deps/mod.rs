//! First-use provisioning of ffmpeg, ffprobe and yt-dlp: resolve each (env
//! override, PATH, the auto-ascii cache), ask once before downloading what is
//! missing, keep a cached yt-dlp fresh, and describe it all for `doctor`.

pub mod fetch;
pub mod platform;

use std::ffi::OsString;
use std::io::{BufRead, IsTerminal};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use auto_ascii::tools::{self, Found, Lookup, Origin, Tool};
use serde::Serialize;

use fetch::{Manifest, Noise};
use platform::Asset;

pub const YES_ENV: &str = "AUTO_ASCII_YES";

pub const NO_DOWNLOAD_ENV: &str = "AUTO_ASCII_NO_DOWNLOAD";

pub const FETCH_COMMAND: &str = "auto-ascii doctor --fetch --yes";

pub const STALE_DAYS: u64 = 30;

pub const CA_BUNDLES: [&str; 4] = [
    "/etc/ssl/cert.pem",
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/ssl/ca-bundle.pem",
];

const DAY_SECS: u64 = 24 * 60 * 60;

pub type Update = Arc<dyn Fn(fetch::Cancel<'_>) -> Result<Option<String>, String> + Send + Sync>;

pub struct Ctx {
    pub yes: bool,
    pub json: bool,
    pub interactive: bool,
    pub no_download: bool,
    pub lookup: Lookup,
    pub asset: Option<Asset>,
}

impl Ctx {
    pub fn new(yes_flag: bool, json: bool) -> Ctx {
        Ctx {
            yes: yes_flag || truthy(std::env::var_os(YES_ENV)),
            json,
            interactive: std::io::stdin().is_terminal() && std::io::stderr().is_terminal(),
            no_download: truthy(std::env::var_os(NO_DOWNLOAD_ENV)),
            lookup: Lookup::from_env(),
            asset: platform::current(),
        }
    }

    fn bin(&self) -> Option<&Path> {
        self.lookup.cache_bin.as_deref()
    }

    fn noise(&self) -> Noise {
        Noise { quiet: self.json }
    }

    pub fn live(&self) -> Live {
        Live { asset: self.asset, noise: self.noise() }
    }
}

pub fn truthy(value: Option<OsString>) -> bool {
    value.is_some_and(|v| {
        let v = v.to_string_lossy().trim().to_ascii_lowercase();
        !v.is_empty() && !matches!(v.as_str(), "0" | "false" | "no" | "off")
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Consent {
    Granted,
    Ask,
    Refused,
}

pub fn consent(yes: bool, json: bool, interactive: bool) -> Consent {
    if yes {
        Consent::Granted
    } else if json || !interactive {
        Consent::Refused
    } else {
        Consent::Ask
    }
}

pub fn accepts(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes")
}

pub trait Provider {
    fn ask(&mut self, prompt: &str) -> bool;
    fn install(&mut self, bin: &Path, tools: &[Tool]) -> Result<(), String>;
    fn update_ytdlp(&mut self, bin: &Path) -> Result<Option<String>, String>;
}

pub struct Live {
    pub asset: Option<Asset>,
    pub noise: Noise,
}

impl Provider for Live {
    fn ask(&mut self, prompt: &str) -> bool {
        eprint!("auto-ascii: {prompt} [Y/n] ");
        let mut answer = String::new();
        match std::io::stdin().lock().read_line(&mut answer) {
            Ok(0) | Err(_) => {
                eprintln!();
                false
            }
            Ok(_) => accepts(&answer),
        }
    }

    fn install(&mut self, bin: &Path, tools: &[Tool]) -> Result<(), String> {
        let asset = self.asset.as_ref().ok_or("no standalone builds for this platform")?;
        fetch::install(bin, asset, tools, self.noise).map(|_| ())
    }

    fn update_ytdlp(&mut self, bin: &Path) -> Result<Option<String>, String> {
        let asset = self.asset.as_ref().ok_or("no standalone builds for this platform")?;
        fetch::update_ytdlp(bin, asset, self.noise, None, &fetch::never)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved(Vec<(Tool, Found)>);

impl Resolved {
    pub fn path(&self, tool: Tool) -> PathBuf {
        self.found(tool).map_or_else(|| PathBuf::from(tool.name()), |f| f.path.clone())
    }

    pub fn origin(&self, tool: Tool) -> Option<Origin> {
        self.found(tool).map(|f| f.origin)
    }

    fn found(&self, tool: Tool) -> Option<&Found> {
        self.0.iter().find(|(t, _)| *t == tool).map(|(_, f)| f)
    }
}

pub fn ensure(ctx: &Ctx, tools: &[Tool], provider: &mut dyn Provider) -> Result<Resolved, String> {
    provide(ctx, tools, false, provider)
}

pub fn provide(ctx: &Ctx, tools: &[Tool], refresh: bool, provider: &mut dyn Provider) -> Result<Resolved, String> {
    let missing: Vec<Tool> = tools.iter().copied().filter(|t| ctx.lookup.find(*t).is_none()).collect();
    let stale = stale_ytdlp(ctx, tools);
    if let Some(age) = stale
        && !refresh
    {
        if ctx.yes {
            refresh_ytdlp(ctx, age, provider);
        } else {
            ctx.noise().say(&format!(
                "the cached yt-dlp was last checked for updates {age} days ago; `{FETCH_COMMAND}` refreshes it \
                 (a failing run also updates it once)"
            ));
        }
    }
    let refresh_now = refresh && stale.is_some();
    if missing.is_empty() && !refresh_now {
        return Ok(resolved(ctx, tools));
    }

    let names = listing(&missing);
    let bin = ctx.bin();
    let install_hint = install_hint(&missing);
    if ctx.no_download {
        if missing.is_empty() {
            return Ok(resolved(ctx, tools));
        }
        return Err(format!(
            "{names} not found on PATH{}, and {NO_DOWNLOAD_ENV} is set: install {} yourself{install_hint} \
             or unset {NO_DOWNLOAD_ENV} to let auto-ascii download standalone builds",
            bin.map_or_else(String::new, |b| format!(" or in {}", b.display())),
            them(&missing),
        ));
    }
    let Some(asset) = ctx.asset else {
        return Err(format!(
            "{names} not found on PATH, and auto-ascii has no standalone build for {}: install {} \
             yourself{install_hint}",
            platform::platform(),
            them(&missing),
        ));
    };
    let Some(bin) = bin else {
        return Err(format!(
            "{names} not found on PATH, and there is no cache folder to download into (set HOME or \
             {}): install {} yourself{install_hint}",
            tools::CACHE_DIR_ENV,
            them(&missing),
        ));
    };

    let mut wanted = missing.clone();
    if refresh_now {
        wanted.push(Tool::YtDlp);
    }
    let what = if missing.is_empty() {
        "the cached yt-dlp is out of date".to_string()
    } else {
        format!("{names} not found")
    };
    let size = asset.download_mb(&wanted);
    let builds = if wanted.len() == 1 { "a standalone build" } else { "standalone builds" };
    match consent(ctx.yes, ctx.json, ctx.interactive) {
        Consent::Granted => {}
        Consent::Ask => {
            let prompt = format!("{what}. Download {builds} (~{size} MB) to {}?", bin.display());
            if !provider.ask(&prompt) {
                return Err(format!(
                    "{what} and the download was declined: install {} yourself{install_hint}, or run \
                     `{FETCH_COMMAND}` later",
                    them(&wanted)
                ));
            }
        }
        Consent::Refused => {
            return Err(format!(
                "{what}; auto-ascii can download {builds} (~{size} MB) to {}, but {} never prompts: \
                 re-run with --yes (or {YES_ENV}=1), pre-fetch with `{FETCH_COMMAND}`, or install {} \
                 yourself{install_hint}",
                bin.display(),
                if ctx.json { "--json" } else { "a non-interactive run" },
                them(&wanted),
            ));
        }
    }
    if !missing.is_empty() {
        provider.install(bin, &missing)?;
    }
    if refresh_now {
        provider.update_ytdlp(bin)?;
    }
    let found = resolved(ctx, tools);
    if let Some(tool) = tools.iter().find(|t| found.origin(**t).is_none()) {
        return Err(format!("{} is still missing from {} after the download", tool.name(), bin.display()));
    }
    Ok(found)
}

fn resolved(ctx: &Ctx, tools: &[Tool]) -> Resolved {
    Resolved(tools.iter().filter_map(|t| ctx.lookup.find(*t).map(|f| (*t, f))).collect())
}

fn stale_ytdlp(ctx: &Ctx, tools: &[Tool]) -> Option<u64> {
    if !tools.contains(&Tool::YtDlp) || ctx.no_download {
        return None;
    }
    let found = ctx.lookup.find(Tool::YtDlp)?;
    if found.origin != Origin::Cache {
        return None;
    }
    let entry = Manifest::read(ctx.bin()?).entry(Tool::YtDlp).cloned()?;
    let age = age_days(entry.checked_unix.max(entry.installed_unix), fetch::now_unix());
    (age >= STALE_DAYS).then_some(age)
}

fn refresh_ytdlp(ctx: &Ctx, age: u64, provider: &mut dyn Provider) {
    let Some(bin) = ctx.bin() else { return };
    let noise = ctx.noise();
    noise.say(&format!("the cached yt-dlp was last checked for updates {age} days ago; checking now"));
    match provider.update_ytdlp(bin) {
        Ok(Some(version)) => noise.say(&format!("updated the cached yt-dlp to {version}")),
        Ok(None) => noise.say("the cached yt-dlp is already the latest release"),
        Err(e) => noise.say(&format!("could not update the cached yt-dlp ({e}); using it as it is")),
    }
}

pub fn age_days(since_unix: u64, now_unix: u64) -> u64 {
    now_unix.saturating_sub(since_unix) / DAY_SECS
}

pub fn ca_file(resolved: &Resolved) -> Option<PathBuf> {
    let own_store = cfg!(windows) || std::env::var_os("SSL_CERT_FILE").is_some();
    if own_store || resolved.origin(Tool::Ffmpeg) != Some(Origin::Cache) {
        return None;
    }
    first_file(CA_BUNDLES.map(PathBuf::from))
}

pub fn first_file(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|p| p.is_file())
}

pub fn ytdlp_updater(ctx: &Ctx, resolved: &Resolved) -> Option<Update> {
    if resolved.origin(Tool::YtDlp) != Some(Origin::Cache) || ctx.no_download {
        return None;
    }
    let bin = ctx.bin()?.to_path_buf();
    let asset = ctx.asset?;
    match consent(ctx.yes, ctx.json, ctx.interactive) {
        Consent::Granted => {}
        Consent::Ask => return None,
        Consent::Refused => {
            let why = format!(
                "{} never downloads without --yes: re-run with --yes (or {YES_ENV}=1) to let auto-ascii update it",
                if ctx.json { "--json" } else { "a non-interactive run" }
            );
            return Some(Arc::new(move |_| Err(why.clone())));
        }
    }
    let ran_with = Manifest::read(&bin).entry(Tool::YtDlp).map(|e| e.version.clone());
    Some(Arc::new(move |cancel| fetch::update_ytdlp(&bin, &asset, Noise { quiet: true }, ran_with.as_deref(), cancel)))
}

pub fn offer_update(ctx: &Ctx, resolved: &Resolved, err: &str, provider: &mut dyn Provider) -> Option<String> {
    let asking = consent(ctx.yes, ctx.json, ctx.interactive) == Consent::Ask;
    let cached = resolved.origin(Tool::YtDlp) == Some(Origin::Cache);
    if !asking || !cached || ctx.no_download || !crate::stream::ytdlp::failed_itself(err) {
        return None;
    }
    let (bin, asset) = (ctx.bin()?, ctx.asset?);
    let prompt = format!(
        "the cached yt-dlp failed ({err}). Download the latest yt-dlp (~{} MB) and try again?",
        asset.download_mb(&[Tool::YtDlp])
    );
    if !provider.ask(&prompt) {
        return None;
    }
    let noise = ctx.noise();
    match provider.update_ytdlp(bin) {
        Ok(Some(version)) => {
            noise.say(&format!("updated the cached yt-dlp to {version}; trying again"));
            Some(version)
        }
        Ok(None) => {
            noise.say("the cached yt-dlp is already the latest release");
            None
        }
        Err(e) => {
            noise.say(&format!("could not update the cached yt-dlp: {e}"));
            None
        }
    }
}

fn listing(tools: &[Tool]) -> String {
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn them(tools: &[Tool]) -> &'static str {
    if tools.len() == 1 { "it" } else { "them" }
}

fn install_hint(tools: &[Tool]) -> String {
    let mut packages: Vec<&str> =
        tools.iter().map(|t| if *t == Tool::YtDlp { "yt-dlp" } else { "ffmpeg" }).collect();
    packages.dedup();
    let packages = packages.join(" ");
    if packages.is_empty() {
        return String::new();
    }
    match std::env::consts::OS {
        "macos" => format!(" (`brew install {packages}`)"),
        "windows" => " (winget, scoop or the projects' own downloads)".to_string(),
        _ => format!(" (your package manager, e.g. `apt install {packages}`)"),
    }
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub platform: String,
    pub supported: bool,
    pub downloads: &'static str,
    pub cache_dir: Option<String>,
    pub bin_dir: Option<String>,
    pub tools: Vec<ToolReport>,
}

#[derive(Debug, Serialize)]
pub struct ToolReport {
    pub name: &'static str,
    pub source: &'static str,
    pub path: Option<String>,
    pub version: Option<String>,
    pub error: Option<String>,
    pub cached: Option<CachedReport>,
}

#[derive(Debug, Serialize)]
pub struct CachedReport {
    pub path: String,
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
    pub installed_unix: u64,
    pub age_days: u64,
    pub stale: bool,
}

pub fn report(ctx: &Ctx) -> Report {
    let manifest = ctx.bin().map(Manifest::read).unwrap_or_default();
    let now = fetch::now_unix();
    let tools = Tool::ALL
        .into_iter()
        .map(|tool| {
            let found = ctx.lookup.find(tool);
            let (version, error) = match &found {
                Some(f) => match fetch::version_of(&f.path, tool) {
                    Ok(v) => (Some(v), None),
                    Err(e) => (None, Some(e)),
                },
                None => (None, None),
            };
            let cached = ctx.lookup.cached(tool).map(|path| {
                let entry = manifest.entry(tool);
                let checked = entry.map_or(0, |e| e.checked_unix.max(e.installed_unix));
                let age = age_days(checked, now);
                CachedReport {
                    path: path.display().to_string(),
                    version: entry.map_or_else(|| "unknown".into(), |e| e.version.clone()),
                    url: entry.map_or_else(String::new, |e| e.url.clone()),
                    sha256: entry.map_or_else(String::new, |e| e.sha256.clone()),
                    bytes: entry.map_or(0, |e| e.bytes),
                    installed_unix: entry.map_or(0, |e| e.installed_unix),
                    age_days: age,
                    stale: tool == Tool::YtDlp && entry.is_some() && age >= STALE_DAYS,
                }
            });
            ToolReport {
                name: tool.name(),
                source: found.as_ref().map_or("missing", |f| f.origin.as_str()),
                path: found.map(|f| f.path.display().to_string()),
                version,
                error,
                cached,
            }
        })
        .collect();
    Report {
        platform: platform::platform(),
        supported: ctx.asset.is_some(),
        downloads: if ctx.no_download {
            "disabled (AUTO_ASCII_NO_DOWNLOAD)"
        } else if ctx.asset.is_none() {
            "unavailable on this platform"
        } else if ctx.yes {
            "allowed without asking (--yes / AUTO_ASCII_YES)"
        } else {
            "allowed after asking"
        },
        cache_dir: ctx.bin().and_then(Path::parent).map(|p| p.display().to_string()),
        bin_dir: ctx.bin().map(|p| p.display().to_string()),
        tools,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::procs::ScratchDir;

    #[derive(Default)]
    struct Fake {
        answer: bool,
        asked: Vec<String>,
        installs: Vec<Vec<Tool>>,
        updates: usize,
        fail: Option<String>,
    }

    impl Provider for Fake {
        fn ask(&mut self, prompt: &str) -> bool {
            self.asked.push(prompt.to_string());
            self.answer
        }

        fn install(&mut self, bin: &Path, tools: &[Tool]) -> Result<(), String> {
            if let Some(e) = &self.fail {
                return Err(e.clone());
            }
            self.installs.push(tools.to_vec());
            std::fs::create_dir_all(bin).unwrap();
            for tool in tools {
                exe(&bin.join(tool.file_name()));
            }
            Ok(())
        }

        fn update_ytdlp(&mut self, _: &Path) -> Result<Option<String>, String> {
            self.updates += 1;
            Ok(Some("2099.01.01".into()))
        }
    }

    fn exe(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    struct Env {
        _scratch: ScratchDir,
        path_dir: PathBuf,
        bin: PathBuf,
    }

    impl Env {
        fn new() -> Env {
            let scratch = ScratchDir::new().unwrap();
            let path_dir = scratch.path().join("path");
            std::fs::create_dir_all(&path_dir).unwrap();
            let bin = tools::bin_dir(&scratch.path().join("cache"));
            Env { _scratch: scratch, path_dir, bin }
        }

        fn ctx(&self, yes: bool, json: bool, interactive: bool) -> Ctx {
            Ctx {
                yes,
                json,
                interactive,
                no_download: false,
                lookup: Lookup { overrides: vec![], search: vec![self.path_dir.clone()], cache_bin: Some(self.bin.clone()) },
                asset: platform::asset_for("macos", "aarch64"),
            }
        }
    }

    const FF: [Tool; 2] = [Tool::Ffmpeg, Tool::Ffprobe];

    #[test]
    fn consent_follows_flag_then_env_then_terminal() {
        assert_eq!(consent(true, true, false), Consent::Granted);
        assert_eq!(consent(true, false, true), Consent::Granted);
        assert_eq!(consent(false, false, true), Consent::Ask);
        assert_eq!(consent(false, true, true), Consent::Refused, "--json never prompts");
        assert_eq!(consent(false, false, false), Consent::Refused, "no terminal to ask on");
        for yes in ["1", "true", "YES", "on", " y "] {
            assert!(truthy(Some(yes.into())), "{yes}");
        }
        for no in ["", "0", "false", "No", "off"] {
            assert!(!truthy(Some(no.into())), "{no:?}");
        }
        assert!(!truthy(None));
        for answer in ["", "\n", "y\n", "Yes", " YES "] {
            assert!(accepts(answer), "{answer:?}");
        }
        for answer in ["n", "no\n", "nope", "q"] {
            assert!(!accepts(answer), "{answer:?}");
        }
    }

    #[test]
    fn present_tools_resolve_without_asking_env_then_path_then_cache() {
        let env = Env::new();
        exe(&env.bin.join(Tool::Ffprobe.file_name()));
        exe(&env.path_dir.join(Tool::Ffmpeg.file_name()));
        let mut ctx = env.ctx(false, false, false);
        ctx.lookup.overrides.push((Tool::YtDlp, PathBuf::from("/opt/custom/yt-dlp")));
        let mut fake = Fake::default();
        let got = ensure(&ctx, &Tool::ALL, &mut fake).unwrap();
        assert_eq!(got.origin(Tool::YtDlp), Some(Origin::Env));
        assert_eq!(got.path(Tool::YtDlp), PathBuf::from("/opt/custom/yt-dlp"));
        assert_eq!(got.origin(Tool::Ffmpeg), Some(Origin::Path));
        assert_eq!(got.origin(Tool::Ffprobe), Some(Origin::Cache));
        assert_eq!(got.path(Tool::Ffprobe), env.bin.join(Tool::Ffprobe.file_name()));
        assert!(fake.asked.is_empty() && fake.installs.is_empty());
    }

    #[test]
    fn missing_tools_are_asked_for_once_then_downloaded_into_the_cache() {
        let env = Env::new();
        let ctx = env.ctx(false, false, true);
        let mut fake = Fake { answer: true, ..Fake::default() };
        let got = ensure(&ctx, &FF, &mut fake).unwrap();
        assert_eq!(fake.asked.len(), 1, "one combined prompt: {:?}", fake.asked);
        let prompt = &fake.asked[0];
        assert!(prompt.starts_with("ffmpeg and ffprobe not found. Download standalone builds (~58 MB) to "), "{prompt}");
        assert!(prompt.ends_with(&format!("{}?", env.bin.display())), "{prompt}");
        assert_eq!(fake.installs, [FF.to_vec()]);
        assert_eq!(got.origin(Tool::Ffmpeg), Some(Origin::Cache));
        assert_eq!(got.origin(Tool::Ffprobe), Some(Origin::Cache));
    }

    #[test]
    fn a_declined_prompt_downloads_nothing() {
        let env = Env::new();
        let mut fake = Fake::default();
        let err = ensure(&env.ctx(false, false, true), &[Tool::YtDlp], &mut fake).unwrap_err();
        assert!(err.starts_with("yt-dlp not found and the download was declined: install it yourself"), "{err}");
        assert!(err.contains(FETCH_COMMAND), "{err}");
        assert_eq!(fake.asked.len(), 1);
        assert!(fake.asked[0].contains("Download a standalone build (~54 MB)"), "{:?}", fake.asked);
        assert!(fake.installs.is_empty());
        assert!(!env.bin.exists(), "nothing created before consent");
    }

    #[test]
    fn yes_downloads_without_asking() {
        let env = Env::new();
        let mut fake = Fake::default();
        let got = ensure(&env.ctx(true, true, false), &[Tool::YtDlp, Tool::Ffmpeg], &mut fake).unwrap();
        assert!(fake.asked.is_empty());
        assert_eq!(fake.installs, [vec![Tool::YtDlp, Tool::Ffmpeg]]);
        assert_eq!(got.origin(Tool::YtDlp), Some(Origin::Cache));
    }

    #[test]
    fn json_and_non_interactive_runs_refuse_with_the_ways_out() {
        let env = Env::new();
        for (json, mode) in [(true, "--json never prompts"), (false, "a non-interactive run never prompts")] {
            let mut fake = Fake::default();
            let err = ensure(&env.ctx(false, json, json), &FF, &mut fake).unwrap_err();
            assert!(err.starts_with("ffmpeg and ffprobe not found; auto-ascii can download standalone builds (~58 MB)"), "{err}");
            for needle in [mode, "--yes", "AUTO_ASCII_YES=1", FETCH_COMMAND] {
                assert!(err.contains(needle), "{needle:?} missing from {err}");
            }
            assert!(fake.asked.is_empty() && fake.installs.is_empty());
        }
    }

    #[test]
    fn opting_out_or_an_unknown_platform_fails_without_network() {
        let env = Env::new();
        let mut ctx = env.ctx(true, false, true);
        ctx.no_download = true;
        let mut fake = Fake::default();
        let err = ensure(&ctx, &[Tool::Ffmpeg], &mut fake).unwrap_err();
        assert!(err.starts_with("ffmpeg not found on PATH or in "), "{err}");
        assert!(err.contains("AUTO_ASCII_NO_DOWNLOAD is set: install it yourself"), "{err}");
        assert!(err.contains("unset AUTO_ASCII_NO_DOWNLOAD"), "{err}");

        let mut ctx = env.ctx(true, false, true);
        ctx.asset = None;
        let err = ensure(&ctx, &FF, &mut fake).unwrap_err();
        assert!(err.contains("has no standalone build for"), "{err}");
        assert!(err.contains("install them yourself"), "{err}");
        assert!(fake.installs.is_empty());
    }

    #[test]
    fn a_failed_download_is_the_error() {
        let env = Env::new();
        let mut fake = Fake { fail: Some("GET https://x: timed out".into()), ..Fake::default() };
        let err = ensure(&env.ctx(true, false, false), &[Tool::YtDlp], &mut fake).unwrap_err();
        assert_eq!(err, "GET https://x: timed out");
    }

    fn cached_ytdlp(env: &Env, days_old: u64) {
        exe(&env.bin.join(Tool::YtDlp.file_name()));
        let then = fetch::now_unix() - days_old * DAY_SECS;
        let entry = fetch::Entry {
            version: "2026.01.01".into(),
            url: "https://github.com/yt-dlp/yt-dlp/releases/download/2026.01.01/yt-dlp_macos".into(),
            sha256: "0".repeat(64),
            bytes: 1,
            installed_unix: then,
            checked_unix: then,
        };
        let manifest = Manifest { tools: [("yt-dlp".to_string(), entry)].into() };
        std::fs::write(env.bin.join(fetch::MANIFEST), serde_json::to_vec(&manifest).unwrap()).unwrap();
    }

    #[test]
    fn a_stale_cached_ytdlp_is_refreshed_only_with_consent() {
        let env = Env::new();
        cached_ytdlp(&env, STALE_DAYS + 5);
        let mut fake = Fake::default();
        ensure(&env.ctx(false, false, true), &[Tool::YtDlp], &mut fake).unwrap();
        assert_eq!((fake.updates, fake.asked.len()), (0, 0), "no --yes: only a note");
        ensure(&env.ctx(true, true, false), &[Tool::YtDlp], &mut fake).unwrap();
        assert_eq!(fake.updates, 1, "--yes refreshes it");

        let fresh = Env::new();
        cached_ytdlp(&fresh, 2);
        let mut fake = Fake::default();
        ensure(&fresh.ctx(true, false, false), &[Tool::YtDlp], &mut fake).unwrap();
        assert_eq!(fake.updates, 0);
    }

    #[test]
    fn fetch_asks_before_refreshing_a_stale_ytdlp() {
        let env = Env::new();
        cached_ytdlp(&env, STALE_DAYS);
        let mut fake = Fake { answer: true, ..Fake::default() };
        provide(&env.ctx(false, false, true), &[Tool::YtDlp], true, &mut fake).unwrap();
        assert_eq!(fake.asked.len(), 1);
        assert!(fake.asked[0].starts_with("the cached yt-dlp is out of date. Download a standalone build"), "{:?}", fake.asked);
        assert_eq!((fake.updates, fake.installs.len()), (1, 0));
    }

    #[test]
    fn a_ytdlp_from_path_or_env_is_never_updated() {
        let env = Env::new();
        cached_ytdlp(&env, 400);
        exe(&env.path_dir.join(Tool::YtDlp.file_name()));
        let ctx = env.ctx(true, false, true);
        let mut fake = Fake::default();
        let got = ensure(&ctx, &[Tool::YtDlp], &mut fake).unwrap();
        assert_eq!(got.origin(Tool::YtDlp), Some(Origin::Path));
        assert_eq!(fake.updates, 0);
        assert!(ytdlp_updater(&ctx, &got).is_none());

        std::fs::remove_file(env.path_dir.join(Tool::YtDlp.file_name())).unwrap();
        let got = ensure(&ctx, &[Tool::YtDlp], &mut fake).unwrap();
        assert_eq!(got.origin(Tool::YtDlp), Some(Origin::Cache));
        assert!(ytdlp_updater(&ctx, &got).is_some(), "the cached copy may be updated");
    }

    #[test]
    fn an_interactive_run_asks_after_the_failure_instead_of_updating_in_the_background() {
        let env = Env::new();
        cached_ytdlp(&env, 1);
        let ctx = env.ctx(false, false, true);
        let got = ensure(&ctx, &[Tool::YtDlp], &mut Fake::default()).unwrap();
        assert!(ytdlp_updater(&ctx, &got).is_none(), "no update without asking");

        let broke = "yt-dlp: Unable to extract initial player response";
        let mut declined = Fake::default();
        assert_eq!(offer_update(&ctx, &got, broke, &mut declined), None);
        assert_eq!(declined.asked.len(), 1);
        assert!(declined.asked[0].starts_with(&format!("the cached yt-dlp failed ({broke}). Download the latest yt-dlp (~54 MB)")));
        assert_eq!(declined.updates, 0);

        let mut accepted = Fake { answer: true, ..Fake::default() };
        assert_eq!(offer_update(&ctx, &got, broke, &mut accepted).as_deref(), Some("2099.01.01"));
        assert_eq!(accepted.updates, 1);

        let mut untouched = Fake { answer: true, ..Fake::default() };
        assert_eq!(offer_update(&ctx, &got, "no videos found for \"x\"", &mut untouched), None);
        assert_eq!(offer_update(&env.ctx(true, false, true), &got, broke, &mut untouched), None, "--yes updated in-run");
        assert!(untouched.asked.is_empty() && untouched.updates == 0);
    }

    #[test]
    fn without_consent_a_failing_cached_ytdlp_is_not_updated() {
        let env = Env::new();
        cached_ytdlp(&env, 1);
        for json in [true, false] {
            let ctx = env.ctx(false, json, json);
            let got = ensure(&ctx, &[Tool::YtDlp], &mut Fake::default()).unwrap();
            let update = ytdlp_updater(&ctx, &got).expect("an explanation, not a download");
            let err = update(&fetch::never).unwrap_err();
            assert!(err.contains("never downloads without --yes") && err.contains("AUTO_ASCII_YES=1"), "{err}");
        }
    }

    #[test]
    fn only_the_cached_ffmpeg_gets_a_ca_bundle() {
        let env = Env::new();
        exe(&env.path_dir.join(Tool::Ffmpeg.file_name()));
        let ctx = env.ctx(false, false, false);
        let mut fake = Fake::default();
        let got = ensure(&ctx, &[Tool::Ffmpeg], &mut fake).unwrap();
        assert_eq!(ca_file(&got), None, "a user's ffmpeg keeps its own trust store");

        let a = env.path_dir.join("a.pem");
        let b = env.path_dir.join("b.pem");
        std::fs::write(&b, "cert").unwrap();
        assert_eq!(first_file([a.clone(), b.clone()]), Some(b.clone()));
        std::fs::write(&a, "cert").unwrap();
        assert_eq!(first_file([a.clone(), b]), Some(a));
        assert_eq!(first_file([env.path_dir.join("none.pem")]), None);
    }

    #[test]
    fn listings_read_as_english() {
        assert_eq!(listing(&[Tool::Ffmpeg]), "ffmpeg");
        assert_eq!(listing(&FF), "ffmpeg and ffprobe");
        assert_eq!(listing(&[Tool::YtDlp, Tool::Ffmpeg, Tool::Ffprobe]), "yt-dlp, ffmpeg and ffprobe");
        assert_eq!(age_days(0, 3 * DAY_SECS - 1), 2);
        assert_eq!(age_days(10, 5), 0);
    }
}
