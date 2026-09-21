<!-- Viewport math & realtime resampling — produced by the ascii-engine-plan workflow, 2026-07-24; input to ../PLAN.md -->

# VIEWPORT MATH & REALTIME RESAMPLING — DIGEST

## 1. Grid derivation (letterboxed 16:9 in cells)

**Inputs:** terminal `cols × rows`; cell aspect `a = cell_h_px / cell_w_px` (default **2.0**).
- Refine `a` at startup + on every resize: send `ESC[16t` (XTWINOPS); reply `ESC[6;h;w t` gives cell px size → `a = h/w`. Also try `TIOCGWINSZ.ws_xpixel/ws_ypixel` (treat 0 as unknown), and `ESC[14t` (reply `ESC[4;h;w t`, text area px) ÷ char grid. Timeout 50–100 ms → fall back to 2.0. Re-query on resize (font zoom changes cell px and fires SIGWINCH).

**Formula.** Target pixel aspect 16:9 means `(Vc·w) / (Vr·h) = 16/9` → `Vc = R·Vr` where `R = (16/9)·a` (a=2 → **R = 32/9 ≈ 3.5556**).

```
r1 = round(cols / R); cand1 = (cols, r1)      valid if r1 <= rows   # width-limited
c2 = round(rows * R); cand2 = (c2, rows)      valid if c2 <= cols   # height-limited
pick valid candidate minimizing |ln( (c/(r*a)) * 9/16 )|            # log aspect error
pad_left = (cols-c)>>1 ; pad_top = (rows-r)>>1   # remainder goes right/bottom
```

**Worked examples (a=2):**
| term | limited by | viewport | pads (L/R/T/B) | actual px aspect |
|---|---|---|---|---|
| 80×24 | width | **80×23** | 0/0/0/1 | 1.739 (tie w/ 80×22 @1.818; prefer more rows) |
| 213×58 | height (r1=60>58) | **206×58** | 3/4/0/0 | 1.776 |
| 320×90 | exact | **320×90** | 0 | 1.7778 exact |

**Minimum size:** if best viewport < 32×9 cells, render centered "enlarge terminal" placeholder; clamp Vc,Vr ≥ 1 everywhere (no div-by-zero).

## 2. Asset sampling — comparison & recommendation

Source: N feature planes at fixed **480×270 u8**. Cell (cx,cy) maps to source rect `[cx·SW/Vc, (cx+1)·SW/Vc) × [cy·SH/Vr, (cy+1)·SH/Vr)` (fractional edges).

Costs for **300×80 grid ← 480×270** (kx≈1.6, ky≈3.4, 24 000 cells), per plane per frame:

| approach | resize rebuild | per-frame/plane | ×5 planes | verdict |
|---|---|---|---|---|
| (a) naive per-cell 2-D box | none | ~360K MAC **+ per-cell rect/weight math every frame** | ~1.8M | wasteful, ugly inner loop |
| **(b) separable precomputed tables** | **~50 µs, <4 KB** | H-pass 300·270·2.6≈210K + V-pass 300·80·4.4≈106K ≈ **316K MAC** | **~1.6M ≈ 0.3–0.5 ms scalar** | **RECOMMENDED** |
| (c) per-frame SAT | none | build 260K adds (u32) + sample 24K·8 ≈ 450K | ~2.3M + 4× mem traffic (u32 SAT 518 KB/plane) | pays build cost every frame even when grid idle; integer-rect only |
| (d) mipmap pyramid in asset | +33% asset size | ~8-tap trilinear ×24K ≈ 200K | factory + format complexity | unnecessary — see below |

**Why (b) wins:** box resample is separable; H-pass ops ≈ Vc·kx·SH ≈ **SW·SH regardless of grid**, so heavy downscale never blows up (80×22 grid: 80·270·6.5 + 80·22·12.7 ≈ **162K/plane — cheaper, not costlier**). That kills the only argument for mipmaps (d). Tables rebuilt only on resize. Total ~1.6M MAC/frame ≈ 48M/s @30 fps → well under 0.5 ms; render bottleneck will be ANSI emission, not sampling.

**Pseudocode:**
```
struct Tap1D { u16 src_start; u8 ntaps; u16 w[MAXTAP]; }  // Q8 weights, sum = 256
build_axis(taps[], out_n, src_n):                          // called on resize only
  for i in 0..out_n:
    lo = i*src_n/out_n ; hi = (i+1)*src_n/out_n            // float
    for each src px j overlapped: w = coverage(j, lo, hi)  // fractional at ends
    normalize w to sum 256

resample_plane(src u8[SH][SW], tmp u16[SH][Vc], dst u8[Vr][Vc]):
  for y in 0..SH: for i in 0..Vc:                          // H-pass
    acc u16 = Σ src[y][s+k] * wx[i].w[k]                   // ≤255·256 < 2^16 ✓
    tmp[y][i] = acc                                        // keep Q8, don't shift
  for j in 0..Vr: for i in 0..Vc:                          // V-pass
    acc u32 = Σ tmp[r+k][i] * wy[j].w[k]
    dst[j][i] = acc >> 16                                  // Q8·Q8 → >>16
```

**Upscale case** (Vc > 480 — huge terminal/tiny font): tables degrade naturally to 1–2 taps with linear weights = bilinear interp. Same code path, no branch; output is slightly soft, acceptable. Optional cap at 2× source width if mush offends.

## 3. Edge-orientation plane

Angles are **π-periodic** (orientation, not direction) — never average raw angles (0° and 179° must agree, naive mean gives 90°). Two valid schemes:
- **RECOMMENDED — doubled-angle vector (structure-tensor trick):** factory emits two planes `Ex = mag·cos(2θ)`, `Ey = mag·sin(2θ)` (i8 stored as bias-128 u8). These are **linear** → run through the exact same separable resampler as every other plane. Per cell: `θ = atan2(Ey,Ex)/2` via 256-entry LUT, or skip atan2 entirely and quantize to 4–8 edge glyphs (`| / — \`) by sign/comparison tests. **Coherence = |(Ex,Ey)| / resampled_mag**: near 0 → edge directions cancelled inside cell → suppress edge layer. Free conflict handling, zero special-case sampling code.
- Alternative (if factory quantizes θ to K=8–16 bins): per-cell magnitude-weighted histogram over the rect, argmax bin. Cost ≈ one extra plane resample but non-separable; only pick if asset format already bins.

Plane set (5): luma, edge-mag, Ex, Ey, highlight.

## 4. Resize path

```
SIGWINCH handler: atomic_store(resize_flag)          # signal-safe, nothing else
render loop, top of frame:
  if resize_flag:
    ioctl(TIOCGWINSZ) → cols,rows (+px dims if nonzero → refine a)
    recompute viewport + pads; rebuild 2 axis tables  # ≈50 µs total
    if now - last_clear > 100 ms: full clear + redraw letterbox bars
    optionally re-send ESC[16t (async; apply answer next frame)
  render frame on current grid
```
- Table rebuild is so cheap (**<1 ms budget, ~50 µs actual**: Σtaps ≈ (SW+Vc)+(SH+Vr) ≈ 1.2K weight entries) that you rebuild **immediately per event** — debounce (~100 ms) applies only to the flicker-causing full-screen clear, not to the math. Next frame after the event renders on the new grid.
- Shrink mid-drag: viewport formula always fits current rows/cols, so no clipping logic needed.

## 5. Data layout & SIMD

- **SoA**: one contiguous row-major u8 plane per feature; per-frame source payload 5×129.6 KB = 648 KB (fits L2). Output cell planes u8 SoA, Vc·Vr each (~24 KB) → glyph-selection pass streams sequentially.
- One reusable **u16 H-pass buffer** `Vc×SH` (300×270×2 = 162 KB), shared across planes.
- Fixed point: Q8 weights (sum 256/axis), u16 H-acc (255·256 < 2¹⁶), u32 V-acc, final `>>16`. No floats in the hot loop.
- **Explicit SIMD: no (80/20).** ~1.6M MAC/frame is <0.5 ms scalar; plain `u8×u16→u32` loops with fixed trip counts autovectorize under `-O2/-O3`. Terminal write throughput + escape-sequence diffing dominates frame time. Revisit only if profiling shows resample >2 ms (e.g., 8-bit builds on Raspberry Pi — then a single SSE2/NEON widening-multiply H-pass is the one loop to hand-vectorize).

## 6. Budget summary

| operation | when | cost |
|---|---|---|
| viewport derivation | resize | ~20 arithmetic ops |
| axis weight tables | resize | ~50 µs, <4 KB |
| resample 5 planes, 300×80 | every frame | ~1.6M MAC ≈ 0.3–0.5 ms |
| orientation decode (LUT/sign-test) | every frame | 24K cells × ~5 ops ≈ 0.05 ms |
| SIGWINCH → new grid rendered | next frame | <1 ms end-to-end |

Frame budget @30 fps = 33 ms → sampling uses <2%; leaves headroom for glyph compositing and terminal I/O.
