Use one shared maximum and default of **128**, in steps of 16 from 0.
The readout explicitly says “default, max”; 0 says “floor”. Old saved
player values above 128 clamp; params.toml is pinned to 128 and factory
configuration rejects larger values. There are nine normal dial positions.

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
1.248x, techno 6 1.207x; held-out windows up to Darth Vader 1.352x), and
settling needed an exactly repeated tone, which video noise almost never
produces, so glyphs within the band stayed stale until a big change.

The hold now has three parts. Changes beyond 5/8 h (80 tone units at 128)
are immediate. Nearer, a smoothed tone moves a quarter of the way toward
each input; when it stays more than h/8 (16) units to one side of the held
tone, on another ramp step, for min(1+h/4, 32) frames, the glyph adopts
the smoothed tone. Floor crossings keep their four-frame rule. Adopting the
smoothed value, not a fresh noisy sample, stops the settle from re-arming;
the wider immediate band trades motion switches for settling. A steady tone
settles within 41 frames onto its own step or within h/8 of it.

Measured on all 21 assets in auto-ascii-run (Codex's windows: the four
standard clips plus the middle of each other asset), plus two held-out
windows per asset at a quarter and three quarters. Each is 90 frames from a
cold start at 80x24 and 200x56, default 128 against pixels at 128.
"Stuck" counts cells whose glyph differs from a cold render on each of the
last 45 of 180 continuous frames; "far" requires two or more ramp steps.

| Windows | Worst ratio before → after | Stuck before → after | Far before → after | Letters / pixels stuck |
|---|---:|---:|---:|---:|
| 21 main | 1.280 → 1.163 | 6.94% → 2.99% | 2.12% → 0.24% | 8.14% / 3.06% |
| 42 held-out | 1.352 → 1.108 | 6.27% → 2.89% | 1.78% → 0.20% | 8.23% / 3.38% |

At 500x140 the seven worst windows go from 1.166–1.324x to 0.644–1.131x.
The cost is lag in motion. At three checkpoints, cells two or more ramp
steps from a cold render rise from 13.6% to 18.5% (main windows; any glyph
difference 37.7% to 39.1%): a fade now moves the glyph in fewer, larger
steps, up to about 40 frames late, while nothing stays far from its tone for
good. A 1/2 or 9/16 band lags less (16.4% / 17.8%) but reaches 1.186x /
1.173x on the Death Star window and fails its test excerpt (1.215x /
1.205x); a second, smoothed-tone jump path at 7/16 gained little (16.5%)
at 1.184x. About 40% of Death Star's ASCII switches involve edge strokes,
which pixels does not draw, so the tone hold carries the whole margin.

Per-window numbers, the search over about 850 hold variants and its probes
are in compare/ascii-hold. A 12-unit margin or a 24-frame wait reduced
"stuck" further but broke 1.2x on the Architect or Death Star. Widening the band under the old exact-repeat settle passed 1.2x
but left more cells stuck (9.2% at 7/16). Tolerance settles that adopted a
raw sample re-armed on noise and reached 1.24–5.5x on the static Architect.

Five new temporal tests fail at b890cea and pass now: excerpts of the
Death Star, Falcon, techno 6 and Darth Vader windows bound switching at 1.2x
pixels, and the Architect excerpt bounds glyphs held two or more steps from
cold for 45 frames at 1% of cells (was 1.7–1.9%).

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
without changes. Startup/cycled and startup/resize/post-dial regressions,
the existing real-asset <=1.2x flicker gate, cap/purity tests, saved-setting
clamping and the new maximum/chatter regressions pass. The maximum now
coincides with default, so dial round-trip tests turn inward at that stop
and assert that further upward presses do not reset temporal state.

Final `scripts/eval.sh`: ALL GREEN, foreground, 103 seconds: workspace
60s, clippy 2s, resize fuzz x2000 10s, perf 31s. No scratch jobs ran beside
the perf gate. Optional external video corpus was empty; embedded real
asset regressions ran. Main and origin/main both resolve to 52f6740.
