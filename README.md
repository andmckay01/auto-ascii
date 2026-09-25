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
video codecs.

## Install

You need a recent stable Rust toolchain (edition 2024). The factory also needs
`ffmpeg` on `PATH` (`brew install ffmpeg`, `apt install ffmpeg`); the player
does not.

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
| `v` | pin or hide the controls overlay |

**Glyph codecs** decide how a cell becomes a glyph. `pixels` (the default)
paints a low-resolution picture from shade ramps, half-blocks and quadrants.
`letters` draws with type: printable characters ordered by ink, ASCII strokes
on edges, and blocks only where the picture is lit (`█` for near-white, `▀▄`
for a bright half). On truecolor and 256-color terminals each character sits
on a dim tint of its cell's colour, so faces and midtones hold their shape;
16-color and mono terminals keep a black background.
`ascii` is letters with only printable ASCII: no blocks, no tint and no
background color at all (the terminal's own shows through), every glyph in
its cell's color brightened to make up for the ink a character leaves
unfilled. Glyph selection is the same on every terminal tier; colours
follow the terminal's capabilities.

**Dials** retune the renderer while the video plays. Shadow lift opens dark
scenes. Edge strength sets how many contours get strokes. Hysteresis trades
flicker against responsiveness. Nothing is rebuilt: the same asset re-renders
at the new setting. `s` saves the dials and codec beside the asset as
`<name>.player.toml`, and they load the next time that video plays.

**The controls overlay** (`v`, and briefly at start-up) lists the keys. Above
them is the clip name, the active codec, whether the settings are saved, and
the grid size (`213x58 cells`).

**Zoom out for detail.** The asset is resolution-independent, so a smaller
terminal font means more cells and a sharper picture. Use your terminal's
zoom-out shortcut (often Cmd - on macOS; bindings vary by terminal). The
player can't change the font itself
([docs/research/zoom.md](docs/research/zoom.md)), so below 160 columns the
overlay says so. At 240 or more columns and 36 or more rows, on a non-ASCII
tier, the overlay text is drawn in big block letters so it stays readable,
except under the `ascii` codec, where overlays stay plain one-character-per-cell
ASCII on the terminal's default background.

Useful flags:
- `--loop`, `--seek 1:30`, `--fps-cap 30`
- `--codec pixels|letters|ascii`
- `--palette ascii|unicode|braille`, `--tier truecolor|256|16|mono`
- `--no-query` (skip capability queries)
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
```

`import` also takes `--name`, `--ss`/`--t` (times as `SS`, `MM:SS` or
`HH:MM:SS`), `--fps`, `--res WxH` and `--force`. `home` prints the folder.

A **composition** is a TOML file that stitches any number of clips on one
timeline. Each clip is placed with `at` and trimmed with `in`/`out`; gaps are
black and a later clip draws on top. The file is the source of truth, so you
can write it by hand; `auto-ascii-player demo.toml` plays one directly.

Every command except interactive `play` and `compose play` accepts `--json`,
which makes stdout exactly one JSON value.
[docs/AGENT-GUIDE.md](docs/AGENT-GUIDE.md) (also printed by `auto-ascii
agent-guide`) documents the folder layout, the JSON shapes and the composition
schema.

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
| `crates/auto-ascii-cli` | the `auto-ascii` CLI |
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
