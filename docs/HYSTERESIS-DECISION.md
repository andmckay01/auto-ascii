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

**ASCII glyph hold, recalibrated.** The 32-frame and 64-frame settles and the
four-frame floor rule in this section are the pre-2026-11-18 hold. The fast
path and floor crossings are superseded by the ASCII confirm below. The
activity jump band, the smoothed tone, the 4-unit boundary exception, and the
long settle for a crossing inside an eighth of the dial remain.

Two problems at b890cea: glyph switching
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
(Superseded: the dial/params maximum was later reopened to 255, with 128
staying the recommended default and max; see the top of this file.)

Final `scripts/eval.sh`: ALL GREEN, foreground, 103 seconds: workspace
60s, clippy 2s, resize fuzz x2000 10s, perf 31s. No scratch jobs ran beside
the perf gate. Optional external video corpus was empty; embedded real
asset regressions ran. Main and origin/main both resolve to 52f6740.

**Pixels ramp confirm, 2026-11-18.** The shared dial stayed a memoryless
Schmitt trigger on the pixels ramp. At the default 128 (half a step of the
6-glyph ramp, about 21 luma) a slowly moving face sat on the old shade,
then every cell in the region crossed on the same frame. Foreground colour
was already tracking the current frame, so the shade popped as a slab.
Hysteresis 0 removes the slab and flickers about 50% more, worst on holds
and dark scenes, so the dial stays.

Pixels now keeps a quarter of the dial as a forever band (at 128, about 5
luma). A one-step crossing past that band waits for the frame slot of a
fixed 16×16 interleaved-gradient phase. The age floor is
clamp(2·dial/128, 1, 4). At the default 128 that floor is 2, so the wait
is 2 to 4 frames. The clamp's upper bound of 4 is not reached: dial 255
floors at 3, and with the phase slot the wait there is 3 to 5 frames.
The phase is not scrolled. The frame counter steps through 0, 1, 2. A
wrapping byte would repeat residue 0 every 256 frames (`256 % 3 == 1`).
A gap of two or more steps snaps to plain quantisation on the frame it
appears. The quarter-dial band is a deliberate permanent deadband: a target
inside it never leaves the held shade, so the bounded wait applies only to a
one-step crossing that stays eligible while the frame clock ticks once per
frame. Deep shadow targets the
darkest step through the same rule, which is what was still stamping
middle-dot regions to space in one frame after the ramp confirm alone.
At dial 0 the pixels hold is off and the quantiser is plain
`floor(n·len/256)` with no history. Saved values and the 0..255 step-16 dial
are unchanged. `CellState` stays 5 bytes; the candidate and age bytes hold the
pending step. `HysteresisState` carries one frame clock, stored as two `u8` fields
(`issued` and `next`).

Measured on the Dune arena clip (wrestle, 841 frames) and the face
close-ups (faces, 541 frames), 192×40, unicode ramp, against hysteresis 0.
"Late blob" is the largest 4-connected set of cells whose glyph changed
while the hysteresis-0 glyph did not. Cuts are masked.

| | glyph chg/frame | late blob max | late blob p99 | rev/frame | stale p90 | bright err vs h0 |
|---|---:|---:|---:|---:|---:|---:|
| wrestle before | 482.3 | 193 | 87.2 | 5.425 | 9 | 7.199 |
| wrestle final | 509.7 | 9 | 4.0 | 4.814 | 4 | 4.592 |
| faces before | 175.4 | 109 | 41.2 | 3.432 | 23 | 4.285 |
| faces final | 221.2 | 4 | 4.0 | 3.362 | 6 | 1.961 |

Stale-run p90 matches the short-confirm prototype (4 and 6). Late blobs
fell from 193 and 109 to 9 and 4, under the phase-class size of the noise
tile. Reversals stayed under the previous pixels figures. Brightness error
against hysteresis 0 dropped 36% and 54%. Glyph changes on wrestle are
1.06× the old pixels path. Faces are 1.26× (221 against 175): the cells
that used to sit stale now step, and a 1.15× cap would mean keeping some
of that lag. A constant tone plus or minus 4 luma changes no glyphs.

Letters is unchanged. It would need its own confirm in place of its tone
deadband. ASCII's candidate and age bytes now share pixels' phase slot on
the fast path; that calibration is the next section. The 1.2× pixels flicker
gates were re-measured against this pixels confirm. Death Star, Architect,
techno 6 and Darth Vader stay inside 1.2×. Falcon does not; the ratios are
with the ASCII confirm below.

**ASCII confirm, 2026-11-18.** The recalibrated hold still released a whole
region together. A face moved, the smoothed tone sat inside the activity
band, and after the 32-frame or 64-frame settle a slab of characters adopted
the new step on one frame. Edge strokes and orientation bins were not that
timer. They are Schmitt gates, and the late slabs on the face clip were ramp
glyphs, not `=-_|/\X`.

The fast path and floor crossings now use the same phase slot as pixels. A
smoothed tone more than an eighth of the dial from the held tone, on another
ramp step, adopts the current sample when its age reaches
`clamp(2·dial/128, 1, 4) + 1` and `frame % 3` equals that cell's
interleaved-gradient phase mod 3. At the default 128 the pixels floor is 2
and this confirm adds one frame, so the wait is 3 to 5 frames. Dial 255
reaches the clamp (base 4, wait 4 to 6). Floor crossings use the same short
confirm and adopt the current sample, which is 0 below the floor. A smaller
crossing that is at least 4 units inside the next step still waits until its
count reaches `min(dial/2, 63)` (64 frames at the default), releases only on
that same slot, and adopts the smoothed tone. The extra fast-path frame is
what puts Millennium Falcon back under 1.2× pixels. The pixels floor alone
was 1.23× there; adopting the smoothed tone instead of the current sample
made that window worse. The phase slot is what removes the slab. Lengthening
the timer does not reassemble it.

Within 4 units of a boundary the neighbouring glyph stays. The activity
jump band is unchanged: a jump wider than the activity band snaps. That is
an activity-band rule, not the pixels two-step rule, so a move inside the
band can cross two or more ramp steps and still wait out the confirm. At
dial 0 the new hold is off (`held_tone` returns the target, capped at 254),
but the floor selection still reads history, so ASCII output at dial 0 is
not wholly stateless. `CellState` stays 5 bytes. Edge and orientation glyphs are
not given a second timer.

Three holds were rendered on this worktree at dial 128, 192×40, palette
auto, against ASCII at hysteresis 0. Brightness is Menlo ink coverage.
"Late blob" is the largest 4-connected set of cells whose character changed
while the hysteresis-0 character did not. Quiet changes are character
changes on cells whose hysteresis-0 character is stable over 8 frames. Cuts
are masked. Decay2 is the shrinking band from the lane-d prototype,
re-rendered here and byte-identical to that lane's dumps. Motion is this
confirm with the dial scaled by a signed 3×3 mean of tone drift (gain 128
in 1/256 luma, noise floor 64, radius 1, smooth 8). It is not shipped:
reversals rose past the cap, and the planes were removed.

| clip | hold | chg/frame | late blob max | late blob p99 | rev/frame | stale p90 | bright err | quiet chg/frame | quiet rev/frame |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| wrestle | before | 183.4 | 40 | 12.6 | 3.594 | 26 | 1.976 | 16.200 | 0.090 |
| wrestle | confirm | 336.7 | 13 | 6.0 | 3.458 | 9 | 1.087 | 20.898 | 0.100 |
| wrestle | decay2 | 410.4 | 12 | 7.0 | 3.793 | 13 | 1.141 | 71.188 | 0.200 |
| wrestle | motion | 646.2 | 23 | 11.0 | 5.622 | 5 | 0.513 | 13.171 | 0.036 |
| faces | before | 169.1 | 720 | 223 | 3.671 | 34 | 1.902 | 34.461 | 0.130 |
| faces | confirm | 266.4 | 23 | 9.0 | 3.379 | 15 | 0.792 | 18.583 | 0.078 |
| faces | decay2 | 330.0 | 54 | 14.2 | 4.351 | 16 | 0.868 | 69.257 | 0.663 |
| faces | motion | 424.2 | 30 | 14.0 | 5.397 | 9 | 0.391 | 10.194 | 0.038 |
| wrestle | shipped | 322.7 | 12 | 7.6 | 3.499 | 10 | 1.168 | 24.373 | 0.093 |
| faces | shipped | 261.5 | 36 | 16.0 | 3.657 | 15 | 0.862 | 21.984 | 0.138 |

The confirm row is the pixels age floor (2 to 4 frames at the default).
Review added one frame after that floor missed the Falcon flicker gate.
Shipped is that longer floor, re-measured on the same clips. Decay2 and
motion were not re-rendered. Of the three holds, confirm is the only
candidate whose reversals per frame stay at or under the previous ASCII
path on both clips. On that row, late-blob max falls from 40 to 13 on
wrestle and from 720 to 23 on the face close-ups. Stale-run p90 falls from
26 to 9 and from 34 to 15. Brightness error falls from 1.976 to 1.087 and
from 1.902 to 0.792. Faces quiet changes fall from 34.5 to 18.6 per frame.
Wrestle quiet changes rise from 16.2 to 20.9 (+29%). Those extras are one
or two updates on a cell, not a new slab: 1860 cells gained a single quiet
change, 20 gained five or more, none gained eight, and quiet reversals on
the whole clip go from 38 to 42. Decay2 misses the reversal cap on faces
(3.671 to 4.351, +18.5%) and multiplies quiet changes. Motion misses it on
both clips (wrestle 3.594 to 5.622, faces 3.671 to 5.397) and leaves larger
late blobs than confirm. Letters stay on their own tone hold.

Shipped keeps the reversal cap (wrestle 3.594 to 3.499, faces 3.671 to
3.657). Late-blob max is 12 and 36. The face blob of 36 is one frame of
`;` and `+` spread across all three phase classes, not one timer class.
Stale-run p90 is 10 and 15. Brightness error is 1.168 and 0.862. Faces
quiet changes fall from 34.5 to 22.0. Wrestle quiet changes rise from 16.2
to 24.4 (+50%). That is still one-way catch-up: 1990 cells gained a single
quiet change, 23 gained five or more, none gained eight, and quiet
reversals on the clip go from 38 to 39. In the face windows that used to
slab, frames 100–130 and 245–270, the largest late blob is 4 and 3.

The flicker gates on the shipped floor stay at 1.2× (`ascii * 5 <= pixels * 6`)
on every window, including Falcon. Architect frames 60–150 at 200×56:
4128 / 3843 = 1.074×. Death Star frames 0–89: 80×24 6021 / 5655 = 1.065×,
200×56 42847 / 39945 = 1.073×. Techno 6 at 80×24: 21932 / 25673 = 0.854×.
Darth Vader: 80×24 3813 / 3855 = 0.989×, 200×56 22854 / 22358 = 1.022×.
Millennium Falcon: 80×24 9445 / 8130 = 1.162×, 200×56 58405 / 50517 =
1.156×. At the pixels age floor those Falcon windows were 10059 / 8130 =
1.237× and 62202 / 50517 = 1.231×, which is why that floor had moved only
the Falcon assert to 1.25×. The extra frame puts both back under 1.2×,
with 311 and 2215 switches of headroom. Adopting the smoothed tone on the
non-floor fast path instead raised Falcon 80×24 to 10299 / 8130 and failed
1.25×. The phase slot, not the age floor, is what spreads the release.

The phase is Jorge Jimenez's interleaved gradient noise from the SIGGRAPH
2014 Advanced Warfare talk, in the form Alan Wolfe published
(blog.demofox.org, 2022-01-01). A Bayer matrix modulo 3 clusters into
components of about 60 cells, which is the slab shape this change removes.
The noise is stationary: scrolling it would move the release slot under a
static cell. A shared threshold band that never expires is the sticking
cost Ninke's hysteresis dither patent already described. Large residuals
snap and small ones wait, the same split a codec uses between a skip and
a coded block.
