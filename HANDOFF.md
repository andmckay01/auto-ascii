# HANDOFF — project save point (2026-09-20)

Resume document for the **auto-ascii** ASCII video engine. Written for a future
work session (human or Claude) returning cold. The spec is `PLAN.md`; this file
is "where things stand and how to pick them up."

## 2026-09-20 — M6–M8 expansion (branch `expand-compositions`, PR pending)

The engine became a tool. Spec: `PLAN-M6-M8.md` (decisions in its §0; do not
re-litigate them without the owner). Landed on the branch, one commit each:

| Milestone | Commit | What landed |
|---|---|---|
| M6 | `e2e2b4b` | Key hints in the player: `<- 5s ->` beside the scrub bar, a hints row (`q quit · 0-9 jump · <- -> 5s · d dial · [ ] adjust · ? keys`) with the overlays, for 3 s at start-up and sticky on `?`; first `d` opens on shadow lift; space pauses (frozen frame, ` PAUSED ` row that does not time out, seeks still land) |
| M7 | `e552bad` | `auto-ascii` CLI (`crates/auto-ascii-cli`): `import`, `list`, `info`, `play`, `agent-guide`, `home`, global `--json`; home folder `~/auto-ascii` (`AUTO_ASCII_HOME`) with `library/`, `compositions/`, `exports/` and a JSON sidecar per clip; the factory is now lib + bin; `auto_ascii::timecode` |
| M8 | `2c8d898` | Compositions: TOML timelines (`in`/`out` trims, `at` placement, gaps black, later clip on top) played virtually (`auto-ascii-player comp.toml`, `--sim` too) and flattened by `compose export`; CLI `cut` and `compose new/add/show/play/export`; `docs/AGENT-GUIDE.md` for agents |

Merged into main the same day, ahead of the branch: `2a47f07` (macOS build:
portable errno + `openpty` pointers) and `0cc2770` (the factory byte-pin test
now builds its fixture in Rust, a raw BGR24 AVI, so it no longer depends on
the ffmpeg build; confirmed identical on macOS/aarch64 and in a Debian
bookworm container).

**Second build box: the owner's Mac** (Apple Silicon, Ghostty 1.3.1, cargo
1.95, Homebrew ffmpeg 9.0.2, Docker Desktop). Everything in `scripts/eval.sh`
runs there; the perf thresholds were calibrated on hetzner. Its data volume
runs at 100 % — check `df -h /System/Volumes/Data` before build-heavy work and
prefer `CARGO_INCREMENTAL=0`. hetzner was unreachable over Tailscale that day;
Linux checks ran in `rust:1.95-slim-bookworm` with the checkout mounted.

Quick start for the new tool (from the branch or after the merge):

```bash
cargo build --release -p auto-ascii-cli -p auto-ascii --features auto-ascii/bin
./target/release/auto-ascii import ~/Desktop/clip.mp4          # -> ~/auto-ascii/library/clip.ascii
./target/release/auto-ascii compose new demo && ./target/release/auto-ascii compose add demo clip --in 0:05 --out 0:20
./target/release/auto-ascii compose play demo                  # or: compose export demo
./target/release/auto-ascii agent-guide                        # what an agent needs, 80 lines
```

Backlog from the spec (§4): an MCP server (`auto-ascii mcp`) over the same
library calls, clip names in the scrub overlay, transitions, per-clip dials.

## Status: COMPLETE through M5 (all planned milestones)

| Milestone | Commit | What landed |
|---|---|---|
| Baseline | `8aa2caf` | PLAN.md, research digests (`docs/research/`), corpus manifest, `tools/prep_video.py` |
| M0 | `ee7d9e3` | Workspace, ASCI-lite, letterbox/resize player, diff renderer, pty-safe restore |
| M1 | `564259d` | Full ASCI v1 (delta+zstd, FIDX seek, NORM, chroma), caps probe, color tiers |
| M2 | `73e4434` | Eval harness: SSIM/flicker/damage metrics, goldens, fuzz, perf gates, `params.toml` |
| M3 | `f5ff5d1` | Layer compositor (edges/highlights/half-blocks), 8 palettes, edge-F1, tuning sweep |
| M4 | `e6170a6` | `auto-ascii` facade crate (Player + RenderSession), feature-gated bin, terminal matrix |
| M5 | `5fcbfb5` | 1-h soak, font tables, quirk table, scrub UX, static binaries, publish hygiene |

Tag `v0.1.0` marks the completed state. Working tree at save time: clean.

**Off-box backup:** GitHub repo `https://github.com/andmckay01/auto-ascii`
(**public** since 2026-09-19, so the crates.io `repository` links resolve)
(`origin`; main + all tags). Renamed there 2026-08-28 from `sleepytime-memory`;
GitHub redirects the old URL. Two neighbours are DIFFERENT projects — do not
push to either: `andmckay01/sleepytime` ("Sleepytime for Mac") and
`andmckay01/auto-ascii-legacy` (a frame-based ASCII animation workspace that
held the `auto-ascii` name until this rename, preserved untouched).

## Naming & layout (settled 2026-08-28)

Four confusable projects; get this right before renaming or publishing anything.

```
/home/mckay/personal/
  sleepytime/auto-ascii/     <- THIS project (origin = andmckay01/auto-ascii)
  sleepy-archive/            <- reference clones, not worked on
    sleepytime-memory/       memory daemon for Claude Code (74 commits, v1.3.0)
    auto-ascii-legacy/       older LLM-authored ASCII animation workspace (1 commit)
```

The facade crate was renamed `sleepytime` -> `auto-ascii` (Rust path `auto_ascii::`)
because a **different, incoming project owns the name `sleepytime`** — another agent is
building it. **Never publish a crate named `sleepytime` from here.**

That paragraph used to end by listing what was deliberately NOT renamed — the
`slpy-*` crates, the `sleepy-*` binaries and the `SLPY` magic. **That exemption
ended on 2026-09-19**: all of it was renamed, and the assets were rebuilt. See
"Rename to `auto-ascii`" below. `auto-ascii` itself, the facade crate, is the
one name that has not moved since 2026-08-28.

## Quick start (this box: hetzner, **2 physical cores + SMT** (4 logical, EPYC-Milan), Rust 1.97.1, ffmpeg installed)

```bash
export PATH="$HOME/.cargo/bin:$PATH"
./target/release/auto-ascii-player assets/sheep-counting-neroni.ascii   # the 13.8-min demo
./scripts/eval.sh          # full quality/perf harness, ~4-10 min, must say ALL GREEN
./scripts/release.sh       # stripped native + musl-static + windows-gnu binaries -> dist/
python3 tools/soak.py --duration 60   # resize-storm smoke (3600 = the real soak)
```

Embedding: `crates/auto-ascii/examples/simple-play.rs` (13 lines) and
`README.md` "Embedding it". Library-only build: `cargo build -p auto-ascii
--no-default-features` (no terminal/CLI deps).

## What is durable vs regenerable

- **Durable (in git):** all code, PLAN.md, INTERFACES.md, params.toml,
  perf/thresholds.toml, goldens, font tables (`crates/auto-ascii-core/fonts/`),
  eval baselines (`runs/base.json`), the M3 sign-off reel (`runs/m3-reel.html`),
  sweep records, docs.
- **Regenerable:** `assets/*.ascii` — the factory is byte-deterministic; rebuild
  with `auto-ascii-factory build corpus/<clip> -o assets/<name>.ascii` (the eval
  harness determinism guard proves rebuild == original). `dist/` via release.sh.
- **External:** `corpus/*.mp4` originals are gitignored (large). Sources live on
  the owner's Mac (`~/Downloads`, see corpus/README.md); re-send via Taildrop —
  `tailscale file cp <files> hetzner:` from the Mac, and that is the whole
  procedure. A `taildrop-receive` systemd **user** unit on this box lands them
  in `~/taildrop` automatically; the old `sudo tailscale file get` pull step is
  gone. See `/home/mckay/CLAUDE.md`.

## Rename to `auto-ascii` — DONE 2026-09-19

The `slpy`/`sleepy` tokens are gone from live code and docs. `RENAME-PLAN.md`
records the procedure, the *verified* crates.io mechanics and the checklist; it
deliberately still uses the old names, as do `docs/research/` and `runs/`
(historical record), and so does the naming section above, which names the
OTHER projects.

| | from | to |
|---|---|---|
| crates | `slpy-core` / `-format` / `-term` / `-eval`, `sleepy-factory` | `auto-ascii-core` / `-format` / `-term` / `-eval` / `-factory` |
| binaries | `sleepy-player`, `sleepy-factory` | `auto-ascii-player`, `auto-ascii-factory` |
| magic | `SLPY` | `ASCI` |
| trailer payload | `SLPY_END` | `ASCI_END` |
| extension | `.slpy` | `.ascii` |
| enlarge-card text | `SLEEPYTIME` | `AUTO-ASCII` |

**The load-bearing invariant held.** Every render golden and all 36 insta
snapshots passed **unchanged**. Stronger than that: reverting exactly 16 bytes
in a rebuilt asset — 4 magic, 8 trailer payload, 4 trailer CRC — reproduces the
*previous* byte-pin `e5bc340e…` exactly, so every compressed plane byte is
bit-identical. `FIXTURE_SLPY_SHA` became `FIXTURE_ASSET_SHA` and was re-pinned
to `b00e3ecb…` for that container-identity change alone (three consecutive
identical builds verified first, per the constant's own convention).

Two on-disk constants the original plan had missed were caught by a survey
before the sweep: the `TRLR_PAYLOAD` trailer, and the `SLEEPYTIME` text the
"enlarge terminal" card actually renders. Both replacements are the same width
as what they replaced, so neither moved a byte offset nor a glyph position.

A **second** byte pin — `GOLDEN_SHA256` in `auto-ascii-format/tests/container.rs`
— surfaced only after the first was fixed, because `cargo test` stops at the
first failing target. **Use `--no-fail-fast` when hunting pins.** Its delta is
not purely format identity: the fixture's own META label lengthened, shifting
every FIDX offset by +7. Checked against a pre-rename worktree chunk by chunk —
all six FRAM (compressed plane) payloads byte-identical.

All six `assets/*.ascii` were rebuilt from `corpus/` and verified playable
(`--sim 213x58:60`, truecolor, 471-690 fps headless). The superseded
`assets/*.slpy` — 1.2 GB — were swept afterwards. `.gitignore` keeps BOTH
patterns: renaming `*.slpy` to `*.ascii` un-ignored those stale binaries and
briefly exposed them to `git add`.

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
3. ~~**crates.io**~~ — **DONE 2026-09-19.** The renamed crates are live and
   verified consumable: a fresh project's `cargo add auto-ascii` resolved
   0.2.0, pulled all three libraries from the registry, and built and ran
   against the real public API.

   | crate | version | state |
   |---|---|---|
   | `auto-ascii` | **0.2.0** | live |
   | `auto-ascii-core` | 0.1.0 | live |
   | `auto-ascii-format` | 0.1.0 | live |
   | `auto-ascii-term` | 0.1.0 | live |
   | `auto-ascii` | 0.1.0 | yanked (pre-rename) |
   | `slpy-core` / `slpy-format` / `slpy-term` | 0.1.0 | yanked (pre-rename) |

   **Nothing was deleted, deliberately.** Deleting `auto-ascii` would have
   locked the name against republishing for 24 hours — for the owner too —
   and then opened it to anyone; the only gain would have been removing three
   unused pages. The mechanics are verified in `RENAME-PLAN.md` §5 and
   recorded in the `crates-io-deletion-mechanics` memory. The three `slpy-*`
   names therefore stay on crates.io forever as yanked 0.1.0s. That is the
   accepted cost.

   Yanks were applied **after** every replacement was live, so there was never
   a window with nothing installable. `auto-ascii-eval` and
   `auto-ascii-factory` remain unpublished, as planned.

   Publishing again later: bump the version, `cargo publish -p <crate>`. A
   version number, once published, can never be reused — not even after a
   delete.
4. ~~**Flicker gate breach on `terminator-flaming-wreckage`**~~ — **RESOLVED
   2026-08-28: the metric is wrong, not the renderer.** The clip measures 2.856
   glyph switches/cell/s against the PLAN §6 gate of &le;2 (43 % over), but the
   owner watched it and signed off with "the fire looks super dope, no notes."
   The gate is specified for *static shots*; this clip is roiling fire plus 6
   hard cuts in 26 s, and the reported number is a whole-clip mean, so the
   breach is the measurement meeting footage it was never written for.
   **Action: the flicker metric wants a static-shot-only definition** (restrict
   the mean to frames inside a shot, excluding cut-adjacent frames) rather than
   a `params.toml` sweep. Until then, do NOT add this clip to `runs/base.json`
   — it would wedge the gate on a false positive. No tuning pass was run;
   `params.toml` still stands as signed off at M3.
5. **macOS binaries** — build on a Mac (`Makefile` macOS section; no osxcross).
   Cannot be done from this box at all. On the Mac:
   `git clone https://github.com/andmckay01/auto-ascii && cd auto-ascii &&
   make build && strip target/release/auto-ascii-player`. Needs Rust; ffmpeg
   (`brew install ffmpeg`) only if building assets, not for playback.

## Measurement hygiene — read before trusting any timing here

This box is **2 physical cores + SMT-2**, not 4 cores (`lscpu`: Core(s) per
socket 2, Thread(s) per core 2). Docs said "4-core" for months; the perf
thresholds stay valid (they were calibrated empirically) but any parallelism
expectation reasoned from "4 cores" is ~40% too optimistic. ~2x is the physical
ceiling for CPU-bound work; threads 3-4 are hyperthread siblings worth ~35%.

**It is also a shared box** — other workspaces burst (1-min load observed
swinging 1.1 -> 5.6 -> 1.3 inside 25 minutes). A single timing run here is
worthless. On 2026-08-31 a single-run A/B produced a "1.04x" speedup figure for
per-plane parallelism that a load-controlled median-of-3 revealed to be
**1.85x** — the slow run had simply been granted ~1.4 cores instead of 4.
**Take a median of at least 3, log `uptime` around every run, and discard
contended reps.**

## Build speed (settled 2026-08-31)

Two changes, audited, together taking a 63 s clip from ~7 min to ~2.5 min:

- **zstd 19 -> 15** (`params.toml [build]`, and the factory's in-code default,
  which now DIVERGES from `auto_ascii_format::WriterOptions::default()` at 19 on
  purpose — level is encoder policy, not a container property). +0.91 % asset
  bytes, 2.84x faster. zstd is lossless: L15 and L19 assets were verified to
  render byte-identically. `FIXTURE_ASSET_SHA` re-pinned; `assets/*.ascii`
  regenerated (a stale asset still PLAYS fine — only byte-reproducibility
  breaks, which is what the corpus determinism guard checks).
- **Per-plane parallel compression** kept (`auto-ascii-format/parallel`). Worth
  **1.85x** on this box, NOT the 1.04x an early contended measurement claimed
  — see the measurement-hygiene note above; commit `fe10e2c`'s mechanism
  paragraph is WRONG and superseded. Keyframes are ~1.4 % of compression time,
  not the dominant cost: on grainy film the deltas carry nearly all the
  entropy, and a keyframe is actually *cheaper* to compress than a delta.
- Next bottleneck is now the SERIAL EXTRACT stage, not compression. GOP-parallel
  encoding will NOT pay on this box (2 physical cores; SMT caps ~2.1x and
  per-plane already reaches it) — only on genuinely multicore hardware.

## Backlog (PLAN §7, explicitly out of v1)

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
  spot-check through the auto-ascii-eval rasterizer.
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
at `~/.claude/projects/-home-mckay-personal-sleepytime-auto-ascii/memory/` and is
auto-loaded by future sessions in this directory.
