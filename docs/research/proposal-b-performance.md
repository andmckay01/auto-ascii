<!-- Architecture proposal B (Performance) — produced by the ascii-engine-plan workflow, 2026-07-24; input to ../../PLAN.md -->

# SLEEPYTIME — Architecture Proposal (Architect B: Performance)

**Design stance:** the worst case defines the architecture. The design point is a 300×80 grid inside tmux over a 20 ms-RTT SSH link; a local Kitty window is the degenerate easy case. Consequences: one render path (diff-always), zero allocation per frame after init/resize, bytes-on-the-wire treated as the scarce resource, and perf budgets enforced by CI gates, not aspiration.

---

## 1. Stack

All-Rust, one Cargo workspace, two binaries (`sleepy-factory`, `sleepy-player`) plus shared crates; video decode via `ffmpeg` CLI subprocess (rawvideo pipe), never libav bindings. Rationale: the asset format is the riskiest contract and must be one set of structs compiled into both sides; `criterion`/`insta`/`proptest` give the req-9 harness in a single `cargo test`; static binaries distribute everywhere the player must run (musl for SSH targets); `imageproc`+`rayon` cover offline CV without Python's second toolchain. Key crates: `zstd`, `memmap2`, `crossterm` (setup/events only — never per-cell commands), `image`, `imageproc`, `rayon`, `clap`, `insta`, `proptest`, `criterion`. I reject the ecosystem digest's `serde/bincode` for frame payloads: the hot path reads fixed-layout bytes straight out of mmap; serde appears only in META/CBOR and factory-side JSON.

## 2. Modules

- `slpy-format` — SLPY container: header/chunk structs, reader (mmap + zero-copy), writer, CRC, fuzz targets. No I/O policy.
- `slpy-core` — engine: viewport math, separable resampler, layer compositor, ramp/palette tables, hysteresis state. `no_std`-adjacent discipline: no allocation outside `resize()`.
- `slpy-term` — Backend trait + implementations: `AnsiBackend` (the real one, parameterized by Caps), `SimBackend` (in-memory PTY simulator with throttled writer — the benchmark/test backend), capability probe (DA1-sentinel volley, caps cache).
- `sleepy-player` — binary: event loop, pacing, auto-downshift governor, stats HUD.
- `sleepy-factory` — binary: ffmpeg ingest, shot detection, Scharr+ETF+XDoG, highlight/levels extraction, delta+zstd encode.
- `sleepy-eval` — binary: renders assets headlessly via `SimBackend`, emits JSON metrics; the automation hook for req 9.

## 3. Data flow

```
video.mp4
   │ ffmpeg subprocess (rawvideo rgb24 @ 480x270, fps-normalized)
   ▼
FACTORY: shots → L* luma → Scharr→ETF→XDoG (E, Ex, Ey) → highlight/shadow flags
   → per-shot levels (p2/p98, EMA) → temporal delta → zstd -19 → SLPY chunks
   ▼
asset.slpy  [HEADER | META | NORM | FRAM… | FIDX | TRLR]
   │ mmap, FIDX seek, zstd decode → memadd into double-buffered plane set
   ▼
PLAYER core: separable resample (planes → Vc×2Vr luma, Vc×Vr others)
   → layer composite (edge ▸ highlight ▸ base ramp) + hysteresis → Cell grid
   ▼
BACKEND: color quantize to tier → diff vs prev grid → SGR-elided spans
   → one write(2), DEC-2026-wrapped → PTY → terminal / tmux / sshd
```

## 4. Key interfaces (sketch)

```rust
pub struct Cell { ch: u32 /*char*/, fg: Rgb, bg: Rgb, attrs: u8 } // 12 B, POD, memcmp-able

pub trait Backend {
    fn caps(&self) -> &Caps;                        // color tier, glyph flags, sync_2026, cell_px, throughput
    fn events(&mut self) -> &mut EventQueue;        // Resize/Key/Quit; SIGWINCH self-pipe
    fn present(&mut self, grid: &Grid<Cell>) -> FrameStats; // quantize→diff→elide→single write
    fn invalidate(&mut self);                       // force full repaint next present
    fn resize(&mut self, c: u16, r: u16);           // ONLY allocation point
    fn shutdown(&mut self);                         // also on Drop + signal path
}
pub struct FrameStats { bytes: u32, cells_damaged: u32, write_ns: u64, dropped: bool }

pub struct LayerCfg { ramp: RampId, gate: Gate }    // Gate: EdgeThresh{on,off} | HighlightMask | Always
pub struct Palette { base: &'static [Glyph], edge_lut: [[Glyph; 4]; 8], hi: &'static [Glyph],
                     charset: GlyphFlags, min_cols: u16 }   // 8 shipped, keyed charset×density

pub struct AssetReader { map: Mmap, fidx: &[FrameIdx], planes: [PlaneBuf; 2] /*double-buffered*/ }
impl AssetReader { fn frame(&mut self, i: u32) -> &Planes; fn seek(&mut self, i: u32); } // keyframe+deltas

pub struct Resampler { wx: Vec<Tap1D>, wy: Vec<Tap1D>, wy2: Vec<Tap1D>, tmp: Vec<u16> } // rebuilt on resize only
impl Resampler { fn run(&mut self, src: &Plane, dst: &mut Plane8, vy: VertRes /*x1|x2*/); }
```

Division of labor per the terminals digest: core always picks final glyphs (req 4) in truecolor; backend owns quantization, diffing, byte emission. **Amendment:** quantization happens *before* diffing — on 256/mono tiers, cells that quantize equal don't get emitted. On slow tiers this is a major damage reducer and it falls out free from ordering.

## 5. Asset format

**Adopt SLPY v1 as specified in the format digest** (temporal byte-delta + per-frame zstd blocks + FIDX + CBOR META), with three amendments: (a) **drop the SQLite fallback entirely** — blobs aren't contiguous for mmap, and "crash-safe appends" is solved by TRLR-detects-truncation + rerun (factory is idempotent); carrying two containers violates 80/20. (b) Plane set per grid-math digest: Y, E, Ex, Ey (doubled-angle, resample-linear), H, plus half-res RGB565 chroma. (c) FRAM per-plane subblocks are 64-byte aligned so decode targets are SIMD/memadd friendly. Decode cost ~0.2 ms/frame, worst-case seek = keyframe + ≤59 deltas ≈ 12 ms — fine for scrub. Compress at zstd-19 offline; codec byte retained.

## 6. Render loop and budget

**One code path.** Full repaint = `invalidate()` + diff. GPU-tier "just repaint" is a config default (invalidate every frame costs nothing there), not a second implementation. Sync-2026 wrapped whenever DECRQM said yes.

Per frame: (1) drain events; if resize-flag: `TIOCGWINSZ`, recompute letterbox (`R = (16/9)·a`, a from `CSI 16t` else 2.0), rebuild resampler tables (~50 µs), `invalidate()`; (2) pacing: pick target frame by wall clock, **latest-frame-wins** — if behind, skip asset frames, never queue; (3) `AssetReader::frame` (zstd + memadd into standing buffers); (4) resample: Y at Vc×2Vr (two vertical samples/cell → half-block fills + `‾/-/_` subposition; this reconciles the algorithms digest's 8-tap idea with the separable resampler — same code path, x2 vertical res), E/Ex/Ey/H at Vc×Vr; (5) composite per cell: edge gate (temporal dual-threshold) ▸ highlight ▸ base ramp with index hysteresis (±0.35 step); scene-cut flag resets hysteresis; (6) `present()`: quantize→diff→SGR-elide→CUP-vs-rewrite (rewrite ≤6 unchanged cells)→single write.

**Budget, 300×80 (24 000 cells) @30 fps, SSH+tmux tier (33.3 ms wall):**

| stage | ms |
|---|---|
| decode (zstd delta + memadd, 5 planes) | 0.3 |
| resample (Y×2 + 4 planes, ~2.2 M MAC) | 0.6 |
| composite + hysteresis (≈80 ops/cell) | 1.4 |
| quantize + diff + buffer assembly | 0.8 |
| **CPU total** | **≈3.1 (<10 % budget)** |
| write(2) | wire-bound, see below |

CPU is never the problem; **bytes are the budget.** Targets enforced by the governor: slow tier ≤64 KB/frame (≈1.9 MB/s @30 fps); typical video coherence + hysteresis yields 10–30 % damage → 256-color diff ≈ 15–40 KB/frame. Governor watches EMA of `write_ns` + `bytes`; on sustained overrun it downshifts in order: harden color quantization (longer SGR runs) → widen hysteresis (less damage) → 256→mono → halve fps. Each step logged in stats; upshift after 3 s of headroom. Frames whose write would block are dropped, never buffered.

## 7. Factory + eval harness

Stages (each a pure function on arrays, rayon-parallel per shot): ingest (ffmpeg pipe) → shot detect (histogram distance) → L* convert → Scharr → ETF smooth → XDoG magnitude + doubled-angle Ex/Ey → highlight/shadow top-hat → per-shot levels (temporally smoothed) → temporal EMA on planes → delta+zstd encode → FIDX/TRLR.

**Automation harness (the req-9 deliverable, built at M3 but stubbed from M0):**
- **Metrics (`sleepy-eval` → JSON):** *fidelity* — render glyphs headlessly, rasterize via stored coverage table, SSIM vs. the downscaled source; *edge recall/precision* vs. factory E-plane at grid res; *flicker score* — mean cell glyph-changes/frame on static shots (gate: <0.5 %); *damage rate* — mean cells_damaged/frame (gate per tier).
- **Golden frames:** `insta` snapshots of the Cell grid (glyph+quantized color) for 3 reference assets × 4 canonical grids (80×24, 132×43, 300×80, 500×140) × 3 palettes. Deterministic by construction (fixed-point math, no per-frame RNG).
- **Resize fuzzing:** `proptest` — random (cols,rows) ∈ [1,1000]², random resize sequences mid-playback against `SimBackend`; invariants: no panic, no OOB, letterbox aspect error < 8 %, viewport ≥1×1, full recovery within one frame.
- **Perf gates:** `criterion` benches with hard thresholds committed to the repo: decode <0.5 ms, resample <1 ms, composite <2 ms, present-to-SimBackend <1.5 ms at 300×80; `SimBackend` with a 2 MB/s throttled writer asserts ≥24 effective fps end-to-end. CI fails on regression >15 %.
- **Iteration loop:** `sleepy-eval --asset X --config Y` is a pure scoring function → parameter sweeps (thresholds, hysteresis widths, ramp choices) run as hundreds of headless jobs; results ranked by JSON score. No terminal needed anywhere in the loop.

## 8. Milestones

- **M0 (demo, ~week 2):** factory: mp4 → SLPY (Y plane only); player: base ramp, truecolor, diff renderer, letterbox, live resize, stats overlay. *Accept:* 3-min clip ≥24 fps on Kitty and xterm-256color; mid-play resize artifacts-free; stats show ms+bytes/frame.
- **M1:** capability probe (DA1-sentinel volley + cache), tiers (truecolor/256/mono), DEC 2026, governor. *Accept:* 300×80 in tmux-over-throttled-SSH (SimBackend 2 MB/s + real link) sustains ≥24 fps, ≤64 KB/frame; probe timeout never hangs piped output.
- **M2:** full plane set, 3-layer compositor, 8 palettes, hysteresis + scene-cut reset. *Accept:* flicker score <0.5 % on static shots; edge layer visibly tracks contours at 80×24 and 300×80.
- **M3:** harness complete: goldens, fuzz, perf gates, `sleepy-eval` JSON, CI green. *Accept:* 10 k fuzz cases pass; a deliberate 20 % perf regression fails CI.
- **M4:** hard targets: Linux console (mono palette), Windows/ConPTY (works, classified slow), auto-downshift verified on real SSH. *Accept:* plays on `TERM=linux` and Windows Terminal; hosed-terminal-on-crash test passes (signal + Drop restore).
- **M5:** seek/scrub, HUD, musl static builds, 3 demo assets. *Accept:* scrub latency <50 ms; single-file binaries <5 MB.

## 9. Top risks

1. **SSH/tmux throughput collapse** (wire slower than any budget). → Governor with measured `write_ns`, latest-frame-wins drops, mono floor ≈8 KB/frame diff; throttled SimBackend in CI so regressions surface pre-ship.
2. **Capability misdetection** (tmux strips COLORTERM, probes swallowed). → Active volley with DA1 sentinel + timeout, quirk table keyed on queried identity, cached caps, `--tier` override; default-safe = 256-color+ASCII when unsure.
3. **Temporal flicker destroys perceived quality** (the failure mode of every naive converter). → Hysteresis on index/edge/orientation, factory temporal EMA, scene-cut resets, flicker metric as CI gate — quality regression becomes a red build, not a review comment.
4. **Real-asset compression misses synthetic ratios** (350 MB+/3 min). → Size gate in factory CI; levers in reserve: per-scene keyframes, half-res E/Ex/Ey, zstd dictionary; format supports all without version bump.
5. **Font variance breaks ramps/edge glyphs** (coverage ±15 %, missing `╱╲`). → Conservative default coverage table + per-font overrides, glyph-support tiers in Caps, ASCII-only floor palette always available; goldens rendered per palette so degradation is tested, not assumed.
