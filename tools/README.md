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
ingest stage (docs/PLAN.md §5, stage 1) if that ever lands. Until then, run
this tool first and point the factory at `corpus/prepared/`.

## soak.py — resize-storm soak harness (M5, PLAN §7)

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

Build the player first: `cargo build --release -p auto-ascii --features bin`.

Continuously: drains the pty into a rotation-capped log (**first 2 MB** →
`head.log`, **last 10 MB** ring → `tail.log`, flushed every 30 s — both
disk *and* write volume stay bounded regardless of player throughput);
samples player VmRSS from `/proc` every 10 s → `rss.csv`; records every
resize → `resizes.csv`. At the deadline it sends `q`, drains the restore
bytes, and writes `summary.json`: exit code, resize/byte counts, whether
`RESTORE_SEQ` appears in the tail, a post-warmup least-squares RSS
slope (MB/h), and the **structural escape-stream check** (`escape_check`).

The structural check is the M5-acceptance "no desync in captured output"
evidence (PLAN §7 M5 A): both `head.log` and `tail.log` are run through a
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

Harness exits 0 iff the player survived the full duration, exited 0 on
`q`, emitted the restore bytes, **and both logs pass the structural
check**; the RSS-slope acceptance (< 1 MB/h after warmup) is reported for
review, not gated.

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
