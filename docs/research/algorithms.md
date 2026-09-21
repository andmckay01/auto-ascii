<!-- ASCII conversion algorithms — produced by the ascii-engine-plan workflow, 2026-07-24; input to ../PLAN.md -->

# ASCII-Art Conversion Algorithms for Realtime Video — Research Digest

## 1. Luminance → glyph ramps

- **Ink coverage measurement (offline, once per font)**: rasterize each candidate glyph at ~64×128 px (matches ~1:2 cell aspect), coverage = Σalpha / (w·h). Perceived brightness on dark bg ≈ coverage in *linear light*; convert to L\* for perceptual spacing. Fonts differ (DejaVu vs Menlo vs Consolas ±15% on `#@%`); ship coverage tables for 3–4 common monospace fonts + one conservative default. Terminals can't be queried for font, so default table + optional config override.
- **Why naive ramps look muddy**: (a) coverage clusters in 0.2–0.5 — classic 70-char ramp has dozens of near-identical mid-grays and almost nothing between 0.5 and solid; (b) linear luminance→index ignores gamma — mapping sRGB values directly overweights shadows; (c) no contrast stretch: typical video occupies p10–p90 of range → everything lands mid-ramp; (d) structured glyphs (`X`,`#`) add texture noise that reads as dirt.
- **Ramp construction**: pick glyphs at ~uniform L\* intervals (not uniform coverage). For the *base* layer prefer low-structure glyphs (uniform subcell ink variance: `.:-=+%@` over `X$&`) — structured glyphs are reserved for the edge layer. Dedupe glyphs within ΔL\* < 3.
- **Normalization**: convert Y→L\*, then apply **per-shot auto-levels** (factory computes p2/p98 per shot, temporally smoothed, stored as metadata; runtime does `(L−lo)·inv_range`). Per-frame auto-levels cause brightness pumping = flicker; fixed global levels waste dynamic range. This is the 80/20 winner.
- **Cell sampling**: always sample a **2×4 tap grid per cell** (matches 1:2 cell aspect and braille geometry). From 8 taps you get: mean (base ramp), 2×2 quadrant pattern → block glyphs `▘▝▖▗▀▄▌▐▚▞`, 2×4 pattern → braille U+2800+ (256 glyphs), and vertical position of features (picks `‾` vs `-` vs `_`). Per-cell mean alone loses thin features; 8 taps costs almost nothing.

## 2. Structure / edges

- **Offline (factory)**: Scharr gradients (better rotational symmetry than Sobel) → **Edge Tangent Flow smoothing** (Kang et al., *Coherent Line Drawing*, NPAR 2007) of the orientation field — essential, kills orientation noise that causes glyph flip-flicker → XDoG (Winnemöller 2011/12) or hysteresis-thresholded magnitude for clean contour mask. Do NOT thin/skeletonize at asset resolution (breaks under resampling); store smoothed magnitude and let runtime max-pool.
- **Xu, Zhang & Wong, "Structure-based ASCII Art," SIGGRAPH 2010**: shape-matching with deformation tolerance (AISS metric); seconds per frame, offline-only. Steal the lesson — match line *direction + subcell position* with misalignment tolerance — implement as a cheap LUT, not optimization.
- **Orientation quantization**: 4 bins → `- / | \`; 8 bins with Unicode → `─ │ ╱ ╲` (U+2571/2), `‾ _` chosen by vertical subcell position, `┼`/`+` for junctions (low orientation coherence + high magnitude). Quantize offline to a coarse angle; runtime re-bins per glyph repertoire.
- **Realtime = lookups only**: sample planes at current grid, threshold, `edge_lut[orientation_bin][subpos]`. All filtering/ETF/temporal work is offline.

## 3. Layer model

- Layers back-to-front: **L0 base luminance** (smooth dense-fill ramp) → **L1 edge/contour** (directional glyphs) → **L2 highlight/specular** (sparse accents `· ° + *`, or ramp-top glyph forced to bright/bold color).
- **Composition = priority/override, never glyph blending** (one glyph per cell): `edge wins if edge_mag>T && base not near-white; else highlight if mask set && base dark-mid; else base ramp`. Color is orthogonal: fg color always sampled from source chroma regardless of winning layer; highlight layer may boost value/saturation.
- **Spectrum effect**: edges inject bright high-frequency strokes (a `/` is ~12% coverage but *reads* as a bright line), highlights extend perceived range above ramp top. Consequence: base ramp can be shorter and smoother (8–16 steps) without flatness, because structure no longer has to come from luminance quantization. This is the core argument for layers.

## 4. Ramp configs — ship 8 (brief's 3–10 is correct)

Keyed by charset tier × role; density band selects ramp length within a config. Full cross-product (3 tiers × 3 densities × 3 roles = 27) is over-engineering.

| # | Key | Ramp string |
|---|-----|-------------|
| 1 | ascii/base/coarse (<70 cols) | `" .:-=+*#%@"` |
| 2 | ascii/base/fine (≥70 cols) | `" .'\":;i!li+tfjrxnmwqpdbkhao*#MW&8%B@$"` → trim to 16: `" .,:;i1tfLCG08@"` |
| 3 | ascii/edge | orientation LUT: `-` `/` `\|` `\` + `_` `=` `+` `#` (junction) — not linear |
| 4 | ascii/highlight | `" .+*"` (3 intensity steps) |
| 5 | unicode/base | `" ·░▒▓█"` + quadrant blocks `▖▘▝▗▀▄▌▐` from 2×2 pattern for partial fills |
| 6 | unicode/edge | `─ │ ╱ ╲ ‾ _ ┼` (8-dir + junction) |
| 7 | unicode/detail (fine density + verified braille support only) | braille U+2800–28FF via 2×4 pattern; edge/texture layer only — braille reads gray, never use for solid fills |
| 8 | mono-fallback/base (16-color or no-color, Linux console) | `" .:coO8@"` (longer ramp compensates for absent color; CP437-safe) |

Truecolor tiers can shorten base ramps (color carries luminance); mono tiers need the longest ramps.

## 5. Temporal coherence

- **Flicker causes**: luminance sitting on a ramp-step boundary; per-frame auto-levels pumping; edge magnitude oscillating around threshold; orientation flipping between adjacent bins; dither re-seeded per frame (crawl); source sensor noise.
- **Fixes**:
  - Glyph-index **hysteresis**: keep `prev_idx[cell]`; switch only when value crosses boundary ± 0.35·step_width. ~Free, eliminates most flicker.
  - Edge **dual-threshold in time**: turn on at T_hi, stay on until below T_lo (Canny-style, temporal).
  - Orientation hysteresis: require angle to move > bin_width/2 + 8° to change glyph.
  - Factory: temporal EMA / flow-aware bilateral filter on all planes; **scene-cut flags in metadata → runtime resets all hysteresis state** (else ghosting across cuts).
  - Dither: if used at all, fixed **screen-space** Bayer/blue-noise indexed by cell (col,row) — stationary pattern never crawls; crawl comes from content-space or per-frame-random dither. 80/20: skip dithering, hysteresis + 16-step ramps look better.

## 6. Precomputed feature planes (per frame, at fixed asset resolution ~480×270)

| Plane | Meaning | Depth | Justification |
|---|---|---|---|
| Y | luminance as L\* (perceptual) | 8-bit | mandatory; L\* storage makes runtime mapping one multiply-add |
| E | ETF-smoothed edge magnitude (unthinned) | 8-bit | runtime max-pools over cell footprint so thin edges survive any grid size |
| O | edge orientation, 0–179° scaled; 255 = invalid | 8-bit | full angle lets runtime re-quantize to 4 or 8 bins per tier; 4-bit bins would lock the repertoire |
| H | flags: bit0 highlight, bit1 deep-shadow | 8-bit (2 used) | offline top-hat/percentile detection is far more stable than runtime thresholding |
| C | chroma/RGB at half res (240×135) | RGB565 (16-bit) | color tiers all derive from this; half-res is invisible at cell granularity |
| M (optional, v2) | motion magnitude from flow | 8-bit | modulates hysteresis width (tighter in motion, wider in statics); skip for v1 |
| meta (per shot) | levels lo/hi, cut flag | — | powers auto-levels + hysteresis reset |

Raw ≈ 6 B/px ≈ 23 MB/s at 30 fps before compression — planes are smooth/sparse, compress well (this feeds the file-format track).

## 7. Realtime mapping pseudocode + budget

```
for each visible cell (precomputed cell→plane sample offsets, rebuilt on resize):
  taps[8] = Y at 2x4 grid            # 8 loads
  y  = avg(taps); e = max4(E taps); o = O[argmax]; h = any(H taps)
  n  = clamp((y - shot_lo) * shot_inv_range)
  idx = hysteresis(n, prev_idx[cell])            # 1 cmp vs boundary±δ
  if e > T_on or (was_edge[cell] and e > T_off):
      g = edge_lut[bin(o)][subpos(E taps)]       # table lookup
  elif h and idx < HI_CUT: g = highlight_glyph(idx)
  else: g = ramp[idx]
  fg = color_map(C sample)                        # tier LUT: truecolor/256/16
  if (g,fg) != cellbuf[cell]: mark damaged        # diff for backend
```

- **Cost**: ~12 plane loads + ~30–50 arithmetic/branch ops ≈ 60–90 ops/cell. 300×80 = 24k cells × 30 fps = 720k cells/s → ~50–65M ops/s ≈ **<2 ms/frame, under 5% of one modern core**. Selection is never the bottleneck — escape-sequence emission and terminal ingest are, which is why the damage-diff at the end is the most performance-critical line, and why glyph hysteresis doubles as a throughput optimization (fewer damaged cells on slow/SSH backends).
- Resize: rebuild the cell→plane offset LUT (O(cells), sub-millisecond); no asset reprocessing — planes are resolution-independent by construction.
