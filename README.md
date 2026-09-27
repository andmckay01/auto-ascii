# auto-ascii

Realtime ASCII-art video for terminals, and a Rust library you can drop into
your own renderer.

An offline **factory** distills a video into a resolution-independent feature
asset (`.ascii`). The asset holds luma, edge magnitude and orientation,
highlights and chroma, but never glyphs. A runtime **player** maps that asset
onto whatever cell grid you have right now. It picks glyph ramps, directional
edge strokes, highlights and half-blocks, letterboxes to the video's aspect,
reflows live on resize, and applies temporal hysteresis so nothing flickers.
Because every glyph is chosen at render time, one asset looks right at 80×24
in a Linux console, at 320×90 in a GPU terminal, and inside your own renderer.

```text
|BBBBBMMMMMMMMMMMMMMBBBBBB8888DDDDGGSSSSSSSSSGGGGDDDDDDDDGGGSSeeeon
|888888BBBBBMMMMBBMMMBBBBBB8888DDDGGGSSSSSSSSSSSGGDDDDDDDDGGGSSeeen
|DGDDGDDDD88BBBBBBBBBBBBBBBBB88DDGGGGSSSSeeeeeSSSGGGGDDDGGGSSeeooon
|GSSGSGDDDD8BBBBBBBBMMMMMMMBBBB88GPFTFPoSeeeeeeeeSSGGGGGGGGSeeoooon
|GoeeeeSGGDBBBBBBBBBMMMMMMMMMMMMDo    .;oDeoooooeeeSGGGDDGGSeonnnnx
|SooonoeooeG88DGMMPPPGBMMMMMMMMMDx     :cPooooooooeSGGGDGGSSonxxxxx
|enonYTxeeeGDeT     :x8MMBMMMMMMBe,    .+uoxc".:xSeeSGSF" ;eonxxxnn
|ennc  ;FeSSex      :+eeS88MMBMMMDn     ;nex;  .;oSeeeo:  .+xxT7TxF
|Y.+c: .+onooc.     .;xGSG: .YD/"""    ,gee+   .:xoSSSSo,  ;o+   ;;
|. ;:  ,*:;ccc:      .;nee;  :F;,,.,uaa+eeT.    :+x;Tnc: .:+;.   ..
 .:;c;:Txccx+":.     .,..7:.:++xeoc+PMGxcSa+au;+a+;.. .+xon;""++++:
      ::cxc:;     .a+*;;  ..   .Fooc;.:ccFeen+++GSc.   .+xxc  :c+xc
       .:xc:c:. :+cxoexnnwc:     .++:    :cn;: +oe++....:xc+  :;oo;
       ..+;:cxx+;;xccnccnSn+   ...++.:.+anc++. +eGn;+c; .+:. :+:nec
   .    ..  :;ncx+;+;;+oeno+:   :+u|.++xen:;+;.+eDeXX:  .;:  .c;++;
    .   .;c:.  . : '  ;nGn+:;    .;cu;;*xc.;cc.7FGDSo;   ::  .++;;+
          .+:  .  .   :+nc..+    ;xeex...:.:cnwa_-X;T+u ..;:.  ..:;
          .      .:aaxcw+:.:c.    .+oea:.  :+xncoeSeX++a.   '.=.+..
         .;+,     :FncSGx:.:x;     :nSSa.  .::;xc+cc:..""":.    ..
         .+c"      .++SDn:.:x:      .7nSw,  ..:;;   ....    .   ..
         .:;      .:x+xSo;.;x:        .;cY+u.. .:::,,   .      .:
          .;.     .;eSeSSou;o+        . .,:;*.,    .;c+
    .:;;+:        .:eSenSGeSSoc.         ."7+xx;     .;.
     .:::.         "PPnnnooowxxc,             .".
       .:               "7FPooeeSoa,
```

<sub>One frame of Apple's "1984" ad on a 90×26 grid, `letters` codec, colors
dropped (`headless-dump` example). In a terminal every cell also carries the
source's color.</sub>

The Rust workspace, supported by Python tools, has a library (`auto-ascii`),
two engine binaries (`auto-ascii-factory` and `auto-ascii-player`), and the `auto-ascii`
CLI, which files clips in a library folder and stitches them into
compositions. The factory runs ffmpeg as a subprocess; the player links no
video codecs (it runs ffmpeg too, only to decode an asset's soundtrack).

## Install

You need a recent stable Rust toolchain (edition 2024). The factory also needs
`ffmpeg` on `PATH` (`brew install ffmpeg`, `apt install ffmpeg`); the player
does not. `auto-ascii stream` needs both `yt-dlp` and `ffmpeg` on `PATH`
(`brew install yt-dlp ffmpeg`).

```bash
git clone https://github.com/andmckay01/auto-ascii && cd auto-ascii
cargo install --path crates/auto-ascii           # auto-ascii-player
cargo install --path crates/auto-ascii-factory   # auto-ascii-factory
cargo install --path crates/auto-ascii-cli       # auto-ascii (the CLI)
```

macOS (Apple Silicon or Intel) and Linux build from source. On Linux,
`scripts/release.sh` builds stripped native, fully static musl and Windows
cross player binaries into `dist/`, each under 5 MB. The static musl binary
runs on any x86-64 Linux with nothing else installed.

To use the library, add it as a dependency:

```toml
auto-ascii = "0.2"
```

## Quick start

```bash
# a 6-second test clip (or use any video you have)
ffmpeg -f lavfi -i testsrc2=size=640x360:rate=30 -t 6 clip.mp4

# distill it into an asset (offline; seconds for short clips)
auto-ascii-factory build clip.mp4 -o clip.ascii

# play it (q quits)
auto-ascii-player clip.ascii

# no terminal handy? render headlessly
auto-ascii-player clip.ascii --sim 213x58:300        # one JSON stats line
cargo run --release -p auto-ascii --example headless-dump -- clip.ascii 3 100x28
```

From a checkout without installing, use `cargo run --release -p
auto-ascii-factory -- …` and `cargo run --release -p auto-ascii --bin
auto-ascii-player -- …`. `auto-ascii-factory inspect clip.ascii` prints the
container's header and chunks and verifies every CRC.

## Playing

| key | does |
|---|---|
| `q` / `Esc` / Ctrl-C | quit (the terminal is always restored, even on panic) |
| space | pause / resume |
| `0`–`9` | jump to 0%–90% |
| `←` / `→` | seek back / forward 5 s |
| `d` | show the dial readout, then cycle **shadow lift → edge strength → hysteresis** |
| `[` / `]` | turn the selected dial down / up |
| `/` | cycle the glyph codec (`pixels` → `letters` → `ascii`) |
| `s` | save this video's dials and codec |
| `m` | turn the sound on / off |
| `v` | pin or hide the controls overlay |

**Glyph codecs** decide how a cell becomes a glyph. `pixels` (the default)
paints a low-resolution picture from shade ramps, half-blocks and quadrants.
`letters` draws with type: printable characters ordered by ink, ASCII strokes
on edges, and blocks only where the picture is lit (`█` for near-white, `▀▄`
for a bright half). On truecolor and 256-color terminals each character sits
on a dim tint of its cell's colour, so faces and midtones hold their shape;
16-color and mono terminals keep a black background.
`ascii` is letters with only printable ASCII and no blocks in the picture
(the controls overlay is the same as in the other codecs). Behind each
character it paints a dim shade of the character's own colour, the way
letters does, with its exact current-tone tint curve and colour preserved.
“Not a full pixel” means printable ASCII ink, unshaded spaces, background
channels at most 154/255, and background linear luminance at most 0.375 times
the foreground as sent. The character stays clearly brighter than its shade.
Truecolor retains every nonblack shade, including dark colours below 8/255.
On 256-colour terminals, safe gray and 0/95/135 cube entries must pass the
same contrast cap and stay within the glyph's hue family. 16-colour and mono
terminals get no shade (the terminal's own background shows through). Glyph selection is the
same on every terminal tier; colours follow the terminal's capabilities.
ASCII colour and shade follow current brightness independently of glyph
hysteresis, so playback cannot retain old brightness bands. Big changes
switch the glyph at once (sooner in busy, fast-changing areas); otherwise it
follows a smoothed tone. A steady change of more than 16 tone units settles
within 41 frames; a smaller one within 74, unless the tone sits within 4
units of the boundary to the neighbouring glyph, which may then stay.
Black-floor crossings take at most four frames.

**Black backdrop.** While it plays, the player sets your terminal's default
background to black (OSC 11). `ascii` needs it: its unshaded cells (shadows,
blank cells, and everything on 16-colour terminals) use the terminal's
background, and on a grey or light theme they would sit on that colour, next
to shades designed for black. `pixels` and `letters`
paint every cell themselves and look the same either way. On exit it sends
OSC 111, which resets the background to the one in your terminal's config or
profile (not to a colour something else set at runtime). That reset runs on a
normal quit, on an error, on a Rust panic, on SIGINT (Ctrl-C), SIGTERM and
SIGHUP, and at process exit. Nothing can run when the player is killed with
SIGKILL or dies from an abort or a segfault; if a tab is left black, run
`printf '\e]111\e\\'` in it or open a new one. `--no-backdrop` keeps your
terminal's background; the mono tier (`--tier mono`) never sets it, since it
draws in your terminal's own foreground colour. `auto-ascii play` always uses
the backdrop. Terminals that don't understand OSC 11 ignore it.

**Dials** retune the renderer while the video plays. Shadow lift opens dark
scenes. Edge strength sets how many contours get strokes. Hysteresis trades
flicker against responsiveness: 0..255 in steps of 16, recommended default
128 (the default was 160 before, so pixels and letters hold glyphs a little
less at default, and are unchanged at any equal value). Values above 128
are allowed for fast-paced videos or video types that benefit from high
hysteresis, but can visibly drift or smear. Every codec keeps it, since switching it off
raises glyph changes markedly; see
[hysteresis measurements](docs/HYSTERESIS-DECISION.md). The readout says
when a dial is at its floor, its default or its top. Nothing is rebuilt: the same asset re-renders
at the new setting. `s` saves the dials and codec beside the asset as
`<name>.player.toml`, and they load the next time that video plays.

**The controls overlay** (`v`, and briefly at start-up) lists the keys. Above
them is the clip name, the active codec, whether the settings are saved, the
sound (`on`, `off`, `wait` while the audio output is stalled or being
re-opened, or `none` when there is nothing to play) and the grid
size (`213x58 cells`), e.g.
` Interstellar   codec: ascii   settings: saved   sound: on `.

**Sound.** The player plays the asset's soundtrack itself. It looks beside the
asset for `<name>.m4a`, then `<name>.mp4`, then `<name>` with `.aac`, `.mp3`,
`.wav`, `.flac`, `.ogg`, `.opus`, `.mov`, `.m4v`, `.mkv` or `.webm`, and last
the folder's `source.mp4`, so `1.ascii` plays `1.mp4` even when `source.mp4`
beside it is the 80-minute mix it was cut from. A track whose length is more
than 3 s and more than 10% off the asset's is a different cut: it is not
played, and the player says so on stderr after it exits. ffmpeg (on `PATH` or
in `/opt/homebrew/bin`) decodes the track into memory in the background, so
the picture starts at once and the sound comes in as it decodes. The picture
then follows the sound: its clock is the audio the output device has
actually played, minus the device's latency. Jumps, arrow scrubs, `--seek`,
pause and `--loop` move the sound with the picture (a loop wraps the sound at
the video's length, so nothing drifts however long it loops); `/` and the
dials don't touch time. `m` mutes and unmutes instantly: a muted track keeps
playing silently, so it stays in sync. `--mute` starts muted; `--no-audio`
never looks for a track or opens a device. With no track, no ffmpeg, no audio
device, a track that won't decode or one over the 512 MiB memory budget
(the track is held at the output's rate: about 46 minutes of 48 kHz stereo,
23 at 96 kHz, 11 at 192 kHz), it plays silently on the wall clock as
before (`sound: none`) and notes why on stderr after exit. If the track is
shorter than the video, the picture keeps its pace in silence after it ends,
and seeking back plays it again. Outages are never permanent: if the output
stops calling back for 1.5 s (a Bluetooth device slow to start, a stuck
driver) the picture carries on silently on the wall clock (`sound: wait`), and
the moment the output calls back the sound is moved to where the picture is
and leads again. A pause of the whole process (Mac sleep, Ctrl-Z) is not a
stall. If the device goes away (unplugged, invalidated across sleep, its audio
host gone), the player drops the dead stream and re-opens the default output
every 2 s for the rest of the session; if the new output runs at a different
sample rate or channel count than the track was decoded for, it stays silent
with a note and keeps retrying (switching back brings the sound back). Each
outage is noted once on stderr after exit. `m` during an outage still flips
mute, and the recovered sound honours it. A switch of the system's default
output or an audio glitch does not interrupt the sound at all; only a decode
failure turns it off for good (`sound: none`). `--duration-secs` always counts
wall time. Limitations: compositions play silently (`sound: none`); the
`.ascii` format has no embedded audio plane yet, so the sound always comes
from a file beside the asset (a library clip only has sound if you put one
beside it); and `--sim` stays silent unless `--sim-audio` is given, which
plays into a null sink and never opens a device.

`scripts/play-with-sound.command PLAYER ASSET [PLAYER_ARGS...]` no longer
starts `afplay`: it is a launcher that restarts the player at each clip end,
stops on quit, and logs every exit to
`~/Library/Logs/auto-ascii/play-with-sound.log` (use `--loop` instead if you
don't need the log). An old third `AUDIO` argument is ignored.

**Zoom out for detail.** The asset is resolution-independent, so a smaller
terminal font means more cells and a sharper picture. Use your terminal's
zoom-out shortcut (often Cmd - on macOS; bindings vary by terminal). The
player can't change the font itself
([docs/research/zoom.md](docs/research/zoom.md)), so below 160 columns the
overlay says so. At 240 or more columns and 36 or more rows, on a non-ASCII
tier, the overlay text is drawn in big block letters so it stays readable.
Every codec draws the same overlay, `ascii` included.

**The `ascii` rule.** Picture cells are printable ASCII 0x20-0x7E, background
default or a shade within the cap; block glyphs and full-strength backgrounds
are allowed only in UI overlay cells (HUD text), which use the same big text as
pixels/letters.

Useful flags:
- `--loop`, `--seek 1:30`, `--fps-cap 30`
- `--codec pixels|letters|ascii`
- `--mute` (start with the sound off), `--no-audio` (no sound at all)
- `--palette ascii|unicode|braille`, `--tier truecolor|256|16|mono`
- `--no-query` (skip capability queries)
- `--no-backdrop` (keep the terminal's own background)
- `--font-table NAME|PATH` (tell the player which font your terminal uses)

`auto-ascii-player --help` lists everything.

## The `auto-ascii` CLI

One command takes a video from anywhere and files it in
`~/auto-ascii/library/` (override with `AUTO_ASCII_HOME`). A JSON sidecar
beside it records where the video came from.

```bash
auto-ascii import ~/Desktop/clip.mp4        # ffmpeg ingest -> library/clip.ascii
auto-ascii list                             # name, duration, fps, frames, bytes, source
auto-ascii info clip                        # one clip's header + sidecar
auto-ascii play clip                        # the player, on a library clip

auto-ascii cut clip --in 0:01 --out 0:04    # -> library/clip-0m01s-0m04s.ascii
auto-ascii compose new demo                 # -> compositions/demo.toml
auto-ascii compose add demo clip --at 0:10  # append a clip at 0:10 (black before it)
auto-ascii compose show demo                # the resolved timeline, gaps and overlaps
auto-ascii compose play demo                # play it without re-encoding anything
auto-ascii compose export demo              # flatten to exports/demo.ascii

auto-ascii stream "https://youtu.be/jNQXAC9IVRw"  # stream a YouTube video, with sound
```

`import` also takes `--name`, `--ss`/`--t` (times as `SS`, `MM:SS` or
`HH:MM:SS`), `--fps`, `--res WxH` and `--force`. `home` prints the folder.

A **composition** is a TOML file that stitches any number of clips on one
timeline. Each clip is placed with `at` and trimmed with `in`/`out`; gaps are
black and a later clip draws on top. The file is the source of truth, so you
can write it by hand; `auto-ascii-player demo.toml` plays one directly.

Every command except interactive `play`, `compose play` and `stream` accepts
`--json`, which makes stdout exactly one JSON value (`stream` takes it only
with `--sim`).
[docs/AGENT-GUIDE.md](docs/AGENT-GUIDE.md) (also printed by `auto-ascii
agent-guide`) documents the folder layout, the JSON shapes and the composition
schema.

## Streaming from YouTube

`auto-ascii stream` plays the first video behind a link as live ASCII with
sound. It downloads nothing:

```bash
auto-ascii stream "https://www.youtube.com/watch?v=jNQXAC9IVRw"   # a video (a &list= is ignored)
auto-ascii stream "https://www.youtube.com/playlist?list=PL…"     # its first video
auto-ascii stream "https://www.youtube.com/@jawed"                # a channel's newest video
auto-ascii stream me at the zoo                                    # the first search result
auto-ascii stream <link> --codec letters --max-height 720 --no-audio
auto-ascii stream <link> --sim 120x40:25 [--sim-dump frames.txt]  # headless, one JSON line
```

A centred loader fills from 0 to 100% while it resolves and buffers. The bar
brightens from left to right, with a soft band sweeping across it, and says
`loading...` underneath. The percentage tracks real stages: yt-dlp running,
first video found, stream URLs ready, ffmpeg started, first audio and first
video bytes, then the buffer filling. Playback starts at 100%. `q`, `Esc` or
`Ctrl-C` quits at any point, even while loading, and `/` cycles the glyph codec.
The default codec is `ascii`, and `--palette` works as it does for the player.

How it works:
- **Resolve.** yt-dlp resolves the input. A playlist, channel or search is
  walked down to its first video (`--flat-playlist -I 1`, at most three
  levels), then `-J` reads that video's stream URLs, HTTP headers and metadata.
- **Stream.** Two ffmpeg children read those URLs directly. One decodes the
  video to raw rgb24 frames at the factory's plane size and a constant frame
  rate. The other decodes the audio to f32 PCM. Each frame goes through the
  factory's own feature extraction and then the player's own pipeline. Bounded
  queues (about 2.5 s of video, 4 s of audio) push back on the pipes, so memory
  stays flat.
- **Sync.** Sound plays through the default output device via cpal. The picture
  follows the audio clock: the samples actually played, minus the device's
  reported latency. Late frames are dropped, never slowed. If the network
  stalls, the picture freezes with the sound and the loader returns until the
  buffer refills.
- **Clean up.** Every child runs in its own process group, inside a private
  temp directory. On quit, error, end of video or signal, every child is
  killed and reaped and the directory is removed.

Limitations:
- YouTube sometimes refuses a DASH URL with HTTP 403. The stream then switches
  that track to the video's HLS formats and carries on from the same position.
  Heavy throttling still shows up as re-buffering.
- Live streams, premieres, and private, members-only, age-gated or geo-blocked
  videos are refused, with yt-dlp's reason printed as one line.
- Without a usable audio device (or with `--no-audio`) the video plays silently
  on a wall clock.
- There is no seeking or pausing in a stream, and nothing is saved to the
  library. Use `import` on a downloaded file for that.

## Embedding

```rust
auto_ascii::Player::builder().asset("intro.ascii").looping(true).build()?.run()?;
```

That one call is the whole player: capability probe, letterbox, live resize,
and terminal restore on quit, Ctrl-C or panic. If you own the event loop and
the output layer (a game engine, a GUI widget, a test), use the terminal-free
`RenderSession`:

```rust
use auto_ascii::RenderSession;

let mut session = RenderSession::open("intro.ascii")?;
let grid = session.render(0, 120, 40)?; // frame 0 on a 120x40 grid -> &Grid<Cell>
for row in 0..grid.rows() {
    for cell in grid.row(row) {
        draw(cell.glyph(), cell.fg, cell.bg); // a char and two RGB colors
    }
}
```

With `default-features = false` the dependency tree is the engine and nothing
else: no clap, no crossterm. [crates/auto-ascii](crates/auto-ascii/README.md)
lists the feature tiers. There are three runnable examples:
[`simple-play`](crates/auto-ascii/examples/simple-play.rs) (the player in one
call), [`embedded-loop`](crates/auto-ascii/examples/embedded-loop.rs)
(`RenderSession` in a hand-rolled loop with a mid-run resize) and
[`headless-dump`](crates/auto-ascii/examples/headless-dump.rs) (frames to
stdout as text). API docs: `cargo doc -p auto-ascii --open`.

## Palettes

Eight palettes are keyed by charset tier and layer role. Density (cell count)
picks ramp length within a tier, and color depth caps it: color already
carries luminance, so truecolor gets shorter ramps than mono. Choose the tier
with `--palette` or `PaletteChoice`; the rest is automatic.

| # | key | ramp / LUT |
|---|-----|------------|
| 1 | `ascii/base/coarse` (<70 cols) | `" .:-=+*#%@"` |
| 2 | `ascii/base/fine` (≥70 cols) | `" .,:;i1tfLCG08@"` |
| 3 | `ascii/edge` | orientation LUT `= - _` / `\|` / `/` `\`, junctions `+` `#` |
| 4 | `ascii/highlight` | `" .+*"` |
| 5 | `unicode/base` | `" ·░▒▓█"` + quadrants `▖▘▝▗▀▄▌▐` |
| 6 | `unicode/edge` | `‾ ─ _` / `│` / `╱` `╲`, junction `┼` |
| 7 | `unicode/detail` (fine density, verified fonts) | braille U+2800–28FF, edge/texture only |
| 8 | `mono-fallback/base` (16-color, no color, Linux console) | `" .:coO8@"` (CP437-safe) |

Everything an ASCII tier emits is printable ASCII, so the Linux console never
gets a glyph its font lacks. Terminal support is capability data, not
per-terminal code: one ANSI backend is parameterized by a probed color tier,
glyph repertoire, synchronized-output support and cell size.
[docs/TERMINAL-CHECKLIST.md](docs/TERMINAL-CHECKLIST.md) is the manual
per-terminal pass.

## Tuning and evaluation

Every tunable lives in [`params.toml`](params.toml): edge thresholds, EMA
constants, hysteresis widths, highlight percentiles. The factory embeds it as
its defaults, and `--params FILE` overrides any subset.
[docs/FEATURE-MAP.md](docs/FEATURE-MAP.md) documents every key. The tuning loop
runs against whatever reference videos you keep in `corpus/` (local and
gitignored; see [corpus/README.md](corpus/README.md)):

```bash
auto-ascii-factory eval --corpus corpus/ --params params.toml \
    --out runs/base.json --html runs/base.html          # once: record a baseline
auto-ascii-factory eval --corpus corpus/ --params params.toml \
    --baseline runs/base.json --out runs/latest.json --html runs/latest.html
```

`eval` builds each clip, cached by input, params and pipeline code. It then
renders headlessly through the real player pipeline. It writes metrics JSON
and a self-contained HTML contact sheet: SSIM, edge F1 against Canny on the
source, flicker, damage rate, bytes per frame and per-stage frame times. It
exits nonzero when a tolerance against the baseline breaks.
`auto-ascii-factory sweep` runs the same eval over a grid of parameter
overrides and ranks the combinations; the grid format is in the feature map.

## How it's built

| path | what |
|---|---|
| `crates/auto-ascii` | the public library + the `auto-ascii-player` binary |
| `crates/auto-ascii-core` | pure engine: viewport, resampler, glyph codecs, palettes, hysteresis |
| `crates/auto-ascii-format` | the ASCI container (zstd + temporal delta, fast seek) |
| `crates/auto-ascii-term` | `Backend` trait, ANSI backend, capability probe, simulator |
| `crates/auto-ascii-factory` | the offline factory (lib + bin), eval and sweep drivers |
| `crates/auto-ascii-cli` | the `auto-ascii` CLI, including `stream` (yt-dlp + ffmpeg + cpal) |
| `crates/auto-ascii-eval` | metrics, synthetic fixtures, report schema |
| `scripts/eval.sh` | the gate: tests, clippy, resize fuzz, perf gates, corpus eval |
| `tools/` | `prep_video.py` (canvas-normalize a source video), `soak.py` (resize-storm soak) |

`auto-ascii` 0.2 and `auto-ascii-core`, `-format` and `-term` 0.1 are on
crates.io; the factory, eval and CLI crates are workspace-only.

Docs:
- [docs/FEATURE-MAP.md](docs/FEATURE-MAP.md): every feature and exactly how the
  pipeline works, with code pointers.
- [CONTRIBUTING.md](CONTRIBUTING.md): build, the gate and the determinism
  rules.
- [docs/AGENT-GUIDE.md](docs/AGENT-GUIDE.md): the CLI for agents.
- [docs/PLAN.md](docs/PLAN.md) and [docs/PLAN-M6-M8.md](docs/PLAN-M6-M8.md):
  the original designs.
- [docs/INTERFACES.md](docs/INTERFACES.md): the internal API registry.
- [docs/research/](docs/research/): the research digests.

`scripts/eval.sh` is what "green" means here. It runs in a few minutes, and
its corpus section skips itself when `corpus/` holds no videos; committed tests
never depend on them.

## License

[MIT](LICENSE). Unless you explicitly state otherwise, any contribution
intentionally submitted for inclusion in this project shall be licensed the
same way, without any additional terms or conditions.
