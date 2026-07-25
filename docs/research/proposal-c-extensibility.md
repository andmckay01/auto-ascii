<!-- Architecture proposal C (Extensibility) — produced by the ascii-engine-plan workflow, 2026-07-24; input to ../../PLAN.md -->

# SLEEPYTIME — Architecture Proposal (Architect C: Extensibility)

## 1. Stack

All-Rust, one Cargo workspace, two binaries (`sleepy-factory`, `sleepy-player`), video decode via ffmpeg CLI subprocess piping rawvideo. Rationale: the riskiest interface is the factory⇄player asset contract — one language means one reader/writer crate with round-trip tests, not a duplicated spec; `imageproc`+`rayon` cover Scharr/ETF/morphology offline so Python buys nothing; single static binaries distribute everywhere the player must run (SSH boxes, containers). Key crates: `crossterm` (session setup/events only — never per-cell commands), `zstd`, `memmap2`, `image`/`imageproc`/`rayon` (factory), `insta`/`proptest`/`criterion` (harness). Deliberately no libav bindings, no OpenCV, no flatbuffers.

## 2. Modules

- **`slpy-asset`** — SLPY container reader/writer, plane-ID registry, META (CBOR) schema. The contract crate; zero deps on the rest.
- **`sleepy-engine`** — pure functions: viewport math, separable resampler, layer compositor, palettes, hysteresis state. Planes in → `FrameGrid` out. **No I/O, no terminal, no clock.** This purity is the load-bearing seam: golden tests run headless, and any future presenter (kitty graphics, GUI, GIF export) reuses it or bypasses glyph selection entirely.
- **`sleepy-term`** — `Backend` trait + the one ANSI implementation: capability volley, tiering, diff, SGR elision, byte assembly, restore-on-panic.
- **`sleepy-player`** — binary: `Clock`, render loop, pacing/downshift policy, CLI.
- **`sleepy-factory`** — binary: ffmpeg ingest, shot detection, feature extraction, temporal filtering, SLPY writing.
- **`sleepy-eval`** — metrics, golden-frame harness, resize fuzzer, perf gates; used by CI and by the factory's improve loop.

## 3. Data flow

```
reference.mp4
  | ffmpeg subprocess: -f rawvideo -pix_fmt rgb24, fps-normalized
  v
FACTORY  shot-detect -> per shot:
         L* luma (+p2/p98 levels) | Scharr -> ETF -> edge mag + Ex/Ey | highlight flags
         temporal EMA -> u8 planes @480x270 -> temporal delta -> zstd-19
  v
asset.slpy  [HEADER | FRAM* | NORM | META | FIDX | TRLR]
  | mmap
  v
PLAYER   AssetReader.frame(i)  ~0.2 ms decode
      -> Resampler: 5 planes -> cell-resolution planes (rebuilt tables on resize)
      -> Engine: compositor (base/edge/highlight) + palette + hysteresis -> FrameGrid
      -> Backend: color quantize to tier, diff, SGR-elide, [?2026h..l], ONE write(2)
  v
terminal  kitty/ghostty/wezterm ... xterm/VTE ... tmux/SSH ... linux console
```

## 4. Key interfaces

```rust
// sleepy-term ------------------------------------------------------------
pub trait Backend {
    fn init(opts: &Opts) -> Result<Self> where Self: Sized; // raw mode, alt screen, cached volley
    fn caps(&self) -> &Caps;
    fn poll(&mut self, dl: Deadline) -> Option<Event>;      // Resize(c,r), Key, Quit; SIGWINCH self-pipe
    fn present(&mut self, g: &FrameGrid) -> FrameStats;     // diff+elide+sync+single write
    fn shutdown(&mut self);                                 // also Drop + signal path
}
pub struct Caps { color: ColorTier, glyphs: GlyphFlags, sync_2026: bool,
                  cells: (u16,u16), cell_px: Option<(u16,u16)>, throughput: Throughput }
pub struct Cell { ch: char, fg: Rgb, bg: Rgb, attrs: Attrs }   // engine always emits truecolor
pub struct FrameGrid { cols: u16, rows: u16, pad: Pads, cells: Vec<Cell> }
pub struct FrameStats { bytes: u32, cells_changed: u32, write_ns: u64 } // feeds pacing + perf gates

// sleepy-engine ----------------------------------------------------------
pub enum Layer {                       // layer TYPES are code (enum, no dyn); layer PARAMS are data
    Base { ramp: Ramp },                              // L* -> index, hysteresis(±0.35 step)
    Edge { lut: EdgeLut, t_on: u8, t_off: u8 },       // (orient bin, subpos) -> glyph, temporal dual-threshold
    Highlight { glyphs: SmallVec<char>, max_base: u8 },
}
pub struct Palette { key: PaletteKey, layers: Vec<Layer> }   // key = repertoire x color tier x density band
// Palettes load from embedded TOML; --palette-dir overrides. Compositor: top-down,
// first layer whose predicate fires owns the cell; fg color sampled from C regardless.

pub struct Resampler { wx: Vec<Tap1D>, wy: Vec<Tap1D>, tmp: Vec<u16> }
impl Resampler {
    fn rebuild(&mut self, vp: Viewport, src: (u16,u16));     // ~50 us, on resize only
    fn run(&self, src: &Plane, tmp: &mut [u16], dst: &mut CellPlane); // separable box, Q8, no floats
}

// slpy-asset -------------------------------------------------------------
pub struct AssetReader { /* mmap, header, FIDX slice, double-buffered PlaneSet */ }
impl AssetReader {
    fn open(p: &Path) -> Result<Self>;
    fn meta(&self) -> &Meta;                          // shot levels, cut flags, palette hints (CBOR)
    fn frame(&mut self, i: u32) -> Result<&PlaneSet>; // decode block, delta-add into prev buffer
    fn seek(&mut self, i: u32) -> Result<()>;         // FIDX -> keyframe, roll forward (<=12 ms)
    fn plane(&self, id: PlaneId) -> Option<&Plane>;   // registry lookup; unknown IDs skipped
}

// sleepy-player ----------------------------------------------------------
pub trait Clock { fn now(&self) -> Instant; fn frame_for(&self, t: Instant) -> u32; }
// v1: MonotonicClock. The trait exists so audio sync later = one AudioClock impl
// slaved to a cpal stream — zero changes to loop or engine. Cost today: ~10 lines.
```

Extensibility positions, explicitly: (a) a future kitty-graphics/sixel path is a **second trait** (`PixelPresenter`, fed planes, bypassing glyphs), added when wanted — I refuse to contort the cell-oriented `Backend` to speculatively serve pixels; the seam is that `sleepy-engine` is presenter-agnostic. (b) New layer types are enum variants, not plugins — dyn-trait layer plugins are gold-plating. (c) New feature planes are new registry IDs the runtime can ignore.

## 5. Asset format: adopt SLPY, two amendments

Adopt the format digest's custom chunked container verbatim (temporal byte-delta + per-frame zstd blocks + FIDX + keyframes every 60): measured 0.2 ms/frame decode and ~14–25x ratio settle it; SQLite stays a documented fallback, not built. Amendments: **(1)** replace the single orientation plane `O` with doubled-angle **Ex/Ey** planes (bias-128 u8) per the grid-math digest — orientation is π-periodic, `O` cannot pass through a linear resampler without averaging artifacts, while Ex/Ey reuse the exact same separable path and give edge-coherence (vector magnitude collapse) for free. Plane set: **Y, Emag, Ex, Ey, H, + C (RGB565 half-res)**. The algorithms digest loses this argument. **(2)** the plane-ID registry + per-plane subblocks + required-chunk flag are the format's entire forward-compat story — a future motion plane (M) or audio-envelope plane ships as a new ID; old players skip it; no version bump. Hand-rolled fixed layout, not serde/bincode: deterministic bytes are a golden-test requirement.

Hard rule restated: no per-cell glyph decisions in the asset (req 4).

## 6. Render loop

Per frame: (1) drain events; on Resize: `TIOCGWINSZ`, async re-send `CSI 16t`, recompute viewport/pads, `Resampler::rebuild`, reset hysteresis, force full repaint (full-clear debounced 100 ms; math never debounced). (2) `clock.frame_for(now)` — latest-frame-wins; if behind, skip to current, never queue. (3) `reader.frame(i)`; on cut flag, reset hysteresis. (4) resample 5 planes. (5) composite: normalize via shot levels, base-ramp hysteresis, edge dual-threshold + `edge_lut[bin][subpos]`, highlight override; write `FrameGrid`. (6) `backend.present()`. (7) if `FrameStats.write_ns` sustained over budget → downshift ladder: harden color quantization (longer SGR runs) → drop to 256 → drop effective fps to 15.

**Budget, 300×80 (24 000 cells), slow tier (SSH/tmux, 256-color, diff mode), 24 fps = 41.6 ms:**

| step | ms |
|---|---|
| decode (delta+zstd, mmap) | 0.3 |
| resample 5 planes (~1.6 M MAC) | 0.5 |
| composite + glyph select (~80 ops/cell) | 1.5 |
| 256-color LUT + diff + span assembly | 1.5 |
| PTY write: ~15–25% damaged (hysteresis keeps it low) ≈ 5–7 K cells × ~5 B elided ≈ 25–40 KB | 10–30 (link-bound) |
| headroom / jitter | remainder |

Hysteresis doubles as throughput control: fewer changed cells = fewer bytes. Fast tier skips diffing (full repaint + 2026 sync).

## 7. Factory pipeline + eval harness

Stages (per shot, `rayon` across shots): **ingest** (ffmpeg → rgb24 @480×270, fps-normalized) → **shot detect** (histogram distance; emits cut flags) → **extract** (L* + p2/p98 levels temporally smoothed; Scharr → ETF → hysteresis-thresholded mag, unthinned; Ex/Ey; top-hat highlights) → **temporal EMA** on all planes → **quantize + write** SLPY.

Harness (`sleepy-eval`, all CI-able, all runnable in a loop by an agent):
- **Quantitative metrics**: render `FrameGrid` headlessly, rasterize via stored glyph-coverage tables to a grayscale image, compare to the downscaled source: SSIM (structure), edge-recall against factory edge mask, temporal-flip rate (% cells changing glyph between visually-static frames — the flicker number). Emitted as JSON per run; improve loop = tweak params → rerun → diff metrics.
- **Golden tests**: `insta` snapshots of (a) SLPY bytes for a fixed synthetic input (format determinism, CRC per chunk), (b) `FrameGrid` dumps for fixed asset × fixed grid × fixed palette. Any diff is reviewable text.
- **Resize fuzzing**: `proptest` over (cols, rows) ∈ [1,500]², random resize sequences mid-playback against a mock backend: invariants — no panic, viewport ≤ terminal, aspect error minimal-among-candidates, letterbox symmetric ±1, hysteresis buffers realloc'd.
- **Perf gates**: `criterion` benches on decode/resample/composite with hard thresholds; mock-backend byte-budget assertions per tier (e.g. slow-tier frame ≤ 60 KB) using `FrameStats`. Regressions fail CI.

## 8. Milestones

- **M0 (demo)**: factory converts a 10 s clip to SLPY (Y plane only); player plays base-ramp ASCII, truecolor fg, letterboxed 16:9, live resize, on kitty + xterm. Accept: ≥24 fps at 200×50 local; resize mid-play never panics; Ctrl-C/SIGTERM restores terminal (asserted by pty test).
- **M1 (tiers)**: full query volley w/ DA1 sentinel + caching, tier ladder (truecolor/256/mono × ASCII/shades+halfblock/full), diff+SGR elision, 2026 sync, drop-frame pacing. Accept: ≥24 fps at 120×34 through tmux+ssh (localhost, throttled to 2 Mb/s); mono output legible; `--tier` override works.
- **M2 (layers)**: ETF edge pipeline, Ex/Ey planes, edge+highlight layers, 8 palettes from TOML. Accept: golden frames for all 8 palettes; edge-recall ≥0.6 on test suite; side-by-side visibly beats M0.
- **M3 (harness)**: full `sleepy-eval` in CI: metrics JSON, goldens, resize fuzz (10 k cases), perf gates. Accept: one command runs the whole loop <5 min; a deliberate param regression trips a gate.
- **M4 (temporal + polish)**: hysteresis everywhere, scene-cut resets, shot auto-levels, 256-color LUT quality pass, seek. Accept: temporal-flip rate <2% on static scenes; no pumping across cuts; scrub decodes ≤12 ms.
- **M5 (hardening + seams)**: Windows/ConPTY functional (slow tier), Linux console (CP437-safe palette), `Clock` trait landed with sync-drift test, `--palette-dir` user palettes documented, quirk table keyed on queried identity. Accept: CI matrix green on Linux/macOS/Windows; 30-min soak with random resizes, zero leaks/desyncs.

## 9. Top risks

1. **Slow-tier throughput blows the budget** (SSH/old VTE). Mitigate: diff+elision+quantize ladder, latest-frame-wins, `FrameStats`-driven auto-downshift; M1 acceptance test on a throttled link.
2. **Temporal flicker makes output look broken** — the make-or-break aesthetic risk. Mitigate: hysteresis + factory EMA + cut resets; temporal-flip metric gated in CI so regressions are caught mechanically, not by eye.
3. **Capability detection lies** (tmux, ConPTY, SSH stripping COLORTERM). Mitigate: active volley with DA1 sentinel + timeouts, cache keyed on environment tuple, quirk table keyed on *queried* identity, always-available `--tier`/`--no-query` escape hatches.
4. **Asset format churn mid-project** doubles rework. Mitigate: `slpy-asset` lands first with round-trip + byte-golden tests at M0; plane registry and required-chunk flag absorb additive change; major-version reject is the only breaking path.
5. **Edge layer quality disappoints** (ETF tuning is the artistic bet). Mitigate: layers are priority-composited, so palettes can ship edge-disabled; metrics harness (M3) exists before deep tuning (M4) so iteration is measured; worst case, M2 ships base+highlight only and the product still works.
