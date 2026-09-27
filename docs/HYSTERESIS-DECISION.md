Use a recommended default of **128**, in steps of 16 from 0, with the shared
dial and params.toml maximum at 255. The readout says “default” at 128 and
“floor”/“max” at the ends. Saved player values and factory configuration
clamp/reject only above 255. Values above 128 are allowed for fast-paced
videos or video types that benefit from high hysteresis, but can visibly
drift or smear.

**Pixels and letters change at default:** the default moves from 160 to 128.
Their rendering algorithms are unchanged, so equal explicit hysteresis
values preserve their previous output. The lower default intentionally
trades slightly more glyph switching for less retained picture history.

128 is two notches below the earlier pixels mismatch rise around 160,
and avoids the severe bright-core loss at 176–192 and above. Letters has
no sharp artifact cliff; 128 is a conservative retention/stability choice.
After the current-tone fix and Part A, ASCII no longer accumulates the old
shade-luma error, but wider glyph bands still retain contours. Its luma
error grows through 144–160. This cap has margin below that range; it does
not promise no temporal difference or eliminate half/edge/orientation state.

Simply lowering the dial was insufficient: the unchanged embedded Architect
regression measured 3,002 ASCII switches against pixels' 2,365 (1.269x).
Lengthening stable-target settling alone gave 2,935 (1.241x). The first
calibration used a 5/16 band (40 tone units at 128) and settled only after
32 frames of a bit-identical tone. Review showed both halves failing on
real video, so ASCII's glyph hold was recalibrated (next section).

**ASCII glyph hold, recalibrated.** Two problems at b890cea: glyph switching
exceeded 1.2x pixels on other assets (Death Star 1.280x, Millennium Falcon
1.248x, techno 6 1.207x; other windows up to Darth Vader 1.495x), and
settling needed an exactly repeated tone, which video noise almost never
produces, so glyphs within the band stayed stale until a big change.

The hold now has five parts. Changes beyond 5/8 h (80 tone units at 128)
are immediate. In busy cells that band narrows, down to h/4 (32): a per-cell
activity level (0-15, in ascii's spare flag bits) tracks a quarter of how far
each input lands from the smoothed tone, rising an eighth of the way per frame
(rounded up) and falling at most one level per frame; each level above 9
takes 3h/16 off the band. Nearer changes feed a smoothed tone that moves a
quarter of the way toward each input. When it stays more than h/8 (16) units
to one side of the held tone, on another ramp step, for min(1+h/4, 32)
frames, the glyph adopts it. When it is nearer than h/8 but at least 4 units
inside another ramp step, the glyph adopts it after min(h/2, 63)+1 frames
(64 at 128), so a steady picture converges. Floor crossings keep their
four-frame rule. Adopting the smoothed value, not a fresh noisy sample, stops
settling from re-arming on noise. Every steady tone ends on its own ramp step
within 74 frames, except a tone within 4 units of a step boundary, which may
keep the neighbouring glyph (ink within about a third of a step).

The first version of this rule (9f316d9) had neither the activity band nor
the convergence path: a smoothed tone within h/8 kept its old glyph forever
(steady 142 -> 158 kept `x`, three steps from cold `e`), and fast-changing
areas such as Interstellar's sphere lagged into ring-like glyph contours.
The 4-unit clearance matters: converging at 3 units pushed the embedded
Architect excerpt to 1.204x, because a static, noisy scene corrects cells
that straddle a boundary around frame 64 of a cold-started window.

Measured on all 21 assets in auto-ascii-run: the four standard clips at their
usual frames, and each asset's quarter, middle and three-quarter points, both
as window end and as window start (130 windows). Each is 90 frames from a cold
start at 80x24 and 200x56, default 128 against pixels at 128. "Stuck" counts
cells whose glyph differs from a cold render on each of the last 45 of 180
continuous frames; "far" requires two or more ramp steps. "Lag" is far cells
after 121 continuous frames; "residual" is after 600 more renders of that
same frame.

| Version | Worst ratio | Stuck | Far stuck | Lag (far) | Residual any / far |
|---|---:|---:|---:|---:|---:|
| b890cea | 1.495x | 6.31% | 1.79% | 12.1% | 1.96% / 0.010% |
| 9f316d9 | 1.298x | 3.11% | 0.18% | 16.8% | 13.3% / 1.19% |
| final | 1.298x | 2.69% | 0.10% | 14.6% | 7.44% / 0.017% |

Letters and pixels are stuck on 8.34% and 3.01%. The remaining residual is
mostly one step within 4 units of a boundary plus the half/edge gates
(b890cea's exact-repeat rule also converged on identical frames). Dune warmed
to f5520 at 200x56 then repeated 600 times differs from cold on 7.5% of cells
and on none by two steps, from 18.1% and 4.61% at 9f316d9.

Flicker: 257 of 260 measurements are <=1.2x pixels. The exceptions are
high-motion windows where letters also exceeds 1.2x: Dune frames 4741-4830
(ascii 1.298x / 1.262x at 80x24 / 200x56; letters 1.275x / 1.291x) and Darth
Vader 1186-1275 at 80x24 (ascii 1.210x, letters 1.235x). Against
max(1.2x pixels, letters) the worst is 1.018, Dune 4741-4830 at 80x24. The
worst window that letters does not exceed is Death Star 526-615 at 80x24,
1.184x. At 500x140 the eleven worst or standard windows reach at most 1.187x.

Motion lag remains the cost of the flicker budget. After 121 frames, cells
two or more steps from a cold render: Interstellar f900 at 200x56 14.0% at
b890cea, 36.9% at 9f316d9, 25.6% now; Dune f5520 26.6%, 36.5%, 35.1%;
Terminator f660 9.6%, 18.3%, 16.9%; Architect f7700 5.0%, 3.9%, 3.8%. The
activity band recovers part of Interstellar's loss; Dune's face changes
slowly enough to stay below the activity threshold. The binding windows are
static and noisy (Death Star, Architect, the Death Star test excerpt at
1.198x): a uniform 1/2 or 9/16 band lags less but fails there (1.215x /
1.205x on the excerpt), as do an immediate jump once the smoothed tone passes
40-56 units (1.21-1.31x), faster counting while it keeps moving away
(1.21-1.59x) or by deviation size (1.21x), and lower activity thresholds
(1.20-1.22x on the excerpt). About 40% of Death Star's ASCII switches
involve edge strokes, which pixels does not draw.

Per-window numbers, the search over about 1,050 hold variants and its probes
are in compare/ascii-hold (final-flicker.md lists every window).

Seven new temporal and core tests fail at b890cea or 9f316d9 and pass now:
excerpts of the Death Star, Falcon, techno 6 and Darth Vader windows bound
switching at 1.2x pixels; the Architect excerpt bounds glyphs two or more
steps from cold for 45 frames at 1% of cells, and after 80 repeats of its
last frame at 0.5% (3 and 2 cells; 9f316d9 and 3cef5ec fail); core tests pin
convergence inside the margin and the activity band. Startup versus codec
cycling and startup/resize versus every dial round trip are reset
invariants, not regressions for the fix: both sides start cold, so they pass
on 3cef5ec too.

Keep hysteresis for **all three codecs**. Zero only removes the dial-controlled
part, not fixed edge/orientation/half/fill gates. Four-clip means at 200x56
(the ascii row predates the recalibration above):

| Codec | Glyph/s at 0 | At new default | Off/default | End luma error at 128 | At old max 255 |
|---|---:|---:|---:|---:|---:|
| pixels | 2.244 | 1.360 | 1.65x | 4.439 | 11.086 |
| letters | 4.041 | 1.475 | 2.74x | 0.936 | 1.485 |
| ascii | 4.678 | 1.493 | 3.13x | 0.803 | 1.331 |

The old-max column uses Part A before the glyph-hold calibration.
These counts include real motion as well as unwanted switching. Keep pixels'
ramp hold to suppress threshold chatter; keep letters' tone hold because
turning it off greatly increases glyph changes; keep ASCII's bounded tone
hold because colour continuity alone does not stabilize the character ink.

At the first 128 calibration (b890cea), against pixels at the SAME default:

| Clip / grid | ASCII glyph/s | ASCII/pixels | Letters/pixels | ASCII bg/s | Letters bg/s | ASCII end luma MAE |
|---|---:|---:|---:|---:|---:|---:|
| interstellar / 500x140 | 1.985 | 1.111x | 1.118x | 15.536 | 15.537 | 0.870 |
| architect / 200x56 | 0.142 | 1.161x | 1.038x | 6.270 | 6.235 | 0.577 |
| terminator / 200x56 | 2.587 | 1.114x | 1.066x | 13.922 | 13.875 | 0.194 |
| dune / 200x56 | 1.235 | 1.040x | 1.082x | 15.356 | 15.357 | 1.558 |
| interstellar / 200x56 | 2.009 | 1.111x | 1.114x | 15.982 | 15.979 | 0.881 |
| dune / 500x140 | 1.261 | 1.044x | 1.084x | 14.950 | 14.950 | 1.548 |

Worst ASCII ratio 1.1615x; target <=1.2x. At 112 the worst
same-value ratio is 1.2102x, so 128 is the lowest shared default notch
that passes all six measurements after calibration.
Exact background churn includes one-code changes and is not perceptual flicker.

The original method is preserved: 120 continuous/cold frame pairs ending at
Architect 7700, Terminator 660, Dune 5520 and Interstellar 900; all 17 original
notches, all clips at 200x56 and Dune/Interstellar at 500x140, truecolor/unicode,
no overlays, shot resets preserved. Part A's all-codec sweep is retained under
ascii-warm/final-evidence. Final ASCII was remeasured at all 17 notches after
calibration; unchanged pixels/letters measurements are reused at equal values.
final-evidence/per-clip.csv and summary.csv use pixels at 128 as denominator;
original REPORT.md/summary.csv remain untouched historical measurements.
Luma error uses the original Ghostty-approximating masks and encoded Rec.709
cell averages, not the Rec.601 rendered-pixel warmth-table definition.

Before/after sheets: ascii-max-before-after-200x56.png and
ascii-max-before-after-500x140.png, Part A at old max 255 versus final at 128.
Raw endpoint grids and frame tables preserve reproducibility. Approximation
uses JetBrains Mono 13px, 8x17 cells, black background; no Ghostty GUI.

Compatibility and replay after Part B:
1,824 pixels/letters streams / 16,416 frames match archived
main 52f6740 byte-for-byte at explicit equal hysteresis values: every raw
value 0..128 in truecolor/unicode, plus all four depths and three palettes
at all nine notches. The 96-stream resize/gap/overlay suite also passes at
explicit 128. The known shadow-lift label exception remains 18 UI cells
on each codec's dial frame, checked separately. Default 160->128 is an
intentional behaviour change; equal-value identity does not hide it.

Independent cap/purity replay: 1440 frames, zero errors; UiRows
HUD comparison: 1,440 checks, no differences. The 128 default zoom runs
check 226,774,984 non-UI cells over 15,488 frames, 40x12..500x140, odd sizes,
grow/shrink and all overlays on/off. All 60 additional 90-frame flicker
windows pass: ASCII worst 1.1661x, letters 1.0884x pixels at 128.

The default artifact found and fixed was glyph chatter from reducing the
hold range without recalibrating ASCII. Its before/after endpoint PNG is
../ascii-zoom/crops/architect-200x56-default-chatter-before-after.png;
temporal regressions establish the improvement, not a single still.
Warm/cold glyph texture differences remain intentional; no new stale-shade,
blotch, split-cell seam, startup/cycle, resize or HUD-boundary failure was
found in the final replay and inspected endpoint comparisons. GUI/GPU
resize races were not tested (no Ghostty GUI was launched).

Six codec goldens intentionally change glyph rows with the new default;
all six colour hashes are unchanged. Eval's synthetic grid snapshots pass
without changes. Startup/cycled and startup/resize/post-dial reset
invariants, the existing real-asset <=1.2x flicker gate, cap/purity tests, saved-setting
clamping and the new maximum/chatter regressions pass. The maximum now
coincides with default, so dial round-trip tests turn inward at that stop
and assert that further upward presses do not reset temporal state.

Final `scripts/eval.sh`: ALL GREEN, foreground, 103 seconds: workspace
60s, clippy 2s, resize fuzz x2000 10s, perf 31s. No scratch jobs ran beside
the perf gate. Optional external video corpus was empty; embedded real
asset regressions ran. Main and origin/main both resolve to 52f6740.
