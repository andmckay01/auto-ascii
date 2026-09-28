The `.bin` files are ASCI v1 test excerpts, not golden renders. Each is
a window of a supplied asset, area-resampled so normal workspace tests can
exercise real playback without an external corpus or a video decoder:

| File | Source asset | Frames (zero based) | Size |
|---|---|---|---|
| `architect-motion.bin` | `The Architect.ascii` | 7580–7729 | 128×72 |
| `death-star-466.bin` | `Death Star Construction.ascii` | 466–555 | 128×72 |
| `falcon-3545.bin` | `Millennium Falcon.ascii` | 3545–3634 | 96×54 |
| `techno6-390.bin` | `techno-tech-house-mix-haluk-arslan/6.ascii` | 390–479 | 96×54 |
| `vader-1126.bin` | `Darth Vader.ascii` | 1126–1215 | 96×54 |

Y/E/Ex/Ey use the core area resampler; H and RGB565 C use nearest samples
(C at half size). Frame timing, aspect ratio, shot levels and cut boundaries
are retained, rebased to frame zero. The containers use temporal deltas,
a keyframe interval of 60, and zstd level 9. Metadata records the source
and frame range. No glyph, palette, style output or expected result is baked in.

`ascii_temporal.rs` plays 121 Architect frames at five zoom sizes. At the
last frame, any-cell differences from cold measure 16.0–19.5%; the limit is
22%. Background/default-background differences stay below the 3% limit.
Glyph differences remain intentional: colour follows current input while
glyph/edge/half hysteresis limits flicker. Frames 105–149 also require that
at most 1% of cells stay two or more ramp steps from cold throughout
(5 of 1,920 at 80x24, 38 of 11,200 at 200x56; b890cea's exact-repeat
settling left 32 and 215). Repeating frame 149 80 more times must leave at
most 0.5% two steps from cold (3 and 2 cells; 9f316d9, which had no
convergence inside its margin, and 3cef5ec fail).

Glyph switches per cell must stay within 1.2× pixels: the Architect's frames
60–149 at 200x56 (1.190×), and each other excerpt's 90 frames from a cold
start. Excerpts at b890cea / now: Death Star 1.296 / 1.198 at 80x24 and
1.288 / 1.180 at 200x56; Falcon 1.294 / 0.990 and 1.266 / 0.963; techno 6
1.206 / 0.652 at 80x24; Darth Vader 1.360 / 1.055 and 1.352 / 1.045. The
full-resolution Death Star window measures 1.164 at 80x24; the excerpt is
harsher and gates the activity band's threshold. Downscaling
Death Star to 96×54 raises its excerpt above the budget, hence 128×72.

Extraction source, full-size comparisons and logs are in the accompanying
`compare/ascii-zoom/reproduce/` and `compare/ascii-hold/` artifacts.
