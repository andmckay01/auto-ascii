//! The whole command line, each flag defined once. Flag groups shared by
//! `play`, `compose play` and `dev sim` / `dev bench-seek` are flattened in;
//! the advanced ones are hidden from `play --help` and shown by `--help-all`.

use std::path::PathBuf;

use auto_ascii::Style;
use auto_ascii_term::ColorTier;
use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};

pub const ROOT_AFTER_HELP: &str = "Start with: auto-ascii add <link-or-file>, then auto-ascii play <name>\n\
                                   More options: auto-ascii play --help-all";

pub const PLAY_AFTER_HELP: &str = "Exit status: 0 when playback reaches the end, 3 when the \
    viewer quits (q, Esc, Ctrl-C), 1 on an error.\n\
    Advanced terminal, headless and benchmark flags: add --help-all";

#[derive(Parser)]
#[command(
    name = "auto-ascii",
    version,
    about = "Play, stream, and make ASCII-art video.",
    after_help = ROOT_AFTER_HELP
)]
pub struct Cli {
    #[arg(long, global = true, display_order = 900, help = "Use JSON output and errors")]
    pub json: bool,
    #[arg(
        short = 'y',
        long,
        global = true,
        display_order = 901,
        help = "Download missing media tools without asking"
    )]
    pub yes: bool,
    #[arg(long = "help-all", global = true, display_order = 902, help = "Show all commands and advanced options")]
    pub help_all: bool,
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand)]
pub enum Cmd {
    #[command(
        about = "Play a file, library clip, or composition",
        after_help = PLAY_AFTER_HELP
    )]
    Play(PlayArgs),
    #[command(
        about = "Stream a video link or search with sound",
        after_help = "Plays the first YouTube video behind a link (or search terms) as live ASCII \
                      with sound, streamed, never downloaded. Needs yt-dlp and ffmpeg (downloaded \
                      on first use if missing).\n\
                      Keys: q / Esc / Ctrl-C quit (also while loading), / cycles the glyph style."
    )]
    Stream(StreamArgs),
    #[command(
        about = "Add a YouTube link or a video file to the library, ready to play",
        after_help = "A link is downloaded with yt-dlp (best video up to 1080p plus AAC audio; \
                      a 403, stall or timeout is retried, then tried at 720p; never with browser \
                      cookies) into \
                      `<folder>/source.mp4`; a local file is used where it lies. The folder \
                      (`library/<slug>/`, or under --library) then gets `<Title>.ascii` (480x270 \
                      @ 30 fps), its soundtrack `<Title>.m4a`, a `<Title>.json` sidecar, a \
                      `play.command` launcher and download/distill logs, after an integrity \
                      check and a length check against the source. Needs ffmpeg and ffprobe, \
                      plus yt-dlp for links (downloaded on first use if missing)."
    )]
    Add(AddArgs),
    #[command(
        about = "Turn a video into a .ascii asset",
        after_help = "Without -o the asset lands in the library as `library/<name>.ascii` plus a \
                      `<name>.json` provenance sidecar. With -o it is written to exactly that \
                      path. Needs ffmpeg and ffprobe (downloaded on first use if missing)."
    )]
    Import(ImportArgs),
    #[command(
        about = "List library clips",
        after_help = "Columns: name, duration, fps, frames, bytes, source."
    )]
    List,
    #[command(about = "Show clip details", after_help = "Reads the clip's header and its sidecar.")]
    Info {
        #[arg(help = "A path if one exists, else `library/<clip>.ascii`")]
        clip: String,
    },
    #[command(
        about = "Save part of a clip",
        after_help = "Slices one clip into a new library clip (an export of a one-clip composition)."
    )]
    Cut {
        #[arg(help = "A path if one exists, else `library/<clip>.ascii`")]
        clip: String,
        #[arg(
            long = "in",
            help = "Start of the slice inside the clip: SS[.f], MM:SS[.f] or HH:MM:SS[.f]"
        )]
        in_spec: String,
        #[arg(long = "out", help = "End of the slice inside the clip, same formats as --in")]
        out_spec: String,
        #[arg(
            long,
            help = "Library name for the slice (kebab-cased). Default: `<clip>-<in>-<out>`, e.g. \
                    `apple-1984-0m05s-0m20s`"
        )]
        name: Option<String>,
        #[arg(long, help = "Replace an existing clip of the same name")]
        force: bool,
    },
    #[command(
        about = "Create, edit, play, or export a composition",
        after_help = "Compositions are the `schema = 1` TOML timelines in `compositions/`."
    )]
    Compose {
        #[command(subcommand)]
        cmd: ComposeCmd,
    },
    #[command(
        about = "Show and create the library folders",
        after_help = "Prints the resolved home folder ($AUTO_ASCII_HOME, default ~/auto-ascii), \
                      creating it and its library/, compositions/ and exports/ subfolders."
    )]
    Home,
    #[command(
        about = "Print the guide for agents",
        after_help = "The folder layout, the JSON shapes and the composition schema."
    )]
    AgentGuide,
    #[command(
        about = "Check or fetch media tools",
        after_help = "Shows where ffmpeg, ffprobe and yt-dlp resolve from, their versions and the \
                      cache folder. Order: AUTO_ASCII_FFMPEG / AUTO_ASCII_FFPROBE / AUTO_ASCII_YTDLP, then PATH, \
                      then the cache (AUTO_ASCII_CACHE_DIR overrides its location). \
                      AUTO_ASCII_NO_DOWNLOAD=1 turns downloads off; AUTO_ASCII_YES=1 is --yes."
    )]
    Doctor {
        #[arg(
            long,
            help = "Download whatever is missing (and refresh a stale cached yt-dlp) now; asks \
                    first unless --yes"
        )]
        fetch: bool,
    },
    #[command(
        about = "Advanced inspection, evaluation, and benchmarks",
        after_help = "Tools for developing auto-ascii itself."
    )]
    Dev {
        #[command(subcommand)]
        cmd: DevCmd,
    },
}

#[derive(Subcommand)]
pub enum ComposeCmd {
    #[command(about = "Start `compositions/<name>.toml` (fails if it is already there)")]
    New {
        #[arg(help = "Composition name (kebab-cased), which is also the file stem")]
        name: String,
    },
    #[command(about = "Append one `[[clip]]` table, leaving the rest of the file alone")]
    Add {
        #[arg(help = "The composition to append to")]
        name: String,
        #[arg(help = "A path if one exists, else `library/<clip>.ascii`")]
        clip: String,
        #[arg(long = "in", help = "Trim start inside the asset (default: its start)")]
        in_spec: Option<String>,
        #[arg(long = "out", help = "Trim end inside the asset (default: its end)")]
        out_spec: Option<String>,
        #[arg(long = "at", help = "Timeline position (default: the end of the previous clip)")]
        at_spec: Option<String>,
    },
    #[command(
        about = "The resolved timeline: each clip's place on it, plus the gaps and the overlaps"
    )]
    Show {
        #[arg(help = "The composition to describe")]
        name: String,
    },
    #[command(about = "Play a composition", after_help = PLAY_AFTER_HELP)]
    Play {
        #[arg(help = "The composition to play: a `.toml` path, else `compositions/<name>.toml`")]
        name: String,
        #[command(flatten)]
        opts: PlayOptions,
    },
    #[command(about = "Flatten a composition into one `.ascii` file")]
    Export {
        #[arg(help = "The composition to flatten")]
        name: String,
        #[arg(
            short = 'o',
            long = "out",
            help = "Where to write it (default: `exports/<name>.ascii`)"
        )]
        out: Option<PathBuf>,
        #[arg(long, help = "Replace an existing file at that path")]
        force: bool,
    },
}

#[derive(Args)]
pub struct PlayArgs {
    #[arg(
        help = "A path if one exists (a `.toml` is a composition), else `library/<target>.ascii`, \
                else `compositions/<target>.toml`"
    )]
    pub target: String,
    #[command(flatten)]
    pub opts: PlayOptions,
}

#[derive(Args)]
#[command(group = ArgGroup::new("headless").args(["sim", "bench_seek"]))]
pub struct PlayOptions {
    #[command(flatten)]
    pub playback: PlaybackArgs,
    #[command(flatten)]
    pub terminal: TerminalArgs,
    #[command(flatten)]
    pub sim: SimArgs,
    #[command(flatten)]
    pub bench: BenchArgs,
}

#[derive(Args)]
pub struct PlaybackArgs {
    #[arg(long = "loop", help = "Loop playback instead of exiting at the last frame")]
    pub loop_playback: bool,
    #[arg(
        long,
        value_name = "TIMESTAMP",
        help = "Start at TIMESTAMP: plain seconds (\"42.5\") or colon form (\"1:30\", \
                \"0:01:30.5\"). Keys 0-9 also jump to 0-90% while playing"
    )]
    pub seek: Option<String>,
    #[arg(long, value_name = "NAME", value_parser = parse_style, help = style_help())]
    pub style: Option<Style>,
    #[arg(
        long,
        value_enum,
        default_value_t = PaletteArg::Auto,
        help = "Charset tier: auto (from the terminal probe), ascii, unicode (blocks/box-drawing) \
                or braille (verified fonts only)"
    )]
    pub palette: PaletteArg,
    #[arg(
        long,
        conflicts_with = "no_audio",
        help = "Start with sound off: the soundtrack still loads and plays silently in sync, and \
                `m` turns it on"
    )]
    pub mute: bool,
    #[arg(
        long,
        help = "Never look for, decode or play a soundtrack, and never open an audio device; the \
                picture runs on the wall clock. Without it the player plays the asset's sidecar \
                (<stem>.m4a, <stem>.mp4, other common containers, then the folder's source.mp4) \
                when its length matches"
    )]
    pub no_audio: bool,
}

#[derive(Args)]
pub struct TerminalArgs {
    #[arg(
        long,
        hide = true,
        value_name = "TIER",
        help = "Force the color tier (truecolor|256|16|mono): skips the probe volley entirely \
                (passive env hints still fill the glyph repertoire)"
    )]
    pub tier: Option<ColorTier>,
    #[arg(
        long,
        hide = true,
        help = "Never write the probe volley; passive env hints only (for hostile PTYs)"
    )]
    pub no_query: bool,
    #[arg(long, hide = true, help = "Bypass the probe cache (no read, no write)")]
    pub no_cache: bool,
    #[arg(
        long,
        hide = true,
        help = "Skip the identity-keyed quirk table: take the probe replies at face value instead \
                of applying the known per-terminal corrections (keyed on the XTVERSION reply, \
                never on TERM). Implies the probe cache is not written"
    )]
    pub no_quirks: bool,
    #[arg(
        long,
        hide = true,
        help = "Keep the terminal's own default background instead of setting it to black for \
                the session (OSC 11, reset with OSC 111 on exit)"
    )]
    pub no_backdrop: bool,
    #[arg(
        long,
        hide = true,
        value_name = "NAME|PATH",
        help = "Assert the terminal's font by ink-coverage table: a built-in name (conservative, \
                dejavu-sans-mono, liberation-mono, ubuntu-mono, noto-sans-mono) or a path to an \
                `auto-ascii dev font-table` TOML. Its repertoire vetoes the palette tier (braille \
                -> unicode -> ascii) so missing glyphs degrade instead of drawing boxes"
    )]
    pub font_table: Option<String>,
    #[arg(
        long,
        hide = true,
        value_enum,
        default_value_t = RepaintArg::Full,
        help = "Repaint mode: full rewrites every cell each frame; diff rewrites only damaged \
                cells and invalidates on resize only"
    )]
    pub repaint: RepaintArg,
    #[arg(
        long,
        hide = true,
        value_name = "FPS",
        help = "Cap the presentation rate below the asset fps (frames are still picked by wall \
                clock, so capping skips frames; it never slows the video down). Minimum 1 fps"
    )]
    pub fps_cap: Option<f64>,
    #[arg(
        long,
        hide = true,
        value_name = "RATIO",
        help = "Cell aspect override `cell_h_px / cell_w_px`. Default: derived from the \
                terminal's reported cell pixel size, else 2.0"
    )]
    pub cell_aspect: Option<f64>,
    #[arg(
        long,
        hide = true,
        value_name = "SECS",
        help = "Stop after SECS seconds (interactive benching); default: play to the end"
    )]
    pub duration_secs: Option<f64>,
}

#[derive(Args)]
pub struct SimArgs {
    #[arg(
        long,
        hide = true,
        value_name = "COLSxROWS:NFRAMES",
        value_parser = parse_sim_spec,
        help = "Headless: render NFRAMES frames to a simulated COLSxROWS terminal as fast as \
                possible, never touch the tty, print one JSON stats line"
    )]
    pub sim: Option<SimSpec>,
    #[arg(
        long,
        hide = true,
        value_name = "TIER",
        requires = "sim",
        help = "With --sim: color tier of the simulated terminal (falls back to --tier; default \
                truecolor)"
    )]
    pub sim_tier: Option<ColorTier>,
    #[arg(
        long,
        hide = true,
        value_name = "PATH",
        requires = "sim",
        help = "With --sim: write every presented frame's raw escape bytes to PATH"
    )]
    pub sim_dump: Option<PathBuf>,
    #[arg(
        long,
        hide = true,
        value_name = "COLSxROWS",
        requires = "sim",
        num_args = 0..=1,
        default_missing_value = "100x40",
        value_parser = parse_size,
        help = "With --sim: inject a resize to COLSxROWS (default 100x40) halfway through; \
                reported as `grid_after` in the JSON"
    )]
    pub sim_resize: Option<(u16, u16)>,
    #[arg(
        long,
        hide = true,
        requires = "sim",
        help = "With --sim: play the soundtrack into a null sink paced in real time, pick every \
                frame from the audio clock, and add an `audio` object to the JSON line; --mute \
                and --no-audio apply"
    )]
    pub sim_audio: bool,
}

#[derive(Args)]
pub struct BenchArgs {
    #[arg(
        long,
        hide = true,
        value_name = "N",
        value_parser = clap::value_parser!(u32).range(1..),
        help = "Headless scrub-latency benchmark: N deterministic random seeks through the \
                interactive scrub path on a 300x80 simulated terminal; prints one JSON line with \
                p50/p95/max latency in ms"
    )]
    pub bench_seek: Option<u32>,
}

#[derive(Subcommand)]
pub enum DevCmd {
    #[command(about = "Print header, chunks, sizes, per-plane value stats; verify CRCs")]
    Inspect {
        #[arg(help = "Asset to inspect")]
        asset: PathBuf,
        #[arg(
            long,
            value_name = "DIR",
            help = "Dump decoded planes of the sampled frames into DIR as PGM/PPM images (Y/E \
                    as gray, Ex/Ey as bias-128 gray, H as a flag map, C as color)"
        )]
        dump_planes: Option<PathBuf>,
        #[arg(
            long,
            value_name = "N",
            help = "Frame index for --dump-planes and the stats sampler (repeatable; default: 4 \
                    frames spread over the asset)"
        )]
        frame: Vec<u32>,
    },
    #[command(
        about = "Print the effective tunables: embedded defaults merged with --params, as TOML"
    )]
    Params {
        #[arg(long, value_name = "PATH", help = "Tunables file to merge over the embedded defaults")]
        params: Option<PathBuf>,
        #[arg(long, required = true, help = "Print the effective config to stdout")]
        dump: bool,
    },
    #[command(
        about = "Build every corpus video (cached by input+params sha), render it headlessly and \
                 write metrics JSON; with --baseline, exit nonzero on a tolerance breach"
    )]
    Eval {
        #[arg(
            long,
            value_name = "DIR",
            help = "Directory of corpus videos (non-recursive; mp4/mov/mkv/webm/avi)"
        )]
        corpus: PathBuf,
        #[arg(long, value_name = "PATH", help = "Tunables file (see `import --params`)")]
        params: Option<PathBuf>,
        #[arg(long, value_name = "PATH", help = "Baseline metrics JSON to compare against")]
        baseline: Option<PathBuf>,
        #[arg(long, value_name = "PATH", help = "Output metrics JSON path (e.g. runs/X.json)")]
        out: PathBuf,
        #[arg(long, value_name = "PATH", help = "Self-contained HTML contact sheet path")]
        html: Option<PathBuf>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Review-reel HTML path: per clip, source|render rows with per-frame metrics \
                    plus an animated GIF of the rasterized render"
        )]
        reel: Option<PathBuf>,
        #[arg(
            long,
            value_name = "DIR",
            default_value = "runs/cache",
            help = "Asset cache directory, keyed by (input sha, params sha)"
        )]
        cache_dir: PathBuf,
        #[arg(
            long,
            value_name = "NAME|PATH",
            help = "Ink-coverage table for the SSIM rasterizer: a built-in name or an `auto-ascii \
                    dev font-table` TOML. Default: conservative (absolute SSIM is only comparable \
                    within one table)"
        )]
        font_table: Option<String>,
    },
    #[command(
        about = "Run eval per combo of the axes in --grid, score and rank the combos, and write \
                 results JSON plus a leaderboard HTML"
    )]
    Sweep {
        #[arg(long, value_name = "DIR", help = "Directory of corpus videos (see `dev eval`)")]
        corpus: PathBuf,
        #[arg(long, value_name = "PATH", help = "Base tunables file every combo starts from")]
        params: Option<PathBuf>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Sweep spec: axes of param overrides + optional [score] weights"
        )]
        grid: PathBuf,
        #[arg(
            long,
            value_name = "DIR",
            help = "Output directory: combo-NN.json + sweep.json + leaderboard.html"
        )]
        out: PathBuf,
        #[arg(
            long,
            value_name = "DIR",
            default_value = "runs/cache",
            help = "Asset cache directory shared with `dev eval`"
        )]
        cache_dir: PathBuf,
    },
    #[command(
        about = "Rasterize every glyph the shipped palettes emit through a monospace font and \
                 write a deterministic ink-coverage table (TOML) for --font-table"
    )]
    FontTable {
        #[arg(help = "Monospace font file (.ttf/.otf). Omit with --conservative")]
        font: Option<PathBuf>,
        #[arg(short, long, value_name = "PATH", help = "Output table path")]
        output: PathBuf,
        #[arg(long, help = "Table name recorded in the file (default: the font file stem)")]
        name: Option<String>,
        #[arg(
            long,
            help = "Emit the built-in conservative (ASCII-repertoire) table instead of \
                    rasterizing a font"
        )]
        conservative: bool,
    },
    #[command(
        about = "Headless playback against a simulated terminal; prints one JSON stats line",
        after_help = "Same as `auto-ascii play <target> --sim ...`."
    )]
    Sim(DevSimArgs),
    #[command(
        about = "Seek-latency benchmark through the interactive scrub path; prints one JSON line",
        after_help = "Same as `auto-ascii play <target> --bench-seek N`."
    )]
    BenchSeek(DevBenchArgs),
}

#[derive(Args)]
#[command(mut_args(|a| a.hide(false)), mut_arg("sim", |a| a.required(true)))]
pub struct DevSimArgs {
    #[arg(help = "A path if one exists, else a library clip or composition name")]
    pub target: String,
    #[command(flatten)]
    pub playback: PlaybackArgs,
    #[command(flatten)]
    pub terminal: TerminalArgs,
    #[command(flatten)]
    pub sim: SimArgs,
}

#[derive(Args)]
#[command(mut_args(|a| a.hide(false)), mut_arg("bench_seek", |a| a.required(true)))]
pub struct DevBenchArgs {
    #[arg(help = "A path if one exists, else a library clip or composition name")]
    pub target: String,
    #[command(flatten)]
    pub playback: PlaybackArgs,
    #[command(flatten)]
    pub terminal: TerminalArgs,
    #[command(flatten)]
    pub bench: BenchArgs,
}

#[derive(Args)]
pub struct StreamArgs {
    #[arg(
        required = true,
        num_args = 1..,
        value_name = "URL|TERMS",
        help = "A video, playlist, channel or search-results URL, or plain search terms (the \
                first result plays)"
    )]
    pub input: Vec<String>,
    #[arg(
        long,
        value_name = "NAME",
        value_parser = parse_style,
        default_value = "ascii",
        help = style_help()
    )]
    pub style: Style,
    #[arg(
        long,
        value_enum,
        default_value_t = PaletteArg::Auto,
        help = "Charset tier: auto (from the terminal probe), ascii, unicode or braille"
    )]
    pub palette: PaletteArg,
    #[arg(
        long,
        value_name = "PX",
        default_value_t = 480,
        value_parser = clap::value_parser!(u32).range(144..=4320),
        help = "Tallest video format to stream"
    )]
    pub max_height: u32,
    #[arg(long, help = "Play the picture only, on a wall clock")]
    pub no_audio: bool,
    #[arg(
        long,
        value_name = "BROWSER",
        help = "Opt-in: let yt-dlp read cookies from your BROWSER's profile; only cookies \
                matching each request are sent. Off by default; yt-dlp config files are never read"
    )]
    pub cookies_from_browser: Option<String>,
    #[arg(
        long,
        hide = true,
        value_name = "COLSxROWS:SECONDS",
        help = "Headless run against a simulated terminal for at most SECONDS (not frames); \
                audio is decoded into a real-time null sink; prints one JSON stats line"
    )]
    pub sim: Option<String>,
    #[arg(
        long,
        hide = true,
        value_name = "PATH",
        requires = "sim",
        help = "With --sim: write one loader frame and one picture frame as text to PATH"
    )]
    pub sim_dump: Option<PathBuf>,
}

#[derive(Args)]
pub struct AddArgs {
    #[arg(value_name = "URL|FILE", help = "A video link, or a local video file (mp4, mov, mkv, ...)")]
    pub input: String,
    #[arg(
        long,
        help = "Title for the clip file; its kebab-case names the folder. Default: the video's \
                title, or the file name"
    )]
    pub title: Option<String>,
    #[arg(
        long,
        value_name = "DIR",
        help = "Folder to put the clip's folder in (default: the library, ~/auto-ascii/library)"
    )]
    pub library: Option<PathBuf>,
    #[arg(long, help = "Replace an existing clip folder of the same name")]
    pub force: bool,
}

#[derive(Args)]
pub struct ImportArgs {
    #[arg(help = "Source video (any ffmpeg-readable container)")]
    pub video: PathBuf,
    #[arg(
        long,
        conflicts_with = "output",
        help = "Library name (kebab-cased). Default: the file stem, kebab-cased"
    )]
    pub name: Option<String>,
    #[arg(
        short = 'o',
        long,
        value_name = "PATH",
        help = "Write the asset to PATH instead of the library (no sidecar, no library folders)"
    )]
    pub output: Option<PathBuf>,
    #[command(flatten)]
    pub build: BuildOptions,
    #[arg(long, help = "Replace an existing clip or file at the destination")]
    pub force: bool,
}

#[derive(Args, Default)]
pub struct BuildOptions {
    #[arg(
        long,
        value_name = "TIME",
        help = "Start offset into the source: SS[.f], MM:SS[.f] or HH:MM:SS[.f]"
    )]
    pub ss: Option<String>,
    #[arg(long = "t", value_name = "TIME", help = "Duration to take from the source, same formats as --ss")]
    pub t: Option<String>,
    #[arg(
        long,
        value_parser = clap::value_parser!(u16).range(1..=1000),
        help = "Output frame rate (default: params.toml `build.fps`, 30)"
    )]
    pub fps: Option<u16>,
    #[arg(
        long,
        value_name = "WxH",
        help = "Stored plane resolution, even dimensions (default: params.toml, 480x270)"
    )]
    pub res: Option<String>,
    #[arg(
        long,
        value_name = "PATH",
        help = "Tunables file merged over the embedded params.toml (`auto-ascii dev params --dump` \
                prints them)"
    )]
    pub params: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum PaletteArg {
    Auto,
    Ascii,
    Unicode,
    Braille,
}

impl From<PaletteArg> for auto_ascii::PaletteChoice {
    fn from(p: PaletteArg) -> auto_ascii::PaletteChoice {
        match p {
            PaletteArg::Auto => auto_ascii::PaletteChoice::Auto,
            PaletteArg::Ascii => auto_ascii::PaletteChoice::Ascii,
            PaletteArg::Unicode => auto_ascii::PaletteChoice::Unicode,
            PaletteArg::Braille => auto_ascii::PaletteChoice::Braille,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum RepaintArg {
    Full,
    Diff,
}

impl From<RepaintArg> for auto_ascii::RepaintMode {
    fn from(r: RepaintArg) -> auto_ascii::RepaintMode {
        match r {
            RepaintArg::Full => auto_ascii::RepaintMode::Full,
            RepaintArg::Diff => auto_ascii::RepaintMode::Diff,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SimSpec {
    pub cols: u16,
    pub rows: u16,
    pub frames: u64,
}

pub fn parse_style(name: &str) -> Result<Style, String> {
    Style::from_name(name)
        .ok_or_else(|| format!("unknown style {name:?} (known: {})", Style::names(", ")))
}

fn style_help() -> String {
    format!(
        "Glyph style: {} (default {}); `/` cycles it while playing, and a chosen style \
         overrides the one saved for the video",
        Style::names(", "),
        Style::default().name()
    )
}

pub fn parse_size(s: &str) -> Result<(u16, u16), String> {
    let (c, r) = s.split_once(['x', 'X']).ok_or("expected COLSxROWS")?;
    let cols: u16 = c.trim().parse().map_err(|_| format!("bad COLS {c:?}"))?;
    let rows: u16 = r.trim().parse().map_err(|_| format!("bad ROWS {r:?}"))?;
    if cols == 0 || rows == 0 {
        return Err("size must be at least 1x1".into());
    }
    Ok((cols, rows))
}

pub fn parse_sim_spec(s: &str) -> Result<SimSpec, String> {
    let (size, n) = s
        .split_once(':')
        .ok_or("expected COLSxROWS:NFRAMES (e.g. 213x58:900)")?;
    let (cols, rows) = parse_size(size)?;
    let frames: u64 = n.trim().parse().map_err(|_| format!("bad NFRAMES {n:?}"))?;
    if frames == 0 {
        return Err("NFRAMES must be >= 1".into());
    }
    Ok(SimSpec { cols, rows, frames })
}
