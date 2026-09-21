<!-- Architecture proposal A (Pragmatist, WINNER) — produced by the ascii-engine-plan workflow, 2026-07-24; input to ../PLAN.md -->

# SLEEPYTIME — Architecture Proposal (Architect A, Pragmatist)

## 1. Stack

All-Rust, one Cargo workspace, two binaries (`sleepy-factory`, `sleepy-player`) plus shared crates. Video decode via `ffmpeg` CLI subprocess piping rawvideo (zero build pain; latency irrelevant offline). Factory CV via `image` + `imageproc` + `rayon` — pure Rust suffices for Scharr, smoothing, thresholds, and removes the only argument for a Python factory. Player uses `crossterm` for raw-mode/alt-screen/events ONLY; all rendering is hand-assembled escape bytes, one `write(2)` per frame. `zstd` + `memmap2` for the asset. Test rig: `insta` (goldens), `proptest` (resize fuzz), `criterion` (perf gates). One language means the factory⇄player contract — the riskiest interface in this system — is one set of shared structs under one test harness; that outweighs Python's CV convenience, and single static binaries make demos trivially distributable.

## 2. Modules

- `slpy-format` — SLPY container read/write, temporal-delta + zstd codec, frame index, CBOR meta. Pure bytes, no I/O policy.
- `slpy-core` — viewport math, separable resampler, layer compositor, glyph palettes, hysteresis. Deterministic, zero terminal deps → fully golden-testable.
- `slpy-term` — `Backend` trait + one `AnsiBackend` parameterized by `Caps`; capability probe (DA1-sentinel volley + cache); diff/SGR-elision emitter; `FakeBackend` for tests.
- `sleepy-player` (bin) — event loop, pacing, auto-downshift, CLI.
- `sleepy-factory` (bin) — ffmpeg ingest, shot detection, feature extraction, SLPY writer.
- `slpy-eval` — metrics, golden harness, fuzzers, perf budgets, HTML reports (factory subcommand + CI entry).

Opinion: "pluggable backends" ≠ N backend structs. Terminals differ by capability *data*, not code shape — ship ONE AnsiBackend driven by `Caps`, keep the trait for FakeBackend and a future ConPTY oddity. Anything more is gold-plating.

## 3. Data flow

```
reference.mp4
   │ ffmpeg subprocess (rawvideo rgb24, 480x270, fps-normalized)
   ▼
FACTORY: shot detect → per-frame features (L*, Scharr→Ex/Ey/E, highlight
         top-hat) → temporal EMA → per-shot levels p2/p98
   │ delta-vs-prev + zstd-19 per plane per frame
   ▼
asset.slpy  [HEADER | META | NORM | FRAM×N | FIDX | TRLR]
   │ mmap + O(1) index seek
   ▼
PLAYER per frame: decode 1 FRAM (+delta add) → separable resample planes
   to Vc×Vr grid → per-cell layer composite (base/edge/highlight) +
   hysteresis → Cell grid (glyph + RGB)
   │ Backend: quantize color to tier, diff vs prev, SGR-elide,
   │ ?2026 wrap, ONE write()
   ▼
terminal (kitty…linux console), letterboxed 16:9, live resize
```

## 4. Key interfaces

```rust
trait Backend {
    fn caps(&self) -> &Caps;              // ColorTier, GlyphFlags, sync_2026,
                                          // cells, cell_px, Throughput
    fn events(&self) -> &Receiver<Event>; // Resize(c,r) | Key | Quit
    fn begin_frame(&mut self);
    fn blit(&mut self, grid: &FrameGrid); // diff+elide+quantize inside
    fn end_frame(&mut self) -> FrameStats;// {bytes, cells_changed, write_ns}
    fn resize(&mut self, c: u16, r: u16);
    fn shutdown(&mut self);               // also on Drop + signal path
}

struct Cell { ch: char, fg: Rgb, attrs: Attrs } // bg only for halfblock pairs

struct RampConfig { glyphs: Vec<char>, lstar: Vec<u8> }   // uniform-L* spaced
struct EdgeLut    { by_bin: [[char; 3]; 8] }              // 8 orient × subpos
struct Palette {   // one of ~6, keyed on GlyphFlags × density × ColorTier
    base: RampConfig, edge: EdgeLut, highlight: RampConfig,
    min_cols: u16, requires: GlyphFlags,
}

struct AssetReader { /* mmap, FIDX slice, prev-frame double buffer */ }
impl AssetReader {
    fn open(path: &Path) -> Result<Self>;
    fn meta(&self) -> &Meta;                       // fps, base res, shots
    fn frame(&mut self, idx: u32) -> &Planes;      // decode+delta, zero alloc
    fn seek(&mut self, idx: u32) -> &Planes;       // keyframe + roll forward
}

struct Resampler { wx: Vec<Tap1D>, wy: Vec<Tap1D>, tmp: Vec<u16> } // Q8 taps
impl Resampler {
    fn rebuild(&mut self, vc: u16, vr: u16);       // on resize, ~50µs
    fn run(&mut self, src: &Plane, dst: &mut Plane); // H then V pass, u8 out
}
```

## 5. Asset format

**Adopt SLPY v1 as digested, with two cuts and one swap.** Keep: RIFF-style chunks, temporal byte-delta + per-frame zstd blocks (the 4–6× ratio multiplier), per-plane subblocks (low tiers skip chroma), FIDX, TRLR truncation sentinel, keyframe interval 60, compress at -19 offline. Cut #1: the SQLite fallback — if index patching gets annoying we fix 30 lines, we don't adopt a second container. Cut #2: motion plane, dictionary training — v2 at best. Swap: store **Ex/Ey doubled-angle planes instead of a raw orientation plane** (grid-math digest is right, algorithms digest is wrong here): orientation is π-periodic and cannot be linearly resampled, while Ex/Ey run through the same separable resampler as everything else and coherence falls out free as |(Ex,Ey)|/E. Plane set (6): Y(L\*), E, Ex, Ey, H flags at 480×270 u8; C chroma at 240×135 RGB565. ~350 MB worst-case for 3 min is acceptable; nothing resolution-specific is baked in (requirement 4/6 honored: planes only, never glyphs).

## 6. Render loop

Per frame: (1) drain events; if resize flag → `TIOCGWINSZ`, recompute letterbox, `Resampler::rebuild`, async re-send `CSI 16t`, force full repaint (clear debounced 100 ms, math never debounced). (2) `AssetReader::frame(next)` — latest-frame-wins; if behind schedule, skip indices, never queue. (3) Resample 6 planes to grid. (4) Per cell: normalize Y by shot levels, hysteresis vs `prev_idx` (±0.35·step), edge test with temporal dual threshold → `EdgeLut`, else highlight, else base ramp; fg from C; scene-cut flag resets all hysteresis state. (5) `blit`: diff vs previous Cell grid, SGR run-length elision, tier color quantize, `?2026` wrap, one write. (6) `FrameStats`: if sustained write_ns > budget → downshift (harder color quantize → diff-only → effective-grid reduction).

**Budget, 300×80 (24 k cells), slow tier (SSH/tmux), 33 ms frame:**

| step | ms |
|---|---|
| decode + delta add | 0.3 |
| resample 6 planes | 0.6 |
| composite + hysteresis | 1.5 |
| diff + elide + quantize + assemble | 1.5 |
| write syscall (local PTY) | 0.5 |
| **CPU total** | **~4.5** |

Remaining ~28 ms absorbs link latency/terminal ingest. Hysteresis doubles as throughput control: static regions produce zero damaged cells, so diff output on SSH typically lands 20–60 KB/frame; if the pipe still blocks, frames drop and playback stays realtime.

## 7. Factory pipeline & iteration harness

Stages: **ingest** (ffmpeg → rgb24 480×270, fps normalize) → **shot detect** (histogram delta) → **features** per frame: L\* conversion; Scharr gradients → orientation smoothing → Ex/Ey/E; top-hat highlight + shadow flags → **temporal EMA** per plane → **per-shot levels** (p2/p98, smoothed) → **encode**. Pragmatist cut: full Kang ETF is overkill for v1 — use two passes of orientation-aware bilateral smoothing on the doubled-angle field; upgrade only if the flicker metric says so. All tunables (thresholds, EMA constants, ramp definitions, hysteresis δ) live in one `params.toml`.

Harness (`slpy-eval`), built for unattended sweeps:
- **Metrics** (JSON per run): structural fidelity — gradient correlation + SSIM between the source downsample and a grayscale re-render of the Cell grid via glyph coverage tables; **flicker** — mean glyph switches/cell/s on static shots; **edge P/R** vs source Canny; **bytes/frame** per tier from FakeBackend.
- **Golden tests**: `insta` snapshots of Cell grids at 80×24 / 206×58 / 320×90 × 3 palettes × 3 fixture frames; plus golden escape-byte streams from FakeBackend. Deterministic core makes these byte-exact.
- **Resize fuzzing**: `proptest` random (cols,rows) sequences, 1×1–500×200 storms; invariants: no panic, viewport ⊆ terminal, |log aspect error| bounded, rebuild <1 ms.
- **Perf gates**: `criterion` CI budgets — decode <1 ms, resample <1 ms, composite <3 ms, emit <5 ms @300×80; regression fails the build.
- **Loop**: `sleepy-factory eval --corpus clips/ --params params.toml` → metrics JSON + HTML contact sheet (source vs render, per-metric deltas vs baseline). Scripted parameter sweeps = hundreds of build-test-improve iterations with humans only reviewing contact sheets.

## 8. Milestones

- **M0 (demo first, ~week 1):** ffmpeg → luma-only uncompressed SLPY-lite → player with letterbox math, live resize, single ASCII ramp, truecolor fg, full repaint, kitty only. *Accept:* 3-min clip ≥24 fps on kitty; resize reflows next frame; Ctrl-C/SIGTERM restores terminal perfectly.
- **M1:** real SLPY v1 (delta+zstd, FIDX, seek); caps probe; truecolor/256/mono tiers; ?2026; one-write frames. *Accept:* asset ≤350 MB/3 min; seek <50 ms; correct tier auto-detected on kitty, xterm-256color, linux console; CPU <8 ms/frame.
- **M2:** eval harness online **before** quality work. *Accept:* goldens ×3 grids ×3 palettes in CI; 10 k fuzz cases clean; perf gates enforced; one command emits metrics + HTML report.
- **M3:** layers — edge (Ex/Ey LUT) + highlight, hysteresis, scene-cut reset, 6 palettes (ASCII/unicode-blocks × base/edge/highlight roles + mono-fallback). *Accept:* flicker ≤2 switches/cell/s on static shots; edge P/R ≥ tuned baseline; goldens re-approved deliberately.
- **M4:** slow tier — diff+elision emitter, drop-frame pacing, auto-downshift, tmux/SSH/ConPTY. *Accept:* effective 24 fps (with drops) over 20 ms-RTT SSH at ≤50 KB/frame average; tmux artifact-free; Windows Terminal plays.
- **M5:** hardening — 1 h resize-storm soak, 4 font coverage tables + conservative default, docs, static Linux/mac/Windows binaries. *Accept:* zero desync/leaks in soak; fresh-machine install-to-playback <2 min.

## 9. Top risks

1. **It's fast but ugly** (metrics pass, art fails). *Mitigation:* HTML contact sheets in every eval run; palette/ramp definitions are data (TOML) so taste iterations don't touch code; M3 gate includes human sign-off on a fixed review reel.
2. **SSH/tmux can't sustain framerate.** *Mitigation:* diff + elision + aggressive quantization measured via FrameStats; auto-downshift ladder; drop-frames pacing is correctness, not failure — demo M4 acceptance over a real WAN link.
3. **Temporal flicker ruins video.** *Mitigation:* three-layer defense (factory EMA, runtime hysteresis, scene-cut resets); flicker is a first-class CI metric from M2 so regressions are caught mechanically.
4. **Capability detection lies** (tmux strips, SSH drops COLORTERM). *Mitigation:* active query volley with DA1 sentinel + timeout, conservative fallback tier, `--tier`/`--palette` override flags, caps cache keyed on (TERM, TERM_PROGRAM, ssh, tmux).
5. **Real-world compression 2–3× worse than synthetic** → bloated assets. *Mitigation:* size is a CI budget from M1 on a real corpus; levers held in reserve: base res 384×216, keyframe interval 120, chroma quarter-res — all format-compatible knobs, no redesign.
