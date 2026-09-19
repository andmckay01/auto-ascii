# AUTO-ASCII — Final Build Plan (PLAN.md)

> **Scope amendment (2026-07-25, owner directive):** connectivity-oriented engineering is OUT of scope. No SSH/WAN-link tuning or testing, no tmux/ConPTY quirk-chasing, no adaptive throughput governor / downshift ladder, no throttled-link CI gates. The engine targets local terminals and stays abstract and simple to embed in another project. The backend abstraction and capability tiers (color depth, glyph repertoire) remain — those are about terminal features, not connectivity. The diff renderer remains as generic efficiency. Prose below that references SSH/tmux worst cases predates this amendment; where it conflicts, this note wins. M4 is re-scoped to "embeddable library API + local-terminal compatibility."

## 1. Executive summary

**auto-ascii** is a realtime ASCII-art video engine: an offline **factory** distills reference video into a resolution-independent feature asset, and a terminal **player** maps that asset onto whatever grid the user has right now — 24–30 fps, letterboxed 16:9, live resize, from Kitty down to a Linux console over SSH. All-Rust, one Cargo workspace, two binaries.

**(a) Asset file format — direct answer.** We recommend a **custom RIFF-style chunked container ("ASCI v1"): per-frame zstd-compressed feature planes with a temporal byte-delta pre-pass, an explicit frame index, and a CBOR metadata chunk.** Why: it was benchmarked against mp4/ffmpeg, npz, flatbuffers, SQLite, and CBOR streams, and it is the only candidate that simultaneously hits every requirement — measured **14–25× compression** (temporal delta multiplies zstd's ratio 4–6×), **~0.2 ms/frame decode** (5× under our 1 ms budget), O(1) seek via the index, trivially mmap-able, single tiny dependency (libzstd, ~300 KB, vendorable), and a reader that is ~300 lines of Rust we fully own. Video codecs are disqualified outright: lossy coding and chroma subsampling corrupt feature planes, and ffmpeg is a dependency disaster for a runtime player. A 3-minute clip lands at ~122 MB on synthetic planes, ~190–350 MB conservatively on real footage — acceptable, with format-compatible shrink levers in reserve. We drop the SQLite fallback entirely: if index patching gets annoying we fix 30 lines, we don't adopt a second container. Crucially, the asset stores **feature planes only, never glyphs** — glyph choice happens at render time (requirement 4).

**(b) "3–10 character configurations" — verdict: agree, with one refinement.** The count is right; the *key* is wrong. Configurations should be keyed by **charset tier × layer role**, not by resolution — density (cell count) selects ramp *length within* a config rather than multiplying the set. The full cross-product (3 tiers × 3 densities × 3 roles = 27) is over-engineering. **We will ship exactly 8 palettes** (concrete ramps in §3.4): two ASCII base ramps (coarse/fine), ASCII edge LUT, ASCII highlight, Unicode block base, Unicode edge, Unicode braille detail (verified-support only), and a mono/CP437-safe fallback. Truecolor tiers get shorter ramps (color carries luminance); mono tiers get the longest.

**Engineering posture:** 80/20 throughout; the eval harness lands *before* the quality-critical layer work so every aesthetic decision from then on is measured; the diff-based output path keeps every terminal cheap, with a local GPU terminal as the primary target (see Scope amendment — slow-link engineering is descoped).

## 2. System overview

```
                         OFFLINE (factory, minutes)                RUNTIME (player, 33 ms/frame)
                                                                 
 reference.mp4                                                    terminal caps probe
      |                                                           (DA1-sentinel volley, cached)
      | ffmpeg subprocess                                                  |
      | rawvideo rgb24 @ 480x270, fps-normalized                           v
      v                                                          +--------------------+
 +-----------------------------+                                 |  auto-ascii-player     |
 |  auto-ascii-factory             |                                 |  event loop, pacing|
 |  shot detect (hist delta)   |                                 |  downshift governor|
 |  L* luma + p2/p98 levels    |                                 +---------+----------+
 |  Scharr -> orient smooth    |                                           |
 |    -> E, Ex, Ey (2-theta)   |      asset.ascii                 +---------v----------+
 |  top-hat highlights (H)     |      [HEADER|META|NORM|         |  auto-ascii-core         |
 |  temporal EMA               |       FRAM..|FIDX|TRLR]         |  viewport/letterbox|
 |  delta + zstd-15 encode     | ---> mmap + O(1) index seek --> |  separable resample|
 +-----------------------------+      0.2 ms/frame decode        |  layer compositor  |
              ^                                                  |  hysteresis        |
              |                                                  |  glyph+RGB Cells   |
 +------------+---------------+                                  +---------+----------+
 |  auto-ascii-eval                 |                                            |
 |  SSIM / edge F1 / flicker  |                                  +---------v----------+
 |  goldens, fuzz, perf gates |                                  |  auto-ascii-term backend |
 |  SimBackend @ 2 MB/s       |                                  |  quantize -> diff  |
 |  JSON + HTML contact sheet |                                  |  -> SGR-elide      |
 +----------------------------+                                  |  ?2026 wrap        |
        ^                                                        |  ONE write(2)      |
        |  parameter sweeps (orchestrating agent loop)           +---------+----------+
        +--- params.toml edits <--- metrics JSON                           |
                                                                           v
                                                          kitty / wezterm / xterm / tmux
                                                          / ssh / Linux console
```

Crates: `auto-ascii-format` (container, no I/O policy), `auto-ascii-core` (pure engine: viewport, resampler, compositor, palettes, hysteresis — no terminal, no clock, fully golden-testable), `auto-ascii-term` (Backend trait, `AnsiBackend`, `SimBackend`, capability probe), `auto-ascii-player` (bin), `auto-ascii-factory` (bin), `auto-ascii-eval` (metrics/harness).

## 3. Runtime engine spec

### 3.1 Backend abstraction & capability tiers

"Pluggable backends" ≠ N backend structs. Terminals differ by capability *data*, not code shape: we ship **one `AnsiBackend` parameterized by `Caps`**, plus `SimBackend` (in-memory, throttleable writer) for tests/benches. The trait exists for those two and a future ConPTY oddity — nothing more.

```rust
pub struct Cell { ch: u32, fg: Rgb, bg: Rgb, attrs: u8 } // 12 B POD, memcmp-able
                                                          // bg is load-bearing: half-block cells
                                                          // are (fg,bg) pixel pairs — no ambiguity
pub trait Backend {
    fn caps(&self) -> &Caps;
    fn events(&mut self) -> &mut EventQueue;          // Resize(c,r) | Key | Quit; SIGWINCH self-pipe
    fn present(&mut self, grid: &Grid<Cell>) -> FrameStats; // quantize -> diff -> elide -> ONE write
    fn invalidate(&mut self);                          // force full repaint next present
    fn resize(&mut self, c: u16, r: u16);              // the ONLY allocation point in the hot path
    fn shutdown(&mut self);                            // also runs from Drop + signal handler
}
pub struct Caps { color: ColorTier /*True|C256|C16|Mono*/, glyphs: GlyphFlags,
                  glyph_support: GlyphSupportTier,     // font-coverage tier: which repertoires trusted
                  sync_2026: bool, cells: (u16,u16), cell_px: Option<(u16,u16)>,
                  throughput: Throughput /*Fast|Normal|Slow*/, can_query: bool }
pub struct FrameStats { bytes: u32, cells_damaged: u32, write_ns: u64, dropped: bool }
```

**One render path (graft from B).** There is no separate "full repaint mode": GPU-tier full repaint is simply `invalidate()` every frame as a config default. Diff-always means one code path, tested everywhere.

**Quantize before diff (graft from B).** `present()` quantizes colors to the terminal's tier *first*, then diffs against the previous quantized grid. On 256/mono tiers, cells that quantize equal produce zero damage — a major free byte reducer on slow links, purely from ordering.

**Capability detection.** Passive signals (`COLORTERM`, `TERM`, `TERM_PROGRAM`, terminfo with `-x` extended caps) are hints, not truth. Active volley in one write, raw mode: XTVERSION (`CSI > 0 q`), DECRQM 2026 (`CSI ? 2026 $ p` — never a hardcoded support table), XTGETTCAP `RGB`, `CSI 16 t` (cell px → aspect), then **DA1 (`CSI c`) last as sentinel** — when its reply arrives, everything that was going to answer has answered. Deadline 150–250 ms local, ~1 s if `SSH_CONNECTION`. `!isatty` or silence → dumb tier. Result cached keyed on `(TERM, TERM_PROGRAM, ssh?, tmux?)`. Quirk table keyed on *queried identity*, not `TERM`. Escape hatches: `--tier <t>` forces a tier, and `--no-query` (graft from C) skips the volley entirely for hostile ConPTYs and logging PTYs. `Throughput::Slow` flagged on SSH/tmux/ConPTY detection.

**Session hygiene.** Alt screen `?1049h`, hide cursor `?25l`, autowrap off `?7l`, termios raw. Restoration (`SGR 0`, cursor, main screen, cooked mode) runs from atexit + SIGINT/SIGTERM handler + `Drop` — a hosed terminal is the #1 TUI bug report and gets its own pty test.

### 3.2 Viewport / letterbox math

Inputs: `cols × rows`, cell aspect `a = cell_h_px / cell_w_px` (from `CSI 16t` or `TIOCGWINSZ` px fields; fallback **2.0**; re-queried on every resize since font zoom changes it). Target 16:9 → `Vc = R·Vr` with `R = (16/9)·a` (a=2 → R ≈ 3.5556).

```
r1 = round(cols / R); cand1 = (cols, r1)   valid if r1 <= rows    # width-limited
c2 = round(rows * R); cand2 = (c2, rows)   valid if c2 <= cols    # height-limited
pick valid candidate minimizing |ln((c/(r*a)) * 9/16)|            # log aspect error
pad_left = (cols-c)>>1; pad_top = (rows-r)>>1                     # remainder right/bottom
```

Worked: 80×24 → **80×23**; 213×58 → **206×58** (pads 3/4/0/0); 320×90 → exact. Below 32×9 we render a centered "enlarge terminal" card; all divisions clamp Vc,Vr ≥ 1.

### 3.3 Resampler

Planes live at 480×270 u8 (chroma 240×135). Per cell we need a box average over a fractional source rect — done as a **separable resample with precomputed Q8 tap tables** per axis (`Tap1D { src_start: u16, ntaps: u8, w: [u16] }`, weights sum 256). H-pass into a shared `u16` buffer, V-pass with u32 accumulator, `>>16` out. No floats in the hot loop; fixed trip counts autovectorize under `-O3` — **no hand SIMD (80/20)** unless profiling shows >2 ms.

- Rebuild on resize only: ~50 µs, <4 KB. Per-frame cost ~1.6M MAC for 5 planes at 300×80 ≈ 0.3–0.5 ms; heavy downscale gets *cheaper* (H-pass ops ≈ SW·SH regardless of grid), which kills any argument for mipmaps.
- **Luma is resampled at Vc × 2Vr (graft from B)** — two vertical samples per cell through the exact same code path. This drives half-block fills (fg/bg = 2 vertical pixels, doubling vertical resolution and correcting the 1:2 cell aspect) and picks `‾` vs `-` vs `_` subposition glyphs. Near-zero extra code, meaningful quality.
- Upscale degrades naturally to 1–2 linear taps (bilinear); same code, no branch.

**Orientation is π-periodic and must never be averaged as angles.** The asset stores doubled-angle vectors `Ex = mag·cos 2θ`, `Ey = mag·sin 2θ` (bias-128 u8) — these are linear and run through the same resampler. Per cell: quantize to 4–8 direction bins by sign/comparison tests (no atan2), and **coherence = |(Ex,Ey)| / resampled E** — near zero means directions cancelled inside the cell → suppress the edge layer. Conflict handling falls out free.

### 3.4 Layer compositor & ramp system

Three layers, back to front: **L0 base luminance** (smooth low-structure ramp), **L1 edge/contour** (directional glyphs), **L2 highlight** (sparse accents). **Composition is priority/override — one glyph per cell, never blended:** edge wins if gated on and base isn't near-white; else highlight if flagged and base is dark-mid; else base ramp. Color is orthogonal: fg always sampled from the chroma plane regardless of winning layer; highlights may boost value/saturation. The payoff: edges inject bright high-frequency strokes and highlights extend perceived range, so the base ramp can be short and smooth (8–16 steps) without flatness.

**The 8 shipped palettes** (embedded TOML, `--palette-dir` override; density band selects ramp length within a config):

| # | Key | Ramp / LUT |
|---|-----|------------|
| 1 | ascii/base/coarse (<70 cols) | `" .:-=+*#%@"` |
| 2 | ascii/base/fine (≥70 cols) | 16-step `" .,:;i1tfLCG08@"` |
| 3 | ascii/edge | orientation LUT: `-` `/` `\|` `\` + `_` `=` `+` `#` (junction) |
| 4 | ascii/highlight | `" .+*"` |
| 5 | unicode/base | `" ·░▒▓█"` + quadrants `▖▘▝▗▀▄▌▐` from 2×2 pattern |
| 6 | unicode/edge | `─ │ ╱ ╲ ‾ _ ┼` (8-dir + junction) |
| 7 | unicode/detail (fine density + verified braille only) | braille U+2800–28FF, edge/texture layer only — never solid fills |
| 8 | mono-fallback/base (16-color/no-color/Linux console) | `" .:coO8@"` (CP437-safe) |

Ramps are built offline from per-font ink-coverage tables (glyphs rasterized 64×128, coverage → L\*, picked at uniform L\* intervals, deduped at ΔL\* < 3, low-structure glyphs only for base). We ship coverage tables for 4 common monospace fonts plus one conservative default; terminals can't be queried for fonts, so `Caps.glyph_support` records the trusted repertoire tier and `--font-table` overrides.

### 3.5 Realtime glyph selection (per cell, per frame)

```
taps    = luma at (cx, 2cy) and (cx, 2cy+1)          # Vc×2Vr plane
n       = clamp((L - shot_lo) * shot_inv_range)       # per-shot auto-levels from NORM chunk
idx     = hysteresis(n, prev_idx[cell])               # switch only past boundary ± 0.35·step
if e > T_on or (was_edge[cell] and e > T_off):        # temporal dual-threshold (Canny-style)
    g = edge_lut[dir_bin(Ex,Ey)][subpos]              # suppressed if coherence low
elif h_flag and idx < HI_CUT:  g = highlight_glyph(idx)
elif halfblock_tier and |top-bottom| large: g = ▀/▄ with fg/bg pair
else: g = ramp[idx]
fg = chroma sample (truecolor; backend quantizes)
```

Cost ≈ 60–90 ops/cell → <2 ms for 24k cells. Hysteresis (index, edge on/off, orientation bin + 8° guard) is the flicker killer *and* a throughput optimization: static regions produce zero damaged cells. Scene-cut flags from the asset reset all hysteresis state (else ghosting across cuts). **All hysteresis/temporal state is reset and reallocated on resize (graft from C).** No dithering in v1 — hysteresis + 16-step ramps look better; if ever added, it's stationary screen-space blue-noise indexed by (col,row).

### 3.6 Render loop, resize path, frame budget

Per frame: **(1)** drain events; on resize flag (SIGWINCH handler only sets an atomic): `TIOCGWINSZ`, recompute viewport/pads, rebuild resampler tables (~50 µs — math is never debounced; only the flicker-causing full clear is debounced 100 ms), async re-send `CSI 16t`, reset hysteresis state, `invalidate()`. **(2)** pacing: pick target frame by wall clock, **latest-frame-wins** — if behind, skip asset frames; frames whose write would block are dropped, never queued. **(3)** `AssetReader::frame(i)` — zstd decode + delta-add into the standing double buffer, zero alloc. **(4)** resample planes. **(5)** composite → `Grid<Cell>`. **(6)** `present()`: tier-quantize → memcmp diff per row → changed spans with skip-vs-move heuristic (rewrite ≤6 unchanged cells rather than emit an 8-byte CUP) → SGR run-length elision → `?2026h…l` wrap when DECRQM said yes → single `write(2)` from a reused ≥64 KB buffer. **(7)** stats: record `FrameStats` (bytes, damage, write time) per frame for the eval harness. (The adaptive downshift governor from the original draft is descoped per the Scope amendment — drop-frame pacing from step 2 is the only degradation mechanism.)

**Budget, 300×80 (24,000 cells) @30 fps, worst tier (SSH+tmux), 33.3 ms wall:**

| stage | ms |
|---|---|
| decode (zstd delta + memadd, 6 planes) | 0.3 |
| resample (luma ×2 vertical + 4 planes + chroma, ~2.2 M MAC) | 0.6 |
| composite + hysteresis (~80 ops/cell) | 1.5 |
| quantize + diff + SGR-elide + buffer assembly | 1.5 |
| write(2) local PTY | 0.5 |
| **CPU total** | **≈4.4 (<14% of budget)** |

CPU is never the problem; **bytes are the budget**: slow-tier target ≤64 KB/frame (≈1.9 MB/s @30 fps); typical coherence + hysteresis yields 10–30% damage → 15–40 KB/frame in 256-color diff mode. Windows/ConPTY: consume `WINDOW_BUFFER_SIZE_EVENT`, enable VT processing, classify slow — make it work, don't optimize it.

## 4. Asset format spec — ASCI v1

Custom chunked container. All integers little-endian. Only dependency: libzstd.

```
HEADER (64 B fixed):
  0  magic "ASCI" 4B          4  version_major u16 (reader rejects if > supported)
  6  version_minor u16 (additive only)   8  header_size u32 (=64)
 12  flags u32 (bit0 index present, bit1 CRCs present)
 16  fps_num/fps_den u16/u16  20  base_w,base_h u16/u16 (480,270)
 24  aspect_num/den u16/u16   28  frame_count u32
 32  plane_count u8           33  codec u8 (0=raw 1=lz4 2=zstd)
 34  filter u8 (0=intra 1=temporal-delta)   35  keyframe_ivl u8 (=60)
 36  plane_ids[8] u8×8 (registry: 1=Y 2=E 3=Ex 4=Ey 5=H 6=C ...)
 44  index_offset u64 (patched at close)    52  meta_offset u64   60 reserved u32

CHUNKS (tag FourCC u32 | flags u8 (bit0=required) | pad u24 | size u64 | payload | [crc32]):
  META  CBOR map: palette hints, per-plane gamma, provenance, factory version.
        Unknown keys ignored.
  NORM  per-scene: {first_frame u32, cut flag, per-plane p2/p98 levels}. Flat, mmap-read.
  FRAM  one per frame, streamed in order:
        frame_idx u32 | flags u8 (bit0=keyframe) |
        [plane_id u8 | comp_size u32 | raw_size u32 | zstd bytes] × plane_count,
        each plane subblock padded to 64-B alignment (SIMD/memadd-friendly decode targets).
        Per-plane subblocks let low tiers skip chroma entirely.
  FIDX  written last: frame_count × 16 B {offset u64, comp_size u32, flags u8, pad u24}.
  TRLR  "ASCI_END" — absence ⇒ truncated ⇒ factory rerun (factory is idempotent).
```

**Planes (6):** Y as L\* 480×270 u8; E = smoothed edge magnitude, unthinned (runtime max-pools so thin edges survive any grid); Ex/Ey doubled-angle u8 bias-128; H flags (bit0 highlight, bit1 deep shadow); C chroma RGB565 at 240×135 (half-res invisible at cell granularity).

**Compression:** temporal byte-delta (`cur−prev mod 256`) then per-frame zstd, level **-15** in the factory (decode speed is unaffected by level; lowered from -19 on 2026-08-31 — +0.91% size for a 2.84x faster build, and zstd is lossless so no quality metric moves). The format crate's `WriterOptions::default()` stays at 19: level is encoder policy, not a container property. Keyframe every 60 frames; seek = binary-search FIDX to nearest keyframe + ≤59 delta rolls ≈ 12 ms worst case. Playback decodes exactly one block + one memadd into a double-buffered plane set — zero alloc/frame.

**Versioning & forward compat (graft from C, explicit):** the plane-ID registry with skip-unknown semantics plus the required-chunk flag *is* the compat story. A future motion plane or audio-envelope plane is a new plane ID old players skip — not a version bump. Unknown non-required chunks are skipped by size; unknown required chunks are a hard error; major-version mismatch is the only other breaking path.

**Determinism:** hand-rolled fixed layout (serde/CBOR only inside META), per-chunk CRC32, byte-identical writer output for identical input — this is a golden-test requirement (§6), landing at M0/M1 because the factory⇄player contract is the riskiest interface in the system.

**Hard rule:** no per-cell glyph decisions in the asset, ever (requirement 4).

**Size:** 3 min @30 fps ≈ 122 MB (measured synthetic), 190–350 MB conservative real-world. Levers if needed, all format-compatible: base res 384×216, keyframe interval 120, quarter-res chroma, per-scene keyframes, zstd dictionary.

## 5. Offline factory spec

**Stages** (each a pure function on arrays; `rayon`-parallel per shot):

1. **Ingest** — `ffmpeg -i in.mp4 -vf scale=480:270,fps=30 -f rawvideo -pix_fmt rgb24 -` piped to stdin; `ffprobe -print_format json` for metadata. Subprocess, never libav bindings.
2. **Shot detect** — histogram distance; emits scene boundaries + cut flags into NORM.
3. **Extract** — sRGB→linear→L\* luma; Scharr gradients (better rotational symmetry than Sobel) → **two passes of orientation-aware bilateral smoothing on the doubled-angle field** (pragmatist cut: full Kang ETF only if the flicker metric later demands it) → hysteresis-thresholded edge magnitude, **unthinned** (thinning breaks under resampling) → Ex/Ey; top-hat highlight + percentile deep-shadow flags (offline detection is far stabler than runtime thresholding).
4. **Temporal EMA** on all planes (sensor-noise suppression, the first line of anti-flicker defense).
5. **Levels** — per-shot p2/p98, temporally smoothed → NORM (per-frame auto-levels pump; global levels waste range; per-shot is the 80/20 winner).
6. **Encode** — u8 quantize, temporal delta, zstd -15, FRAM/FIDX/TRLR.

**CLI shape:**

```
auto-ascii-factory build   <in.mp4> -o out.ascii --params params.toml
auto-ascii-factory eval    --corpus clips/ --params params.toml --baseline runs/base.json
auto-ascii-factory inspect <asset.ascii>          # header, chunks, sizes, CRC check
auto-ascii-factory sweep   --params params.toml --grid sweeps/edge_thresholds.toml
```

**Every tunable lives in `params.toml`** — edge thresholds, EMA constants, hysteresis δ, ramp definitions, highlight percentiles. **This is the agent socket:** an orchestrating agent runs `build → eval → read metrics JSON → edit params.toml → repeat`, hundreds of headless iterations with no terminal and no human in the loop; humans review only the HTML contact sheets. Palettes-as-TOML means taste iteration never touches code.

## 6. Eval & iteration harness (`auto-ascii-eval`)

Built at **M2, before layer/quality work** — every subjective engineering decision afterward is measured. Deterministic core (fixed-point math, no per-frame RNG) makes all of this byte-exact.

**Quantitative metrics** (JSON per run):
- **Downscale-SSIM / gradient correlation:** render the Cell grid headlessly, rasterize through the stored glyph-coverage tables to grayscale, compare against the downscaled source.
- **Edge F1 (precision/recall)** vs **source Canny at grid resolution** — ground truth, deliberately *not* the factory's own edge plane (no self-grading).
- **Flicker score:** mean glyph switches/cell/s on static shots. Gate: ≤2 switches/cell/s (≈<0.5% cells/frame).
- **Damage rate & bytes/frame** per tier from Sim/FakeBackend `FrameStats`.
- **Frame time** per pipeline stage.

**Golden tests (`insta`):**
- **ASCI byte-level goldens (graft from C, lands M0/M1):** deterministic writer output for a fixed synthetic input; per-chunk CRC asserted. The synthetic plane generator from the format research is the regression baseline.
- Cell-grid snapshots: 3 fixture assets × grids 80×24 / 206×58 / 320×90 × 3 palettes.
- **Per-palette golden escape-byte streams (graft from B):** degradation on fonts missing `╱╲` or braille is *tested* via `glyph_support` tiers, not assumed.

**Resize fuzzing (`proptest`):** random (cols,rows) sequences 1×1–1000×1000, storms mid-playback against SimBackend. Invariants: no panic, no OOB, viewport ⊆ terminal, **aspect error minimal-among-candidates**, **letterbox symmetric ±1**, **hysteresis buffers realloc'd to the new grid**, table rebuild <1 ms, full recovery within one frame.

**Perf gates (`criterion`, thresholds committed to repo):** decode <0.5 ms, resample <1 ms, composite <2 ms, present-to-SimBackend <1.5 ms @300×80; CI fails on >15% regression. **Plus the end-to-end hard gate (graft from B, at M2): SimBackend with a 2 MB/s throttled writer must sustain ≥24 effective fps at 300×80** — the SSH catastrophe becomes a red build, not a demo-day discovery.

**The human loop:** every `eval` run emits an HTML contact sheet (source vs render side-by-side, per-metric deltas vs baseline). Metrics catch regressions; contact sheets catch "metrics pass, art fails."

**Enforced discipline:** `resize()` is the only allocation point in the hot path — asserted in `auto-ascii-core` tests via a counting allocator, not aspiration.

## 7. Milestones

- **M0 — visible end-to-end demo (~week 1).** ffmpeg → luma-only ASCI-lite → player: letterbox math, live resize, ASCII base ramp, truecolor fg, diff renderer with invalidate-every-frame default, kitty target. ASCI writer already byte-deterministic with CRC goldens. *Accept:* 3-min clip ≥24 fps on kitty; resize reflows next frame; Ctrl-C/SIGTERM/panic restores terminal (pty test); ASCI byte-golden green.
- **M1 — real format + tiers.** Full ASCI v1 (delta+zstd, 64-B-aligned subblocks, FIDX, seek); caps probe with DA1 sentinel + cache + `--tier`/`--no-query`; truecolor/256/mono; ?2026; quantize-before-diff; one-write frames. *Accept:* asset ≤350 MB/3 min on real corpus; seek <50 ms; correct tier auto-detected on kitty, xterm-256color, linux console; probe never hangs piped output; CPU <8 ms/frame.
- **M2 — harness online (before quality work).** Metrics JSON, goldens ×3 grids ×3 palettes, 10k fuzz cases with full invariant set, criterion gates, end-to-end SimBackend fps gate (unthrottled — throttled-link gate descoped per Scope amendment), HTML contact sheets. *Accept:* one command runs the loop <5 min; a deliberate 20% perf regression and a deliberate param regression both fail CI.
- **M3 — layers.** Ex/Ey edge LUT + coherence suppression, highlight layer, luma at Vc×2Vr with half-block fills + subposition glyphs, full hysteresis + scene-cut + resize resets, all 8 palettes. *Accept:* flicker ≤2 switches/cell/s static; edge F1 ≥ tuned baseline vs source Canny; goldens re-approved deliberately; human sign-off on the fixed review reel.
- **M4 — embeddable library API + local-terminal compatibility (re-scoped per Scope amendment).** Extract a clean library crate API so the engine drops into another project: `auto_ascii::Player::builder().asset(path).palette(p).build()?.run()` plus a low-level `render_frame(&asset, grid) -> Grid<Cell>` entry for callers who own their event loop; feature-gate the binary; docs.rs-quality API docs with an embedding example. *Accept:* a fresh 20-line example crate depending on the library plays an asset; local terminals verified: kitty, alacritty, wezterm, gnome-terminal, xterm; `TERM=linux` legible with palette 8; no connectivity-specific code paths anywhere.
- **M5 — hardening + ship.** 1 h resize-storm soak; 4 font coverage tables + conservative default; quirk table keyed on queried identity; scrub UX; docs; static musl/mac/Windows binaries. *Accept:* zero desync/leaks in soak; scrub <50 ms; fresh-machine install-to-playback <2 min; binaries <5 MB.
- **M6+ (backlog, explicitly cut from v1):** motion plane (new plane ID, no version bump), audio via a `Clock` trait impl, braille polish, zstd dictionaries, kitty-graphics `PixelPresenter`.

## 8. Stack & dependencies

All-Rust workspace; ffmpeg strictly as CLI subprocess.

- **`auto-ascii-format`:** `zstd`, `crc32fast`, `ciborium` (META only — frame payloads are hand-rolled fixed layout, not serde).
- **`auto-ascii-core`:** no deps beyond `std` (pure, golden-testable).
- **`auto-ascii-term`:** `crossterm` (raw mode/alt screen/events ONLY — never per-cell commands), `libc` (ioctl/termios/self-pipe).
- **`auto-ascii-player`:** `memmap2`, `clap`, `anyhow`.
- **`auto-ascii-factory`:** `image`, `imageproc`, `rayon`, `ndarray`, `clap`, `indicatif`, `serde_json` (ffprobe), `std::process::Command` (ffmpeg).
- **`auto-ascii-eval` / dev:** `insta`, `proptest`, `criterion`.
- Rejected: libav bindings (`ffmpeg-next`), OpenCV, flatbuffers/capnproto, SQLite, ratatui-for-video (optional HUD only), Python anywhere in the shipping path.

License hygiene: all deps MIT/Apache/BSD. chafa (LGPL), mpv/timg/jp2a (GPL) are concept-only sources — algorithms studied, zero code reuse.

## 9. Risks & mitigations

1. **Fast but ugly — metrics pass, art fails.** *Mitigation:* HTML contact sheets in every eval run; palettes/ramps as TOML data so taste iteration is config-only; M3 human sign-off gate on a fixed review reel.
2. **~~SSH/tmux throughput collapse~~ — descoped per Scope amendment.** Slow-link adaptation is explicitly out of scope. The generic mitigations that remain anyway: quantize-before-diff + SGR elision + hysteresis keep output bytes small on every terminal, and drop-frame pacing handles any transient stall.
3. **Temporal flicker ruins video.** *Mitigation:* three-layer defense (factory EMA → runtime hysteresis on index/edge/orientation → scene-cut + resize resets); flicker is a first-class CI metric from M2.
4. **Capability detection lies** (tmux strips COLORTERM, ConPTY scrubs, probes swallowed). *Mitigation:* DA1-sentinel volley with timeouts, quirk table on queried identity, conservative default (256-color+ASCII), caps cache, `--tier` and `--no-query` escape hatches.
5. **Font/glyph coverage variance** (±15% ink coverage across fonts; missing `╱╲`, broken braille). *Mitigation:* first-class — `glyph_support` tiers in `Caps`, per-palette golden streams so degradation is tested, 4 font tables + conservative default, ASCII floor palette always available.
6. **Real-world compression 2–3× worse than synthetic → bloated assets.** *Mitigation:* size is a CI budget from M1 on a real corpus; reserve levers (384×216, keyframe 120, quarter-res chroma, dictionaries) are all format-compatible knobs — no redesign path exists or is needed.

## 10. Open questions for the project owner

1. **Corpus:** which reference videos define "good"? We need 3–5 canonical clips (mix of high-contrast, dark, fast-motion) locked early — they become the golden/metric baseline for every iteration.
2. **Asset size ceiling:** is ~200–350 MB per 3-minute clip acceptable for the distribution story, or should we pull the 384×216/keyframe-120 levers preemptively at some cost to fine detail?
3. **Windows priority:** ConPTY is planned as "works, classified slow" (M4). Is functional-but-unoptimized Windows acceptable for v1?
4. **Audio:** v1 is silent; the `Clock` seam makes audio sync a v2 add (cpal-slaved clock). Confirm silence is acceptable for the v1 demo narrative.
5. **Distribution:** static binaries only, or also a curl-able hosted demo (server streams pre-rendered escape sequences at a few fixed sizes — a fun marketing hack, but deliberately outside the resize-correct engine)?
6. **License for our code:** MIT/Apache-2.0 dual (Rust convention) unless you have other plans — affects nothing technically but should be set before first public commit.
