# fonts/ — glyph ink-coverage tables (PLAN §3.4, M5 item B)

Committed, deterministic coverage tables for the glyphs the 8 shipped
palettes can emit — the data behind `--font-table NAME|PATH` (player,
`RenderSession`, `sleepy-factory eval`) and the offline reference for ramp
work. Terminals cannot be queried for their font; these tables let the user
*assert* one, giving the engine (a) a measured ink model for metrics and
(b) the font's **repertoire**, which vetoes palette tiers the font cannot
render (braille → unicode → ascii) instead of drawing missing-glyph boxes.

## The tables

| table | font file (Debian/Ubuntu package) | repertoire result |
|---|---|---|
| `dejavu-sans-mono.toml` | `dejavu/DejaVuSansMono.ttf` (fonts-dejavu-core) | full blocks/box-drawing; **no braille** |
| `liberation-mono.toml` | `liberation/LiberationMono-Regular.ttf` (fonts-liberation) | no `╱ ╲ ▖ ▗ ▘ ▝`, no braille → unicode vetoed to ascii |
| `ubuntu-mono.toml` | `ubuntu/UbuntuMono-R.ttf` (fonts-ubuntu) | no `‾ ╱ ╲` and no half/quadrant blocks (has `░▒▓█`), no braille → unicode vetoed to ascii |
| `noto-sans-mono.toml` | `noto/NotoSansMono-Regular.ttf` (fonts-noto-mono) | full blocks/box-drawing; **no braille** |
| `conservative.toml` | none — the committed slpy-eval `CONSERVATIVE_COVERAGE` constants (DejaVu-derived, M2) in table form | ASCII repertoire only (the probe-default posture) |

Repertoire claims were verified against each font's own `cmap` (and
`fc-list :charset=`), not eyeballed. Notable: **no common monospace font
ships palette-7 braille** — DejaVu's braille lives in DejaVu Sans/Serif,
not the Mono face; terminals render braille through font *fallback*, which
is exactly why `PaletteChoice::Braille` is opt-in/verified-only (PLAN §3.4)
and why every builtin table vetoes `BrailleVerified` down to unicode.

## Generation model (deterministic)

`sleepy-factory font-table <font.ttf> --name NAME -o fonts/NAME.toml`

- Glyph set: `slpy_core::palette::all_palette_glyphs()` — enumerated from
  the palette data (ramps, edge LUTs incl. junctions, subposition triplet,
  quadrants/half-blocks, reachable braille masks), never a hardcoded list.
- Cell: 64×128 px (§3.4). The font is scaled so its monospace **advance**
  equals 64 px (terminals size text by advance); the glyph ink box is
  centered and clipped to the cell.
- `coverage` = Σ antialiased ink / (64·128) — same fractional-ink integral
  as the M2 `derive_coverage.py` ffmpeg reference (ab_glyph vs freetype
  agree within ~1–2% on solid glyphs, e.g. DejaVu `@` 0.2665 vs 0.2627;
  thin hinting-sensitive strokes like `_` can differ more).
- `lstar` = CIE L\* of coverage as linear luminance (white on black).
- Missing glyph (`.notdef`): coverage 0 + `missing = [...]` entry + a WARN
  on stderr. `missing` is the repertoire-veto input.
- Determinism: byte-identical output for identical font bytes
  (unit-tested: `sleepy-factory font_table::generator_is_deterministic`;
  re-running the commands above reproduces these files byte-for-byte).

Regenerate all five:

```sh
cargo run --release -p sleepy-factory -- font-table --conservative -o fonts/conservative.toml
cargo run --release -p sleepy-factory -- font-table /usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf        --name dejavu-sans-mono  -o fonts/dejavu-sans-mono.toml
cargo run --release -p sleepy-factory -- font-table /usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf --name liberation-mono -o fonts/liberation-mono.toml
cargo run --release -p sleepy-factory -- font-table /usr/share/fonts/truetype/ubuntu/UbuntuMono-R.ttf          --name ubuntu-mono       -o fonts/ubuntu-mono.toml
cargo run --release -p sleepy-factory -- font-table /usr/share/fonts/truetype/noto/NotoSansMono-Regular.ttf    --name noto-sans-mono    -o fonts/noto-sans-mono.toml
```

(`sudo apt-get install fonts-dejavu-core fonts-liberation fonts-ubuntu
fonts-noto-mono` supplies the inputs. The tables are embedded into
`slpy-core` at build time — `FontTable::builtin` — and pinned by
`slpy-core` unit tests.)

## Per-font L\* comparison (M5 report)

Over the 37 glyphs present in **all four** fonts:

- mean per-glyph ΔL\* across fonts: **5.0**; max: **ΔL\* 19.6 on `▒`**
  (DejaVu draws MEDIUM SHADE much denser: L\* 75.1 vs ≈55.5 elsewhere).
  Runners-up: `_` ΔL\* 10.0, `#` 8.8, `░` 8.7 — the ±15% ink-variance risk
  PLAN §9.5 predicted, now measured.
- vs `conservative`, per-font tables also *extend* the scale: `█` reaches
  L\* ≈ 100 while the conservative (ASCII-only) table tops out at `@`
  (L\* ≈ 59). SSIM scored under `--font-table` therefore normalizes to a
  different ink anchor — absolute SSIM values are only comparable within
  one table choice.

**Ramp-ordering inversions exist under every font** (this is precisely what
§3.4 built these tables to expose; ramps are deliberately NOT retuned in
M5). Palettes 5 (` ·░▒▓█`) and 8 (` .:coO8@`) are monotone under all four
fonts + conservative. The hand-specified ASCII ramps are not:

- palette 1 `" .:-=+*#%@"`: `:`→`-` inverts under **all** fonts (a `-` has
  less ink than `:` everywhere); `=`→`+` under all; `+`→`*` under
  DejaVu/Liberation/conservative; `#`→`%` under DejaVu/Ubuntu/conservative.
- palette 2 `" .,:;i1tfLCG08@"`: `f`→`L` inverts under all fonts;
  `1`→`t` (DejaVu/Liberation/conservative), `0`→`8`
  (Liberation/Ubuntu/Noto), `i`→`1` (Noto).
- palette 4 `" .+*"`: `+`→`*` inverts under DejaVu/Liberation/conservative
  (`*` is smaller-inked than `+` in those fonts).

Follow-up (out of M5 scope, per the task directive): rebuild the ASCII
ramps from these tables at uniform L\* intervals (§3.4's stated offline
procedure) — the committed data is already sufficient.

## Wiring

- Player/CLI: `sleepy-player --font-table NAME|PATH`, facade
  `PlayerBuilder::font_table(..)` — resolved at `build()`; the repertoire
  veto applies after Caps-based palette resolution.
- Embedders: `RenderSession::set_font_table(Some("NAME|PATH"))`.
- Metrics: `sleepy-factory eval --font-table NAME|PATH` scores downscale-
  SSIM through the chosen table (`slpy_eval::CoverageTable::from_font_table`).
  The default remains `conservative` — the committed `runs/base.json`
  baseline's table.
