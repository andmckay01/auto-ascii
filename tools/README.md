# tools/

## prep_video.py — corpus preparation front door

Normalizes any source video onto a consistent canvas (default **1920x1080**)
before it enters the corpus / the offline factory. Python 3 stdlib only; all
heavy lifting is a **single ffmpeg invocation per output** (plus ffprobe for
metadata). Originals are never modified; output directories are created as
needed.

```
tools/prep_video.py INPUT [--canvas WxH] [--fill mirror|mirror-invert]
                    [--clip START-END]... [--min-duration S] [--out PATH] [--verbose]
```

Processing order: **cut/concat clips → contain-scale → boomerang → mirror-fill
→ encode** (h264 crf 18 preset medium, yuv420p, `+faststart`; audio re-encoded
aac 192k — required anyway for clean concat). Default output:
`<input-dir>/prepared/<stem>-prepared.mp4`.

### Examples

```sh
# Full video onto the default 1920x1080 canvas, mirror fill:
tools/prep_video.py corpus/portrait-clip.mp4

# Cut two snippets, stitch them (audio preserved), then normalize:
tools/prep_video.py corpus/long-clip.mp4 --clip 0:30-0:45 --clip 5:00-5:15

# Loop a short clip out to >= 20 s (forward/reverse/forward/...):
tools/prep_video.py corpus/short-clip.mp4 --min-duration 20

# Compare the two fill modes:
tools/prep_video.py corpus/portrait-clip.mp4 --fill mirror        --out corpus/prepared/portrait-clip-mirror.mp4
tools/prep_video.py corpus/portrait-clip.mp4 --fill mirror-invert --out corpus/prepared/portrait-clip-mirror-invert.mp4
```

Timestamps accept `SS`, `MM:SS`, `HH:MM:SS` with optional fractional seconds
(`1:23.5`). Clips are validated against the ffprobe duration; a start at or
after its end, or an end beyond the source duration, is a hard error.

### Fill modes

When the source aspect doesn't match the canvas (e.g. a portrait
phone clip on 16:9), the leftover space is filled with
**alternating reflected tiles** of the scaled image: horizontally the strip is
`[...O][M][O][M][O...]` centered on the original (`M` = hflip), vertically the
same idea with vflip. Adjacent tiles are mirror images about their shared
edge, so every seam is invisible. The tile count per side is *computed* — at
1080p a 9:16 portrait tile is 608 px wide but each side gap is 656 px, so
it takes **2 tiles per side** (a strip of 5, cropped to the canvas center).

Two readings of "reflect" are implemented — pick one by eye:

- `--fill mirror` — pure reflection.
- `--fill mirror-invert` — reflection with color negation applied to the
  **fill tiles only** (the centered original is never altered).

A 16:9 source on a 16:9 canvas produces no fill panels at all — the scaled
image covers the canvas exactly.

### Boomerang (`--min-duration`)

If the assembled output is shorter than the target, the tool appends a
reversed copy, then a forward copy, alternating, until it is long enough
(audio is reversed in the same pattern). **Memory caveat:** ffmpeg's
`reverse`/`areverse` buffer the *entire* stream in RAM — roughly
`w*h*1.5` bytes per frame at the point they run in the graph (the tool
deliberately reverses at scaled-tile resolution, before mirror-fill, to
minimize this). Fine for short loops (a 6.5 s 1080p clip is ~190 MB);
do not boomerang multi-minute 1080p sources. The tool refuses more than 64
segments.

### Portrait-source policy & relationship to auto-ascii-factory

Portrait (or any off-aspect) sources are **preprocessed
onto the standard canvas with seamless mirror-fill before ingest** — the
factory always receives canvas-normalized, even-dimensioned yuv420p input and
never needs per-aspect logic. The filtergraph construction in
`prep_video.py` (contain-fit scale → odd-length alternately-flipped
hstack/vstack strips → centered crop) is deliberately kept as small, pure,
documented functions (`contain_fit`, `build_mirror_axis`, `build_concat`,
`build_boomerang`) so the same logic can be absorbed into the `auto-ascii-factory`
ingest stage (docs/FEATURE-MAP.md §1, ingest) if that ever lands. Until then, run
this tool first and point the factory at `corpus/prepared/`.

## soak.py — resize-storm soak harness (M5)

Forks the release player onto a fresh pty (`pty.fork`, so the pty is the
player's controlling terminal and `TIOCSWINSZ` delivers real SIGWINCHes),
plays an asset with `--loop`, and storms randomized resizes at it: every
50–200 ms, uniform over 20x6..500x140, with ~8% sub-minimum 5x3 (below
the 32x9 viewport floor → the "enlarge terminal" card path). Python 3
stdlib only; single-threaded select loop.

```
tools/soak.py --asset PATH --outdir DIR [--duration SECS=3600] [--seed N]
              [--player PATH]
```

Build the player first: `cargo build --release -p auto-ascii` (the soak
runs `target/release/auto-ascii play`).

Continuously: drains the pty into a rotation-capped log (**first 2 MB** →
`head.log`, **last 10 MB** ring → `tail.log`, flushed every 30 s — both
disk *and* write volume stay bounded regardless of player throughput);
samples player VmRSS from `/proc` every 10 s → `rss.csv`; records every
resize → `resizes.csv`. At the deadline it sends `q`, drains the restore
bytes, and writes `summary.json`: exit code, resize/byte counts, whether
`RESTORE_SEQ` appears in the tail, a post-warmup least-squares RSS
slope (MB/h), and the **structural escape-stream check** (`escape_check`).

The structural check is the M5-acceptance "no desync in captured output"
evidence: both `head.log` and `tail.log` are run through a
strict VT parser (`check_escape_stream`, in the spirit of the byte-exact
interpreter in `crates/auto-ascii/tests/scrub_overlay.rs`) that accepts
exactly the player's specified output vocabulary — the probe volley, the
session enter/restore modes, CUP within the storm's size bounds (≤500×140),
well-formed tier SGRs, the `?2026` wrap, printable/UTF-8 ground text — and
reports anything else (truncated CSI, out-of-bounds CUP, stray control
bytes, malformed SGR/UTF-8) as a structural error. This catches a
diff-baseline desync the player *survives*, which the exit code cannot see.
`head.log` may end mid-sequence (byte cap — allowed); `tail.log` starts at
an arbitrary ring cut (the parser resyncs to the first ESC) and must end on
a complete sequence.

Harness exits 0 iff the player survived the full duration, exited 3 (the
quit status) on `q`, emitted the restore bytes, **and both logs pass the
structural check**; the RSS-slope acceptance (< 1 MB/h after warmup) is
reported for review, not gated.

Standalone: `tools/soak.py --check-logs DIR` re-runs the structural check
over an existing outdir (exit 1 on errors); `tools/soak.py --self-test`
runs the validator's own regression cases (a clean specified-vocabulary
stream passes; each corruption class — truncated CSI, OOB CUP, unknown
finals, bad SGR, control bytes, malformed UTF-8 — is caught).

Smoke mode (~1 min): `tools/soak.py --asset clip.ascii --duration 60 --outdir /tmp/soak-smoke`.
Full detached soak:

```sh
setsid nohup tools/soak.py --asset clip.ascii --duration 3600 --outdir runs/soak-1h \
    > runs/soak-1h/harness.out 2>&1 &
```

## readme_gif.py — the README demo GIF

Renders `docs/assets/architect-2m14s.gif` from the Architect asset. It runs the
`headless-dump` example with `--cells` (every cell's glyph, fg and bg), draws
each kept frame with Pillow (Menlo 13 on black, 8x16 px cells, 120x34 grid
with the 4-row letterbox trimmed top and bottom → 960x416), builds one global
256-color palette with ffmpeg (`palettegen=stats_mode=diff`,
`paletteuse=dither=none`) and optimizes with gifsicle. Needs Python 3 with
Pillow, `ffmpeg`, `gifsicle` and macOS's `/System/Library/Fonts/Menlo.ttc`.
Run it from the repo root:

```sh
python3 -m venv /tmp/gifenv && /tmp/gifenv/bin/pip install pillow
/tmp/gifenv/bin/python tools/readme_gif.py \
    "path/to/matrix-reloaded-architect-scene/The Architect.ascii"
```

```
tools/readme_gif.py ASSET [--settings PLAYER.toml] [--start F=4038] [--end F=4339]
                    [--step N=3] [--delay CS=10] [--warm N=2] [--grid 120x34]
                    [--trim 4] [--font-size 13] [--lossy 10]
                    [--out docs/assets/architect-2m14s.gif]
```

`--settings` defaults to the asset's saved `<stem>.player.toml` (style and
dials). Every source frame from `--start` to `--end` is rendered, so
hysteresis runs as in the player; every `--step`-th one is kept and shown for
`--delay` centiseconds. The defaults are one shot, cut to cut: the zoom that
starts at frame 4036 (2:14.53) and runs up to the cut at 2:24.70. `--warm 2`
renders 4036 and 4037 to settle hysteresis, then 4038–4339 are kept at step 3,
10 cs (10 fps, real time, 101 frames, 10.1 s). Starting at 4038 rather than
4037 keeps every kept step under twice the median. The shot is a zoom, so no
two frames 8 s or more apart match (the closest pair differs by about 24
mean abs RGB, against about 8 between kept neighbours); the loop point is the
film's own cut on both sides instead. Keep `--warm` inside the shot: warming
up across the cut at 4036 leaves pre-cut glyphs in about an eighth of the
first frame's cells, and a cold start inflates the first step. The
committed file was made with Pillow 12.3.0, ffmpeg 9.0.2 and gifsicle 1.96;
other versions can change the bytes.
