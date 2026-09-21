# corpus/ — local reference videos

`auto-ascii-factory eval` measures the renderer against whatever video files
sit directly in this directory (mp4, m4v, mov, mkv, webm or avi; subdirectories are
not scanned). Nothing here is committed except this file: reference videos
are large and rarely redistributable, and every committed test, golden and
perf gate is reproducible without them (the synthetic fixtures live in
`auto-ascii-eval`).

## Using it

1. Drop a few short clips here. A useful set mixes a bright, high-contrast
   clip, a dark one with soft shadow gradients, fine high-frequency texture
   (grass, rain, film grain) and some fast motion or hard cuts; 5–30 s each
   keeps the loop fast. Kebab-case file names, no spaces.
2. Off-aspect sources (portrait, 4:3) can be normalized onto a 16:9 canvas
   first with `tools/prep_video.py` (contain-scale plus seamless mirror fill),
   which writes into `corpus/prepared/`; point `--corpus` there or move the
   result up here.
3. Record a baseline once, then compare every change against it:

   ```bash
   auto-ascii-factory eval --corpus corpus/ --out runs/base.json --html runs/base.html
   auto-ascii-factory eval --corpus corpus/ --baseline runs/base.json \
       --out runs/latest.json --html runs/latest.html
   ```

   `scripts/eval.sh` runs the second form when this directory holds videos
   and uses `runs/base.json` when it exists; `runs/` is gitignored. Built
   assets are cached under `runs/cache`, keyed by input, params and pipeline
   fingerprint, so only changed inputs rebuild.

Re-baseline deliberately: `params.toml` edits and factory changes move the
numbers on purpose, and the baseline should follow only after the HTML
contact sheet has been looked at.
