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
  slpy-eval       lib   deps: slpy-core, slpy-term, slpy-format, serde, serde_json
                        dev: insta, proptest   (NEW at M2; slpy-format added
                        at item C for the synthetic fixture builders)
  sleepy-factory  bin   deps: slpy-format, slpy-core, slpy-term, slpy-eval,
                        sleepy-player(lib), clap, indicatif, serde, serde_json,
                        toml, memmap2               (M2 item B additions)
  sleepy-player   bin+lib  deps: slpy-core, slpy-term, slpy-format, memmap2,
                        clap, anyhow   (lib target NEW at M2: `pipeline`
                        module only — see the sleepy-player lib section)
```

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

// ---- M3 (slpy-core layers agent): §3.4 palettes + §3.5 three-layer
// compositor. compose_luma and ramp.rs are UNCHANGED (M0/M2 goldens). ----

// palette.rs (§3.4) — all 8 palettes as data + PaletteSet selection.
// slpy-core stays terminal-free: the player maps Caps → these enums
// (Caps.glyphs/glyph_support → GlyphTier, Caps.color → ColorDepth).
pub enum GlyphTier { Ascii, UnicodeBlocks, BrailleVerified }
pub enum ColorDepth { True, C256, C16, Mono }   // mirrors slpy-term ColorTier
pub enum DensityBand { Coarse, Fine }           // + from_cols (< 70 = Coarse)
pub enum LayerRole { Base, Edge, Highlight, Detail }  // doc/selection axis
pub enum GlyphClass { H, DiagDown, V, DiagUp }  // + from_bin(u8); 8 bins → 4
    // screen classes; image coords are y-down: θ∈(0°,90°) renders '\'
pub enum SubPos { Top, Mid, Bottom }  // + subpos(lt, lb, delta)
pub struct EdgeLut { pub by_class: [[char;3];4],  // [class][SubPos]
                     pub junction: char, pub junction_strong: char }
pub const ASCII_EDGE: EdgeLut;    // palette 3: - / | \ _ = + # ('=' fills the
                                  // H-top slot: ASCII has no overline; '+'
                                  // junction, '#' strong junction ≥ edge_strong)
pub const UNICODE_EDGE: EdgeLut;  // palette 6: ─ │ ╱ ╲ ‾ _ ┼ (H row ‾ ─ _)
pub const ASCII_HIGHLIGHT: &[char];   // palette 4 " .+*" (all tiers)
pub const UNICODE_BASE: &[char];      // palette 5 " ·░▒▓█"
pub const UNICODE_QUADRANTS: &[char]; // palette 5 "▖▘▝▗▀▄▌▐" (▌▐ unreachable
                                      // until 2Vc×2Vr sampling — documented)
pub const MONO_FALLBACK_BASE: &[char]; // palette 8 " .:coO8@" (CP437-safe)
pub const SUBPOS_GLYPHS: [char; 3];   // ‾ - _ (§3.3; ascii-tier subposition)
pub struct BrailleLut { pub by_class: [[u8;3];4], pub junction: u8 }
pub const BRAILLE_EDGE: BrailleLut;   // palette 7 dot masks, ≤5 dots (never
pub fn braille_glyph(mask: u8) -> char;               // solid; edge-only)
pub fn quadrant_for(class: GlyphClass, top_bright: bool) -> Option<char>;
    // diagonal classes only — vertical-pair + dominant-orientation approx
pub const RAMP_CAP_TRUE: u8 = 8;  pub const RAMP_CAP_256: u8 = 12;
pub struct RampView;  // &'static glyphs + effective len (per-tier cap, §1b:
                      // NOT duplicated data); glyph(idx) spreads 0..len over
                      // the full ramp with exact endpoints
pub struct PaletteSet { pub base: RampView, pub highlight: RampView,
                        pub edge: &'static EdgeLut, pub halfblock: bool,
                        pub quadrant: bool, pub braille: bool, pub subpos: bool }
pub fn select_palettes(GlyphTier, ColorDepth, viewport_cols: u16) -> PaletteSet;
// Key (§1b): C16/Mono → palette 8 base uncapped ("mono longest"); True caps
// ramps to 8, C256 to 12; Ascii → coarse/fine by density + subpos; unicode
// tiers → halfblock + quadrant; braille only BrailleVerified × Fine density.

// orient.rs (§3.3) — sign/comparison only, no atan2, no floats.
// PLANE CONTRACT (matches factory features.rs/edges.rs): Ex/Ey are bias-128
// HALF-SCALE bytes of the GRADIENT doubled-angle vector in y-down coords
// (ex = 128 + (m·cos 2θg)/2); coherence = 2·|(Ex−128, Ey−128)| / max(E,1).
// compose_cell negates on decode (tangent = −gradient in doubled space).
pub const BIN_UNSET: u8 = 0xFF;
pub fn debias(v: u8) -> i32;                 // v − 128
pub fn octant_bin(dx, dy: i32) -> u8;        // 8 bins × 22.5° of tangent θ
pub fn bin_with_guard(dx, dy: i32, prev: u8) -> u8;  // ±8° θ hysteresis via
                                             // Q14 boundary-vector cross tests
pub fn coherence_at_least(dx, dy: i32, e: u8, t_q8: u8) -> bool; // squared, no sqrt

// hysteresis.rs (§3.5) — 3 B/cell state; alloc ONLY in new/resize.
pub const IDX_UNSET: u8 = 0xFF;
pub const IDX_HYST_Q8: u32 = 90;             // round(0.35·256) — the spec
                                             // DEFAULT; live width is
                                             // ComposeParams::idx_hyst_q8
                                             // (M3 Tune, note 21)
pub mod cell_flags { pub const WAS_EDGE: u8 = 1; }
pub struct CellState { pub idx: u8, pub bin: u8, pub flags: u8 }  // + Default
pub struct HysteresisState;  // new(cols,rows)/cols/rows/cell/cell_mut +
                             // reset() = scene cut (no realloc) +
                             // resize(cols,rows) = realloc + reset (§3.5)
pub fn hysteresis_idx(n: u8, len: u8, prev: u8, hyst_q8: u32) -> u8;
    // ±hyst_q8/256-step boundary (SIGNATURE CHANGED at M3 Tune: width was
    // the IDX_HYST_Q8 constant; hyst_q8 < 256, u8-sourced by contract)
pub fn edge_gate(e: u8, was_edge: bool, t_on: u8, t_off: u8) -> bool;
    // e > T_on || (was_edge && e > T_off) — both strict

// compose.rs M3 additions (§3.5 per-cell selection; priority/override, never
// blended; fg is ALWAYS the chroma sample (gray(n) fallback), bg black except
// half-block/quadrant (fg,bg) pairs; no dithering).
pub mod h_flags { HIGHLIGHT = 1, DEEP_SHADOW = 2 }   // §4 H plane bits
pub struct CellInputs { pub luma_top, luma_bottom, e, ex, ey, h: u8,
                        pub chroma: Option<Rgb> }
pub struct ComposeParams { pub edge_t_on/edge_t_off: u8,        // 32/16 (M3
  // integration re-anchor to the factory E scale — note 20i; was 96/48)
  pub coh_min_q8/coh_dir_q8: u8,     // 96/160: <min suppress, min..dir
                                     // junction glyph, ≥dir directional
  pub hi_cut_q8: u8,                 // 160: highlight iff idx < len·q8/256
  pub edge_white_cut_q8: u8,         // 240: §3.4 near-white edge suppression
                                     // ((plain_idx+1)·256/len > cut = top
                                     // step). Since the Tune finish (note
                                     // 22) the veto rides the PLAIN
                                     // quantized index, not the hysteresis-
                                     // held one — decouples edge F1 from
                                     // idx_hyst_q8
  pub halfblock_min_delta: u8,       // 64: |top−bottom| "large"
  pub edge_strong: u8,               // 96: ascii junction '+' → '#' (note 20i)
  pub idx_hyst_q8: u8 }              // §3.5 idx hysteresis width in Q8 steps
                                     // (promoted at M3 Tune, note 21);
                                     // default 160 since the Tune finish
                                     // (corpus-swept, note 22; spec nominal
                                     // 0.35·step = IDX_HYST_Q8 = 90)
  // + Default (the M3 baseline; all params.toml candidates)
pub fn compose_cell(&CellInputs, lut: &[u8;256], &PaletteSet, &ComposeParams,
                    &mut HysteresisState, col: u16, row: u16) -> Cell;
// Order: per-shot LUT on both taps → idx hysteresis (deep shadow clamps idx
// to 0) → dual-threshold edge gate (magnitude memory kept even when
// coherence suppresses drawing) → edge glyph (braille masks replace the LUT
// on braille sets; junction when bins conflict) → deep-shadow darkest step →
// highlight (boosted fg +25% toward white; hidx spread over the gate range)
// → |Δ|≥delta: quadrant (coherent diagonal) / half-block ▀▄ (chroma scaled
// per tap around the cell mean) / ascii ‾ _ subposition → base ramp.
pub struct FramePlanes<'a> { pub luma2: &'a [u8],       // Vc × 2Vr
  pub e, ex, ey, h: Option<&'a [u8]>,                   // Vc × Vr
  pub chroma: Option<(&'a [u8], &'a [u8], &'a [u8])> }  // r,g,b at Vc × Vr
pub fn compose_frame(&FramePlanes, &Viewport, lut: &[u8;256], &PaletteSet,
                     &ComposeParams, &mut HysteresisState, &mut Grid<Cell>);
// BLANK pads; edge layer runs only when E+Ex+Ey all present (M1 Y+C assets
// compose pure-base — the §4 back-compat auto-disable); panics on any size
// mismatch (incl. state ≠ vp dims); never allocates.

// M3 render metadata (edge-F1 agent, note 19) — the LayerMask: which layer
// won each cell (§3.4 priority is override-only, so it's a single id).
pub mod layer { BASE=0, EDGE=1, HIGHLIGHT=2, SHADOW=3, STRUCTURE=4 }
    // STRUCTURE = half-block/quadrant/subposition; pads are BASE
pub fn compose_cell_layer(...same args as compose_cell) -> (Cell, u8);
    // compose_cell == compose_cell_layer(...).0 (thin wrapper, same state)
pub fn compose_frame_masked(...same args, out: &mut Grid<Cell>,
                            mask: &mut Grid<u8>);
    // compose_frame + mask fill; mask must match out's full terminal dims;
    // cells byte-identical to compose_frame (unit-tested)
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
// (TERM, TERM_PROGRAM, COLORTERM, tmux?) — M2 fix (M1 review low 3): key
// includes COLORTERM, and a cache hit only ever UPGRADES the tier passive
// evidence proves this run (never downgrades); silence is never cached.
// The volley runs under a Drop-guarded termios (echo/canon off) and drains
// stragglers so no reply bytes leak into the app's input. M2 fix (M1
// review low 2): replies dribbling past the deadline are consumed by a
// bounded quiet-gap grace drain (a late DA1 still upgrades caps; silent
// terminals return at the deadline unchanged), and when the volley DID
// time out sentinel-less, AnsiBackend's event pump arms a crate-private
// straggler filter that discards DCS-reply fragments (Alt+P … Alt+'\') so
// late probe bytes never surface as key events (digits are seek bindings).
// M3 hardening (M2-low fix a, note 22): the filter is SESSION-LONG in armed
// sessions (the old ~2 s disarm window let post-window bursts through) and
// handles the SPLIT intro — a lone ESC (which crossterm tokenizes as the
// Esc key = Quit) is held ≤150 ms and either completed by a following 'P'
// (reply: swallowed) or flushed as a real Esc (quit still works, just
// ≤150 ms later, armed sessions only). Pty-tested (tests/pty_probe.rs:
// probe-latereply, probe-straggler harness modes incl. split-burst,
// post-2s-burst and lone-Esc-quit regressions).
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
// §4 GEOMETRY TERM (M2, review fix 1): base_w and base_h MUST be even and
// >= 2 — the C plane lives at (base_w/2, base_h/2), so odd/degenerate base
// dims imply a zero-dimension chroma plane (base_w == 1 → C width 0, which
// panicked the player's Resampler::build). Enforced at writer new() AND
// reader open() (and friendliest-first in factory --res/params validation);
// no asset violating the rule can be produced or opened.
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

## slpy-eval (PLAN §6; M2 item A — metrics library, no I/O beyond serde)

Library-only measurement primitives + the versioned JSON report schema.
The driver that builds assets, runs SimBackend and writes `runs/*.json` +
HTML contact sheets is `sleepy-factory eval` (M2 item B) — not this crate.

```rust
// coverage.rs — glyph ink-coverage table (§6 "rasterize through the stored
// glyph-coverage tables"). Built-in conservative table derived from DejaVu
// Sans Mono via ffmpeg drawtext at 64×128 px/cell (§3.4's raster size),
// coverage = mean gray / 255 (antialiased ink integral); derivation script
// committed at crates/slpy-eval/tools/derive_coverage.py, constants are the
// artifact (no corpus/font dependency at test time). Covers all printable
// ASCII (⊇ every shipped palette incl. the PLAN §3.4 mono ramp " .:coO8@").
pub const CONSERVATIVE_COVERAGE: &[(char, f32)];  // 95 entries, sorted
pub struct CoverageTable;   // sorted entries + max; per-font tables at M5
impl CoverageTable {
  pub fn conservative() -> &'static CoverageTable;
  pub fn from_entries(Vec<(char, f32)>) -> CoverageTable; // panics: empty/dup/!0..=1
  pub fn coverage(&self, char) -> Option<f32>;
  pub fn coverage_or_fallback(&self, char) -> f32;  // unknown → 0.5·max (mid-gray)
  pub fn max_coverage/len/is_empty();
}

// raster.rs — Grid<Cell> → grayscale. Cell block = constant value
// g·luma(fg) + (1−g)·luma(bg), g = min(coverage·gain, 1); no sub-cell glyph
// shape (measures exactly what the compositor controls, at cell granularity).
pub struct GrayImage;  // u16 dims, row-major u8
impl GrayImage { pub fn new/from_raw(w, h, Vec<u8>)/w/h/get/as_slice;
                 pub fn crop(x, y, w, h) -> GrayImage }  // drop letterbox pads
pub fn luma8(Rgb) -> u8;  // gamma-space Rec.709, integer fixed-point
pub struct RasterOptions { pub cell_w_px: u16, pub cell_h_px: u16,
                           pub normalize_ink: bool }
// Default: 1×2 px/cell (1:2 cell aspect, §3.2) + normalize_ink = true
// (gain = 1/max_coverage: metric compares in relative ink — raw physical
// coverage tops out ~0.26 and would drown SSIM's luminance term).
pub fn rasterize(&Grid<Cell>, &CoverageTable, &RasterOptions) -> GrayImage;

// ssim.rs — mean SSIM per Wang/Bovik/Sheikh/Simoncelli, IEEE TIP 13(4) 2004:
// 11×11 Gaussian window σ=1.5 (the paper's choice over 8×8 uniform — no
// blocking artifacts, comparable to every reference impl), K1=.01 K2=.03
// L=255, valid-mode windows, no variance clamping (keeps ssim(x,x) == 1.0
// bit-exact). Images < 11 px on a side: single uniform global window.
pub const SSIM_WINDOW: usize = 11;  pub const SSIM_SIGMA: f64 = 1.5;
pub fn ssim(&GrayImage, &GrayImage) -> f64;        // panics on dim mismatch
pub fn downscale_ssim(rendered: &GrayImage, src_luma: &[u8],
                      src_w: u16, src_h: u16) -> f64;
// = §6 downscale-SSIM: source resampled to rendered dims through slpy-core's
// own Resampler (same box-average semantics as the player), then ssim.
// Pass the viewport-cropped raster (GrayImage::crop) — pads are not scored.

// edge.rs — NEW at M3: §6 edge F1 vs SOURCE Canny at grid resolution
// (ground truth is never the factory's own planes — the driver streams the
// raw fps-normalized gray source; imageproc canny; slpy-eval gained the
// codec-less image+imageproc deps). Prediction = cells where the edge layer
// WON, read from the LayerMask (note 19). NaN-free by construction.
pub const CANNY_LOW: f32 = 60.0;   pub const CANNY_HIGH: f32 = 140.0;
    // eval-owned + fixed (like the SSIM reference percentiles): Sobel-
    // magnitude thresholds on the σ1.4-blurred downscale, chosen on the M3
    // corpus at 300×80 so truth density lands ~1–10% of cells (sheep
    // outline/fence/horizon + silhouette limbs traced; grass micro-texture
    // and dim stars dropped — masks eyeballed at M3)
pub const EDGE_MATCH_TOLERANCE: u16 = 1;   // Chebyshev tolerance ring (cells)
pub struct EdgeMask;  // binary cell mask: new(w,h)/w/h/get/set/count
pub fn canny_edge_truth(src: &[u8], src_w, src_h, grid_w, grid_h) -> EdgeMask;
    // source luma → slpy-core Resampler box-average downscale to the
    // viewport grid (same semantics as the player) → imageproc canny.
    // "grid resolution" = viewport cells (anisotropic ~1:2) — the metric is
    // cell-level by definition
pub fn edge_cells_from_layers(&Grid<u8>, &Viewport) -> EdgeMask; // crop pads,
    // select slpy_core::compose::layer::EDGE
pub struct EdgeScore { precision, recall, f1: f64,
                       truth_cells, predicted_cells: u32 }
pub fn edge_f1(truth, pred: &EdgeMask, tol: u16) -> EdgeScore;
    // TP(pred) = pred cell with truth within tol; TP(truth) symmetric.
    // Empty-mask conventions (all finite): none/none → P=R=F1=1;
    // truth-only → P=1,R=0,F1=0; pred-only → P=0,R=1,F1=0.

// flicker.rs — §6 flicker score (M3 gate ≤ 2 switches/cell/s). Streaming;
// compares Cell::ch only (color-only changes aren't flicker); a grid-dim
// change resets the pair state (resize legitimately reglyphs everything).
// Static-segment selection is the driver's job (it has the NORM shot table).
pub struct FlickerAccum;
impl FlickerAccum { pub fn new(); pub fn push(&mut self, &Grid<Cell>);
  pub fn switches/cell_pairs() -> u64;
  pub fn switches_per_cell_frame() -> Option<f64>;
  pub fn score(&self, fps: f64) -> Option<f64> }   // switches/cell/SECOND

// stats.rs — damage/bytes aggregation from slpy-term FrameStats + stage timers.
pub struct DamageStats { frames, dropped_frames: u32, bytes_total: u64,
  avg_bytes_per_frame: f64, max_bytes_per_frame: u32,
  avg_damage_rate, max_damage_rate: f64 /*fraction of grid, 0..=1*/,
  avg_write_ms: f64, bytes_per_sec: f64 }          // serde
pub fn aggregate_frame_stats(&[FrameStats], grid_cells: u32, fps: f64)
    -> DamageStats;                                // empty slice → zeros
pub enum Stage { Decode, Resample, Compose, Present }  // §3.6 stages; ALL, as_str
pub struct StageStat { frames: u32, mean_ms, max_ms: f64 }       // serde
pub struct StageTimesMs { decode, resample, compose, present: StageStat } // serde
pub struct StageAccum;  // record(Stage, Duration) → report() -> StageTimesMs

// report.rs — versioned JSON schema (the §5 agent socket's machine half).
// Deterministic serialization (no timestamps/host info in the body; BTreeMap
// keys sorted); additive fields don't bump the version (serde defaults).
pub const SCHEMA_VERSION: u32 = 2;  // M3 bump: edge_f1/edge_precision/
    // edge_recall in ClipMetrics (deliberate generation marker; v1 reports
    // still deserialize, and compare accepts an OLDER baseline with a note
    // — only a NEWER/unknown baseline version fails fast)
pub struct EvalReport { schema_version: u32, generator: String,
                        clips: Vec<ClipReport> }   // new/to_json/from_json/clip
pub struct ClipReport { name: String, frames: u32, fps: f64,
                        grid_cols, grid_rows: u16, metrics: ClipMetrics }
pub struct ClipMetrics {                            // all-default, additive
  ssim: Option<f64>, flicker_switches_per_cell_sec: Option<f64>,
  edge_f1, edge_precision, edge_recall: Option<f64>,  // M3 (schema v2):
                                                     // means over sampled
                                                     // frames; only edge_f1
                                                     // is gated by compare
  shot_count: Option<u32>, cut_count: Option<u32>,   // NORM roster (M2 review
  keyframe_count: Option<u32>, asset_bytes: Option<u64>, // fix 4a: factory-
                                                     // tunable regressions
                                                     // must be visible)
  damage_by_tier: BTreeMap<String, DamageStats>,   // keys = ColorTier canon
  stage_ms: Option<StageTimesMs> }

// compare.rs — baseline compare (M2 acceptance 4). Direction-aware;
// improvements always pass; metric/clip/tier present in baseline but missing
// from current → FAIL (coverage must not silently shrink); new-in-current →
// ignored. Zero baseline + fractional tolerance: nonzero current fails.
pub struct Tolerances { ssim_max_drop: f64,            // default 0.02 (abs)
  flicker_max_increase: f64,                           // 0.5 sw/cell/s (abs)
  edge_f1_max_drop: f64,                               // 0.05 (abs, M3 —
                                                       // the aesthetic-
                                                       // regression drill's
                                                       // gate)
  bytes_frac_max_increase: f64,                        // 0.20
  damage_rate_max_increase: f64,                       // 0.05 (abs)
  stage_ms_frac_max_increase: f64,                     // 0.50 — INFORMATIONAL
                                                       // band only since M3
                                                       // (M2-low fix b, note
                                                       // 22): stage deltas
                                                       // never gate; beyond
                                                       // the band → `info:`
                                                       // note. Item E gates
                                                       // stage time precisely
  shot_structure_max_delta: f64,                       // 0.0 (abs, BOTH
                                                       // directions — shot/
                                                       // cut_count changes
                                                       // are deliberate acts)
  keyframes_frac_max_drop: f64,                        // 0.0 (drop only;
                                                       // increases pass)
  asset_bytes_frac_max_increase: f64 }                 // 0.20 (bloat only)
                                                       // — serde defaults:
                                                       // params.toml may
                                                       // override a subset
pub struct MetricDelta { clip, metric: String, baseline, current, delta: f64,
                         pass: bool }
pub struct CompareReport { pass: bool, deltas: Vec<MetricDelta>,
                           notes: Vec<String> }        // + failures()
pub fn compare_reports(current, baseline: &EvalReport, &Tolerances)
    -> CompareReport;

// fixtures.rs — NEW at M2 item C: deterministic synthetic fixtures + golden
// render support (repo rule: committed goldens reproducible WITHOUT the
// corpus). Pure integer plane generators → SlpyWriter in memory (no ffmpeg,
// no files, no floats). Test support: invalid assets PANIC (not Result).
pub const FIXTURE_BASE_W/H: u16 = 192/108;   // 16:9, C plane 96×54 RGB565
pub const FIXTURE_FRAMES: u32 = 72;          // 30 fps, keyframes every 24
pub const FIXTURE_KEYFRAME_IVL: u8 = 24;
pub const HARD_CUT_FRAME: u32 = 36;          // mid-GOP shot boundary
pub enum Fixture { GradientMotion, HardCut, CheckerDrift }  // + ALL, name()
pub fn luma_plane/chroma_plane(Fixture, frame: u32) -> Vec<u8>;  // pure
pub fn shot_records(Fixture) -> Vec<ShotRecord>;  // HardCut: 2 shots, CUT
pub fn build_fixture(Fixture) -> Vec<u8>;    // full SLPY v1 (Y+C, delta,
                                             // zstd-19, CRCs, NORM), byte-
                                             // deterministic (unit-tested)
pub enum GoldenPalette { Ascii, Unicode, MonoGlyphOnly }
    // + ALL, name() ("ascii"/"unicode"/"mono"), is_glyph_only(),
    // config() -> (GlyphTier, ColorDepth). RE-KEYED at M3 (was
    // AsciiCoarse/AsciiFine/MonoGlyphOnly): configs are now the player's
    // own select_palettes key — density is no longer a hand-forced axis
    // (it falls out of the viewport), and the unicode config joined
    // because half-blocks/quadrants are M3 acceptance surface. Mono =
    // (Ascii, Mono): palette 8 base, no chroma decode, glyph-only
    // serialization.
pub struct FixtureRenderer<'a>;  // player-pipeline replay on public APIs,
                                 // pinned cell-for-cell to the REAL Player by
                                 // sleepy-player/tests/pipeline_parity.rs
                                 // (M2 review fix 4c — goldens transitively
                                 // cover the shipping renderer via that pin;
                                 // M3: parity covers the temporal state
                                 // trajectory too — both sides render the
                                 // same frame sequence)
impl FixtureRenderer<'a> {      // M3: decode(seq roll/FIDX seek)→resample
  pub fn new(asset: &'a [u8], GoldenPalette) -> Self;   // (luma Vc×2Vr)→
  pub fn reflow(&mut self, cols, rows);   // NORM LUT (+ shot-change state
                                          // reset)→ §3.5 compose_frame
                                          // (fixtures are Y+C ⇒ edge/
                                          // highlight auto-disabled)
  pub fn render(&mut self, frame: u32) -> &Grid<Cell>;  // BLANK below 32×9
  pub fn viewport() -> Option<Viewport>;  pub fn frame_count() -> u32;
  pub fn resampler_dims() -> Option<((u16,u16),(u16,u16))>;  // fuzz invariant
  pub fn grid(&self) -> &Grid<Cell>;
}
pub fn snapshot(title, term: (u16,u16), GoldenPalette, Option<Viewport>,
                &Grid<Cell>) -> String;
    // the committed cell-grid golden serialization: header + glyph grid
    // framed in |…| + FNV-1a 64 digest of each row's fg (r,g,b) bytes
    // (glyph-only palettes omit the fg section)
```

## sleepy-player lib (`sleepy_player::pipeline`) — NEW at M2 (item B)

The binary crate gained a lib target so `sleepy-factory eval` drives the
EXACT player frame pipeline headlessly (metrics must measure the real
renderer, not a reimplementation — decision recorded in note 14). The
binary keeps the CLI/clock/tty; the pipeline is pure w.r.t. both.

```rust
// pipeline.rs — M3: the full §3.5 three-layer path (integrator; note 20).
pub struct StageNs { pub decode, resample, compose, present: u64 } // ns, Copy
pub struct Drained { pub quit: bool, pub jump_digit: Option<u8> }
    // M3 review fix (medium, seek ghosting): when jump_digit is Some,
    // drain_events has ALREADY reset all hysteresis state — a digit seek is
    // a temporal discontinuity (same class as the §3.5 cut/resize resets;
    // update_levels only covers jumps that cross a shot boundary, so a
    // same-shot jump used to ghost pre-seek was_edge/idx into the landing
    // frame). Callers just repoint their clock and render (regression:
    // sleepy-player/tests/m3_layers.rs digit_jump_seek_resets_hysteresis_state).
pub fn glyph_tier_from_caps(&Caps) -> GlyphTier;  // AsciiOnly/Cp437→Ascii,
      // UnicodeCore→UnicodeBlocks, UnicodeFull→UnicodeBlocks unless
      // Caps.glyphs has BRAILLE (verified-only) → BrailleVerified
pub fn color_depth(ColorTier) -> ColorDepth;      // 1:1 variant map
pub struct Player<'a>;   // decode → resample → NORM LUT → compose → present
impl<'a> Player<'a> {
  pub fn new(reader: SlpyReader<'a>, cell_aspect: f64, repaint_full: bool,
             color: ColorDepth, glyph_tier: GlyphTier)
      -> anyhow::Result<Player<'a>>;
      // SIGNATURE CHANGED at M3 (was want_color: bool): the player owns
      // palette selection, keyed Caps-shaped (charset tier × color depth;
      // density falls out of the viewport at reflow via select_palettes).
      // Mono ⇒ chroma subblocks are never decoded (M1 rule, unchanged).
      // Plane registry detection happens here: E+Ex+Ey all present ⇒ edge
      // layer on; H present ⇒ highlight/shadow on; absent ⇒ auto-disabled
      // (M1-era Y+C assets play with pure base+structure — §4 back-compat,
      // regression-tested in tests/m3_layers.rs).
  pub fn set_compose_params(&mut self, ComposeParams);  // eval wires
      // params.toml [compose]; interactive keeps the core defaults (pinned
      // equal to the committed [compose] by factory unit test)
  pub fn reflow<B: Backend>(&mut self, backend: &mut B, cols, rows);
      // M3 additions: palette reselection, luma tap tables at Vc×2Vr (ONE
      // build, §3.3), feature tap tables at Vc×Vr (only when planes exist),
      // HysteresisState.resize (realloc+reset — §3.5 graft from C)
  pub fn drain_events<B: Backend>(&mut self, backend: &mut B) -> Drained;
  pub fn render_present<B: Backend>(&mut self, backend: &mut B, frame_idx: u32)
      -> anyhow::Result<FrameStats>;
      // M3 frame: decode Y(+E/Ex/Ey/H/C present-planes; sequential roll or
      // FIDX seek per plane) → update_levels (LUT rebuild on shot change
      // ALSO resets hysteresis — deliberate superset of the §3.5 CUT rule:
      // a changed LUT makes every remembered ramp index stale, and every
      // CUT is a shot change) → resample (luma Vc×2Vr; E/Ex/Ey box-avg at
      // Vc×Vr; H bits expanded to 0/255 masks, box-avg'd, re-thresholded
      // at ≥64 highlight / ≥128 shadow — bitflags don't box-average) →
      // compose_frame (or compose_frame_masked when the LayerMask is
      // enabled — note 19 contract CONNECTED; eval edge-F1 is live) with
      // the NORM LUT applied per tap inside compose_cell.
  // Read-only accessors (M2, unchanged): frame_count/viewport/grid/
  // luma_src/levels_lut/stage.
  pub fn enable_layer_mask(&mut self);       // eval + --sim (layer counts)
  pub fn layer_mask() -> Option<&Grid<u8>>;
  pub fn resampler_dims() -> Option<((u16,u16),(u16,u16))>;
      // LUMA resampler; dst is (Vc, 2·Vr) since M3 — fuzz asserts exactly
  pub fn hysteresis_dims() -> (u16, u16);    // NEW: §6 fuzz invariant
      // "hysteresis buffers realloc'd to the new grid" ((0,0) below 32×9)
}
pub fn build_levels_lut(lut: &mut [u8; 256], levels: Option<PlaneLevels>);
pub fn unpack_rgb565(src: &[u8], r, g, b: &mut [u8]);
pub fn draw_enlarge_card(grid: &mut Grid<Cell>);
// REMOVED at M3: compose_cells (the M1 base-only compositor) — the §3.5
// path replaced it wholesale; keeping it invited silent drift between the
// shipping renderer and the golden harness.
```

The M4 embeddable API will grow from here; until then nothing else in the
crate is `pub`.

## Binaries

- `sleepy-factory` (PLAN §5), CLI as of M3:
  `build <in> -o <out> [--ss T] [--t T] [--fps N] [--res WxH] [--params F]`,
  `inspect <asset> [--dump-planes DIR] [--frame N]...` (M3: per-plane value
  stats over sampled frames + optional PGM/PPM plane dumps for eyeballing),
  `params --dump [--params F]`,
  `eval --corpus <dir> [--params F] [--baseline B.json] --out X.json
  [--html X.html] [--reel R.html] [--cache-dir D]`,
  `sweep --corpus <dir> --grid G.toml --out DIR [--params F]
  [--cache-dir D]` (M3 Tune, note 21: the PLAN §5 sweep CLI — G.toml
  declares `[[axes]]` of dotted-param override sets (values within an axis
  travel together, axes cross) + optional `[score]` weights; default
  composite score `0.4·mean(ssim) + 0.4·mean(edge_f1) −
  0.2·mean(flicker/2.0)`; per combo the eval pipeline runs in sweep mode —
  truecolor pass only, contact_frames forced 0, source-Canny truth memoized
  across combos — through the SHARED asset cache, so `[compose]`-only
  combos never rebuild assets; outputs `combo-NN.json` EvalReports +
  ranked `sweep.json` (schema v1; skipped-on-validation combos recorded
  with `skip_reason`, never silently dropped) + self-contained
  `leaderboard.html`; typo'd param paths are hard errors).
  `--reel` (M3) emits the review-reel sign-off artifact: per clip an
  animated GIF of the rasterized render (10 s @ 10 fps, 256-gray palette,
  gif crate) + 6 timestamp rows (source PNG | render raster PNG | metric
  strip: ssim, edge F1 + P/R + truth/pred cell counts, flicker-to-date),
  fully self-contained base64 HTML (reel.rs; constants REEL_ROWS/GIF_SECS/
  GIF_FPS).
  **params.toml contract:** the committed repo-root `params.toml` is
  embedded via `include_str!` and IS the default config; `--params FILE`
  overrides any key subset (serde defaults; unknown keys are hard errors);
  CLI `--fps`/`--res` override last; `params --dump` prints the effective
  merged TOML. Tables: `[build] fps/base_w/base_h/zstd_level/keyframe_ivl`,
  `[shots] sad_threshold_milli/min_shot_frames`, `[levels] lo_pct/hi_pct`,
  `[edges] scharr_shift/bilateral_passes/bilateral_radius/t_hi/t_lo`,
  `[highlights] tophat_radius/tophat_thresh/shadow_pct/shadow_max_l`,
  `[temporal] ema_alpha_{y,e,c}_milli` (M3, note 18),
  `[compose] edge_t_on/edge_t_off/coh_min_q8/coh_dir_q8/hi_cut_q8/
  edge_white_cut_q8/halfblock_min_delta/edge_strong` (M3 integrator, note
  20: RENDERER knobs — mapped onto `slpy_core::ComposeParams` and handed to
  the Player by the eval driver; deliberately EXCLUDED from
  `build_fingerprint`, so compose sweeps never rebuild assets; defaults
  pinned to `ComposeParams::default()` by unit test),
  `[eval] grid_cols/grid_rows/max_frames/ssim_every/contact_frames` +
  `[eval.tolerances]` (slpy-eval `Tolerances` subset). `build.keyframe_ivl`
  is u32 in params with a validate() range of 1..=255 (M2 review fix 4a:
  the wire field is u8; the acceptance drill value 600 must be a clean
  range error, not a serde type error) — every M3 field is deliberately
  wide (u32) for the same clean-range-error reason. In-code defaults and
  the committed file are pinned to each other by unit test; the default
  build output is byte-pinned by tests/m2_params_eval.rs (determinism
  guard). **eval flow:** per corpus video (sorted, non-recursive) — asset
  cached under `--cache-dir` keyed `(input sha256, build-params sha256,
  pipeline source fingerprint)` (eval-only knobs excluded via
  `Params::build_fingerprint`; the fingerprint is an FNV-1a 64 over all
  sleepy-factory + slpy-format `src/*.rs`, emitted by build.rs — M2 review
  fix 4d: factory/format code changes must invalidate cached corpus
  assets) → edge-F1 ground-truth pass (M3: one streaming ffmpeg gray decode
  of the source through the identical scale/fps chain; Canny masks at the
  `ssim_every` cadence + reel timestamps, see slpy-eval edge.rs) → three
  SimBackend passes in pure diff mode (truecolor: SSIM sampled every
  `ssim_every` frames + edge F1 against the truth masks from the player's
  LayerMask + cut-segmented flicker + per-stage times + damage;
  256/mono: damage only; plus per-asset structure metrics
  shot/cut/keyframe counts + asset bytes) → `--out` JSON (`EvalReport`
  schema v2) →
  optional `--baseline` compare (tolerances from params; artifacts still
  written on breach; nonzero exit) → optional `--html` self-contained
  contact sheet (base64 PNGs via ffmpeg subprocess: fps-normalized source
  frame vs viewport-cropped render raster at `contact_frames` timestamps
  + per-metric deltas vs baseline).
  Build semantics as of M3 (factory stages 3–4, note 18): two passes over
  the identical ffmpeg rgb24 decode:
  **pass 1** L\* luma → shot detection on the RAW histograms (256-bin
  histogram SAD ≥ 0.30 normalized, min shot length 8 frames — every honored
  boundary is a hard cut) + per-shot pooled p2/p98 of the EMA'd (= stored)
  luma, EMA reset at every honored boundary (identical schedule to pass 2,
  so NORM levels equal stored-plane percentiles exactly); **pass 2** NORM
  (levels applied at RUNTIME — the LUT folds only sRGB→linear→L\*) + the
  full M3 plane set per frame (`features.rs`): Y (L\* + EMA), E/Ex/Ey
  (Scharr on stored Y → rational doubled-angle field → 2× orientation-aware
  bilateral → |v|≤E cap → hysteresis-thresholded UNTHINNED E → EMA), H
  (top-hat highlight bit0 + percentile deep-shadow bit1, from stored Y, not
  EMA'd itself) and C (2×2 area average → per-channel EMA → RGB565 LE),
  streamed through the v1 writer default profile (temporal delta, keyframe
  interval 60, zstd-19, CRCs) in registry order [Y, E, Ex, Ey, H, C]. NORM
  levels: position 0 (Y) = shot p2/p98; all other positions = (0,0). Output
  goes to `<out>.part`, renamed only after `finish()`. `inspect` additionally
  reports shots + cut flags, keyframe count, per-plane compressed/raw sizes,
  compression ratio vs raw planes, and per-plane value stats over sampled
  frames (E nonzero %, Ex/Ey bias deviation, H flag rates).
- `sleepy-player` (PLAN §3), CLI as of M3 (see notes 9, 11 and 20):
  `<asset> [--repaint full|diff] [--loop] [--fps-cap FPS] [--cell-aspect F]
  [--duration-secs N] [--seek TIMESTAMP] [--tier TIER] [--no-query]
  [--no-cache] [--palette auto|ascii|unicode|braille]
  [--sim COLSxROWS:NFRAMES] [--sim-tier TIER] [--sim-dump PATH]
  [--sim-resize [COLSxROWS]]`.
  M3: `--palette` overrides the Caps-derived charset tier for palette
  selection (`auto` = `glyph_tier_from_caps`; `--sim` derives auto from
  `Caps::default()` — ascii). The `--sim` JSON line gained
  `"layers":{"base","edge","highlight","shadow","structure"}` — cumulative
  winning-layer cell counts over the run (the §3.4 priority decision made
  observable headlessly; sim always enables the LayerMask).
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
4. **Factory deps trimmed** for M0 (see edges above). **NORM chunk not
   written at M0** (tag + registry reserved here so M1 is additive, not a
   format break). `slpy-eval` was absent M0/M1; created at M2 (item A,
   note 12).
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
12. **slpy-eval created** (M2 item A agent): metrics library per its section
    above — nothing else in the workspace consumes it yet (the
    `sleepy-factory eval` wiring is M2 item B). Decisions recorded:
    (a) built-in coverage constants derived from DejaVu Sans Mono (the
    conservative default; per-font tables M5) via the committed
    `crates/slpy-eval/tools/derive_coverage.py` — constants are the
    committed artifact, the script is the reproducible reference;
    (b) rasterizer default = 1×2 px/cell with ink normalization
    (gain = 1/max_coverage) so SSIM compares relative ink, not the ~4×
    physically-darkened raw coverage; `normalize_ink = false` gives the raw
    physical model;
    (c) SSIM = Wang et al. 2004 reference form (11×11 Gaussian σ 1.5,
    valid-mode, unclamped variances → `ssim(x,x) == 1.0` bit-exact;
    < 11 px images fall back to one global uniform window);
    (d) report JSON carries no timestamps/host info — reruns on identical
    inputs are byte-identical (determinism-guard friendly); provenance
    lives in run filenames and git;
    (e) deps: slpy-core + slpy-term (FrameStats is consumed directly, no
    mirror type) + serde/serde_json. insta/proptest/criterion arrive with
    M2 items C/D/E, not here.
13. **M2 items C/D landed** (goldens + resize fuzzing agent). New surface:
    `slpy_eval::fixtures` (see the slpy-eval section) — three deterministic
    synthetic fixture assets (gradient-motion / hard-cut / checker-drift,
    192×108 Y+C, 72 frames, keyframe 24, production delta+zstd-19+CRC
    profile) and a player-pipeline-parity renderer used by every committed
    golden and the fuzzer. Committed goldens (all corpus-free):
    (a) 27 insta cell-grid snapshots at
    `crates/slpy-eval/tests/snapshots/` — 3 fixtures × 80×24 / 206×58 /
    320×90 × ascii-coarse / ascii-fine / mono-glyph-only; serialization =
    glyph grid verbatim + per-row FNV-1a 64 fg digest; re-bless with
    `INSTA_UPDATE=always cargo test -p slpy-eval --test golden_grids`;
    (b) per-tier escape-stream byte goldens at
    `crates/slpy-term/tests/goldens/gradient_f10_48x12_{truecolor,256,16,
    mono}.ansi` (one fixture frame, 48×12, sync_2026 wrap on, byte-exact;
    re-bless with `SLPY_UPDATE_GOLDENS=1`); slpy-term gained a
    DEV-dependency on slpy-eval for this (a legal dev-dep cycle — dev-deps
    sit outside the package's own dep graph).
    Resize fuzzing (§6 invariant set as explicit assertions; MOVED to
    `crates/sleepy-player/tests/resize_fuzz.rs` against the real `Player`
    by review fix 4c, note 17):
    originally `crates/slpy-eval/tests/resize_fuzz.rs` — random
    1×1..=1000×1000 resize
    storms through `SimBackend::push_event` + player-style coalescing drain,
    asserting viewport ⊆ terminal, aspect error minimal-among-candidates
    (spec formula recomputed), pads symmetric ±1, backend/painter/resampler/
    grid realloc'd consistently, tap rebuild < 1 ms (min-of-3, worst
    observed 0.038 ms), and full-frame present after every resize; 256 cases
    under plain `cargo test`, `PROPTEST_CASES=10000` in scripts (measured
    84 s wall on this box). The same letterbox/aspect invariants also run
    directly on `compute_viewport` in
    `crates/slpy-core/tests/viewport_props.rs` (proptest, incl. full-u16
    dims and degenerate aspects). Workspace: `insta` + `proptest` added to
    `[workspace.dependencies]`; `[profile.dev.package.*] opt-level = 3` for
    the four libs + zstd so goldens/fuzz stay fast under `cargo test`
    (debug-assertions unchanged). No existing `pub` signature changed.
14. **M2 item B + review fix 1 landed** (params/eval agent). Decisions
    recorded:
    (a) **pipeline extraction over binary-shelling**: `sleepy-player` gained
    a lib target (`pipeline` module, section above) and `sleepy-factory
    eval` drives `Player` in-process against `SimBackend` — chosen over
    calling the player binary because the metrics need per-frame
    `Grid<Cell>` access (rasterize/flicker) and per-frame `FrameStats`,
    which the `--sim` JSON line cannot carry; `main.rs` is now CLI-only and
    no pipeline semantics changed (truecolor byte-parity untouched);
    (b) **params.toml** per the Binaries section — single source of truth
    enforced three ways: in-code `Default`s reference the shipped constants
    (`WriterOptions::default`, `BASE_W/H`, shot/level consts), a unit test
    pins the committed file to `Params::default()`, and the determinism
    guard byte-pins the default build (synthetic lavfi fixture, committed
    sha; corpus grass check is the `#[ignore]`d integration half);
    (c) **§4 geometry term** (M1 review fix 1): base dims even and >= 2,
    enforced writer + reader + factory (see slpy-format section); player
    regression test drives the binary on a header-patched asset (base_w ∈
    {1, 0, odd}) and asserts clean error, no panic;
    (d) **eval SSIM source side** was the per-shot-NORMALIZED luma (the
    player's own `levels_lut()` applied to `luma_src()`) — SUPERSEDED by
    review fix 4a (note 17): that construction self-graded (both sides of
    SSIM saw the factory's levels damage). The source side is now the raw
    `luma_src()` normalized by eval-owned per-frame p2/p98 percentiles,
    independent of the tunables under test; the NORM stretch itself is
    still not scored;
    (e) **flicker segmentation**: fresh `FlickerAccum` per NORM cut, counts
    summed across segments — scene cuts contribute zero pairs (§6 "static
    segments" without needing per-shot metric plumbing);
    (f) **damage passes run `repaint_full = false`** (pure diff): damage
    rate is meaningless under invalidate-every-frame; the player's
    interactive default (`--repaint full`) is unchanged;
    (g) eval cache under `runs/cache/` (gitignored via `*.slpy`), key
    `(input sha256, build-params sha256)` with a hand-rolled tested SHA-256
    (`sha256.rs`) — no new hashing dependency; PNGs for the contact sheet
    come from the ffmpeg subprocess (rawvideo→png and
    scale/fps/select→png), so no image crate either; `toml` is the one new
    workspace dependency.
15. **M2 item E + review fixes 2/3 landed** (perf-gate agent). No `pub`
    signature changed. Perf gates (PLAN §6): criterion benches at
    `crates/sleepy-player/benches/pipeline.rs` over the REAL pipeline —
    `decode_delta_roll_480x270` (Y+C sequential delta roll),
    `resample_480x270_to_300x80`, `compose_300x80` (viewport inside the
    300×80 grid), `present_truecolor_300x80` / `present_256_300x80`
    (full-invalidate SimBackend present), `e2e_frame_300x80`
    (`Player::render_present`) — fed by a deterministic synthetic 480×270
    Y+C delta asset built in-memory (corpus-free rule). Thresholds are
    COMMITTED at `perf/thresholds.toml` (median-of-3-runs ×1.30 on the
    reference box; ids mirror the bench ids); `scripts/perf-gate.sh
    [--no-run]` compares criterion's `estimates.json` medians and exits
    nonzero on any breach or missing estimate. The unthrottled end-to-end
    gate is a plain test, `crates/sleepy-player/tests/perf_fps.rs`
    (asserts ≥ 24 fps @300×80 truecolor; ~500 fps measured under the dev
    profile). Verified: 5 consecutive green gate runs (incl. under load
    ~16) and a deliberate spin in `Resampler::apply` tripping the gate
    (then reverted byte-clean). Probe fixes (M1 review lows 2/3) per the
    updated probe.rs section: grace drain + `AnsiBackend` straggler event
    filter (crate-private; armed via a probe-side atomic only when the
    volley timed out sentinel-less), COLORTERM in the caps-cache key
    (CACHE_VERSION 1→2, stale lines self-clean on store) and
    upgrade-only cache-hit merge. New harness modes `probe-latereply` /
    `probe-straggler`; regression tests in `tests/pty_probe.rs` (scripted
    post-deadline reply burst → zero surfaced key events, playback keys
    still live) plus unit tests for the filter state machine and the
    cache-merge rule. `criterion` (default-features off +
    `cargo_bench_support`) added to `[workspace.dependencies]`.
16. **M2 item F landed** (integrator). `scripts/eval.sh` is the one-command
    loop (PLAN §6/§7): workspace tests (incl. all goldens, the 256-case
    fuzz and the ≥24 fps e2e gate) → clippy `-D warnings` → resize fuzz at
    `FUZZ_CASES` (default 2000; acceptance depth `FUZZ_CASES=10000`) →
    `scripts/perf-gate.sh` → corpus section. The corpus section runs ONLY
    when the three canonical clips (corpus/README.md; gitignored,
    local-only) are present — it assembles `target/eval-corpus/` symlinks
    (grass-field-windy-mirror + sheep-counting-neroni-clips from
    corpus/prepared/, silhouette-dance from corpus/) so `eval`'s
    non-recursive scan never picks up prep-tool variants, runs
    `sleepy-factory eval` against `runs/base.json` writing
    `runs/latest.{json,html}`, then the `#[ignore]`d real-corpus
    determinism guard (grass rebuild byte-identical to assets/). Absent
    corpus → notice + skip (committed gates stay corpus-free).
    `runs/base.json` + `runs/base.html` are the committed corpus baseline,
    generated exactly that way (all three clips, default params,
    `--cache-dir runs/cache`); stage_ms/write_ms fields are wall-clock and
    vary run-to-run — the compare tolerances absorb that; every other
    metric is deterministic (regeneration reproduced ssim/flicker/damage
    bit-for-bit). No `pub` signature changed.
17. **M2 adversarial-review fixes landed** (review-fix agent). Four
    confirmed findings, each with regression tests:
    (4a) **eval baseline blindness** [high]: `frame_ssim`'s source side no
    longer runs through the player's `levels_lut()` (self-grading — a
    params change that killed shot detection barely moved any gated
    metric); it now normalizes raw `luma_src()` by eval-owned per-frame
    p2/p98 percentiles (`reference_levels`, nearest-rank, constants in
    eval.rs). `ClipMetrics` gained `shot_count`/`cut_count`/
    `keyframe_count`/`asset_bytes` and `Tolerances` gained
    `shot_structure_max_delta` (0.0, directionless) /
    `keyframes_frac_max_drop` (0.0, drop-only) /
    `asset_bytes_frac_max_increase` (0.20, bloat-only) — killed cut
    detection, inflated keyframe cadence and zstd downgrades now trip the
    compare on structure alone (verified live on the sheep clip vs
    runs/base.json, and pinned by m2_params_eval drills on the synthetic
    corpus). `BuildParams.keyframe_ivl` widened u8→u32 with validate()
    range 1..=255 so the drill value 600 errors cleanly.
    runs/base.json + base.html re-baselined (SSIM reference changed:
    grass 0.6402, sheep 0.4272, silhouette 0.8983 + structure metrics).
    (4b) **perf thresholds** [medium]: perf/thresholds.toml re-calibrated
    from ×1.30 to ×1.15 over fresh 3-run medians — PLAN §6 promises
    failure on >15% regressions, and at ×1.30 the mandated 20% drill was
    arithmetically impossible. Verified: +20–25% spin in Resampler::apply
    trips the gate (resample −8.4% headroom → FAIL); reverted; two
    consecutive clean-gate PASS runs.
    (4c) **goldens/fuzz exercised a replica** [medium]: the resize fuzz
    moved to `crates/sleepy-player/tests/resize_fuzz.rs` and now drives
    the real `Player` through `drain_events`/`reflow`/`render_present`
    (new read-only accessor `Player::resampler_dims`); new
    `crates/sleepy-player/tests/pipeline_parity.rs` pins FixtureRenderer
    to Player cell-for-cell (3 fixtures × grid sweep incl. all golden
    sizes + 48×12 tier-golden size × color/mono × seq/seek/cut frames ×
    mid-run reflows), so the 27 insta goldens + 4 tier goldens
    transitively cover the shipping renderer (mutation-tested: dropping
    reflow's ramp update fails parity). sleepy-player gained dev-deps
    slpy-eval + proptest; slpy-eval dropped its proptest dev-dep;
    scripts/eval.sh fuzz section now targets sleepy-player.
    (4d) **eval cache staleness** [medium]: the eval asset cache key
    gained a third component — `SLPY_PIPELINE_FINGERPRINT`, an FNV-1a 64
    over every `.rs` in sleepy-factory/src + slpy-format/src emitted by
    the new `crates/sleepy-factory/build.rs` — so pipeline code changes
    invalidate cached corpus assets (over-invalidation by eval-driver
    edits is accepted as the safe direction). Existing runs/cache entries
    were migrated to the new names after the grass byte-identity guard
    proved output unchanged.
18. **M3 factory plane extraction landed** (factory agent; PLAN §5 stages
    3–4). `sleepy-factory build` now writes all six §4 planes — see the
    Binaries section for the pipeline. **Wire semantics the player relies
    on (factory⇄player contract):**
    (a) **E** (plane 2): u8, unthinned local Scharr magnitude of the
    stored (EMA'd) Y, `min(255, isqrt(gx²+gy²) >> scharr_shift)` — at the
    default shift 4 this reads as L\* contrast (sharp step of contrast Δ →
    E ≈ Δ). Hysteresis-thresholded (default t_hi 28 / t_lo 12,
    8-connected, Canny-style but never thinned), then temporal EMA
    (default α 0.5, reset at cuts) — a vanished edge decays geometrically,
    which the player's `T_on`/`T_off` dual threshold rides.
    (b) **Ex/Ey** (planes 3/4): bias-128 u8 of the HALVED doubled-angle
    vector: `byte = 128 + (v >> 1)` where `(vx, vy) = E·(cos 2θg, sin 2θg)`
    in the **gradient** convention, y-down raster, computed rationally
    (`vx = E·(gx²−gy²)/(gx²+gy²)`, `vy = E·2gxgy/(gx²+gy²)` — no atan2
    anywhere). Decode `v ≈ (byte − 128)·2`; §3.3 coherence =
    `2·|(Ex−128, Ey−128)| / E`; the edge TANGENT doubled vector is
    `−(vx, vy)` (the player's LUT maps gradient bins → stroke glyphs with
    one negation). Sign anchors: vertical edge → Ex > 128; horizontal →
    Ex < 128; "/" contour → Ey > 128; "\" contour → Ey < 128; no-edge
    pixels store exactly (128, 128). The field is orientation-smoothed
    (2-pass alignment-gated bilateral) and capped to `|v| ≤ E` per pixel;
    E itself is never spatially smoothed (localization).
    (c) **H** (plane 5): u8 flags, bit0 highlight (white top-hat ≥ thresh,
    box SE), bit1 deep shadow (darkest shadow_pct% capped at
    shadow_max_l); other bits zero. Computed from the stored Y, so flags
    are temporally stable without EMA-ing bits.
    (d) **Y and C are now temporally EMA'd** (α 0.7 default, reset at
    cuts); C channels are smoothed pre-packing. NORM levels still equal
    the stored-Y percentiles exactly (pass 1 pools the EMA'd histogram —
    build_e2e pins this).
    Params: new tables `[edges]`, `[highlights]`, `[temporal]` (validated,
    fingerprint-relevant: all three invalidate the eval asset cache).
    `ShotDetector::push` split into `boundary`/`pool` (pass 1 detects on
    raw, pools EMA'd). Deliberate re-pins: `FIXTURE_SLPY_SHA` in
    tests/m2_params_eval.rs (new pipeline = new default-build bytes);
    `assets/*.slpy` are still M1-era Y+C and must be REBUILT at M3
    integration (the `#[ignore]`d grass byte-identity guard fails until
    then, by design). Memory: extraction state is O(plane), ~4 MB fixed
    (features.rs memory note); planes stream to the writer.
19. **M3 edge-F1 metric + review reel landed** (edge-F1/reel agent; PLAN
    §6 "Edge F1 vs source Canny", §7 M3 review-reel gate). Decisions:
    (a) **ground truth** = imageproc Canny on the RAW source (one streaming
    ffmpeg gray decode per clip through the identical
    `scale=W:H:flags=area,fps=N` ingest chain — independent of every
    factory tunable, same no-self-grading posture as the SSIM reference),
    downscaled to viewport-cell resolution through slpy-core's own
    `Resampler` BEFORE Canny ("at grid resolution", literally); fixed
    eval-owned thresholds `CANNY_LOW/HIGH = 60/140` picked on the corpus at
    300×80 (truth density ~1–10% of cells; sheep outline/fence/horizon and
    silhouette limbs traced, grass micro-texture + dim stars dropped —
    masks visually verified). Scored at the `ssim_every` cadence with a
    1-cell Chebyshev tolerance ring both ways (glyph quantization +
    deliberately-unthinned E make off-by-one correct, not lenient);
    NaN-free empty-frame conventions in edge.rs docs.
    (b) **prediction side / LayerMask contract**: §3.4 composition is
    override-only, so per-cell render metadata is a single u8 layer id —
    additive slpy-core API (`compose::layer`, `compose_cell_layer`,
    `compose_frame_masked`; masked output byte-identical to unmasked,
    unit-tested) + opt-in `Player::enable_layer_mask()`/`layer_mask()`
    (eval-only; interactive playback allocates nothing). **The M1 compose
    path still active in Player honestly tags every cell BASE, so eval
    currently reports edge_f1 = 0.0 with real nonzero truth — the M3
    pipeline integrator MUST switch an enabled mask to
    `compose_frame_masked` when wiring the three-layer compose (field doc
    in pipeline.rs); F1 then becomes live with zero eval-side changes.**
    (c) **schema/compare**: report schema v2 (edge_f1/precision/recall;
    deliberate M3 generation marker), `Tolerances.edge_f1_max_drop` 0.05
    (abs, drop-only — the aesthetic-regression drill's gate; only F1 is
    gated, P/R travel as diagnosis). Compare version policy changed:
    OLDER baseline → informational note + shared-metric compare (keeps the
    tuning loop unblocked against the v1 `runs/base.json` until the M3
    re-baseline); NEWER/unknown baseline → fail fast (the old both-ways
    fail-fast test was replaced by a both-directions pin, deliberate).
    (d) **review reel**: `eval --reel R.html` (a flag, not a subcommand —
    it reuses the same passes/cache; rows and GIF are collected during the
    truecolor pass). GIF = lossless 256-gray palette of the viewport-
    cropped ink raster (1×2 px/cell ≈ square pixels, so no aspect
    correction), 10 s @ 10 fps, infinite loop, `gif` crate; HTML rendering
    is a pure function (reel.rs) with self-containment unit tests; e2e
    coverage on the synthetic corpus in m2_params_eval.rs.
    (e) **deps**: workspace gains `image` (default-features off,
    codec-less buffers only) + `imageproc` (default-features off) for
    slpy-eval, `gif` for sleepy-factory — PNG I/O stays with the ffmpeg
    subprocess.
20. **M3 pipeline integration landed** (integrator). The player runs the
    full §3.5 path — see the sleepy-player lib section for the surface.
    Decisions recorded:
    (a) **Player::new signature** `want_color: bool` → `(ColorDepth,
    GlyphTier)`: palette selection is the player's job (Caps mapped via the
    new `glyph_tier_from_caps`/`color_depth`; `--palette` CLI override);
    no external crate consumed the old form outside this workspace.
    (b) **Resampler topology**: ONE luma tap-table build at Vc×2Vr (§3.3);
    one shared feature resampler at Vc×Vr for E/Ex/Ey + the H masks, built
    only when those planes exist; chroma unchanged. E/Ex/Ey are
    box-averaged (not max-pooled, despite the §4 "runtime max-pools"
    parenthetical): coherence = 2|(Ex,Ey)|/E is only meaningful when all
    three planes share the same linear resample — max-pooling E would
    depress coherence on perfectly coherent edges and mis-fire the
    junction band. Thin-edge survival is instead carried by the factory's
    UNTHINNED multi-px E ridges + the tunable [compose] edge gate.
    (c) **H bitflags** expand to per-bit 0/255 masks, box-average, then
    re-threshold: highlight ≥ 64/255 of the cell (sparse accents survive
    fine grids, single-px noise cannot own a coarse cell), deep shadow
    ≥ 128/255 (area feature). Constants in pipeline.rs.
    (d) **Hysteresis lifecycle**: reset on every levels-LUT rebuild (shot
    change — deliberate superset of the §3.5 CUT rule: a changed LUT makes
    remembered ramp indices stale, and every CUT is a shot change; pinned
    by tests/m3_layers.rs warmed-vs-cold-at-cut equality), realloc+reset on
    reflow, (0,0) below the viewport minimum. Fuzz invariants extended
    (`hysteresis_dims`, luma dst == Vc×2Vr).
    (e) **Rendering is now history-dependent by design**, so the M1
    "seek lands byte-identical" binary test was restated at the decode
    level (`m1_sim.rs::seek_lands_on_identical_decoded_planes` compares
    `luma_src` planes through the real Player) — the decode contract is
    unchanged; rendered-byte identity across different render histories is
    exactly what hysteresis intentionally breaks.
    (f) **Goldens re-keyed + re-pinned** (renderer changed by design):
    GoldenPalette → ascii/unicode/mono (= select_palettes configs), grids
    gained 48×12 (keeps the coarse density band covered) → 36 cell-grid
    snapshots; 4 tier goldens re-blessed. Parity now sweeps all three
    configs and the temporal state trajectory. compose_cells removed from
    the pipeline lib (see the section note).
    (g) **Benches/thresholds**: the synthetic bench asset carries all six
    §4 planes; compose bench = compose_frame with hysteresis + alternating
    inputs; perf/thresholds.toml recalibrated (3-run medians ×1.15) for
    decode (6-plane roll), compose (three-layer), present (busier M3
    grids) and e2e — resample unchanged.
    (h) **eval driver**: every pass measures the ASCII charset tier —
    deliberate: the SSIM ink-coverage table is ASCII-only (unknown glyphs
    fall back to mid-gray), so a unicode pass would score block fills as
    noise; the layer/hysteresis decisions under test are charset-
    independent. Unicode is covered by goldens/parity/fuzz/fps gates;
    per-font coverage tables are the M5 upgrade. [compose] flows via
    `Player::set_compose_params`; the enabled LayerMask flows through
    `compose_frame_masked`, making eval edge-F1 live (note 19 closed).
    (i) **ComposeParams defaults re-anchored to the factory's E scale**
    (core agent shipped T_on/T_off = 96/48 against synthetic magnitudes;
    factory E ≈ L\* contrast, t_hi 28, grass-corpus max ~129, then box-
    average dilution): edge_t_on/edge_t_off/edge_strong → 32/16/96,
    measured on the corpus (grass F1 0.72@32 vs 0.52@40 vs 0.00@96 with
    precision ≈ 0.77 — the coherence gates carry noise suppression).
    Sweep evidence in the M3 integration report.
21. **M3 Tune landed** (tune agent): `sleepy-factory sweep` per the Binaries
    section (PLAN §5 CLI — sweep.rs; ranked `sweep.json` schema v1 +
    `leaderboard.html`; committed axis grids under `sweeps/`). Decisions:
    (a) **composite score** = `0.4·mean(ssim) + 0.4·mean(edge_f1) −
    0.2·mean(flicker/2.0)` (means over clips; `[score]` overridable per
    grid file; flicker_norm 2.0 = the §6 gate, so a clip at the gate costs
    its full flicker weight);
    (b) **sweep mode plumbing**: `EvalArgs.truecolor_only` (sweep skips the
    256/mono damage passes; plain `eval` keeps full tier coverage) +
    `eval::TruthCache` (source-Canny masks memoized across combos — they
    depend on no tunable under test); `eval_clip`/`discover_corpus` are
    `pub(crate)` for the sweep driver; axes-crossed combos that fail
    `Params::validate()` are recorded as skipped with the reason;
    (c) **idx hysteresis width promoted to a tunable** (the §3.5 "0.35·step"
    constant): `slpy_core::hysteresis_idx` gained a `hyst_q8` parameter,
    `ComposeParams`/params.toml `[compose]` gained `idx_hyst_q8`
    (default 90 = the spec value; `IDX_HYST_Q8` remains as the documented
    default constant) — axis 3 of the mandated sweep plan trades cell
    stickiness against responsiveness with zero asset rebuilds.
22. **M3 Tune finish + M2-low fixes** (fix agent). Tuning (renderer-only —
    zero factory/asset changes; the SLPY byte pins and assets/ stay valid):
    (a) **`ComposeParams::idx_hyst_q8` default 90 → 160** (params.toml
    `[compose]` in lockstep; the pin tests still tie file ⇄ ComposeTable ⇄
    ComposeParams). Corpus sweep (note 21 composite score): 160 scores
    0.4160 vs 0.3932 @ 90; grass flicker 2.313 → 1.651 (fixes the M3 ≤ 2
    gate breach; sheep 1.288, silhouette 1.599), edge F1 byte-identical per
    clip, mean ssim flat. `IDX_HYST_Q8` (= 90) remains the §3.5 spec
    nominal.
    (b) **Near-white edge veto rides the PLAIN quantized index** (was: the
    hysteresis-held idx), so edge recall no longer couples to the width
    knob — measured F1 exactly invariant across widths 90–160 after the
    change. Single-frame renders are unaffected (fresh state quantizes
    plainly), so all 36 grid + 4 tier goldens stand unchanged.
    (c) **stage_ms informational-only in compare.rs** (M2-low fix b): see
    the Tolerances entry — deltas always pass, over-band jumps add `info:`
    notes, metric disappearance still fails. eval.sh can no longer go
    spuriously red from co-tenant load.
    (d) **Straggler filter session-long + split-ESC hold** (M2-low fix a):
    see the slpy-term probe section — the 2 s disarm window and the
    lone-ESC-kills-session hole are gone; new unit + pty regressions.
    (e) **perf-gate.sh coverage hardening** (M2-low fix c): thresholds
    entry with missing estimates → FAIL; entry not refreshed by this run's
    bench pass → FAIL (stale/renamed); fresh estimates without a
    thresholds entry → FAIL (silent coverage shrink). `--no-run` checks
    existence/coverage over whatever estimates exist.
    (f) **eval.sh corpus stages run release binaries** (<5 min wall budget;
    dev/release byte-identity is verified by the determinism guard each
    run) and **runs/base.json is the tuned-M3 baseline** (deliberate
    re-baseline: schema v2, edge-F1 family + tuned metrics; the M1-era
    baseline was unreproducible against the M3 renderer by design).
