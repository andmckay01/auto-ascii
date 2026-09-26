ASCII warmth validation, Part A (over 7943758)

Letters retains the original colour in its dim background. ASCII previously
mixed pale shades toward gray and overbrightened ordinary lettering. The new
shade uses letters' exact tone-32 curve and 154/256 gain. Foreground uses its
gain with uniform gamut limiting; current-tone colour and bounded glyph
settling are preserved.

A picture cell is never a full pixel: it is printable ASCII 0x20–0x7E,
spaces are unshaded, background channels are <=154 and linear Rec.709
Y(background)<=0.375*Y(foreground as sent). Truecolor keeps every nonblack
scaled shade. The 256-color selection checks all 41 gray/cube candidates
below the ceiling against the quantized foreground's contrast and hue.
16/mono remain unshaded.

Four 200x56 crops, letters / previous ASCII / new ASCII rendered luma:

| Clip | Letters | 7943758 | New |
|---|---:|---:|---:|
| Architect 7700 | 51.82 | 58.85 | 52.19 |
| Terminator 660 | 14.56 | 19.43 | 14.86 |
| Dune 5520 | 40.02 | 47.56 | 40.29 |
| Interstellar 900 | 72.33 | 61.97 | 64.50 |

Background HSV saturation (letters / previous / new): 36.11/26.93/36.11%,
83.16/63.92/83.16%, 1.47/1.26/1.47%, 31.64/18.51/31.59%. Retaining the dark
1..7 shades restores 4.7–19.9% of the crop cells, while omitting exact black
avoids redundant explicit-black SGR. This is a continuity choice, not a
claim that all those shades are visibly distinct on every display.

At the unchanged Part A default 160, 120-frame windows at 200x56 plus
Dune/Interstellar at 500x140 have ASCII/pixels glyph-switch ratios
1.0115–1.1886. Exact background churn is 12.8795/cell/s vs letters 12.8616
at 200x56 (equal clip weighting). Counts include real motion and one-code
changes; they are not a perceptual flicker score.

128 zoom runs cover 40x12 through 500x140, odd sizes, grow/shrink, overlays
on/off and continuous/cold endpoints. Independent terminal replay covers
1,440 frames across four depths and three palettes, with no purity, cap,
hue, or screen mismatch. Actual UiRows boundaries exclude HUD rows;
1,440 HUD comparisons match across codecs. Maximum shade channels are
153 truecolor / 148 indexed; observed maximum luminance ratios are
0.3263 / 0.3594; maximum indexed hue separation is 28.88 degrees.

96 pixels/letters streams, 30 frames each, are byte-identical to archived
main 52f6740. The deliberate live shadow-lift dial label differs in 18 UI
cells per codec's dial frame; picture output is unchanged.

External evidence lives in `../compare/ascii-warm/`: README.txt,
huesat-final.txt, eight `*-final-3way.png` full/face comparisons and
`final-evidence/` metrics, replay streams, contact sheets and reproduction
sources. These are headless CoreText/JetBrains Mono 13px approximations
(8x17 cells on black), not Ghostty screenshots. Stills use ten preceding
frames; temporal measurements use the existing 120-frame method.
Interstellar remains 10.8% dimmer than letters because solid-block coverage
is excluded. Temporal glyph/half/edge history still differs from cold.

`scripts/eval.sh`: ALL GREEN, 100s (workspace 57s, clippy 2s, resize fuzz
2,000 cases 10s, perf 31s), foreground with no concurrent scratch work.
Optional video corpus was empty; embedded real-asset regressions ran.
