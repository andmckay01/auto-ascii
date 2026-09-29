//! The standalone builds auto-ascii downloads for each OS and CPU: yt-dlp's
//! own release builds (a one-folder zip on macOS, which starts in a fraction
//! of the one-file build's time), and ffmpeg/ffprobe from Martin Riedl's
//! build server (macOS, Linux) or gyan.dev's essentials zip (Windows), plus
//! parsers for the URLs and checksum files they publish.

use auto_ascii::tools::Tool;

pub const YTDLP_LATEST: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest";

pub const YTDLP_RELEASES: &str = "https://github.com/yt-dlp/yt-dlp/releases/download";

pub const YTDLP_SUMS: &str = "SHA2-256SUMS";

pub const RIEDL_LATEST: &str = "https://ffmpeg.martin-riedl.de/redirect/latest";

pub const GYAN_LATEST: &str = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfmpegBuild {
    Riedl(&'static str),
    Gyan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YtdlpBuild {
    File(&'static str),
    Folder { zip: &'static str, exe: &'static str },
}

impl YtdlpBuild {
    pub fn asset(self) -> &'static str {
        match self {
            YtdlpBuild::File(name) => name,
            YtdlpBuild::Folder { zip, .. } => zip,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Asset {
    pub ytdlp: YtdlpBuild,
    pub ytdlp_mb: u32,
    pub ffmpeg: FfmpegBuild,
    pub ffmpeg_mb: u32,
}

pub fn asset_for(os: &str, arch: &str) -> Option<Asset> {
    let macos = YtdlpBuild::Folder { zip: "yt-dlp_macos.zip", exe: "yt-dlp_macos" };
    let (ytdlp, ytdlp_mb, ffmpeg, ffmpeg_mb) = match (os, arch) {
        ("macos", "aarch64") => (macos, 54, FfmpegBuild::Riedl("macos/arm64"), 29),
        ("macos", "x86_64") => (macos, 54, FfmpegBuild::Riedl("macos/amd64"), 34),
        ("linux", "x86_64") => (YtdlpBuild::File("yt-dlp_linux"), 41, FfmpegBuild::Riedl("linux/amd64"), 34),
        ("linux", "aarch64") => {
            (YtdlpBuild::File("yt-dlp_linux_aarch64"), 41, FfmpegBuild::Riedl("linux/arm64"), 29)
        }
        ("windows", "x86_64") => (YtdlpBuild::File("yt-dlp.exe"), 18, FfmpegBuild::Gyan, 115),
        _ => return None,
    };
    Some(Asset { ytdlp, ytdlp_mb, ffmpeg, ffmpeg_mb })
}

pub fn current() -> Option<Asset> {
    asset_for(std::env::consts::OS, std::env::consts::ARCH)
}

pub fn platform() -> String {
    format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH)
}

impl Asset {
    pub fn download_mb(&self, tools: &[Tool]) -> u32 {
        let programs = tools.iter().filter(|t| **t != Tool::YtDlp).count() as u32;
        let ffmpeg = match self.ffmpeg {
            FfmpegBuild::Riedl(_) => programs * self.ffmpeg_mb,
            FfmpegBuild::Gyan if programs > 0 => self.ffmpeg_mb,
            FfmpegBuild::Gyan => 0,
        };
        ffmpeg + if tools.contains(&Tool::YtDlp) { self.ytdlp_mb } else { 0 }
    }
}

impl FfmpegBuild {
    pub fn latest_url(self) -> String {
        match self {
            FfmpegBuild::Riedl(platform) => format!("{RIEDL_LATEST}/{platform}/release/ffmpeg.zip"),
            FfmpegBuild::Gyan => GYAN_LATEST.to_string(),
        }
    }

    pub fn version_of(self, resolved: &str) -> Option<String> {
        let file = resolved.rsplit('/').next()?;
        match self {
            FfmpegBuild::Riedl(_) => {
                let dir = resolved.rsplit('/').nth(1)?;
                let (_, version) = dir.split_once('_')?;
                Some(version.to_string()).filter(|v| safe_label(v))
            }
            FfmpegBuild::Gyan => file
                .strip_prefix("ffmpeg-")?
                .strip_suffix("-essentials_build.zip")
                .map(str::to_string)
                .filter(|v| safe_label(v)),
        }
    }
}

pub fn ytdlp_tag(resolved: &str) -> Option<String> {
    let (_, tag) = resolved.split_once("/releases/tag/")?;
    let tag = tag.trim_end_matches('/');
    safe_label(tag).then(|| tag.to_string())
}

pub fn sums_entry(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hex, name) = line.trim().split_once(char::is_whitespace)?;
        let name = name.trim_start().trim_start_matches('*');
        (name == file).then(|| hex.to_ascii_lowercase()).filter(|h| is_sha256(h))
    })
}

pub fn single_sum(text: &str) -> Option<String> {
    let hex = text.split_whitespace().next()?.to_ascii_lowercase();
    is_sha256(&hex).then_some(hex)
}

pub fn is_sha256(hex: &str) -> bool {
    hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

fn safe_label(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_platform_has_an_asset() {
        let riedl = |p| FfmpegBuild::Riedl(p);
        for (os, arch, ytdlp, ffmpeg) in [
            ("macos", "aarch64", "yt-dlp_macos.zip", riedl("macos/arm64")),
            ("macos", "x86_64", "yt-dlp_macos.zip", riedl("macos/amd64")),
            ("linux", "x86_64", "yt-dlp_linux", riedl("linux/amd64")),
            ("linux", "aarch64", "yt-dlp_linux_aarch64", riedl("linux/arm64")),
            ("windows", "x86_64", "yt-dlp.exe", FfmpegBuild::Gyan),
        ] {
            let asset = asset_for(os, arch).unwrap_or_else(|| panic!("{os}/{arch}"));
            assert_eq!((asset.ytdlp.asset(), asset.ffmpeg), (ytdlp, ffmpeg), "{os}/{arch}");
            assert!(asset.ytdlp_mb > 0 && asset.ffmpeg_mb > 0);
        }
        for (os, arch) in [("windows", "aarch64"), ("freebsd", "x86_64"), ("linux", "riscv64"), ("macos", "x86")] {
            assert_eq!(asset_for(os, arch), None, "{os}/{arch}");
        }
        assert_eq!(
            asset_for("macos", "aarch64").unwrap().ffmpeg.latest_url(),
            "https://ffmpeg.martin-riedl.de/redirect/latest/macos/arm64/release/ffmpeg.zip"
        );
        assert_eq!(FfmpegBuild::Gyan.latest_url(), GYAN_LATEST);
        assert_eq!(
            asset_for("macos", "x86_64").unwrap().ytdlp,
            YtdlpBuild::Folder { zip: "yt-dlp_macos.zip", exe: "yt-dlp_macos" }
        );
    }

    #[test]
    fn download_sizes_count_each_riedl_program_but_one_gyan_zip() {
        let mac = asset_for("macos", "aarch64").unwrap();
        assert_eq!(mac.download_mb(&[Tool::Ffmpeg]), 29);
        assert_eq!(mac.download_mb(&[Tool::Ffmpeg, Tool::Ffprobe]), 58);
        assert_eq!(mac.download_mb(&Tool::ALL), 112);
        let win = asset_for("windows", "x86_64").unwrap();
        assert_eq!(win.download_mb(&[Tool::Ffmpeg, Tool::Ffprobe]), 115);
        assert_eq!(win.download_mb(&[Tool::YtDlp]), 18);
        assert_eq!(win.download_mb(&[]), 0);
    }

    #[test]
    fn versions_come_from_the_resolved_urls() {
        let riedl = FfmpegBuild::Riedl("macos/arm64");
        let url = "https://ffmpeg.martin-riedl.de/download/macos/arm64/1789931890_9.0.2/ffmpeg.zip";
        assert_eq!(riedl.version_of(url).as_deref(), Some("9.0.2"));
        assert_eq!(riedl.version_of("https://ffmpeg.martin-riedl.de/redirect/latest/x.zip"), None);
        let gyan = "https://www.gyan.dev/ffmpeg/builds/packages/ffmpeg-9.0.2-essentials_build.zip";
        assert_eq!(FfmpegBuild::Gyan.version_of(gyan).as_deref(), Some("9.0.2"));
        assert_eq!(FfmpegBuild::Gyan.version_of(GYAN_LATEST), None);
        let tag = "https://github.com/yt-dlp/yt-dlp/releases/tag/2026.08.19";
        assert_eq!(ytdlp_tag(tag).as_deref(), Some("2026.08.19"));
        assert_eq!(ytdlp_tag("https://github.com/yt-dlp/yt-dlp/releases/latest"), None);
        assert_eq!(ytdlp_tag("https://github.com/x/releases/tag/a%2F..%2Fb"), None);
    }

    const A: &str = "66674953fe251b89f4d08c5f0e35e0728679bd67ab3d7d05c0562af101dd3e7a";
    const B: &str = "0f192b7ec147ab6288885d6351d9ab67367640029b4377576ef46dd79cf7b202";

    #[test]
    fn sha2_256sums_lines_are_matched_by_exact_name() {
        let sums = format!("{A}  yt-dlp.exe\n{B}  yt-dlp_macos\nabc  yt-dlp_linux\n{A} *yt-dlp_macos_legacy\n");
        assert_eq!(sums_entry(&sums, "yt-dlp_macos").as_deref(), Some(B));
        assert_eq!(sums_entry(&sums, "yt-dlp.exe").as_deref(), Some(A));
        assert_eq!(sums_entry(&sums, "yt-dlp_macos_legacy").as_deref(), Some(A));
        assert_eq!(sums_entry(&sums, "yt-dlp_linux"), None, "a malformed digest is no digest");
        assert_eq!(sums_entry(&sums, "yt-dlp"), None, "no prefix matches");
        assert_eq!(sums_entry("", "yt-dlp_macos"), None);
    }

    #[test]
    fn single_digest_files_take_the_first_token() {
        assert_eq!(single_sum(&format!("{B}  ffmpeg.zip\n")).as_deref(), Some(B));
        assert_eq!(single_sum(&format!("{}\n", A.to_uppercase())).as_deref(), Some(A));
        assert_eq!(single_sum("<html>not found</html>"), None);
        assert_eq!(single_sum(""), None);
    }
}
