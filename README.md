# auto-ascii

Realtime ASCII-art video for terminals — and a library you can drop into your
own project.

An offline **factory** distills a reference video into a resolution-independent
feature asset (`.ascii`: luma, edge magnitude + orientation, highlights, chroma —
never glyphs). A runtime **player** maps that asset onto whatever cell grid you
have *right now*: glyph ramps, directional edge strokes, highlights and
half-blocks, letterboxed to 16:9, reflowing live on resize, with temporal
hysteresis so nothing flickers. Because glyph choice happens at render time, one
asset looks right at 80×24 in a Linux console and at 320×90 in a GPU terminal —
and equally right inside *your* renderer, if you'd rather draw the cells
yourself.

All Rust, one workspace: one library crate (`auto-ascii`), two engine binaries
(`auto-ascii-factory`, `auto-ascii-player`) and the `auto-ascii` CLI that files
clips in a library and stitches them into compositions. ffmpeg is used by the
factory as a subprocess; the player links no codecs.

**Status.** The engine milestones (M0–M5) are complete and signed off; the tool
layer (M6–M8: on-screen key hints, the `auto-ascii` CLI, compositions) landed on
2026-09-20. `auto-ascii` 0.2.0 and `auto-ascii-core` / `-format` / `-term`
0.1.0 are on crates.io. `scripts/eval.sh` is the gate: workspace tests, clippy
at `-D warnings`, a resize fuzz, criterion perf gates, and a corpus eval when
local clips are present. [CONTRIBUTING.md](CONTRIBUTING.md) has the build, the
gate and the rules.

## Quickstart

```bash
# 1. distill a video into an asset (offline, minutes; needs ffmpeg on PATH)
cargo run --release -p auto-ascii-factory -- build clip.mp4 -o intro.ascii

# 2. play it
cargo run --release -p auto-ascii --bin auto-ascii-player -- intro.ascii
#    q/Esc quit · space pause · 0-9 jump · ←/→ 5 s · d dial · [ ] adjust
#    / codec · s save · v controls

# 3. no terminal? render frames as text instead
cargo run --release -p auto-ascii --example headless-dump -- intro.ascii 3 100x28
```

Useful player flags: `--loop`, `--fps-cap 30`, `--seek 1:30`, `--palette
ascii|unicode|braille`, `--tier truecolor|256|16|mono`, `--no-query` (skip the
capability probe), `--codec pixels|letters`, `--sim 213x58:300` (headless
render + one JSON stats line).
`auto-ascii-factory inspect intro.ascii` prints the container's header, chunks and
CRC status.

**Glyph codecs.** How a cell becomes a glyph is a pluggable codec: `pixels`
(the default — shade ramps and half-blocks, a low-res picture) or `letters`
(printable characters ordered by ink, ASCII strokes on edges, blocks only
where the picture is lit: `█` for near-white, `▀▄` for a bright half). `/` cycles them while playing; `v` shows the clip
name and active codec above the key hints. The dials (`d`, `[ ]`) and the
codec are per video: `s` saves them beside the asset as
`<name>.player.toml`, and they load the next time that video plays.

`auto-ascii-player comp.toml` plays a **composition** — an unbounded stitch of
clips on one timeline, each placed with `at` and trimmed with `in`/`out`, gaps
black — without re-encoding anything; the `schema = 1` file is documented in
[docs/AGENT-GUIDE.md](docs/AGENT-GUIDE.md).

## The `auto-ascii` CLI

One command that takes a video from anywhere, processes it, and files the
result in `~/auto-ascii/library/` (override with `AUTO_ASCII_HOME`) beside a
JSON sidecar recording where it came from:

```bash
cargo install --path crates/auto-ascii-cli   # or: cargo run -p auto-ascii-cli --

auto-ascii import ~/Desktop/clip.mp4   # ffmpeg-ingest -> library/clip.ascii
auto-ascii list                        # name, duration, fps, frames, bytes, source
auto-ascii play clip                   # the player above, on a library clip

auto-ascii cut clip --in 0:05 --out 0:20        # -> library/clip-0m05s-0m20s.ascii
auto-ascii compose new demo                     # -> compositions/demo.toml
auto-ascii compose add demo clip --at 0:10      # append a [[clip]] table
auto-ascii compose play demo                    # or: compose export demo
```

`import` also takes `--name`, `--ss`/`--t` (`SS`, `MM:SS` or `HH:MM:SS`),
`--fps`, `--res WxH` and `--force`; `info <clip>` prints one clip's header
plus sidecar, `compose show <name>` prints a composition's resolved timeline
with its gaps and overlaps, and `home` prints the folder. Every command
accepts `--json`, which makes stdout exactly one JSON value to parse — see
[docs/AGENT-GUIDE.md](docs/AGENT-GUIDE.md), which `auto-ascii agent-guide`
prints verbatim.

## Install

The fast path is a prebuilt player binary — copy it, run it, done (measured
0.2 s from binary-in-hand to the first presented frame on a 2-core Linux box,
probe deadline included; the M5 acceptance budget was 2 *minutes*):

```bash
# from a release tarball / dist/ directory produced by scripts/release.sh:
install -m 0755 auto-ascii-player-x86_64-unknown-linux-musl ~/.local/bin/auto-ascii-player
auto-ascii-player intro.ascii        # the musl build is fully static: zero deps
```

Building each flavor yourself (`scripts/release.sh` does all of this and
enforces the < 5 MB stripped-size gate):

| target | how | notes |
|---|---|---|
| Linux (native) | `cargo build --release -p auto-ascii --features bin` | binary at `target/release/auto-ascii-player`; `strip` it |
| Linux (static musl) | `rustup target add x86_64-unknown-linux-musl` + `apt install musl-tools`, then `cargo build --release --target x86_64-unknown-linux-musl -p auto-ascii --features bin` | `ldd` reports "statically linked" — runs on any x86-64 Linux |
| Windows (cross) | `rustup target add x86_64-pc-windows-gnu` + `apt install mingw-w64`, then `cargo build --release --target x86_64-pc-windows-gnu -p auto-ascii --features bin` | **untested-cross**: it compiles and links here (headless Linux CI, no wine); the session layer uses crossterm's Windows console API — report issues |
| macOS | build **on a Mac**: `make build` or the native cargo line above (see the Makefile's macOS section) | no osxcross by policy; Apple Silicon and Intel both build from source |

A from-source build on a clean checkout (fresh `target/`, warm crates.io
cache) takes 30–45 s on a 2-core Linux box — `time cargo build --release -p
auto-ascii --features bin`.

To embed the library (turn the `bin` feature off if you only want
`RenderSession`):

```toml
auto-ascii = "0.2"
```

Note the **0.2**: `auto-ascii` 0.1.0 was published under the project's
previous crate names and is yanked — 0.2.0 is the first release of the
renamed engine, built on `auto-ascii-core` / `-format` / `-term` 0.1.0.

## Embedding it

```rust
// Cargo.toml:  auto-ascii = "0.2"
auto_ascii::Player::builder().asset("intro.ascii").looping(true).build()?.run()?;
```

That is the whole player: capability probe, letterbox, live resize, terminal
restore on quit/Ctrl-C/panic. If you own your event loop and your output layer —
a game engine, a GUI widget, a test — use the terminal-free entry instead:

```rust
use auto_ascii::RenderSession;

let mut session = RenderSession::open("intro.ascii")?;
let grid = session.render(/*frame*/ 0, /*cols*/ 120, /*rows*/ 40)?; // -> &Grid<Cell>
for row in 0..grid.rows() {
    for cell in grid.row(row) {
        draw(cell.glyph(), cell.fg, cell.bg); // char + RGB, that's the whole contract
    }
}
```

Build it with `default-features = false` and the dependency tree is the engine
and nothing else — no clap, no crossterm ([`crates/auto-ascii`](crates/auto-ascii)
documents the three feature tiers). Runnable examples:
[`simple-play`](crates/auto-ascii/examples/simple-play.rs) (12 lines, the whole
player), [`embedded-loop`](crates/auto-ascii/examples/embedded-loop.rs)
(`RenderSession` in a hand-rolled loop, with a mid-run resize) and
[`headless-dump`](crates/auto-ascii/examples/headless-dump.rs) (frames to stdout
as text). API docs: `cargo doc -p auto-ascii --open`.

## Palettes

Eight shipped palettes, keyed **charset tier × layer role** — density (cell
count) picks the ramp *length* within a tier and color depth caps it (color
already carries luminance, so truecolor gets shorter ramps than mono). Pick a
charset tier with `--palette` / `PaletteChoice`; the rest is automatic.

| # | key | ramp / LUT |
|---|-----|------------|
| 1 | `ascii/base/coarse` (<70 cols) | `" .:-=+*#%@"` |
| 2 | `ascii/base/fine` (≥70 cols) | `" .,:;i1tfLCG08@"` |
| 3 | `ascii/edge` | orientation LUT `= - _` / `\|` / `/` `\`, junctions `+` `#` |
| 4 | `ascii/highlight` | `" .+*"` |
| 5 | `unicode/base` | `" ·░▒▓█"` + quadrants `▖▘▝▗▀▄▌▐` |
| 6 | `unicode/edge` | `‾ ─ _` / `│` / `╱` `╲`, junction `┼` |
| 7 | `unicode/detail` (fine density, verified fonts) | braille U+2800–28FF, edge/texture only — never solid fills |
| 8 | `mono-fallback/base` (16-color / no color / Linux console) | `" .:coO8@"` (CP437-safe) |

Sub-cell vertical structure (the two luma taps per cell) draws half-blocks
`▀▄` and corner quadrants on the unicode tiers, and the `" - _` subposition
triplet on the ASCII tiers. Everything an ASCII tier can emit is printable
ASCII `0x20..=0x7E` — a strict subset of CP437, so the Linux console never
sees a glyph its font lacks.

Terminal support is capability *data*, not per-terminal code: one ANSI backend
parameterized by a probed `Caps` (color tier, glyph repertoire, synchronized
output, cell pixel size). See [`docs/TERMINAL-CHECKLIST.md`](docs/TERMINAL-CHECKLIST.md)
for the per-terminal manual pass.

## Corpus & tuning workflow

Every tunable — edge thresholds, EMA constants, hysteresis deltas, highlight
percentiles — lives in [`params.toml`](params.toml), never in code. The loop is
`build → eval → read metrics → edit params → repeat`, driven by whatever
reference videos you keep in `corpus/` (local and gitignored, see
[`corpus/README.md`](corpus/README.md)):

```bash
auto-ascii-factory eval --corpus corpus/ --params params.toml \
    --out runs/base.json --html runs/base.html          # once: record a baseline
auto-ascii-factory eval --corpus corpus/ --params params.toml \
    --baseline runs/base.json --out runs/latest.json --html runs/latest.html
```

`eval` builds each clip (cached by input+params hash), renders it headlessly,
and writes metrics JSON plus a self-contained HTML contact sheet — SSIM, edge F1
against a Canny ground truth, flicker (glyph switches/cell/s), damage rate and
bytes/frame, per-stage frame times. Nonzero exit on a tolerance breach against
the baseline. `runs/` is gitignored output. `auto-ascii-factory sweep` runs the
same eval over a grid of parameter overrides and ranks the combos; the sweep
file format is documented at the top of
[`crates/auto-ascii-factory/src/sweep.rs`](crates/auto-ascii-factory/src/sweep.rs).

## Repo map & development

| path | what |
|---|---|
| `crates/auto-ascii` | **the public library** + the `auto-ascii-player` binary |
| `crates/auto-ascii-cli` | the `auto-ascii` CLI: a library of clips, `cut`, `compose`, `--json` |
| `crates/auto-ascii-format` | ASCI v1 container (zstd + temporal delta, O(1) seek) |
| `crates/auto-ascii-core` | pure engine: viewport, resampler, compositor, palettes, hysteresis |
| `crates/auto-ascii-term` | `Backend` trait, ANSI backend, capability probe, simulator |
| `crates/auto-ascii-factory` | offline factory (lib + bin) + the eval and sweep drivers |
| `crates/auto-ascii-eval` | metrics, fixtures, report schema |
| `scripts/eval.sh` | the one-command gate: tests, clippy, resize fuzz, perf gates, corpus eval |
| `docs/AGENT-GUIDE.md` | the CLI for agents (also `auto-ascii agent-guide`) |
| `docs/TERMINAL-CHECKLIST.md` | the manual per-terminal pass |
| `docs/PLAN.md`, `docs/PLAN-M6-M8.md` | the build plans: the engine, then the CLI and compositions |
| `docs/INTERFACES.md` | the internal API registry, milestone by milestone |
| `docs/research/` | pre-implementation research digests (they use the project's earlier working name) |
| `tools/` | `prep_video.py` (canvas-normalize a source video), `soak.py` (resize-storm soak) |

`scripts/eval.sh` is what "green" means here; it runs in a few minutes on a
2-core box, and the corpus section skips itself when `corpus/` holds no videos —
committed tests never depend on them.

## License

[MIT](LICENSE). Unless you explicitly state otherwise, any contribution
intentionally submitted for inclusion in this project shall be licensed the
same way, without any additional terms or conditions.
