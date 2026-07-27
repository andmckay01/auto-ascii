# sleepytime

Realtime ASCII-art video for terminals — and a library you can drop into your
own project.

An offline **factory** distills a reference video into a resolution-independent
feature asset (`.slpy`: luma, edge magnitude + orientation, highlights, chroma —
never glyphs). A runtime **player** maps that asset onto whatever cell grid you
have *right now*: glyph ramps, directional edge strokes, highlights and
half-blocks, letterboxed to 16:9, reflowing live on resize, with temporal
hysteresis so nothing flickers. Because glyph choice happens at render time, one
asset looks right at 80×24 in a Linux console and at 320×90 in a GPU terminal —
and equally right inside *your* renderer, if you'd rather draw the cells
yourself.

All Rust, one workspace, two binaries (`sleepy-factory`, `sleepy-player`) and one
library crate (`sleepytime`). ffmpeg is used by the factory as a subprocess; the
player links no codecs.

## Quickstart

```bash
# 1. distill a video into an asset (offline, minutes; needs ffmpeg on PATH)
cargo run --release -p sleepy-factory -- build clip.mp4 -o intro.slpy

# 2. play it
cargo run --release -p sleepytime --bin sleepy-player -- intro.slpy
#    q / Esc quit · 0-9 seek to 0-90% · resize the window any time

# 3. no terminal? render frames as text instead
cargo run --release -p sleepytime --example headless-dump -- intro.slpy 3 100x28
```

Useful player flags: `--loop`, `--fps-cap 30`, `--seek 1:30`, `--palette
ascii|unicode|braille`, `--tier truecolor|256|16|mono`, `--no-query` (skip the
capability probe), `--sim 213x58:300` (headless render + one JSON stats line).
`sleepy-factory inspect intro.slpy` prints the container's header, chunks and
CRC status.

## Embedding it

```rust
// Cargo.toml:  sleepytime = "0.1"
sleepytime::Player::builder().asset("intro.slpy").looping(true).build()?.run()?;
```

That is the whole player: capability probe, letterbox, live resize, terminal
restore on quit/Ctrl-C/panic. If you own your event loop and your output layer —
a game engine, a GUI widget, a test — use the terminal-free entry instead:

```rust
use sleepytime::RenderSession;

let mut session = RenderSession::open("intro.slpy")?;
let grid = session.render(/*frame*/ 0, /*cols*/ 120, /*rows*/ 40)?; // -> &Grid<Cell>
for row in 0..grid.rows() {
    for cell in grid.row(row) {
        draw(cell.glyph(), cell.fg, cell.bg); // char + RGB, that's the whole contract
    }
}
```

Build it with `default-features = false` and the dependency tree is the engine
and nothing else — no clap, no crossterm ([`crates/sleepytime`](crates/sleepytime)
documents the three feature tiers). Runnable examples:
[`simple-play`](crates/sleepytime/examples/simple-play.rs) (12 lines, the whole
player), [`embedded-loop`](crates/sleepytime/examples/embedded-loop.rs)
(`RenderSession` in a hand-rolled loop, with a mid-run resize) and
[`headless-dump`](crates/sleepytime/examples/headless-dump.rs) (frames to stdout
as text). API docs: `cargo doc -p sleepytime --open`.

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
`build → eval → read metrics → edit params → repeat`:

```bash
sleepy-factory eval --corpus corpus/ --params params.toml \
    --baseline runs/base.json --out runs/latest.json --html runs/latest.html
```

`eval` builds each clip (cached by input+params hash), renders it headlessly,
and writes metrics JSON plus a self-contained HTML contact sheet — SSIM, edge F1
against a Canny ground truth, flicker (glyph switches/cell/s), damage rate and
bytes/frame, per-stage frame times. Nonzero exit on a tolerance breach against
the baseline. The reference clips are described in
[`corpus/README.md`](corpus/README.md) (the videos themselves are local, not
committed).

## Repo map & development

| path | what |
|---|---|
| `crates/sleepytime` | **the public library** + the `sleepy-player` binary |
| `crates/slpy-format` | SLPY v1 container (zstd + temporal delta, O(1) seek) |
| `crates/slpy-core` | pure engine: viewport, resampler, compositor, palettes, hysteresis |
| `crates/slpy-term` | `Backend` trait, ANSI backend, capability probe, simulator |
| `crates/sleepy-factory` | offline factory + the eval driver |
| `crates/slpy-eval` | metrics, fixtures, report schema |
| `scripts/eval.sh` | the one-command gate: tests, clippy, resize fuzz, perf gates, corpus eval |
| `PLAN.md` / `INTERFACES.md` | the build plan and the API registry |

`scripts/eval.sh` is what "green" means here; it runs in under five minutes on a
four-core box (corpus sections skip automatically when the local clips are
absent — committed tests never depend on them).

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Unless you explicitly state otherwise, any contribution
intentionally submitted for inclusion in this project shall be dual licensed as
above, without any additional terms or conditions.
