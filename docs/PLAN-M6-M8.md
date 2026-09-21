# PLAN M6–M8 — key hints, the agent-first `auto-ascii` CLI, compositions

> **Status: landed 2026-09-20** (PR #1). `PLAN.md` §7 carries the as-built
> M6–M8 bullets and `INTERFACES.md` the API notes; §0 below records the
> decisions, §4 the backlog.

Spec for the 2026-09-20 expansion. `PLAN.md` stays the engine spec; this file
carries the three milestones that turn the engine into a tool, and each
milestone adds its one-line bullet to `PLAN.md` §7 and its API notes to
`INTERFACES.md` when it lands. Owner intent, verbatim in spirit:

> The player should show, terse but clear, what the arrow keys do — to the
> left of the scrub bar — and what the other keys do (shadow lift, etc.).
> An agent-first CLI should take a video from anywhere on the desktop,
> process it, and land it in the folder where the user's processed videos
> live. It should cut videos and stitch them: `.ascii` files are the clips,
> and a *composition* stitches an unbounded number of them, each placed at a
> chosen point on the timeline with its start and end trimmed.

## 0. Decisions (settled here so implementers do not re-litigate them)

1. **CLI first, MCP later.** The agent surface is a CLI named `auto-ascii`
   with a global `--json` flag. Every agent harness can run a CLI; an MCP
   server is a thin wrapper over the same library and is backlog (M9).
2. **The home folder is `~/auto-ascii/`** (override: `AUTO_ASCII_HOME`):
   `library/` holds processed clips (`<name>.ascii` + `<name>.json` sidecar
   with provenance), `compositions/` holds `<name>.toml`, `exports/` holds
   flattened compositions. Visible, discoverable, one sentence to explain.
3. **A composition is a TOML file and is the source of truth.** Agents may
   write it directly; `auto-ascii compose …` subcommands are conveniences that
   edit the same file. Schema in §3.
4. **Compositions play virtually and export optionally.** The player maps
   composition time → (clip, local frame) and switches decoders at clip
   boundaries; nothing is re-encoded to play. `export` flattens to one
   `.ascii` when a single file is wanted. "Unbounded" therefore costs one mmap
   per clip, not a re-encode.
5. **Overlays stay event-driven.** No always-on chrome: the parity and
   console-golden tests render the real `Player` grid and must keep passing
   unchanged. Hints appear with the existing overlays, on `v`, and for a short
   window at start-up (run-loop only, never inside `pipeline::Player`).
6. **Printable ASCII only in overlays** (`scrub_overlay.rs` parser), so arrows
   are `<-` and `->`.

## 1. M6 — key hints in the player

Today (`crates/auto-ascii/src/pipeline.rs:869-972`, `player.rs:441-491`): the
bottom row shows either the progress overlay (`0-9`, `<-`/`->`; hidden 1000 ms
after the last seek) or the dial overlay (`d` cycles shadow lift / edge
strength / hysteresis, `[`/`]` turn it; hidden after 2500 ms). Keys: `q`,
`Esc`, `Ctrl-C` quit; nothing else is bound.

### Changes

- **Progress row gets an arrow-hint block at the far left.** Layout becomes
  `" <- 5s -> "` · `" M:SS / M:SS "` · bar · `" NNN% "`. The number is
  `SCRUB_STEP_SECS`, not a literal. When `cols < 64` the hint block is dropped
  and the row is exactly today's layout, so narrow terminals lose nothing.
- **A hints row on `rows-2`,** same colours as the progress row, one fixed
  line, truncated at `cols` on word boundaries in this priority order (drop
  from the right):
  `q quit   space pause   0-9 jump   <- -> 5s   d dial   [ ] adjust   v controls`
  Shown whenever the progress or dial overlay is visible, for the first 3 s
  after start-up, and toggled sticky by `v` (sticky hides on the next
  `v`). `pipeline::Player` gains `set_hint_overlay(bool)` and
  `draw_hint_overlay`; `drain_events` reports `v` in `Drained`; the timers
  and stickiness live in `player.rs` beside the existing ones. Hiding any
  overlay keeps the existing `overlay_hide_pending` → `invalidate()` contract.
- **Space pauses** (added after M6 landed): the frame freezes, the progress
  row stays up reading ` PAUSED ` with a `|` bar head, seeks still work and
  stay frozen, and resuming continues from the frozen frame (`--duration-secs`
  remains a wall-clock budget).
- **Enlarge card unchanged.**

### Accept

- `scrub_overlay.rs` updated to the new contract: while an overlay is shown,
  every row **above the bottom two** matches the no-overlay reference; hidden
  → full-screen match; hide damages `cols*rows`; all bytes printable ASCII.
- New tests: hint row content at 80, 64 and 40 columns (truncation order);
  progress row with and without the arrow block at the 64-column threshold;
  `v` toggles `Drained`; start-up window shows then hides (run-loop test on
  the SimBackend or pty).
- `pipeline_parity.rs`, `linux_console_golden.rs`, tier goldens and the 36
  insta snapshots pass **without re-blessing**.
- README "Quickstart" key line and `docs/TERMINAL-CHECKLIST.md:27` list all
  keys: `q/Esc quit · 0-9 jump · <-/-> 5 s · d dial · [ ] adjust · v controls`.
- `PLAN.md` §7 gains `- **M6 — key hints.** …` and INTERFACES gets a landed
  note in the numbered style.

## 2. M7 — the agent-first `auto-ascii` CLI

New crate `crates/auto-ascii-cli`, binary name `auto-ascii`, depending on the
facade (`auto-ascii`, default features), `auto-ascii-format`, and the factory
as a **library**: `auto-ascii-factory` becomes lib + thin bin (`src/lib.rs`
owns the modules; `main.rs` keeps the clap surface and calls into the lib).
The facade cannot host the CLI because the factory already depends on the
facade. The factory stays unpublished.

### Commands (all accept `--json`; humans get aligned text, agents get one
JSON object on stdout; errors are `{"error": …}` on stderr with exit 1)

| command | does |
|---|---|
| `auto-ascii import <video> [--name N] [--ss T] [--t T] [--fps N] [--res WxH] [--force]` | ffmpeg-ingests via the factory into `library/<name>.ascii` and writes `library/<name>.json` (source path, source sha256, frames, fps, duration_s, bytes, created). `<name>` defaults to the kebab-cased file stem; a collision errors unless `--force`. |
| `auto-ascii list` | library clips: name, duration, fps, frames, bytes, source. |
| `auto-ascii info <clip>` | header + sidecar for one clip (`inspect` stays in the factory). |
| `auto-ascii cut <clip> --in T --out T [--name N]` | new library clip = the slice, via the M8 export path (single-clip composition). Lands in M8, declared here. |
| `auto-ascii play <clip \| composition.toml>` | interactive player (`Player::builder()`), same keys as `auto-ascii-player`. |
| `auto-ascii compose …` | M8, §3. |
| `auto-ascii agent-guide` | prints the embedded agent guide (below). |
| `auto-ascii home` | prints the resolved home folder and creates it. |

`<clip>` resolves as: an existing path, else `library/<clip>.ascii`.
Timestamps accept `SS[.f]`, `MM:SS[.f]`, `HH:MM:SS[.f]` — one shared parser in
the facade (`auto_ascii::timecode::parse`), which the player's `--seek` also
uses from now on.

### The agent guide (`docs/AGENT-GUIDE.md`, also embedded)

Under 60 lines: what the tool is, the home folder layout, the four-step loop
(import → list → compose/cut → play or export), the composition schema with
one example, the `--json` shapes for `import`/`list`/`compose show`, and the
two rules agents get wrong (names are kebab-case; `at` places, `in`/`out`
trim — both optional).

### Accept

- Integration tests in the new crate with `AUTO_ASCII_HOME` pointed at a temp
  dir: `import` of a Rust-written BGR24 AVI fixture (lift the writer from
  `m2_params_eval.rs` into `auto_ascii_eval::fixtures::write_bgr24_avi` so
  both crates share it) produces the clip + sidecar; `list --json` shape;
  `info`; name collision + `--force`; `agent-guide` prints; `home` creates.
- `cargo test --workspace`, `cargo clippy --workspace --all-targets -D
  warnings` green. The factory's own tests unchanged after the lib split.
- `PLAN.md` §7 `M7` bullet, INTERFACES `## Binaries` gains the CLI shape.

## 3. M8 — compositions

### Schema (`schema = 1`)

```toml
schema = 1
name = "demo"                 # optional; defaults to the file stem
[[clip]]
asset = "apple-1984"          # library name, or a path (absolute, or relative to this file)
in = "0:05"                   # optional trim start inside the asset (default 0)
out = "0:20"                  # optional trim end inside the asset (default asset end)
at = "0:00"                   # optional timeline position (default: end of the previous clip; first clip 0)
```

Semantics, in this order: clips are placed in file order; `at` overrides the
default position; the composition ends at the latest clip end; a gap is
black; where clips overlap, the **later-listed** clip is on top. Duration and
`frame_count` use the composition fps = the highest clip fps. Clips may
differ in fps and base resolution for playback; `export` requires one base
resolution and errors otherwise (say which clip differs and suggest
`import --res`).

### Library

- `auto_ascii::Composition` (core tier, no new deps): `clips`, `fps`,
  `frame_count`, `duration_secs`, `locate(t_secs) -> Option<(clip_idx,
  local_frame)>` — the single time→frame function; the two copies at
  `player.rs:456` and `player.rs:500` route through it for single assets as
  a one-clip composition. TOML parsing (`Composition::from_toml_str`, path
  resolution against the file and the home library) behind the default-on
  `compose` feature (`toml` + `serde`).
- `RenderSession::open_composition(path)`; `render` unchanged. Internally one
  `pipeline::Player` per distinct (base res, plane set, fps) shape; at every
  clip boundary and every backward jump: `reset_temporal_state`; the NORM
  levels come from the active clip's own tables.
- `Player::builder().composition(path)`; the player binary and `auto-ascii
  play` treat a `.toml` argument as a composition. Overlays show composition
  time; the progress row's time block gets ` c/N ` (current clip / count) when
  `cols >= 64`.
- **Export** (`auto_ascii::compose::export(&Composition, out, keyframe_ivl)`):
  walks output frames at the composition fps, decodes the top clip's planes
  (`decode_plane_into`), writes them with `AsciiWriter::write_frame`; one NORM
  record per (clip slice ∩ source shot), rebased, cut-flagged at every clip
  boundary; `write_norm` once before the first frame (pre-pass over the shot
  tables, no decoding). Gaps write black planes. `cut` = export of a one-clip
  composition into the library.

### CLI (`auto-ascii compose …`, M7 conventions)

`new <name>` · `add <name> <clip> [--in T] [--out T] [--at T]` · `show <name>`
(resolved timeline: each clip's start/end on the composition, gaps, overlaps)
· `play <name>` · `export <name> [-o path]` (default `exports/<name>.ascii`).

### Accept

- Unit tests for `locate`: sequential default, explicit `at`, gap → `None`,
  overlap → later clip, mixed fps, in/out trims, end exclusive.
- Integration: two synthetic assets from `auto_ascii_eval::fixtures`
  (visually distinct, e.g. GradientMotion + HardCut) → composition →
  `RenderSession` frames provably come from the right clip at the boundary;
  export → reopen → `frame_count`, NORM shot count and cut flags as expected;
  `auto-ascii-player composition.toml --sim 120x40:60` prints the stats line;
  `--seek` and `0-9` on a composition land on the composition timeline.
- `cut` round-trip: slice frames equal the source's frames at the offset.
- Existing goldens and parity tests unchanged. Workspace tests and clippy
  green. `PLAN.md` §7 `M8` bullet, INTERFACES facade block updated.

## 4. Backlog

- **M9 — MCP server.** `auto-ascii mcp` speaking MCP over stdio with tools
  mirroring the CLI (import, list, info, cut, compose_*, export). Thin: each
  tool calls the same library functions and returns the `--json` object.
- Composition-aware scrub overlay showing clip names; transitions (fade to
  black between clips); per-clip dial settings.
