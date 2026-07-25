# Reference corpus

Canonical clips that define "good" for sleepytime (PLAN.md §10, question 1). Every
eval metric baseline, golden test, and contact sheet is generated from these — once
locked, they should not change without re-baselining.

## Clips

| # | file | probed | what it stress-tests |
|---|---|---|---|
| 1 | `sheep-counting-neroni.mp4` | h264 1920×1080 @ 24 fps, 13m48s, 96 MB | bright high-contrast kids animation, repetitive motion (sheep over fence) — hysteresis/flicker, long-duration asset size |
| 2 | `grass-field-windy.mp4` | h264 720×1280 (PORTRAIT) @ ~29.9 fps, 7 s, 1.9 MB | fine high-frequency texture in wind — temporal-delta compression worst case, flicker torture test; portrait aspect handling |
| 3 | `silhouette-dance.mp4` | h264 480×360 (4:3) @ 25 fps, 2m18s, 5.2 MB | silhouette dance animation — white-on-black then black-on-white panels; extreme binary contrast (edge layer with no color crutch), large flat regions (damage-tracking should emit near-zero bytes), human motion (temporal coherence); 4:3 exercises horizontal mirror fill on a landscape source |

Received via Taildrop 2026-07-25; originals pristine, factory reads them read-only.

## Notes & flags

- **Portrait source (clip 2) — RESOLVED:** off-aspect sources are preprocessed with
  `tools/prep_video.py`, which contain-scales onto a consistent canvas (default
  1920×1080) and fills the leftover space with seamless alternating mirror tiles
  (`--fill mirror` or `--fill mirror-invert`; sample frames in
  `prepared/samples/`). The factory always receives canvas-normalized input and
  needs no per-aspect logic; the tool's filtergraph will be absorbed into the
  `sleepy-factory` ingest stage later. See `tools/README.md`.
- **Clip 1 duration (13.8 min):** plan sizing assumed ~3-min clips (~122–350 MB/asset).
  At 13.8 min expect roughly 4.6× that. For iteration speed, cut a canonical 2–3 min
  excerpt (factory `--ss/--t` passthrough) and keep the full video for soak tests.
- **Clip 2 duration (7 s):** ideal golden-test fixture — fast to encode, byte-small.

## Gaps vs. PLAN.md recommendation (3–5 clips, mixed)

- one dark / low-light clip with soft shadow gradients — still wanted (clip 3 is
  dark but binary, so it never exercises the deep-shadow ramp or mid-tone rolloff)
- fast-motion: partially covered by grass texture; a live-action pan/cut clip would
  round it out

## Conventions

- Short kebab-case filenames (spaces poison shell commands downstream).
- Probe each arrival with `ffprobe -print_format json -show_format -show_streams`
  and record it in the table above.
- `prepared/` holds canvas-normalized derivatives from `tools/prep_video.py`
  (`<stem>-prepared.mp4` by default) plus extracted reference frames under
  `prepared/samples/`. Originals in `corpus/` stay pristine; the factory should
  ingest from `prepared/`.
