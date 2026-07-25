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

## slpy-term (PLAN §3.1, §3.6)

```rust
// caps.rs
pub enum ColorTier { True, C256, C16, Mono }
pub struct GlyphFlags(pub u8);   // consts ASCII/BLOCKS/BOX_DRAWING/BRAILLE; contains(), with()
pub enum GlyphSupportTier { AsciiOnly, Cp437, UnicodeCore, UnicodeFull }
pub enum Throughput { Fast, Normal, Slow }
pub struct Caps { pub color: ColorTier, pub glyphs: GlyphFlags,
                  pub glyph_support: GlyphSupportTier, pub sync_2026: bool,
                  pub cells: (u16,u16), pub cell_px: Option<(u16,u16)>,
                  pub throughput: Throughput, pub can_query: bool }
impl Default for Caps;  // M0 kitty-class: True color, ASCII, (80,24), Fast
pub struct FrameStats { pub bytes: u32, pub cells_damaged: u32,
                        pub write_ns: u64, pub dropped: bool }

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

// ansi.rs
pub struct AnsiBackend;
impl AnsiBackend { pub fn new(caps: Caps) -> std::io::Result<AnsiBackend> } // enters session
// sim.rs — all headless M0 verification runs here (no GUI on this box)
pub struct SimBackend;
impl SimBackend {
  pub fn new(cols: u16, rows: u16) -> SimBackend;              // implemented
  pub fn set_throughput(&mut self, bytes_per_sec: Option<u64>); // implemented (2 MB/s gate @M2)
  pub fn push_event(&mut self, ev: Event);                      // implemented
  pub fn take_output(&mut self) -> Vec<u8>;                     // implemented
}

// restore.rs (§3.1 session hygiene; M0 acceptance 3 pty test)
pub const RESTORE_SEQ: &[u8] = b"\x1b[0m\x1b[?25h\x1b[?7h\x1b[?1049l";
pub fn install_restore_hooks();   // panic hook + SIGINT/SIGTERM + atexit
```

## slpy-format (PLAN §4; container only, no I/O policy)

```rust
// header.rs — 64-B layout IMPLEMENTED with offset-freezing tests
pub const MAGIC: [u8;4] = *b"SLPY"; pub const HEADER_SIZE: u32 = 64;
pub const VERSION_MAJOR: u16 = 1;   pub const VERSION_MINOR: u16 = 0;
pub const BASE_W: u16 = 480;        pub const BASE_H: u16 = 270;
pub mod plane_id { Y=1, E=2, EX=3, EY=4, H=5, C=6 }
pub mod codec   { RAW=0, LZ4=1, ZSTD=2 }
pub mod filter  { INTRA=0, TEMPORAL_DELTA=1 }
pub mod header_flags { INDEX_PRESENT=1, CRCS_PRESENT=2 }
pub struct SlpyHeader { version_major/minor, flags, fps_num/den, base_w/h,
                        aspect_num/den, frame_count, plane_count, codec,
                        filter, keyframe_ivl, plane_ids: [u8;8],
                        index_offset: u64, meta_offset: u64 }  // all pub
impl SlpyHeader { pub fn to_bytes(&self) -> [u8;64];
                  pub fn from_bytes(&[u8;64]) -> Result<SlpyHeader> } // implemented

// chunk.rs — framing IMPLEMENTED (16-B chunk header, 16-B FIDX entry, pads asserted)
pub const TAG_META/NORM/FRAM/FIDX/TRLR: [u8;4];
pub const TRLR_PAYLOAD: &[u8] = b"SLPY_END";
pub const CHUNK_HEADER_SIZE = 16;  pub const FIDX_ENTRY_SIZE = 16;
pub mod chunk_flags { REQUIRED=1 }  pub mod frame_flags { KEYFRAME=1 }
pub struct ChunkHeader { pub tag: [u8;4], pub flags: u8, pub size: u64 }  // + to/from_bytes, is_required
pub struct FrameIndexEntry { pub offset: u64, pub comp_size: u32, pub flags: u8 } // + to/from_bytes
// CRC spec: crc32fast (IEEE) over payload bytes only, appended when CRCS_PRESENT

// meta.rs — the ONLY serde/CBOR surface; NO timestamps (byte-determinism)
pub struct Meta { pub factory_version: String, pub source: String,
                  pub palette_hints: Vec<String> }   // serde, append-only evolution

// error.rs
pub enum SlpyError { Io, BadMagic, UnsupportedVersion{found,supported}, Truncated,
                     UnknownRequiredChunk, CrcMismatch{tag}, BadFrameIndex,
                     BadPlaneId, Corrupt(&'static str), BadMeta }  // Display + Error + From<io>
pub type Result<T>;

// write.rs — M0 stream: HEADER | META | FRAM×n | FIDX | TRLR
pub struct WriterOptions { fps_num/den, base_w/h, aspect_num/den,
                           plane_ids: Vec<u8>, codec, filter, keyframe_ivl,
                           zstd_level: i32, with_crc: bool }  // Default = M0 profile (zstd-19, intra, [Y], crc on)
pub struct PlaneRef<'a> { pub id: u8, pub data: &'a [u8] }
pub struct SlpyWriter<W: Write + Seek>;
impl SlpyWriter<W> {
  pub fn new(w: W, opts: WriterOptions, meta: &Meta) -> Result<Self>;
  pub fn write_frame(&mut self, planes: &[PlaneRef<'_>]) -> Result<()>; // 64-B-aligned subblocks
  pub fn finish(self) -> Result<W>;  // FIDX + TRLR + header patch (frame_count, index_offset)
}

// read.rs — over &[u8] (player mmaps via memmap2)
pub struct SlpyReader<'a>;
impl SlpyReader<'a> {
  pub fn open(bytes: &'a [u8]) -> Result<Self>;   // header + FIDX + TRLR check
  pub fn header(&self) -> &SlpyHeader;  pub fn frame_count(&self) -> u32;
  pub fn meta(&self) -> Result<Meta>;
  pub fn plane_dims(&self, plane_id: u8) -> Option<(u16,u16)>;  // implemented; C = half res
  pub fn decode_plane_into(&mut self, frame_idx: u32, plane_id: u8, dst: &mut [u8])
      -> Result<usize>;                            // one zstd decode, zero alloc/frame
  pub fn verify(&self) -> Result<()>;              // full CRC walk (factory `inspect`)
}
```

## Binaries

- `sleepy-factory` (PLAN §5): subcommands `build <in> -o <out> [--params]`,
  `inspect <asset>` (`eval`/`sweep` at M2). M0 build = ffmpeg rawvideo pipe →
  L\* luma → **global p2/p98 baked into the plane** (approved M0 simplification;
  NORM + per-shot levels at M1/M3) → `SlpyWriter`.
- `sleepy-player` (PLAN §3), CLI as implemented at M0 (integrator task
  directive superseded the scaffold sketch — see note 7):
  `<asset> [--repaint full|diff] [--loop] [--fps-cap FPS] [--cell-aspect F]
  [--duration-secs N] [--sim COLSxROWS:NFRAMES] [--sim-resize [COLSxROWS]]`.
  Default `--repaint full` = invalidate-every-frame (§3.1/§7 one render path).
  `--sim` is the headless acceptance path (renders NFRAMES as fast as
  possible to SimBackend, prints one JSON line:
  `{fps, frames, bytes_total, avg_bytes_per_frame,
  stage_ms:{decode,resample,compose,present}, grid_after}`);
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
7. **Player CLI replaced** (integrator, M0): the scaffold sketch
   (`--sim COLSxROWS`, `--no-invalidate`) was superseded by the M0
   integration directive — `--repaint full|diff` (default full),
   `--loop`, `--fps-cap`, `--sim COLSxROWS:NFRAMES` + JSON stats line,
   `--sim-resize [COLSxROWS]`. `--cell-aspect` and `--duration-secs` kept;
   cell aspect defaults to the terminal-reported cell pixel ratio
   (`Caps::cell_px`, interactive only) with 2.0 fallback (§3.2). No library
   `pub` signature changed during integration — this note is CLI-only.
