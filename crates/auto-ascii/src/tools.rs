//! Where auto-ascii finds ffmpeg, ffprobe and yt-dlp without the network: an
//! explicit `AUTO_ASCII_<TOOL>` override, then PATH (the player adds the
//! Homebrew prefixes), then the standalone builds the `auto-ascii` CLI keeps
//! in `<cache dir>/bin`. Downloading lives in the CLI, never here.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const CACHE_DIR_ENV: &str = "AUTO_ASCII_CACHE_DIR";

pub const EXTRA_DIRS: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Tool {
    Ffmpeg,
    Ffprobe,
    YtDlp,
}

impl Tool {
    pub const ALL: [Tool; 3] = [Tool::Ffmpeg, Tool::Ffprobe, Tool::YtDlp];

    pub fn name(self) -> &'static str {
        match self {
            Tool::Ffmpeg => "ffmpeg",
            Tool::Ffprobe => "ffprobe",
            Tool::YtDlp => "yt-dlp",
        }
    }

    pub fn env(self) -> &'static str {
        match self {
            Tool::Ffmpeg => "AUTO_ASCII_FFMPEG",
            Tool::Ffprobe => "AUTO_ASCII_FFPROBE",
            Tool::YtDlp => "AUTO_ASCII_YTDLP",
        }
    }

    pub fn file_name(self) -> String {
        format!("{}{}", self.name(), std::env::consts::EXE_SUFFIX)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Env,
    Path,
    Cache,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Env => "env",
            Origin::Path => "path",
            Origin::Cache => "cache",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    pub origin: Origin,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lookup {
    pub overrides: Vec<(Tool, PathBuf)>,
    pub search: Vec<PathBuf>,
    pub cache_bin: Option<PathBuf>,
}

impl Lookup {
    pub fn from_env() -> Lookup {
        let overrides = Tool::ALL
            .into_iter()
            .filter_map(|tool| non_empty_var(tool.env()).map(|p| (tool, PathBuf::from(p))))
            .collect();
        let path = std::env::var_os("PATH").unwrap_or_default();
        let search = std::env::split_paths(&path).collect();
        Lookup { overrides, search, cache_bin: cache_dir().map(|dir| bin_dir(&dir)) }
    }

    pub fn with_extra_dirs(mut self) -> Lookup {
        self.search.extend(EXTRA_DIRS.map(PathBuf::from));
        self
    }

    pub fn find(&self, tool: Tool) -> Option<Found> {
        if let Some((_, path)) = self.overrides.iter().find(|(t, _)| *t == tool) {
            return Some(Found { path: path.clone(), origin: Origin::Env });
        }
        let name = tool.file_name();
        if let Some(path) = self.search.iter().map(|dir| dir.join(&name)).find(|p| is_executable(p)) {
            return Some(Found { path, origin: Origin::Path });
        }
        self.cached(tool).map(|path| Found { path, origin: Origin::Cache })
    }

    pub fn cached(&self, tool: Tool) -> Option<PathBuf> {
        self.cache_bin.as_ref().map(|dir| dir.join(tool.file_name())).filter(|p| is_executable(p))
    }

    pub fn program(&self, tool: Tool) -> PathBuf {
        self.find(tool).map_or_else(|| PathBuf::from(tool.name()), |found| found.path)
    }
}

pub fn cache_dir() -> Option<PathBuf> {
    let dir = match non_empty_var(CACHE_DIR_ENV) {
        Some(dir) => PathBuf::from(dir),
        None => platform_cache_dir(std::env::consts::OS, &non_empty_var)?.join("auto-ascii"),
    };
    Some(absolute(dir))
}

pub fn absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        return path;
    }
    std::env::current_dir().map_or(path.clone(), |cwd| cwd.join(path))
}

pub fn platform_cache_dir(os: &str, var: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    match os {
        "macos" => var("HOME").map(|home| PathBuf::from(home).join("Library").join("Caches")),
        "windows" => var("LOCALAPPDATA").map(PathBuf::from),
        _ => var("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .or_else(|| var("HOME").map(|home| PathBuf::from(home).join(".cache"))),
    }
}

pub fn bin_dir(cache_dir: &Path) -> PathBuf {
    cache_dir.join("bin")
}

pub fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

fn non_empty_var(key: &str) -> Option<OsString> {
    std::env::var_os(key).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dirs(PathBuf);

    impl Dirs {
        fn new(tag: &str) -> Dirs {
            let root = std::env::temp_dir().join(format!("auto-ascii-tools-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Dirs(root)
        }

        fn dir(&self, name: &str) -> PathBuf {
            let dir = self.0.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn exe(&self, dir: &str, tool: Tool) -> PathBuf {
            let path = self.dir(dir).join(tool.file_name());
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            path
        }
    }

    impl Drop for Dirs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn env_beats_path_beats_cache() {
        let d = Dirs::new("order");
        let mut lookup = Lookup {
            overrides: vec![],
            search: vec![d.dir("path-a"), d.dir("path-b")],
            cache_bin: Some(d.dir("cache/bin")),
        };
        assert_eq!(lookup.find(Tool::Ffmpeg), None);
        assert_eq!(lookup.program(Tool::Ffmpeg), PathBuf::from("ffmpeg"));

        let cached = d.exe("cache/bin", Tool::Ffmpeg);
        assert_eq!(lookup.find(Tool::Ffmpeg), Some(Found { path: cached.clone(), origin: Origin::Cache }));
        assert_eq!(lookup.cached(Tool::Ffmpeg), Some(cached));

        let later = d.exe("path-b", Tool::Ffmpeg);
        assert_eq!(lookup.find(Tool::Ffmpeg), Some(Found { path: later, origin: Origin::Path }));
        let first = d.exe("path-a", Tool::Ffmpeg);
        assert_eq!(lookup.find(Tool::Ffmpeg), Some(Found { path: first, origin: Origin::Path }));

        lookup.overrides.push((Tool::Ffmpeg, PathBuf::from("/custom/ffmpeg")));
        assert_eq!(
            lookup.find(Tool::Ffmpeg),
            Some(Found { path: PathBuf::from("/custom/ffmpeg"), origin: Origin::Env })
        );
        assert_eq!(lookup.find(Tool::Ffprobe), None, "an override is per tool");
    }

    #[test]
    fn extra_dirs_come_after_path() {
        let lookup = Lookup { search: vec![PathBuf::from("/on/path")], ..Lookup::default() }.with_extra_dirs();
        let expected: Vec<PathBuf> =
            ["/on/path", "/opt/homebrew/bin", "/usr/local/bin"].into_iter().map(PathBuf::from).collect();
        assert_eq!(lookup.search, expected);
    }

    #[cfg(unix)]
    #[test]
    fn a_file_without_the_executable_bit_is_skipped() {
        let d = Dirs::new("exec");
        let dir = d.dir("path");
        std::fs::write(dir.join("yt-dlp"), "not a program").unwrap();
        let lookup = Lookup { search: vec![dir], ..Lookup::default() };
        assert_eq!(lookup.find(Tool::YtDlp), None);
    }

    #[test]
    fn platform_cache_dirs() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| OsString::from(v))
        };
        let home = env(&[("HOME", "/Users/me")]);
        assert_eq!(platform_cache_dir("macos", &home), Some(PathBuf::from("/Users/me/Library/Caches")));
        assert_eq!(platform_cache_dir("linux", &home), Some(PathBuf::from("/Users/me/.cache")));
        let xdg = env(&[("HOME", "/home/me"), ("XDG_CACHE_HOME", "/xdg")]);
        assert_eq!(platform_cache_dir("linux", &xdg), Some(PathBuf::from("/xdg")));
        let relative = env(&[("HOME", "/home/me"), ("XDG_CACHE_HOME", "rel")]);
        assert_eq!(platform_cache_dir("linux", &relative), Some(PathBuf::from("/home/me/.cache")));
        let win = env(&[("LOCALAPPDATA", r"C:\Users\me\AppData\Local")]);
        assert_eq!(platform_cache_dir("windows", &win), Some(PathBuf::from(r"C:\Users\me\AppData\Local")));
        assert_eq!(platform_cache_dir("windows", &home), None);
        assert_eq!(platform_cache_dir("macos", &env(&[])), None);
    }

    #[test]
    fn relative_cache_dirs_become_absolute() {
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(absolute(PathBuf::from("cache")), cwd.join("cache"));
        assert_eq!(absolute(PathBuf::from("/abs/cache")), PathBuf::from("/abs/cache"));
    }

    #[test]
    fn tool_names_and_overrides() {
        assert_eq!(Tool::ALL.map(Tool::name), ["ffmpeg", "ffprobe", "yt-dlp"]);
        assert_eq!(Tool::ALL.map(Tool::env), ["AUTO_ASCII_FFMPEG", "AUTO_ASCII_FFPROBE", "AUTO_ASCII_YTDLP"]);
        assert!(Tool::YtDlp.file_name().starts_with("yt-dlp"));
    }
}
