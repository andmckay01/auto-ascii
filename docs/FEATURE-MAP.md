# Feature Map — auto-ascii

This map traces how a video becomes ASCII art in a terminal. It covers the offline factory
(ffmpeg ingest → feature planes → `.ascii` container), the runtime player (probe → letterbox →
resample → glyph codec → hysteresis → present), the interactive controls (transport, dials,
codecs, overlays), compositions, the `auto-ascii` CLI and its library folder, the embedding API,
and the eval/perf gates. It doesn't cover the research digests under `docs/research/`.

Verified against the tree at `6526f3f`. The commit that adds this map only removes comments, so
behaviour is unchanged. Line numbers drift, so cite and search by symbol name.

## Overview

- **Two halves, one contract.** `auto-ascii-factory` (offline, needs ffmpeg) distills a video
  into an `.ascii` asset of **feature planes**: luma, edge magnitude, edge orientation,
  highlight/shadow flags and chroma. `auto-ascii-player` maps those planes onto the terminal's
  current grid at render time. **Assets never store glyphs**, which is why one asset is correct
  at 80×24 and at 320×90, and after every resize.
- **One pipeline, many callers.** `crates/auto-ascii/src/pipeline.rs` `Player` is the only frame
  pipeline. The interactive `Player` (`crates/auto-ascii/src/player.rs`), the terminal-free
  `RenderSession` (`crates/auto-ascii/src/session.rs`), the `--sim` harness, the factory's
  `eval`, the resize fuzz, the benches and the goldens all drive it.
- **Deterministic by construction.** The factory writes the same bytes for the same input and
  params. The engine (`auto-ascii-core`) is integer-only on the hot path, with no clock and no
  I/O. Tests pin the output byte for byte.
- **Tunables are data.** Every factory and compositor knob lives in `params.toml`. The player's
  live dials are `[compose]` fields, so turning one re-renders the asset in memory and never
  rebuilds it.
- **Crates:** `auto-ascii-core` (engine), `auto-ascii-format` (container), `auto-ascii-term`
  (terminal backend), `auto-ascii` (facade + player binary) are published. `auto-ascii-factory`,
  `auto-ascii-eval` and `auto-ascii-cli` are workspace-only.

## Features / flows

### 1. Distill a video (`auto-ascii-factory build`)
- **Does:** turns any ffmpeg-readable video into an `.ascii` asset at 480×270, 30 fps (defaults
  from `params.toml [build]`).
- **User:** `auto-ascii-factory build clip.mp4 -o intro.ascii [--ss T] [--t T] [--fps N]
  [--res WxH] [--params F]`, or `auto-ascii import` (flow 12).
- **Code:** `crates/auto-ascii-factory/src/main.rs` (clap surface) →
  `crates/auto-ascii-factory/src/lib.rs` `build` / `BuildRequest` / `effective_params` →
  `crates/auto-ascii-factory/src/build.rs` `run`:
  1. **Ingest:** `crates/auto-ascii-factory/src/ffmpeg.rs` `probe` (ffprobe JSON) and
     `FrameStream::spawn`, which runs
     `ffmpeg -vf scale=W:H:flags=area,fps=N,format=rgb24 -f rawvideo -` and yields whole rgb24
     frames. stderr is drained on a thread.
  2. **Pass 1:** stream the frames once. `crates/auto-ascii-factory/src/lut.rs` `LumaLut`
     converts sRGB → linear → CIE L\*. `crates/auto-ascii-factory/src/shots.rs` `ShotDetector`
     finds cuts from the SAD between consecutive 256-bin L\* histograms against
     `[shots] sad_threshold_milli`, debounced by `min_shot_frames`. Per-shot p2/p98 levels
     (`percentile_levels_pct`) are pooled from the EMA'd luma that pass 2 will store.
  3. **Pass 2:** the same ffmpeg invocation again. `build.rs` `shot_records` writes the NORM
     table, then `crates/auto-ascii-factory/src/features.rs` `FeatureExtractor::process` makes
     six planes per frame:
     - `Y`: L\* luma, EMA'd (`crates/auto-ascii-factory/src/temporal.rs` `EmaPlane`).
     - `E`, `Ex`, `Ey`: from `crates/auto-ascii-factory/src/edges.rs`. Scharr gradients feed a
       doubled-angle orientation field, two orientation-aware bilateral passes, then
       hysteresis-thresholded, **unthinned** magnitude.
     - `H`: from `crates/auto-ascii-factory/src/highlights.rs`. bit0 is a top-hat highlight,
       bit1 a percentile deep shadow.
     - `C`: RGB565 chroma at half resolution (`crates/auto-ascii-factory/src/extract.rs`
       `pack_rgb565`).
     Every EMA resets at shot cuts. Frames stream through `AsciiWriter` (flow 2).
- **Invariants:**
  - Byte-deterministic: LUT and integer pixel math, a fixed zstd level, no timestamps. The
    default-params output is byte-pinned in
    `crates/auto-ascii-factory/tests/m2_params_eval.rs`.
  - The asset is written to `<out>.part` and renamed only after `finish()`, so a killed build
    never leaves a truncated `.ascii`.
  - Memory is O(plane): planes stream to the writer and are never accumulated.
  - Progress goes to a caller-supplied writer (stderr). stdout stays clean for `--json` callers.
  - Levels are **measured** here and **applied at runtime** by the player. Nothing is baked into
    the planes.

### 2. The ASCI container (write, open, decode, seek)
- **Does:** a chunked, CRC-checked, zstd-compressed container with O(1) open and fast seek.
- **User:** `auto-ascii-factory inspect <asset>` prints the header and chunks and verifies CRCs.
- **Code:** `crates/auto-ascii-format/src/write.rs` `AsciiWriter` (`write_norm`, `write_frame`,
  `finish`) → `crates/auto-ascii-format/src/read.rs` `AsciiReader` (`open`, `decode_plane_into`,
  `seek_plane_into`, `shot_for_frame`, `norm_levels`, `verify`). Header:
  `crates/auto-ascii-format/src/header.rs` `AsciiHeader`. NORM:
  `crates/auto-ascii-format/src/norm.rs` `ShotRecord`, `PlaneLevels`.
- **Invariants:**
  - Stream shape: `HEADER(64 B) | META | [NORM] | FRAM × n | FIDX | TRLR`. `frame_count` and
    `index_offset` are patched in at close.
  - Temporal delta: non-keyframes store `cur − prev (mod 256)` per byte. Keyframes fall on
    `frame_idx % keyframe_ivl == 0` (default 60).
  - `open` validates the header, trailer, META/NORM and FIDX but never frame payloads, so a cold
    open plus seek stays fast on multi-GB files. Each decode re-validates its frame.
  - Seek is a binary search to the nearest keyframe, then at most `keyframe_ivl − 1` delta rolls.
    The player decodes sequentially when frames are consecutive and seeks otherwise
    (`crates/auto-ascii/src/pipeline.rs` `Player::load_frame`).
  - The reader takes `&[u8]` (the player mmaps with `memmap2`). The crate does no file I/O.
  - Writer output is byte-pinned (`GOLDEN_SHA256` in `crates/auto-ascii-format/tests/container.rs`).

### 3. Probe the terminal
- **Does:** works out the color tier, glyph repertoire, synchronized-output support and cell
  pixel size without hanging and without leaking reply bytes as keypresses.
- **User:** automatic. `--tier`, `--no-query`, `--no-cache`, `--no-quirks`, `--palette`,
  `--font-table` override it.
- **Code:** `crates/auto-ascii-term/src/probe.rs` `probe_caps` / `ProbeOptions`: passive env
  hints (`EnvHints::from_env`), then one volley (XTVERSION, DECRQM 2026, XTGETTCAP `RGB`,
  `CSI 16 t`, DA1 last as the sentinel) parsed by `ProbeParser`. The deadline is
  `DEFAULT_PROBE_TIMEOUT` (200 ms). Results are cached at `$XDG_CACHE_HOME/auto-ascii/caps`
  (`cache_load`, `cache_store`, `apply_cache_hit`). Identity-keyed corrections live in
  `crates/auto-ascii-term/src/quirks.rs` `QUIRKS` / `apply_quirks`. The palette tier is resolved
  by `crates/auto-ascii/src/lib.rs` `PaletteChoice::resolve_for_caps`. Font tables are in
  `crates/auto-ascii-core/src/font_table.rs` `FontTable::veto_tier`.
- **Invariants:**
  - Silence or no TTY gives the conservative default (256-color, ASCII) at the deadline.
  - A cache hit can only upgrade the current run's passive evidence. The one exception is a
    stored quirk clamp.
  - Quirks key on the queried identity (XTVERSION/DA1), never on `TERM`.
  - Late replies are drained or filtered (`crates/auto-ascii-term/src/ansi.rs`
    `StragglerFilter`), so digits in a reply never trigger a seek.

### 4. Fit the picture: viewport, letterbox, resample
- **Does:** fits the asset's aspect into `cols × rows` cells, then box-averages each plane down
  (or up) to the viewport.
- **Code:** `crates/auto-ascii/src/pipeline.rs` `Player::reflow_grid` →
  `crates/auto-ascii-core/src/viewport.rs` `compute_viewport_for` (cell aspect
  `a = cell_h_px / cell_w_px` from the probe, else `DEFAULT_CELL_ASPECT` 2.0; column/row ratio
  `R = P · a`) → `crates/auto-ascii-core/src/resample.rs` `Resampler::build` / `apply` (separable
  Q8 tap tables). `Player::render_grid` resamples luma at `Vc × 2·Vr` (two vertical taps per cell
  for half-blocks), features at `Vc × Vr`, and chroma from its half-res plane. H flags are
  resampled as masks and re-thresholded (`H_HIGHLIGHT_MIN`, `H_SHADOW_MIN`).
- **Invariants:**
  - Below `MIN_COLS × MIN_ROWS` (32×9) there is no viewport, and the player draws the "enlarge
    terminal" card (`draw_enlarge_card`).
  - Worked examples at 16:9 are frozen as unit tests: 80×24 → 80×23, 213×58 → 206×58, 320×90 is
    an exact fit.
  - Tap tables are pure integer arithmetic and byte-deterministic across platforms.
  - `reflow_grid` and `HysteresisState::resize` are the only hot-path allocation points, overlay text aside.

### 5. Glyph codecs: features → glyphs (`pixels`, `letters`)
- **Does:** turns each cell's features into one glyph plus fg/bg colors. `pixels` (default)
  paints a low-res picture from shade ramps, half-blocks and quadrants. `letters` draws with
  type: characters ordered by ink, ASCII strokes on edges, `█`/`▀▄` only where the picture is
  lit. On truecolor and 256-color it sets each cell on a dim tint of its own colour
  (`PaletteSet::bg_tint`), so midtones and faces keep their shape at pixels' brightness.
- **User:** `/` cycles codecs while playing, `--codec pixels|letters` picks one at startup, and
  `s` saves it for this video (flow 9).
- **Code:** `crates/auto-ascii-core/src/codec/mod.rs` `GlyphCodec` (trait: `NAME`, `cell`),
  `Codec` (`ALL`, `next`, `from_name`, `names`), `compose_frame_codec`, and the `registry!` list.
  `crates/auto-ascii-core/src/codec/pixels.rs` `Pixels` and
  `crates/auto-ascii-core/src/codec/letters.rs` `Letters`. The frame loop is
  `crates/auto-ascii-core/src/compose.rs` `compose_frame` / `compose_frame_masked` over
  `FramePlanes`. The pixels layer contract is `compose_cell`: base ramp, then the edge layer
  (orientation bins in `crates/auto-ascii-core/src/orient.rs`), deep-shadow clamp, highlight and
  sub-cell structure. Palettes: `crates/auto-ascii-core/src/palette.rs` `select_palettes` /
  `PaletteSet` (eight palettes keyed charset tier × layer role; density picks ramp length, color
  depth caps it via `RAMP_CAP_TRUE` / `RAMP_CAP_256`).
- **Invariants:**
  - Dispatch is once per frame, not per cell. The pixels path is the exact loop every golden
    pins.
  - One glyph per cell, priority composition, never blended.
  - Every ASCII-tier glyph is printable ASCII `0x20..=0x7E` (CP437-safe).
    `crates/auto-ascii-core/tests/codec_props.rs` holds `letters` to its repertoire on every
    tier.
  - Adding a codec means one module plus one `registry!` line. The line generates the `Codec`
    variant, its place in the `/` cycle, its name and its dispatch arm.
  - Codec design constants (the `letters` ramps, thresholds and color curve) are codec data,
    pinned by its tests and goldens. They are not `params.toml` tunables.

### 6. Temporal stability: hysteresis and resets
- **Does:** stops cells flickering between neighbouring glyphs.
- **Code:** `crates/auto-ascii-core/src/hysteresis.rs` `HysteresisState` / `CellState` (3 B per
  cell): ramp-index hysteresis (`idx_hyst_q8`, a fraction of a ramp step), a dual-threshold edge
  gate (`edge_t_on` / `edge_t_off`, `WAS_EDGE`), an orientation bin with an 8° guard, and the
  quadrant flag (`WAS_QUADRANT`, `quad_e_on` / `quad_e_off`).
- **Invariants — every temporal discontinuity resets all per-cell state:**
  - a shot change or shadow-lift change (`crates/auto-ascii/src/pipeline.rs`
    `Player::update_levels`, keyed on `(shot, shadow_lift)`);
  - a digit jump or arrow scrub (`ClipDeck::drain_events`);
  - a resize (`HysteresisState::resize` in `reflow_grid`);
  - any change to the compose params, including a dial turn that moves
    (`Player::set_compose_params`; an equal value is a no-op);
  - a codec change (`Player::set_codec`) or clip switch (`ClipDeck::activate`);
  - a backward jump in `RenderSession::render` (`Player::reset_temporal_state`).
  After a reset the next frame is a cold start, identical to seeking straight to that frame.

### 7. Present: quantize, diff, restore
- **Does:** writes the grid to the terminal with minimal bytes, then always puts the terminal
  back.
- **Code:** `crates/auto-ascii-term/src/render.rs` `FramePainter::paint`: quantize to the color
  tier first (`crates/auto-ascii-term/src/quant.rs`), then per-row diff, changed spans with a
  skip-vs-move heuristic, SGR run-length elision, and a DEC 2026 synchronized-output wrap when
  DECRQM confirms support. `crates/auto-ascii-term/src/ansi.rs` `AnsiBackend` (the real
  terminal) and `crates/auto-ascii-term/src/sim.rs` `SimBackend` (in memory, throttleable) both
  present through it, behind the `crates/auto-ascii-term/src/backend.rs` `Backend` trait.
  `crates/auto-ascii-term/src/restore.rs` `install_restore_hooks` / `arm` / `restore_now` emit
  `RESTORE_SEQ` exactly once, from `Drop`, the panic hook, SIGINT/SIGTERM or atexit.
- **Invariants:**
  - Quantize before diff, so cells that quantize equal cost zero bytes.
  - Repaint mode `full` (default) invalidates every frame. `diff` rewrites only damaged cells.
  - Hiding an overlay schedules a one-shot invalidate so the diff baseline never keeps stale
    overlay cells.
  - crossterm is used for raw mode, alt screen and events only, never per-cell output. The
    player links no video codecs and no rayon.

### 8. Interactive player: transport and keys
- **Does:** plays an asset or composition at its own fps with pause, jump and scrub.
- **User:** `auto-ascii-player <asset|comp.toml>` or `auto-ascii play <clip|composition>`.
  `q`/`Esc`/Ctrl-C quit · space pause · `0`–`9` jump to 0–90% · `←`/`→` ±5 s · `d` / `[` `]`
  dials · `/` codec · `s` save · `v` controls. Flags: `--loop`, `--fps-cap N`, `--seek T`,
  `--duration-secs S`, `--repaint full|diff`, `--cell-aspect R`.
- **Code:** `crates/auto-ascii/src/bin/auto-ascii-player.rs` (clap; argv maps 1:1 onto
  `PlayerBuilder`) → `crates/auto-ascii/src/player.rs` `PlayerBuilder::build` (validates before
  touching the terminal) → `Player::run`, the event loop over `ClipDeck`
  (`crates/auto-ascii/src/deck.rs`). Keys are decoded in
  `crates/auto-ascii/src/pipeline.rs` `drain_backend_events` into a `Drained` record. The clock
  is `Transport` (`seek_to`, `toggle_pause`), with the free function `freeze_target`. Frame selection uses
  `Composition::frame_after`. `RepaintGate` paints a frozen frame once until something changes.
  Timestamps are parsed by `crates/auto-ascii/src/timecode.rs` `parse` (`SS`, `MM:SS`,
  `HH:MM:SS`, fractions allowed).
- **Invariants:**
  - Frames are chosen by wall clock. `--fps-cap` skips asset frames and never slows the video.
  - Pause freezes on the frame actually presented, not the clock's frame.
  - Held keys coalesce: `v`, space and `s` are flags. `/`, `d` and arrows are counted.
  - Quit wins and stops the drain.

### 9. Live dials and per-video settings
- **Does:** retunes the renderer during playback and remembers the result per video.
- **User:** `d` shows the dial readout and then cycles **shadow lift → edge strength →
  hysteresis**. `[`/`]` turn the selected dial. `s` writes `<name>.player.toml` beside the
  asset, and the next time that video comes to the front its dials and codec load.
- **Code:** `crates/auto-ascii/src/player.rs` `Dial` (`ALL`, `label`, `step`, `max`, `get`,
  `turn`, `param_key`, `set_param`) and `dial_after_cycle` (the first `d` only reveals the
  readout). `LiveSettings` (`front`, `turn`, `cycle`, `save`, `status`, `write_info`) tracks the fronted
  clip, a `--codec` override and the session's `/` and dial choices. `crates/auto-ascii/src/settings.rs`
  `VideoSettings` (`path_for`, `to_toml`, `parse`, `load`, `save`). Shadow lift bends the NORM
  LUT in `crates/auto-ascii/src/pipeline.rs` `build_levels_lut_lifted` / `apply_shadow_lift`.
- **Invariants:**
  - Dials map to `[compose]` fields (`shadow_lift`, `edge_t_on` shown inverted as "edge
    strength", `idx_hyst_q8`). No asset is touched.
  - A dial walk retraces its own steps: the top of the scale is a stop, and a press away from it
    counts from the detent above `max()` (`Dial::turn`; `crates/auto-ascii/tests/dials.rs`).
  - Codec precedence: a `/` press this session beats `--codec`, which beats the saved file,
    which beats the default (`pixels`).
  - Saved dials load per clip until a turn sets the full session compose override; it survives
    cuts and wraps. A visible dial readout refreshes when the next clip fronts.
  - The settings file is optional per key. Unknown keys and unknown codec names are ignored. It
    is hand-parsed, so the facade takes no TOML dependency for it.
  - An unreadable settings file falls back to defaults under session/CLI overrides, and the
    info row says `unreadable`.

### 10. Overlays: progress, hints, info row, zoom hint, big text
- **Does:** event-driven on-screen chrome. Nothing is always on.
- **User:** a progress row on seek, resume or pause (a timeline and ` c/N ` clip index in a
  composition). The dial readout. The key-hints row for 3 s at startup and whenever another
  overlay is up; `v` pins it. The info row above it shows clip name, codec, settings status and
  grid size (` 213x58 cells `). Below 160 columns a zoom hint appears (`Cmd - to zoom out: more
  cells, a sharper picture`, `Ctrl` off macOS). From 240×36 on block tiers, overlay text is drawn
  in big 3×5 block letters.
- **Code:** `crates/auto-ascii/src/pipeline.rs` `draw_progress_overlay_clips`,
  `draw_dial_overlay`, `draw_hint_overlay` (`hint_line`, which drops items by `HINT_DROP_ORDER`
  to fit), `draw_info_overlay` (`zoom_line`, `ZOOM_HINT_MAX_COLS`), `OverlayScale::for_grid`
  (`BIG_OVERLAY_MIN_COLS`, `BIG_OVERLAY_MIN_ROWS`, `BIG_FONT`, `paint_line`). Visibility policy
  is `crates/auto-ascii/src/player.rs` `ProgressTimer`, `HintState`, `DIAL_OVERLAY_HIDE_AFTER`
  (2.5 s) and `OVERLAY_HIDE_AFTER` (1 s).
- **Invariants:**
  - Overlays are drawn over the composed grid and never touch temporal state or the layer mask.
    The parity and console goldens render the bare grid unblessed.
  - Overlay text is printable ASCII (other characters print as `?`). Big text needs `▀▄█`, so
    the ASCII tier keeps one-cell text at every size.
  - The player cannot change the terminal font. The zoom hint is the whole feature
    (`docs/research/zoom.md`).

### 11. Compositions: play, show, export, cut
- **Does:** stitches any number of clips on one timeline (gaps are black, a later clip draws on
  top) and plays it without re-encoding, or flattens it into one `.ascii`.
- **User:** write `compositions/<name>.toml` (`schema = 1`, `[[clip]]` with `asset`, `in`,
  `out`, `at`), or use `auto-ascii compose new|add|show|play|export`. `auto-ascii cut` is an
  export of a one-clip composition.
- **Code:** `crates/auto-ascii/src/composition.rs` `Composition` (`from_toml_file`,
  `from_toml_str`, `resolve`, `timeline`, `locate_frame`, `gaps`, `overlaps`, `frame_after`,
  `default_library_dir`). Playback goes through `crates/auto-ascii/src/deck.rs` `ClipDeck`: one
  `pipeline::Player` per clip reached, capped at `MAX_LIVE_CLIPS` (8) live with the oldest
  evicted, `compose_gap` for black gaps, and presentation state carried across clip switches.
  Export is `crates/auto-ascii/src/compose.rs` `export` / `ExportOptions`: planes are copied, not
  re-derived, and `norm_records` rebuilds NORM from the clips' shot tables with cuts at every
  boundary.
- **Invariants:**
  - Timeline math works on the frame grid, so no sub-frame sliver can exist between abutting
    clips.
  - Single assets play as one-clip compositions, so there is exactly one clip-switch
    implementation.
  - The TOML file is the source of truth. `compose new`/`add` are text operations: `add` appends
    one `[[clip]]` table with one `O_APPEND` write and never re-serializes the file.

### 12. The `auto-ascii` CLI and the library folder
- **Does:** imports videos into a visible home folder, lists and describes them, cuts and
  stitches them, plays them. Commands other than `play` and `compose play` have a `--json` mode.
- **User:** `auto-ascii home | import | list | info | cut | compose … | play | agent-guide`.
  The home is `~/auto-ascii` or `$AUTO_ASCII_HOME`, holding `library/`, `compositions/` and
  `exports/`.
- **Code:** `crates/auto-ascii-cli/src/main.rs` (`Cli`, `Cmd`, `ComposeCmd`, `cmd_import` →
  `auto_ascii_factory::build`, `cmd_cut`, `cmd_list`, `cmd_info`, `cmd_compose_*`, `cmd_play`,
  `emit` / `emit_err`). `crates/auto-ascii-cli/src/home.rs` `Home` (`resolve`, `create`,
  `resolve_clip`, `resolve_playable`), plus free functions `kebab_case` and `cut_name`.
  `crates/auto-ascii-cli/src/library.rs` `Sidecar`, `Provenance`, `list`, `describe`,
  `write_sidecar`. `crates/auto-ascii-cli/src/composition.rs` `create`, `append_clip`, `report`.
  `docs/AGENT-GUIDE.md` is embedded and printed by `agent-guide`.
- **Invariants:**
  - `--json` puts exactly one JSON value on stdout. Errors, including a mistyped command line,
    are `{"error": "..."}` on stderr with exit 1. ffmpeg and factory chatter always go to stderr.
  - Names are kebab-case (`My Clip (2).mp4` → `my-clip-2`). `import --force` replaces a clip
    only if the rebuild succeeds.
  - `list` never aborts on one bad entry; the bad row carries an `error`. `info` and `play` are
    strict.
  - `import` records the source's SHA-256 (`auto_ascii_factory::sha256_file`, streamed) in the
    `<name>.json` sidecar.
  - `play` is interactive only (no `--json`). For headless checks use `auto-ascii-player --sim`.

### 13. Embedding: `Player` and `RenderSession`
- **Does:** the published library API.
- **Code:** `crates/auto-ascii/src/lib.rs` (re-exports `Player`, `PlayerBuilder`,
  `RenderSession`, `Composition`, `Codec`, `Dial`, `Error`, `Cell`, `Grid`, `Rgb`).
  `crates/auto-ascii/src/session.rs` `RenderSession` (`open`, `open_composition`, `render`,
  `set_palette`, `set_font_table`, `set_codec`, `set_cell_aspect`).
  `crates/auto-ascii/src/error.rs` `Error`. Examples:
  `crates/auto-ascii/examples/simple-play.rs`, `crates/auto-ascii/examples/embedded-loop.rs`,
  `crates/auto-ascii/examples/headless-dump.rs`.
- **Invariants:**
  - Feature tiers (`crates/auto-ascii/Cargo.toml`): `bin` (default; clap + anyhow) implies
    `terminal` (crossterm session, `Player`), which implies `compose` (TOML compositions).
    `default-features = false` is `RenderSession` plus the engine only.
  - `RenderSession::render` advancing monotonically is full quality. A backward jump resets
    temporal state automatically.
  - `pipeline` is `#[doc(hidden)]`: a workspace harness contract, exempt from semver.

### 14. Headless rendering
- **Does:** renders without a terminal, for tests, goldens and checks on a box with no TTY.
- **User:** `auto-ascii-player <asset> --sim COLSxROWS:N [--sim-tier T] [--sim-dump PATH]
  [--sim-resize WxH]` prints one JSON stats line. `--bench-seek N` reports scrub latency.
  `cargo run --release -p auto-ascii --example headless-dump -- <asset|comp.toml> [FRAMES]
  [COLSxROWS] [--codec C] [--palette P] [--from F]` prints frames as text.
- **Code:** `crates/auto-ascii/src/bin/auto-ascii-player.rs` (the `--sim` harness drives
  `ClipDeck` against `SimBackend`) and `crates/auto-ascii/examples/headless-dump.rs`.

### 15. Eval, sweep and the gates
- **Does:** measures the real renderer on a local corpus (`eval`), searches parameter space
  (`sweep`), and gates every change (`scripts/eval.sh`).
- **User:** `auto-ascii-factory eval --corpus DIR [--params F] [--baseline B.json] --out X.json
  [--html X.html] [--reel R.html] [--cache-dir D] [--font-table NAME|PATH]`.
  `auto-ascii-factory sweep --corpus DIR --grid G.toml --out DIR`. `scripts/eval.sh`.
- **Code:** `crates/auto-ascii-factory/src/eval.rs` `run` / `eval_clip` builds or reuses each
  asset (`asset_cache_name`: input sha + `Params::build_fingerprint` + `PIPELINE_FINGERPRINT`
  from `crates/auto-ascii-factory/build.rs`). It drives `pipeline::Player` against `SimBackend`
  and scores:
  - downscale-SSIM (`crates/auto-ascii-eval/src/raster.rs`, `crates/auto-ascii-eval/src/ssim.rs`,
    `crates/auto-ascii-eval/src/coverage.rs`);
  - edge F1 against Canny on the *source* (`crates/auto-ascii-eval/src/edge.rs`);
  - flicker on static segments (`crates/auto-ascii-eval/src/flicker.rs`);
  - damage and bytes per tier (`crates/auto-ascii-eval/src/stats.rs`);
  - asset structure.
  The report is `crates/auto-ascii-eval/src/report.rs` `EvalReport`, and the baseline compare is
  `crates/auto-ascii-eval/src/compare.rs`. The review reel is
  `crates/auto-ascii-factory/src/reel.rs`. Sweeps are `crates/auto-ascii-factory/src/sweep.rs`
  `run`, `enumerate_combos`, `score_of`, `rank`. Synthetic fixtures are
  `crates/auto-ascii-eval/src/fixtures.rs` (`Fixture`, `FixtureRenderer`, `snapshot`,
  `write_bgr24_avi`).
- **Invariants:**
  - No self-grading: SSIM normalizes the source with eval-owned percentiles, and edge truth is
    Canny on the source, never the factory's E plane.
  - `[compose]` and `[eval]` knobs never invalidate the asset cache. Any factory or format source
    change does.
  - Stage times are informational and never gate the compare. `scripts/perf-gate.sh` is the
    timing gate.
  - Committed tests never depend on the corpus; synthetic fixtures back every golden, fuzz and
    perf check.

## Data & wire

**Feature planes** (`crates/auto-ascii-format/src/header.rs` `plane_id`; producer
`crates/auto-ascii-factory/src/features.rs`):

| Plane | Id | Size | Meaning |
|---|---|---|---|
| `Y` | 1 | base_w × base_h u8 | CIE L\* luma, EMA'd; levels applied at runtime from NORM |
| `E` | 2 | base u8 | edge magnitude ≈ L\* contrast, unthinned, EMA'd |
| `Ex` / `Ey` | 3 / 4 | base u8 | doubled-angle gradient orientation, `128 + (v >> 1)` (bias-128, half scale) |
| `H` | 5 | base u8 | bit0 highlight (top-hat), bit1 deep shadow |
| `C` | 6 | (base_w/2) × (base_h/2) × u16 LE | RGB565 chroma, 2×2 area average, EMA'd |

Absent planes disable their layers, so older Y+C assets still play.

**ASCI header** (64 B, little-endian, `crates/auto-ascii-format/src/header.rs` `AsciiHeader`):
`magic "ASCI"`, `version_major/minor`, `header_size`, `flags` (bit0 index, bit1 CRCs),
`fps_num/den`, `base_w/h`, `aspect_num/den`, `frame_count`, `plane_count`, `codec` (2 = zstd),
`filter` (1 = temporal delta), `keyframe_ivl`, `plane_ids[8]`, `index_offset`, `meta_offset`.
Chunks are `tag | flags | size | payload | crc32` (`crates/auto-ascii-format/src/chunk.rs`).
Plane subblocks are 64-byte aligned. NORM records are 24 B: `first_frame u32`, `flags u8` (bit0
cut), then eight `(p2, p98)` pairs indexed by plane position (`crates/auto-ascii-format/src/norm.rs`).

**Per-video settings** (`<asset stem>.player.toml`, `crates/auto-ascii/src/settings.rs`):

```toml
codec = "letters"
shadow_lift = 64
edge_t_on = 32
idx_hyst_q8 = 160
```

**Library sidecar and composition schema:** `docs/AGENT-GUIDE.md` (JSON shapes, `schema = 1`
TOML) and `crates/auto-ascii-cli/src/library.rs` `Sidecar`.

**`params.toml`** (repo root; embedded into the factory by
`crates/auto-ascii-factory/src/params.rs` `EMBEDDED_PARAMS`):
- `--params FILE` overrides any subset, unknown keys are errors, and `params --dump` prints the
  merged config.
- `crates/auto-ascii-factory/tests/m2_params_eval.rs` pins the file to the in-code defaults, and
  `[compose]` to `ComposeParams::default()`.
- Editing `[build]`, `[shots]`, `[levels]`, `[edges]`, `[highlights]` or `[temporal]` changes
  asset bytes and moves the byte pin.

| Key | Default | Meaning |
|---|---|---|
| `build.fps` | 30 | output frame rate (ffmpeg `fps` filter) |
| `build.base_w` / `base_h` | 480 / 270 | stored plane resolution; both even and ≥ 2 (C is half res) |
| `build.zstd_level` | 15 | frame compression; zstd is lossless, so only size and build time move. `WriterOptions::default()` stays at 19 |
| `build.keyframe_ivl` | 60 | keyframe cadence, 1..=255 (stored as u8) |
| `shots.sad_threshold_milli` | 300 | cut threshold, thousandths of the maximum histogram SAD (hard cuts land ~500–1200, in-shot motion ~20–150) |
| `shots.min_shot_frames` | 8 | minimum shot length; debounces flashes |
| `levels.lo_pct` / `hi_pct` | 2 / 98 | per-shot percentile levels stored in NORM |
| `edges.scharr_shift` | 4 | right shift on raw Scharr magnitude; at 4, E of a sharp edge ≈ its L\* contrast |
| `edges.bilateral_passes` / `bilateral_radius` | 2 / 2 | orientation smoothing passes and window radius (window 2r+1) |
| `edges.t_hi` / `t_lo` | 28 / 12 | edge hysteresis: ≥ t_hi seeds, t_lo..t_hi survives only when 8-connected to a seed |
| `highlights.tophat_radius` | 3 | top-hat structuring-element radius; wider bright features are areas, not highlights |
| `highlights.tophat_thresh` | 48 | minimum top-hat response (L\*) for the highlight bit |
| `highlights.shadow_pct` / `shadow_max_l` | 8 / 40 | deep shadow iff `y ≤ min(percentile(shadow_pct), shadow_max_l)` |
| `temporal.ema_alpha_{y,e,c}_milli` | 700 / 500 / 700 | per-plane EMA weight of the new frame in thousandths (1000 = off); reset at cuts |
| `compose.edge_t_on` / `edge_t_off` | 32 / 16 | runtime edge gate on/hold thresholds (strict >) on the cell-averaged E plane |
| `compose.coh_min_q8` / `coh_dir_q8` | 96 / 160 | coherence bands: below min, no edge; min..dir, junction glyph; ≥ dir, directional glyph |
| `compose.hi_cut_q8` | 160 | highlight fires only while the base index < len × hi_cut / 256 |
| `compose.edge_white_cut_q8` | 240 | edges never override a near-white base cell |
| `compose.halfblock_min_delta` | 64 | top/bottom luma delta that counts as "large" (half-block, quadrant, subposition) |
| `compose.edge_strong` | 96 | ASCII junction `+` upgrades to `#` at this magnitude |
| `compose.quad_e_on` / `quad_e_off` | 2 / 1 | quadrant-refinement noise floor (arm/hold), Unicode tiers only |
| `compose.idx_hyst_q8` | 160 | ramp-index hysteresis width as a Q8 fraction of one step (< 1 step) |
| `compose.shadow_lift` | 0 | bends the NORM LUT toward the shadows (0 off, 255 a full sqrt curve); endpoints fixed |
| `eval.grid_cols` / `grid_rows` | 300 / 80 | eval render grid |
| `eval.max_frames` | 900 | frames per clip per tier (0 = all) |
| `eval.ssim_every` | 30 | SSIM sampled every N-th frame |
| `eval.contact_frames` | 3 | contact-sheet snapshots per clip |
| `eval.tolerances.*` | see file | baseline-compare tolerances: `ssim_max_drop`, `flicker_max_increase`, `edge_f1_max_drop`, `bytes_frac_max_increase`, `damage_rate_max_increase`, `stage_ms_frac_max_increase` (informational only), `shot_structure_max_delta`, `keyframes_frac_max_drop`, `asset_bytes_frac_max_increase` |

**Sweep grid** (`--grid G.toml`, `crates/auto-ascii-factory/src/sweep.rs` `SweepSpec`): values
within one `[[axes]]` entry travel together, and axes cross. Combos that fail validation are
recorded as skipped. The default score is
`0.4·mean(ssim) + 0.4·mean(edge_f1) − 0.2·mean(flicker / 2.0)`. Outputs are `combo-NN.json`,
`sweep.json` and `leaderboard.html`.

```toml
[score]
ssim = 0.4
edge_f1 = 0.4
flicker = 0.2
flicker_norm = 2.0

[[axes]]
name = "edge-runtime"
values = [
  { "compose.edge_t_on" = 24, "compose.edge_t_off" = 12 },
  { "compose.edge_t_on" = 32, "compose.edge_t_off" = 16 },
]
```

**Perf thresholds** (`perf/thresholds.toml`): `[bench.<criterion id>] max_median_ns`,
`measured_median_ns`. The procedure is in `CONTRIBUTING.md`.

## Tests

| Area | Files |
|---|---|
| Engine units + properties | `crates/auto-ascii-core/tests/codec_props.rs`, `crates/auto-ascii-core/tests/compose_props.rs`, `crates/auto-ascii-core/tests/viewport_props.rs`, unit tests in each `crates/auto-ascii-core/src/` module |
| Container | `crates/auto-ascii-format/tests/container.rs` (byte golden), `crates/auto-ascii-format/tests/m1_format.rs` (delta, seek, NORM, hostile input) |
| Terminal | `crates/auto-ascii-term/tests/m1_tiers.rs`, `crates/auto-ascii-term/tests/tier_goldens.rs`, `crates/auto-ascii-term/tests/sim_diff.rs`, `crates/auto-ascii-term/tests/probe_parser.rs`, `crates/auto-ascii-term/tests/pty_probe.rs`, `crates/auto-ascii-term/tests/pty_restore.rs`, `crates/auto-ascii-term/tests/terminal_identity.rs` |
| Cell-grid goldens | `crates/auto-ascii-eval/tests/golden_grids.rs` (36 insta snapshots), `crates/auto-ascii/tests/linux_console_golden.rs`, `crates/auto-ascii/tests/pipeline_parity.rs`, `crates/auto-ascii/tests/codecs.rs` (`letters` goldens) |
| Player | `crates/auto-ascii/tests/m1_sim.rs`, `crates/auto-ascii/tests/m3_layers.rs`, `crates/auto-ascii/tests/sim_e2e.rs`, `crates/auto-ascii/tests/scrub_overlay.rs`, `crates/auto-ascii/tests/zoom_overlay.rs`, `crates/auto-ascii/tests/dials.rs`, `crates/auto-ascii/tests/render_session.rs` |
| Compositions | `crates/auto-ascii/tests/composition.rs`, unit tests in `crates/auto-ascii/src/deck.rs` and `crates/auto-ascii/src/composition.rs` |
| Fuzz / perf | `crates/auto-ascii/tests/resize_fuzz.rs`, `crates/auto-ascii/tests/perf_fps.rs`, `crates/auto-ascii/benches/pipeline.rs` |
| Factory | `crates/auto-ascii-factory/tests/build_e2e.rs`, `crates/auto-ascii-factory/tests/m2_params_eval.rs` (params plumbing, byte pin, eval/sweep) |
| Metrics | `crates/auto-ascii-eval/tests/metrics.rs` |
| CLI | `crates/auto-ascii-cli/tests/cli.rs` |

## Related docs

- [README.md](../README.md): what it is, install, quick start, controls.
- [CONTRIBUTING.md](../CONTRIBUTING.md): build, the `scripts/eval.sh` gate, determinism rules,
  perf calibration, publishing.
- [AGENT-GUIDE.md](AGENT-GUIDE.md): the CLI for agents, JSON shapes, composition schema.
- [PLAN.md](PLAN.md) and [PLAN-M6-M8.md](PLAN-M6-M8.md): the original engine and tool-layer
  designs (the "§" references in older code history point here).
- [INTERFACES.md](INTERFACES.md): the internal API registry and decision log.
- [TERMINAL-CHECKLIST.md](TERMINAL-CHECKLIST.md): the manual per-terminal pass.
- [research/zoom.md](research/zoom.md): why the player hints at zoom instead of changing it.
- [research/](research/): pre-implementation digests (algorithms, file format, grid math,
  terminals).

## Agent navigation tips

- **Start here:** `crates/auto-ascii/src/pipeline.rs` `Player::render_grid` is the whole runtime
  in one function (decode → levels → resample → `compose_frame_codec` → overlays). Then read
  `crates/auto-ascii/src/player.rs` `Player::run` for the interactive loop, and
  `crates/auto-ascii-factory/src/build.rs` `run` for the factory.
- **"Where does a key do X?"** Keys map in `drain_backend_events` (`crates/auto-ascii/src/pipeline.rs`).
  The meaning is applied in `Player::run` (`crates/auto-ascii/src/player.rs`). The hint text is
  `hint_line`. A new key touches all three, plus `crates/auto-ascii/tests/scrub_overlay.rs`.
- **Changing the look** is a codec change (`crates/auto-ascii-core/src/codec/`) or a `[compose]`
  tunable, never an asset change. Expect insta snapshots, `.ansi` tier goldens, the console
  golden and `pipeline_parity` to move for `pixels`. Re-pin deliberately (`CONTRIBUTING.md`).
- **Changing the factory** moves `FIXTURE_ASSET_SHA` in
  `crates/auto-ascii-factory/tests/m2_params_eval.rs` and invalidates every eval cache entry.
- **Two players:** `pipeline::Player` (per-clip frame pipeline, no clock, no tty) is not
  `auto_ascii::Player` (the blocking terminal session that owns a `ClipDeck` of them).
- **Two "codecs":** `auto_ascii_core::Codec` (glyph codecs, render time) is not the container's
  `header::codec` (zstd). The player links no *video* codecs.
- **Don't "fix":** the player has no always-on chrome (overlays are event-driven), there is no
  SSH/tmux/throughput tuning, `WriterOptions::default()` stays at zstd 19 while the factory uses
  15, and `pipeline` stays `#[doc(hidden)]`. All four are deliberate (`CONTRIBUTING.md`).
