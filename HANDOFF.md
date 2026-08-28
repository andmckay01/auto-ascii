# HANDOFF — project save point (2026-08-28)

Resume document for the **sleepytime** ASCII video engine. Written for a future
work session (human or Claude) returning cold. The spec is `PLAN.md`; this file
is "where things stand and how to pick them up."

## Status: COMPLETE through M5 (all planned milestones)

| Milestone | Commit | What landed |
|---|---|---|
| Baseline | `8aa2caf` | PLAN.md, research digests (`docs/research/`), corpus manifest, `tools/prep_video.py` |
| M0 | `ee7d9e3` | Workspace, SLPY-lite, letterbox/resize player, diff renderer, pty-safe restore |
| M1 | `564259d` | Full SLPY v1 (delta+zstd, FIDX seek, NORM, chroma), caps probe, color tiers |
| M2 | `73e4434` | Eval harness: SSIM/flicker/damage metrics, goldens, fuzz, perf gates, `params.toml` |
| M3 | `f5ff5d1` | Layer compositor (edges/highlights/half-blocks), 8 palettes, edge-F1, tuning sweep |
| M4 | `e6170a6` | `sleepytime` facade crate (Player + RenderSession), feature-gated bin, terminal matrix |
| M5 | `5fcbfb5` | 1-h soak, font tables, quirk table, scrub UX, static binaries, publish hygiene |

Tag `v0.1.0` marks the completed state. Working tree at save time: clean.

**Off-box backup:** private GitHub repo `https://github.com/andmckay01/auto-ascii`
(`origin`; main + all tags). Renamed there 2026-08-28 from `sleepytime-memory`;
GitHub redirects the old URL. Two neighbours are DIFFERENT projects — do not
push to either: `andmckay01/sleepytime` ("Sleepytime for Mac") and
`andmckay01/auto-ascii-legacy` (a frame-based ASCII animation workspace that
held the `auto-ascii` name until this rename, preserved untouched).

## Quick start (this box: hetzner, 4-core, Rust 1.97.1, ffmpeg installed)

```bash
export PATH="$HOME/.cargo/bin:$PATH"
./target/release/sleepy-player assets/sheep-counting-neroni.slpy   # the 13.8-min demo
./scripts/eval.sh          # full quality/perf harness, ~4-10 min, must say ALL GREEN
./scripts/release.sh       # stripped native + musl-static + windows-gnu binaries -> dist/
python3 tools/soak.py --duration 60   # resize-storm smoke (3600 = the real soak)
```

Embedding: `crates/sleepytime/examples/simple-play.rs` (13 lines) and
`README.md` "Embedding it". Library-only build: `cargo build -p sleepytime
--no-default-features` (no terminal/CLI deps).

## What is durable vs regenerable

- **Durable (in git):** all code, PLAN.md, INTERFACES.md, params.toml,
  perf/thresholds.toml, goldens, font tables (`crates/slpy-core/fonts/`),
  eval baselines (`runs/base.json`), the M3 sign-off reel (`runs/m3-reel.html`),
  sweep records, docs.
- **Regenerable:** `assets/*.slpy` — the factory is byte-deterministic; rebuild
  with `sleepy-factory build corpus/<clip> -o assets/<name>.slpy` (the eval
  harness determinism guard proves rebuild == original). `dist/` via release.sh.
- **External:** `corpus/*.mp4` originals are gitignored (large). Sources live on
  the owner's Mac (`~/Downloads`, see corpus/README.md); re-send via Taildrop
  (`tailscale file cp` to hetzner, then `sudo tailscale file get <dir>`).

## Open items (owner)

1. ~~**M3 human sign-off**~~ — **DONE 2026-08-28.** Owner reviewed the reel and
   signed off on the look. No tuning pass needed; `params.toml` stands as-is.
   (Reel was published as a Claude artifact for review, the build box being
   headless; `runs/m3-reel.html` remains the durable copy.)
2. **GUI terminal pass** — `docs/TERMINAL-CHECKLIST.md`, ~5 min in real
   kitty/alacritty/wezterm/gnome-terminal/xterm (this box is headless; only
   pty-fixture verification was possible here). Practical route: SSH in from
   each terminal on the Mac — the probe reads the *local* terminal, so color,
   fonts and aspect are faithful. Judge **tearing locally only**; link latency
   confounds that one signal.
3. **crates.io publish** — *prepped, awaiting owner's token.* All four
   publishable crates now carry `repository`/`keywords`/`categories`/`readme`
   and pass `cargo package` **with `--verify`** (a real build from each
   tarball); the "no documentation, homepage or repository" warning is gone.
   `slpy-eval` and `sleepy-factory` are deliberately NOT published. As of
   2026-08-28 all four names were free on crates.io. No token exists on this
   box and a publish is permanent (yank-only), so the owner runs:

   ```bash
   cargo login <token>
   cargo publish -p slpy-format     # no internal deps
   cargo publish -p slpy-core       # no internal deps
   cargo publish -p slpy-term       # needs slpy-core
   cargo publish -p sleepytime      # needs all three
   ```

   Order matters — each crate must be live on the index before the next
   resolves it (allow a minute between). Afterwards, swap the README's git
   dependency for `sleepytime = "0.1"`.
4. **macOS binaries** — build on a Mac (`Makefile` macOS section; no osxcross).
   Cannot be done from this box at all. On the Mac:
   `git clone https://github.com/andmckay01/auto-ascii && cd auto-ascii &&
   make build && strip target/release/sleepy-player`. Needs Rust; ffmpeg
   (`brew install ffmpeg`) only if building assets, not for playback.

## Backlog (PLAN §7 M6+, explicitly out of v1)

Motion plane (new plane ID, no format break), audio via the `Clock` seam,
braille polish, zstd dictionaries, kitty-graphics presenter. Known cosmetic
nits: `--no-quirks` help text mentions only the cache-write bypass; README's
~29 s clean-build figure was 43 s under co-tenant load.

## Ground rules that must survive into future sessions

- **Scope amendment (PLAN.md top): NO connectivity engineering.** No SSH/WAN/
  tmux/ConPTY code, no throughput governor. The engine stays an abstract,
  embeddable library. (Owner directive 2026-07-25.)
- **perf/thresholds.toml** is calibrated to THIS 4-core box; recalibrate only
  via the documented median-of-3-runs ×1.15 procedure, never casually. (It was
  once silently reverted by a drill cleanup — check it if the gate misbehaves.)
- **Goldens re-pin only deliberately**, with justification and a visual
  spot-check through the slpy-eval rasterizer.
- **Never bake glyphs into assets** (PLAN §4 hard rule) — glyph choice is
  runtime-only; that is what makes resize correct.
- The asset cache keys on the **effective** params + pipeline fingerprint;
  a slow `corpus eval` section after touching core crates is the cache
  correctly rebuilding, not a bug.

## Process record

Built via multi-agent workflows (research panel → per-milestone build/verify/
adversarial-review loops), ~60 agents total. Every milestone was independently
re-verified by a fresh-eyes agent and adversarially reviewed; 9 serious bugs
were caught pre-commit this way. Claude session memory for this project lives
at `~/.claude/projects/-home-mckay-personal-sleepytime-ascii/memory/` and is
auto-loaded by future sessions in this directory.
