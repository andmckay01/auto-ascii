# INTERFACES.md — frozen M0 public API

Written by the scaffold agent. This freezes the M0 `pub` surface pinned in code.
Implementers fill `todo!()` bodies; **signature/layout changes require updating
this file and a deliberate decision** — the factory⇄player format contract
(PLAN §4) and the §3.1 types are the riskiest interfaces in the system.

## Workspace & dependency edges (PLAN §2, §8)

```
crates/
  slpy-core       lib   deps: (none beyond std)
  slpy-term       lib   deps: slpy-core, crossterm, libc
  slpy-format     lib   deps: zstd, crc32fast, ciborium, serde(derive, META struct only)
  sleepy-factory  bin   deps: slpy-format, clap, indicatif, serde_json
  sleepy-player   bin   deps: slpy-core, slpy-term, slpy-format, memmap2, clap, anyhow
```

- `slpy-eval` deferred to M2 (not created).
- Root workspace: resolver 3, edition 2024, `license = "MIT OR Apache-2.0"`,
  `[profile.release] opt-level = 3`, all versions via `[workspace.dependencies]`.
- Factory deps `image`/`imageproc`/`ndarray`/`rayon` (PLAN §8) deliberately
  deferred: M0 is luma-only from ffmpeg rawvideo — add at M1/M3 when a stage
  needs them (per M0 scoping guidance).

## slpy-core (PLAN §3.1–§3.4; pure, std-only)

```rust
// cell.rs (§3.1)
#[repr(C)] pub struct Rgb { pub r: u8, pub g: u8, pub b: u8 }          // 3 B POD
impl Rgb { pub const BLACK; pub const WHITE;
           pub const fn new(r,g,b) -> Rgb; pub const fn gray(v: u8) -> Rgb }
pub mod attrs { pub const NONE: u8 = 0; }
#[repr(C)] pub struct Cell { pub ch: u32, pub fg: Rgb, pub bg: Rgb, pub attrs: u8 }
// compile-time asserted: size_of::<Cell>() == 12 (memcmp-able POD; bg is
// load-bearing for M3 half-blocks — do not remove)
impl Cell { pub const BLANK; pub const fn new(ch: char, fg, bg) -> Cell;
            pub fn glyph(&self) -> char }   // Default = BLANK

// grid.rs (§3.1, §6)
pub struct Grid<T>;   // dense row-major, (col,row) indexed, u16 dims
impl<T: Copy + Default> Grid<T> {
  pub fn new(cols: u16, rows: u16) -> Grid<T>;
  pub fn cols/rows() -> u16;  pub fn len() -> usize;  pub fn is_empty() -> bool;
  pub fn resize(&mut self, cols, rows);   // realloc + reset; THE hot-path alloc point
  pub fn fill(&mut self, v: T);
  pub fn get(&self, col, row) -> T;  pub fn set(&mut self, col, row, v: T);
  pub fn row/row_mut(row) -> &[T]/&mut [T];  pub fn as_slice/as_mut_slice();
}

// viewport.rs (§3.2) — IMPLEMENTED + worked-example tests (80×24→80×23,
// 213×58→206×58 pads 3/4/0/0, 320×90 exact)
pub const DEFAULT_CELL_ASPECT: f64 = 2.0;
pub const MIN_COLS: u16 = 32;  pub const MIN_ROWS: u16 = 9;
pub struct Viewport { pub cols, rows, pad_left, pad_right, pad_top, pad_bottom: u16 }
pub fn compute_viewport(term_cols: u16, term_rows: u16, cell_aspect: f64)
    -> Option<Viewport>;   // None = below 32×9 → "enlarge terminal" card

// resample.rs (§3.3) — IMPLEMENTED
pub struct Tap1D { pub src_start: u16, pub ntaps: u16, pub w_off: u32 } // Q8, sum 256
// ntaps widened u8→u16 by the slpy-core implementer: 480 src cols → 1 dst col
// (legal §6 fuzz case) needs 480 taps in one run, overflowing u8. See note 1b.
pub struct Resampler;   // private: taps_x/taps_y, shared weight pool, shared u16 hbuf
impl Resampler {
  pub fn build(src_w, src_h, dst_w, dst_h: u16) -> Resampler;  // resize-only, ~50 µs
  pub fn apply(&mut self, src: &[u8], dst: &mut [u8]);         // zero alloc, no floats
  pub fn src_dims/dst_dims() -> (u16, u16);
}

// ramp.rs (§3.4, palettes 1–2 as data) — IMPLEMENTED
pub const ASCII_BASE_COARSE: &[char];      // " .:-=+*#%@"
pub const ASCII_BASE_FINE:   &[char];      // " .,:;i1tfLCG08@"
pub const FINE_MIN_COLS: u16 = 70;
pub fn base_ramp_for_cols(viewport_cols: u16) -> &'static [char];
pub fn ramp_glyph(ramp: &[char], n: u8) -> char;   // ramp[(n·len)>>8]

// compose.rs (§3.4 L0-only, M0) — ADDED by the slpy-core implementer (task e);
// M0 compositor: base ramp glyph + Rgb::gray(n) fg + black bg in the viewport,
// Cell::BLANK pads. Never allocates; `out` must already be term-grid-sized.
// Panics on grid/viewport mismatch, short luma, or empty ramp.
pub fn compose_luma(luma: &[u8], vp: &Viewport, ramp: &[char], out: &mut Grid<Cell>);
```

## slpy-term (PLAN §3.1, §3.6) — M1: caps probe + color tiers + ?2026

```rust
// caps.rs — M1: `Throughput` enum + `Caps.throughput` REMOVED (Scope
// amendment: capability tiers are color depth + glyph repertoire only; no
// connectivity classification). Deliberate break from the M0 freeze.
pub enum ColorTier { True, C256, C16, Mono }
impl FromStr for ColorTier;      // --tier parsing: truecolor|256|16|mono (+aliases)
pub struct GlyphFlags(pub u8);   // consts ASCII/BLOCKS/BOX_DRAWING/BRAILLE; contains(), with()
pub enum GlyphSupportTier { AsciiOnly, Cp437, UnicodeCore, UnicodeFull }
pub struct Caps { pub color: ColorTier, pub glyphs: GlyphFlags,
                  pub glyph_support: GlyphSupportTier, pub sync_2026: bool,
                  pub cells: (u16,u16), pub cell_px: Option<(u16,u16)>,
                  pub can_query: bool }
impl Default for Caps;  // kitty-class: True color, ASCII, (80,24)
pub struct FrameStats { pub bytes: u32, pub cells_damaged: u32,
                        pub write_ns: u64, pub dropped: bool }

// probe.rs — NEW at M1 (PLAN §3.1 capability detection, minus connectivity)
pub const DEFAULT_PROBE_TIMEOUT: Duration;  // 200 ms (PLAN: 150–250 local)
pub const VOLLEY: &[u8];  // ONE write: XTVERSION, DECRQM 2026, XTGETTCAP RGB,
                          // CSI 16 t, then DA1 (CSI c) LAST as sentinel
pub struct ProbeOptions { pub forced_tier: Option<ColorTier>,  // --tier
                          pub no_query: bool,                  // --no-query
                          pub no_cache: bool, pub timeout: Duration,
                          pub cache_dir: Option<PathBuf> }     // + Default
pub fn probe_caps(&ProbeOptions) -> Caps;
// Never hangs: !isatty(stdin/stdout) or DA1 silence → conservative default
// (256-color, ASCII glyphs). tty flow: passive env hints (COLORTERM / TERM /
// TERM_PROGRAM / locale→glyph tier) as base; volley upgrades (XTGETTCAP RGB
// valid → True; DECRPM 2026 Ps∈1..=4 → sync_2026; CSI 16 t → cell_px);
// forced_tier overrides color last. Result cached at
// $XDG_CACHE_HOME/sleepytime/caps (fallback ~/.cache) keyed on
// (TERM, TERM_PROGRAM, tmux?); silence is never cached. The volley runs
// under a Drop-guarded termios (echo/canon off) and drains stragglers so no
// reply bytes leak into the app's input.
pub struct ProbeParser;   // incremental VT reply parser (pure; scripted-byte tests)
impl ProbeParser { pub fn new(); pub fn feed(&mut self, &[u8]) -> bool /*DA1 seen*/;
                   pub fn done(&self) -> bool; pub fn replies(&self) -> &ProbeReplies }
pub struct ProbeReplies { pub xtversion: Option<String>, pub decrqm_2026: Option<u8>,
                          pub xtgettcap_rgb: Option<bool>,
                          pub cell_px: Option<(u16,u16)>, pub da1: bool }

// quant.rs — NEW at M1 (PLAN §3.1 quantize-before-diff; pure math)
pub fn rgb_to_256(Rgb) -> u8;     // xterm 6×6×6 cube (16–231) + gray ramp (232–255)
pub fn rgb_to_16(Rgb) -> u8;      // nearest of the standard 16 (xterm defaults)
pub fn ansi256_to_rgb(u8) -> Rgb; // canonical inverse (roundtrip-exact 16..=255)
pub fn ansi16_to_rgb(u8) -> Rgb;

// event.rs — crossterm types never leak into the pub API
pub enum Key { Char(char), Ctrl(char), Esc }
pub enum Event { Resize(u16, u16), Key(Key), Quit }
pub struct EventQueue;  // new/push/pop/is_empty/clear (implemented, VecDeque)

// backend.rs (§3.1) — exactly AnsiBackend + SimBackend implement this
pub trait Backend {
  fn caps(&self) -> &Caps;
  fn events(&mut self) -> &mut EventQueue;
  fn present(&mut self, grid: &Grid<Cell>) -> FrameStats; // quantize→diff→elide→ONE write
  fn invalidate(&mut self);          // full repaint next present (M0 default: every frame)
  fn resize(&mut self, cols: u16, rows: u16);  // ONLY hot-path allocation point
  fn shutdown(&mut self);            // idempotent restore; also from Drop + signals
}
// Shared painter semantics (both backends, M1): cells are quantized to
// CANONICAL tier RGB BEFORE the diff (True = identity, byte-parity with M0),
// so equal-after-quantize cells produce zero damage; SGR per tier —
// 38;2;R;G;B / 38;5;N / 30–37;90–97 (bg 40–47;100–107) / none on Mono; every
// non-empty frame wrapped in CSI ?2026h…l iff Caps.sync_2026 (empty frames
// emit nothing, no bare wrap).

// ansi.rs — M1: accepts ALL color tiers (the M0 truecolor-only guard is gone)
pub struct AnsiBackend;
impl AnsiBackend { pub fn new(caps: Caps) -> std::io::Result<AnsiBackend> } // enters session
// sim.rs — all headless M0/M1 verification runs here (no GUI on this box)
pub struct SimBackend;
impl SimBackend {
  pub fn new(cols: u16, rows: u16) -> SimBackend;              // implemented
  pub fn set_throughput(&mut self, bytes_per_sec: Option<u64>); // implemented (2 MB/s gate @M2)
  pub fn push_event(&mut self, ev: Event);                      // implemented
  pub fn take_output(&mut self) -> Vec<u8>;                     // implemented
  pub fn set_caps(&mut self, caps: Caps);   // NEW at M1: tier/sync tests; keeps current cells
}

// restore.rs (§3.1 session hygiene; M0 acceptance 3 pty test)
pub const RESTORE_SEQ: &[u8] = b"\x1b[0m\x1b[?25h\x1b[?7h\x1b[?1049l";
pub fn install_restore_hooks();   // panic hook + SIGINT/SIGTERM + atexit
```

## slpy-format (PLAN §4; container only, no I/O policy) — M1: full SLPY v1

```rust
// header.rs — 64-B layout frozen (offset-freezing tests)
pub const MAGIC: [u8;4] = *b"SLPY"; pub const HEADER_SIZE: u32 = 64;
pub const VERSION_MAJOR: u16 = 1;   pub const VERSION_MINOR: u16 = 1; // M1 bump (additive)
pub const BASE_W: u16 = 480;        pub const BASE_H: u16 = 270;
pub mod plane_id { Y=1, E=2, EX=3, EY=4, H=5, C=6;
                   pub const fn is_known(id: u8) -> bool }   // 1..=6
pub fn plane_raw_size(base_w, base_h, id: u8) -> Option<usize>;
    // Y/E/Ex/Ey/H = w×h u8; C = (w/2)×(h/2)×2 (RGB565); None for unknown ids
    // (their raw size travels in the FRAM subblock header — skip-unknown, §4)
pub mod codec   { RAW=0, LZ4=1, ZSTD=2 }
pub mod filter  { INTRA=0, TEMPORAL_DELTA=1 }
pub mod header_flags { INDEX_PRESENT=1, CRCS_PRESENT=2 }
pub struct SlpyHeader { ... unchanged ... }  // + to/from_bytes

// chunk.rs — unchanged framing (16-B chunk header, 16-B FIDX entry)
// FIDX entry flags mirror FRAM flags (bit0 = KEYFRAME) — the seek roster.

// norm.rs — NEW at M1: NORM chunk records (runtime per-shot levels, PLAN §4/§5)
pub mod norm_flags { CUT=1 }        // bit0: hard cut (player resets hysteresis)
pub const NORM_RECORD_SIZE: usize = 24;  // fixed rows, flat, binary-searchable
pub struct PlaneLevels { pub p2: u8, pub p98: u8 }
pub struct ShotRecord { pub first_frame: u32, pub flags: u8,
                        pub levels: [PlaneLevels; 8] }  // + to/from_bytes, is_cut()
// levels indexed by plane POSITION in header plane_ids (not by plane id);
// wire: first_frame u32 | flags u8 | pad u8×3 | (p2,p98)×8

// meta.rs / error.rs — unchanged (Meta CBOR via ciborium; SlpyError as at M0)

// write.rs — M1 stream: HEADER | META | [NORM] | FRAM×n | FIDX | TRLR
pub struct WriterOptions { ... same fields ... }
// Default CHANGED at M1: filter = TEMPORAL_DELTA (keyframe_ivl 60, zstd-19,
// [Y], crc on). INTRA remains valid (M0 profile). new() now also rejects:
// fps_num/fps_den == 0 (adversarial-review fix), plane ids outside the known
// registry (writer needs geometry).
impl SlpyWriter<W> {
  pub fn new(w, opts, meta: &Meta) -> Result<Self>;
  pub fn write_norm(&mut self, shots: &[ShotRecord]) -> Result<()>;
      // NEW: ≤1×, before the first frame; first_frame strictly increasing from 0
  pub fn write_frame(&mut self, planes: &[PlaneRef<'_>]) -> Result<()>;
      // TEMPORAL_DELTA: frame_idx % keyframe_ivl == 0 ⇒ intra + KEYFRAME flag,
      // else per-byte cur−prev (mod 256) per plane before zstd. Rejected frames
      // never touch the delta reference (tested).
  pub fn finish(self) -> Result<W>;
}

// read.rs — over &[u8]; open() now O(pre-frame chunks + FIDX): header
// validation, TRLR tail anchor, META/NORM roll, whole-FIDX load + keyframe
// roster. It never touches FRAM payloads (cold open+seek stays <50 ms on
// multi-GB assets); per-frame structure re-validated on every decode;
// verify() is still the full walk + CRC pass. open() now also rejects
// (clean SlpyError, adversarial-review fixes): base_w/h == 0, fps_num/den
// == 0, keyframe_ivl == 0, dup/zero plane ids, unknown filter, malformed
// NORM, delta asset whose frame 0 is not a keyframe, non-increasing FIDX,
// index_offset near u64::MAX (checked_add bounds check — the unchecked add
// wrapped and open() panicked at the FIDX header slice).
impl SlpyReader<'a> {
  pub fn open(bytes: &'a [u8]) -> Result<Self>;
  pub fn header/frame_count/meta();                 // unchanged
  pub fn plane_dims(&self, plane_id: u8) -> Option<(u16,u16)>;
      // now None for unknown (future) ids even when present in the registry
  pub fn shots(&self) -> &[ShotRecord];             // NEW ([] if no NORM)
  pub fn shot_for_frame(&self, frame: u32) -> Option<&ShotRecord>;  // NEW, bsearch
  pub fn plane_index(&self, plane_id: u8) -> Option<usize>;         // NEW, levels idx
  pub fn norm_levels(&self, frame: u32, plane_id: u8) -> Option<PlaneLevels>; // NEW
  pub fn is_keyframe(&self, frame: u32) -> Result<bool>;            // NEW
  pub fn nearest_keyframe_at_or_before(&self, frame: u32) -> Result<u32>; // NEW, bsearch
  pub fn decode_plane_into(&mut self, frame, plane_id, dst) -> Result<usize>;
      // SEMANTICS EXTENDED: keyframes/INTRA decode standalone; delta frames
      // REQUIRE dst to hold the decoded previous frame (standing double
      // buffer) — delta is decoded to internal scratch and memadded in place.
      // Sequential playback loops call it exactly as at M0.
  pub fn seek_plane_into(&mut self, frame, plane_id, dst) -> Result<usize>;
      // NEW (PLAN §4 seek): keyframe bsearch + ≤ keyframe_ivl−1 delta rolls;
      // dst contents on entry irrelevant. Frame-skipping players MUST use
      // this (or roll every frame) on TEMPORAL_DELTA assets.
  pub fn verify(&self) -> Result<()>;
}
```

## Binaries

- `sleepy-factory` (PLAN §5): subcommands `build <in> -o <out> [--ss T]
  [--t T] [--fps N] [--res WxH] [--params]`, `inspect <asset>` (`eval`/`sweep`
  at M2). M1 build = two passes over the identical ffmpeg rgb24 decode:
  **pass 1** L\* luma → shot detection (256-bin histogram SAD ≥ 0.30
  normalized, min shot length 8 frames — every honored boundary is a hard
  cut) + per-shot pooled p2/p98; **pass 2** NORM (levels applied at RUNTIME —
  M0's baked-in global stretch is REMOVED, the LUT folds only
  sRGB→linear→L\*) + Y (L\*, full res) + C (RGB565 little-endian, half res,
  2×2 area average) planes through the v1 writer default profile (temporal
  delta, keyframe interval 60, zstd-19, CRCs). NORM levels: position 0 (Y)
  = shot p2/p98; position 1 (C) = (0,0), chroma is never stretched. Output
  goes to `<out>.part`, renamed only after `finish()`. `inspect` additionally
  reports shots + cut flags, keyframe count, per-plane compressed/raw sizes,
  and compression ratio vs raw planes.
- `sleepy-player` (PLAN §3), CLI as of M1 (see notes 9 and 11):
  `<asset> [--repaint full|diff] [--loop] [--fps-cap FPS] [--cell-aspect F]
  [--duration-secs N] [--seek TIMESTAMP] [--tier TIER] [--no-query]
  [--no-cache] [--sim COLSxROWS:NFRAMES] [--sim-tier TIER] [--sim-dump PATH]
  [--sim-resize [COLSxROWS]]`.
  Default `--repaint full` = invalidate-every-frame (§3.1/§7 one render path).
  M1: interactive startup runs `probe_caps` (§3.1) — `--tier TIER`
  (truecolor|256|16|mono) and `--no-query` both SKIP the volley (passive
  hints only; the forced tier still wins), `--no-cache` bypasses the probe
  cache; `--sim` never probes. `--seek` accepts seconds ("42.5") or colon
  form ("1:30", "0:01:30.5") and starts playback there via the FIDX seek
  path; interactively keys 0–9 jump to 0–90%. `--sim` is the headless
  acceptance path (renders NFRAMES as fast as possible to SimBackend,
  prints one JSON line: `{fps, frames, bytes_total, avg_bytes_per_frame,
  tier, stage_ms:{decode,resample,compose,present}, grid_after}` — `tier`
  added at M1); `--sim-tier` sets the simulated color tier (falls back to
  `--tier`, default truecolor); `--sim-dump PATH` writes the concatenated
  raw escape stream of every presented frame (byte-level tier checks);
  `--sim-resize` (default value 100x40, requires `--sim`) injects a
  `Event::Resize` at frame NFRAMES/2 through the same event path the
  interactive loop uses, proving next-frame reflow.

## M0 scope notes & deviations from PLAN

1. **`Tap1D` weights**: PLAN/digest sketch inline `w: [u16]` / `w[MAXTAP]`;
   frozen API stores `w_off: u32` into a shared pool in `Resampler`. Reason: a
   fixed MAXTAP can't cover legal extreme downscales (480 src cols → 1-col
   viewport under §6 resize fuzz); the pool keeps `Tap1D` fixed-size POD with
   identical semantics (Q8, per-run sum 256).
   **1b (implementation follow-up):** `Tap1D::ntaps` widened `u8` → `u16` —
   the same 480→1 collapse puts 480 taps in a single run, which overflows
   `u8`. Weight *construction* is exact integer rational (cumulative method,
   `floor(cum·256/span)` deltas), so every run sums to exactly 256 with no
   floats anywhere in resample.rs; V-pass emits `(acc + 0x8000) >> 16`
   (round-half-up, still shift-out, still float-free). No external code
   consumed `Tap1D` at the time of the change.
2. **Palette 2 length**: PLAN §3.4 says "16-step" but specifies 15 glyphs
   (`" .,:;i1tfLCG08@"`); the glyph string is authoritative → 15 entries.
3. **`Cell` layout**: 11 payload bytes + 1 tail pad = 12 B (`align 4`),
   compile-time asserted; constructors (`new`/`BLANK`) leave padding zeroed.
4. **Factory deps trimmed** for M0 (see edges above). **`slpy-eval` absent**
   (M2). **NORM chunk not written at M0** (tag + registry reserved here so M1
   is additive, not a format break).
5. **Implemented now** (beyond skeletons): viewport math + worked-example
   tests, ramps, Grid/Cell, header/chunk byte codecs + layout-freezing tests,
   EventQueue, SimBackend construction/throttle/capture plumbing,
   **Resampler build/apply + `compose_luma` (slpy-core complete for M0)**.
   `todo!()`: both `present`/`resize` paths, AnsiBackend session setup,
   restore hooks, SlpyWriter/SlpyReader streaming bodies, both bin mains.
6. **`compose_luma` added** (not in the original freeze; mandated by the
   slpy-core task item e): `compose::compose_luma(luma, &Viewport, ramp,
   &mut Grid<Cell>)`, re-exported at crate root. L0-only M0 compositor —
   ramp glyph + gray fg from the same luma sample, BLANK pads, zero
   allocation.
7. **slpy-format M1 upgrade** (M1 format agent): full SLPY v1 per the section
   above. Deliberate format changes: `VERSION_MINOR` 0→1;
   `WriterOptions::default()` filter is now `TEMPORAL_DELTA`; the committed
   byte golden (`GOLDEN_SHA256` in `tests/container.rs`) was re-baselined.
   Back-compat: minor-0 intra (M0) assets still open/decode/verify —
   confirmed against the committed `assets/*.slpy`. (`sleepy-factory build`
   briefly pinned the intra profile here; superseded by the M1 factory
   upgrade, note 10 — it now emits the full v1 profile.) Adversarial-review
   fixes: zero fps and zero base dims are rejected by both writer and reader
   (regression-tested in `tests/container.rs` + `tests/m1_format.rs`), and
   the player's fps guard in `main()` now covers `fps_num == 0` too.
   Measured on the synthetic coherent sequence (tests/m1_format.rs):
   delta+zstd-19 is ~16.8× smaller than intra+zstd-19.
8. **slpy-term M1 upgrade** (M1 term agent): caps probe (`probe.rs`),
   quantizers (`quant.rs`), tier-aware painter with quantize-before-diff,
   `?2026h…l` wrap, `SimBackend::set_caps`, `FromStr for ColorTier`
   (`--tier`), `ProbeOptions.no_query` (`--no-query`) — all per the section
   above. Deliberate breaks from the M0 freeze: **`Throughput` enum and
   `Caps.throughput` removed** (Scope amendment — no connectivity
   classification; no external user existed), and **`AnsiBackend::new` no
   longer rejects non-truecolor tiers** (the M0-only guard + its test were
   replaced by real tier support and byte-level tier tests). Truecolor
   output is byte-identical to M0 (locked by `truecolor_unchanged_from_m0`
   and the untouched `sim_diff.rs` suite). The pty harness gained
   `probe-silent`/`probe-reply` modes (`tests/pty_probe.rs`: no-reply probe
   → conservative defaults < 300 ms, zero stray stdin bytes; scripted kitty
   replies → True + sync_2026 + cell_px). The player still constructs
   `Caps::default()` — wiring `probe_caps` + `--tier`/`--no-query` into the
   player CLI is the M1 integration step.
9. **Player CLI replaced** (integrator, M0): the scaffold sketch
   (`--sim COLSxROWS`, `--no-invalidate`) was superseded by the M0
   integration directive — `--repaint full|diff` (default full),
   `--loop`, `--fps-cap`, `--sim COLSxROWS:NFRAMES` + JSON stats line,
   `--sim-resize [COLSxROWS]`. `--cell-aspect` and `--duration-secs` kept;
   cell aspect defaults to the terminal-reported cell pixel ratio
   (`Caps::cell_px`, interactive only) with 2.0 fallback (§3.2). No library
   `pub` signature changed during integration — this note is CLI-only.
10. **sleepy-factory M1 upgrade** (M1 factory agent): full v1 pipeline per
    the Binaries section above — two-pass build (shot detection + per-shot
    levels → NORM; Y + C planes through the v1 writer default
    delta/keyframe-60/zstd-19 profile), M0's baked-in normalization removed
    (`lut.rs` folds only sRGB→linear→L\*; `percentile_levels` feeds NORM),
    `inspect` extended (shots/cuts, keyframe count, per-plane sizes,
    compression vs raw). New modules `shots.rs` (histogram-SAD detector,
    integer-only, `SHOT_SAD_THRESHOLD_MILLI = 300`, `MIN_SHOT_FRAMES = 8`)
    and `extract.rs` (L\* + RGB565 chroma from one rgb24 decode). e2e tests
    re-baselined for v1 (`tests/build_e2e.rs`): Y+C header profile, NORM
    levels == decoded-plane percentiles (no baking), chroma present,
    seek-vs-sequential, plus a two-scene lavfi concat asserting a CUT-flagged
    shot boundary exactly at the splice with distinct per-shot levels.
    Factory binary output stays byte-deterministic (determinism test kept).
11. **sleepy-player M1 integration** (M1 integrator): CLI per the Binaries
    section above; no library `pub` signature changed. Decisions recorded:
    (a) decode policy on TEMPORAL_DELTA assets — a loaded-frame tracker
    rolls sequential successors through `decode_plane_into` (standing double
    buffer) and routes everything else (startup, `--seek`, 0–9 jumps,
    latest-frame-wins skips, loop wrap) through `seek_plane_into`;
    (b) NORM applied at runtime as a 256-entry LUT folded from the shot's
    Y p2/p98, rebuilt ONLY on shot change (no per-frame pumping); missing
    NORM / degenerate spans (p98 ≤ p2) → identity, so M0 assets render
    byte-identically; the LUT is applied post-resample (monotone linear map
    — order-equivalent, 24k lookups instead of 130k);
    (c) chroma fg — C plane (RGB565 LE) unpacked to three 8-bit channel
    planes (bit-replicating expand) and AREA-resampled per cell through the
    same shared separable `Resampler` as luma (not nearest: consistent with
    §3.3, no extra code path), fg = sampled RGB on truecolor/256/16; mono
    (and luma-only assets) keep the M0 gray/glyph-only path and skip C
    subblock decode entirely (PLAN §4 "low tiers skip chroma");
    (d) `--tier` implies no volley (probe escape hatches per the task
    directive: the volley runs only when none of `--sim`/`--tier`/
    `--no-query` is present).
    Composition lives in the player (`compose_cells`: ramp glyph from
    normalized luma + chroma/gray fg, BLANK pads) — slpy-core's M0
    `compose_luma` is unchanged. Player integration tests:
    `tests/m1_sim.rs` (tier byte checks via `--sim-dump`,
    seek-vs-sequential byte identity, runtime NORM per shot, probe no-hang).
