# INTERFACES.md — frozen M0 public API

Written by the scaffold agent. This freezes the M0 `pub` surface pinned in code.
Implementers fill `todo!()` bodies; **signature/layout changes require updating
this file and a deliberate decision** — the factory⇄player format contract
(PLAN §4) and the §3.1 types are the riskiest interfaces in the system.

**As of M4 the OUTWARD-facing API is the `auto-ascii` facade crate** (see
"auto-ascii — THE public facade"); every other crate section below is the
workspace-internal registry behind it.

## Workspace & dependency edges (PLAN §2, §8)

```
crates/
  auto-ascii-core       lib   deps: (none beyond std)
  auto-ascii-term       lib   deps: auto-ascii-core; feature "session" (default ON) =
                        crossterm + libc — gates ansi/probe/restore + the
                        pty harness bin; Backend/Caps/EventQueue/quant/
                        diff-render/SimBackend are unconditional (M4 item B:
                        a sessionless build is terminal-free)
  auto-ascii-format     lib   deps: zstd, crc32fast, ciborium, serde(derive, META struct only)
  auto-ascii-eval       lib   deps: auto-ascii-core, auto-ascii-term, auto-ascii-format, serde, serde_json
                        dev: insta, proptest   (NEW at M2; auto-ascii-format added
                        at item C for the synthetic fixture builders)
  auto-ascii-factory lib+bin deps: auto-ascii-format, auto-ascii-core,
                        auto-ascii-term, auto-ascii-eval,
                        auto-ascii(default-features=false — pipeline only),
                        clap, indicatif, serde, serde_json,
                        toml, memmap2               (M2 item B additions)
                        M7: lib + THIN bin — src/lib.rs owns the modules and
                        the build entry; src/main.rs is the clap surface and
                        `inspect`. Unpublished (ffmpeg subprocess).
  auto-ascii      lib+bin  THE public facade (M4 item A; absorbed the
                        auto-ascii-player crate — pipeline, tests, benches, bin).
                        deps: auto-ascii-core, auto-ascii-format, memmap2,
                        auto-ascii-term(default-features=false)
                        features: default = ["bin"];
                          terminal = auto-ascii-term/session (Player/PlayerBuilder);
                          bin = terminal + clap + anyhow (the auto-ascii-player
                          binary, required-features gated).
                        --no-default-features = pure embedder: RenderSession
                        only; dep tree has NO clap/anyhow/crossterm
                        (M4 acceptance 4)
  auto-ascii-cli      bin   the `auto-ascii` binary (NEW at M7; unpublished).
                        deps: auto-ascii(path, DEFAULT features — `play`
                        needs the terminal Player, and the workspace entry
                        sets default-features=false), auto-ascii-factory(path,
                        the lib), auto-ascii-format, clap, memmap2, serde,
                        serde_json; dev: auto-ascii-eval (AVI fixtures).
                        No new external dependency entered the workspace.
                        It is a THIRD crate because the factory already
                        depends on the facade, so the binary needing both
                        cannot live in either.
```

- Root workspace: resolver 3, edition 2024, `license = "MIT OR Apache-2.0"`,
  `[profile.release] opt-level = 3`, all versions via `[workspace.dependencies]`.
- Factory deps `image`/`imageproc`/`ndarray`/`rayon` (PLAN §8) deliberately
  deferred: M0 is luma-only from ffmpeg rawvideo — add at M1/M3 when a stage
  needs them (per M0 scoping guidance).

## auto-ascii-core (PLAN §3.1–§3.4; pure, std-only)

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
    -> Option<Viewport>;   // None = below 32×9 → "enlarge terminal" card;
    // 16:9 convenience wrapper over compute_viewport_for (bit-identical)
pub fn compute_viewport_for(term_cols: u16, term_rows: u16, cell_aspect: f64,
    aspect_num: u16, aspect_den: u16) -> Option<Viewport>;  // M5 fix 2: the
    // letterbox targets the ASSET's header aspect (PLAN §4 aspect_num/den);
    // zero num/den falls back to 16:9. pipeline::Player::reflow_grid feeds
    // the header values through this, so Player AND RenderSession letterbox
    // non-16:9 assets correctly (tests: auto-ascii-core viewport.rs,
    // auto-ascii/tests/render_session.rs letterbox suite)

// resample.rs (§3.3) — IMPLEMENTED
pub struct Tap1D { pub src_start: u16, pub ntaps: u16, pub w_off: u32 } // Q8, sum 256
// ntaps widened u8→u16 by the auto-ascii-core implementer: 480 src cols → 1 dst col
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

// compose.rs (§3.4 L0-only, M0) — ADDED by the auto-ascii-core implementer (task e);
// M0 compositor: base ramp glyph + Rgb::gray(n) fg + black bg in the viewport,
// Cell::BLANK pads. Never allocates; `out` must already be term-grid-sized.
// Panics on grid/viewport mismatch, short luma, or empty ramp.
pub fn compose_luma(luma: &[u8], vp: &Viewport, ramp: &[char], out: &mut Grid<Cell>);

// ---- M3 (auto-ascii-core layers agent): §3.4 palettes + §3.5 three-layer
// compositor. compose_luma and ramp.rs are UNCHANGED (M0/M2 goldens). ----

// palette.rs (§3.4) — all 8 palettes as data + PaletteSet selection.
// auto-ascii-core stays terminal-free: the player maps Caps → these enums
// (Caps.glyphs/glyph_support → GlyphTier, Caps.color → ColorDepth).
pub enum GlyphTier { Ascii, UnicodeBlocks, BrailleVerified }
pub enum ColorDepth { True, C256, C16, Mono }   // mirrors auto-ascii-term ColorTier
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
pub const SUBPOS_GLYPHS: [char; 3];   // " - _ (§3.3; ascii-tier subposition;
                                      // CHANGED at M4 review: was ‾ U+203E,
                                      // which is NOT CP437 and boxed out on
                                      // the Linux console — the tier is now
                                      // pure ASCII, pinned by
                                      // every_ascii_tier_glyph_is_ascii)
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
pub fn tier_glyphs(GlyphTier) -> Vec<char>;   // M5 item B: every glyph the
pub fn all_palette_glyphs() -> Vec<char>;     // compositor can emit at a
    // tier / across all 8 palettes, ENUMERATED from the palette data
    // (select_palettes walked over depths × densities + edge LUTs +
    // subpos + quadrants + reachable braille masks); sorted, deduped.
    // Consumers: the font-table generator + the repertoire veto below.

// font_table.rs (§3.4 per-font ink-coverage tables) — NEW at M5 item B.
// Hand-rolled reader of exactly the `auto-ascii-factory font-table` TOML
// emitter subset (auto-ascii-core stays zero-dep); structural problems are
// Err(String), not panics (tables arrive via --font-table PATH).
pub const BUILTIN_FONT_TABLES: &[&str];  // conservative, dejavu-sans-mono,
                                         // liberation-mono, ubuntu-mono,
                                         // noto-sans-mono
pub struct FontTable;   // name + sorted (char, coverage 0..=1) + missing[]
impl FontTable {
  pub fn parse(&str) -> Result<FontTable, String>;
  pub fn builtin(name: &str) -> Option<&'static FontTable>;  // include_str!
      // of the committed fonts/*.toml, parsed once (LazyLock)
  pub fn name/entries/missing();
  pub fn coverage(char) -> Option<f32>;  // Some(0.0) for listed-but-missing
  pub fn has_glyph(char) -> bool;        // listed AND not in missing[]
  pub fn veto_tier(&self, want: GlyphTier) -> GlyphTier;
      // §3.4 repertoire veto: highest tier ≤ want whose tier_glyphs() the
      // font fully covers; degrades Braille → UnicodeBlocks → Ascii; Ascii
      // is the floor. Researched reality (cmap-verified, fonts/README.md):
      // NO common monospace font ships braille (DejaVu's braille is in the
      // Sans face, not Mono), Liberation lacks ╱╲ + corner quadrants,
      // Ubuntu Mono also lacks ‾ and every half/quadrant block — both veto
      // any unicode request to Ascii. Pinned by
      // builtins_load_and_veto_as_researched.
}

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
pub mod cell_flags { pub const WAS_EDGE: u8 = 1;
                     pub const WAS_QUADRANT: u8 = 2; }  // M4: quadrant
                     // noise-floor memory, independent of the edge gate
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
  pub quad_e_on/quad_e_off: u8,      // 2/1 (M4 review): quadrant-refinement
                                     // NOISE floor, dual-threshold via
                                     // edge_gate(). Guards the coherence
                                     // divide-by-max(e,1) at E 0–1 WITHOUT
                                     // eating the E∈[2,15] fine-diagonal
                                     // band (resampled E is box-diluted:
                                     // corpus source E mean 3.6) — an
                                     // earlier fix used edge_t_off (16)
                                     // here and silently downgraded every
                                     // real fine diagonal to a half-block
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
// per tap around the cell mean) / ascii " _ subposition → base ramp.
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

## auto-ascii-term (PLAN §3.1, §3.6) — M1: caps probe + color tiers + ?2026

M4 (item B): feature `"session"` (default ON) gates everything that touches
a real terminal — `ansi`/`probe`/`quirks`/`restore` modules, their re-exports
(`AnsiBackend`, `probe_caps`/`ProbeOptions`/`ProbeParser`/`ProbeReplies`/
`DEFAULT_PROBE_TIMEOUT`, `RESTORE_SEQ`/`install_restore_hooks`) and the pty
harness bin — plus the crossterm + libc deps. Everything below that is
capability data or pure code and stays unconditional (`Backend`, `Caps`,
`ColorTier`, `GlyphFlags`, `GlyphSupportTier`, `FrameStats`, `Event`/
`EventQueue`/`Key`, quantizer, diff renderer, `SimBackend`) — the
sessionless build is what the auto-ascii facade's pure-embedder
configuration links.

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
                          pub no_quirks: bool,   // --no-quirks (M5 item C):
                          // skip the quirk table AND bypass the cache both
                          // ways — never stored (cached entries must be the
                          // fully adjusted truth — cache hits can't
                          // re-quirk) and never read (cached entries embed
                          // quirk adjustments); always re-volleys
                          pub no_cache: bool, pub timeout: Duration,
                          pub cache_dir: Option<PathBuf> }     // + Default
pub fn probe_caps(&ProbeOptions) -> Caps;
// Never hangs: !isatty(stdin/stdout) or DA1 silence → conservative default
// (256-color, ASCII glyphs). tty flow: passive env hints (COLORTERM / TERM /
// TERM_PROGRAM / locale→glyph tier) as base; volley upgrades (XTGETTCAP RGB
// with a usable width → True; DECRPM 2026 Ps∈{1,2} → sync_2026; CSI 16 t →
// cell_px, overriding the TIOCGWINSZ pixel fields); forced_tier overrides
// color last. M4 (item D) tightened two reply readings against researched
// terminal behavior: Ps 3/4 are "permanently set/reset" = NOT support (VTE
// answers 4 for 2026), and the RGB cap is read BY VALUE — xterm answers the
// *valid* form `1+r524742=` hex("-1") when it is not in direct-color mode, so
// a prefix test used to promote every plain xterm to truecolor. Result cached at
// $XDG_CACHE_HOME/auto-ascii/caps (fallback ~/.cache) keyed on
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
pub fn ProbeReplies::sync_supported(&self) -> bool;  // NEW at M4: DECRPM 1|2
// M4 item D — per-terminal pty identity fixtures (tests/terminal_identity.rs,
// shared pty plumbing in tests/common/mod.rs): kitty / alacritty / wezterm /
// gnome-terminal (VTE) / xterm / xterm-direct / Linux console are replayed
// through probe_caps on a real pty — their env, their TIOCGWINSZ, their canned
// reply stream (sourced from each terminal's own code, cited inline) — and the
// resulting Caps asserted. The harness PROBE-DONE line gained `support=` and
// `glyphs=` fields, and mode `caps` (alias of probe-silent) is the human-facing
// diagnostic documented in docs/TERMINAL-CHECKLIST.md.
pub struct ProbeParser;   // incremental VT reply parser (pure; scripted-byte tests)
impl ProbeParser { pub fn new(); pub fn feed(&mut self, &[u8]) -> bool /*DA1 seen*/;
                   pub fn done(&self) -> bool; pub fn replies(&self) -> &ProbeReplies }
pub struct ProbeReplies { pub xtversion: Option<String>, pub decrqm_2026: Option<u8>,
                          pub xtgettcap_rgb: Option<bool>,
                          pub cell_px: Option<(u16,u16)>, pub da1: bool }

// quirks.rs — NEW at M5 item C (PLAN §3.1 "quirk table keyed on queried
// identity"); session-gated. Matched on the XTVERSION reply prefix + the
// XTGETTCAP-RGB reading — never TERM; applied by probe_caps post-volley,
// pre-forced-tier, never on cache hits / --no-query (details + the two
// sourced entries: note 26a).
pub struct Quirk { pub name: &'static str, pub xtversion_prefix: &'static str,
                   pub rgb_reply: Option<Option<bool>>,    // None = don't care
                   pub color_at_least: Option<ColorTier>,  // caps adjustments
                   pub color_at_most: Option<ColorTier> }
pub const QUIRKS: &[Quirk];   // kitty-rgbless-xtgettcap, xterm-no-direct-color
pub fn apply_quirks(caps: &mut Caps, &ProbeReplies) -> Vec<&'static str>;
    // returns the names applied (probe logging/tests)

// quant.rs — NEW at M1 (PLAN §3.1 quantize-before-diff; pure math)
pub fn rgb_to_256(Rgb) -> u8;     // xterm 6×6×6 cube (16–231) + gray ramp (232–255)
pub fn rgb_to_16(Rgb) -> u8;      // nearest of the standard 16 (xterm defaults)
pub fn ansi256_to_rgb(u8) -> Rgb; // canonical inverse (roundtrip-exact 16..=255)
pub fn ansi16_to_rgb(u8) -> Rgb;

// event.rs — crossterm types never leak into the pub API
pub enum Key { Char(char), Ctrl(char), Esc, Left, Right }
    // Left/Right NEW at M5 (scrub UX): arrow keys mapped by AnsiBackend;
    // the player turns them into ±5 s seeks (SCRUB_STEP_SECS)
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

## auto-ascii-format (PLAN §4; container only, no I/O policy) — M1: full ASCI v1

```rust
// header.rs — 64-B layout frozen (offset-freezing tests)
pub const MAGIC: [u8;4] = *b"ASCI"; pub const HEADER_SIZE: u32 = 64;
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
pub struct AsciiHeader { ... unchanged ... }  // + to/from_bytes

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

// meta.rs / error.rs — unchanged (Meta CBOR via ciborium; AsciiError as at M0)

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
impl AsciiWriter<W> {
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
// (clean AsciiError, adversarial-review fixes): base_w/h == 0, fps_num/den
// == 0, keyframe_ivl == 0, dup/zero plane ids, unknown filter, malformed
// NORM, delta asset whose frame 0 is not a keyframe, non-increasing FIDX,
// index_offset near u64::MAX (checked_add bounds check — the unchecked add
// wrapped and open() panicked at the FIDX header slice).
impl AsciiReader<'a> {
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

## auto-ascii-eval (PLAN §6; M2 item A — metrics library, no I/O beyond serde)

Library-only measurement primitives + the versioned JSON report schema.
The driver that builds assets, runs SimBackend and writes `runs/*.json` +
HTML contact sheets is `auto-ascii-factory eval` (M2 item B) — not this crate.

```rust
// coverage.rs — glyph ink-coverage table (§6 "rasterize through the stored
// glyph-coverage tables"). Built-in conservative table derived from DejaVu
// Sans Mono via ffmpeg drawtext at 64×128 px/cell (§3.4's raster size),
// coverage = mean gray / 255 (antialiased ink integral); derivation script
// committed at crates/auto-ascii-eval/tools/derive_coverage.py, constants are the
// artifact (no corpus/font dependency at test time). Covers all printable
// ASCII (⊇ every shipped palette incl. the PLAN §3.4 mono ramp " .:coO8@").
pub const CONSERVATIVE_COVERAGE: &[(char, f32)];  // 95 entries, sorted
pub struct CoverageTable;   // sorted entries + max; per-font tables: M5 ↓
impl CoverageTable {
  pub fn conservative() -> &'static CoverageTable;
  pub fn from_font_table(&auto_ascii_core::FontTable) -> CoverageTable;  // M5 item
      // B: per-font scoring (eval --font-table). Missing glyphs enter at
      // coverage 0 (blank ink, the §3.4 missing-glyph policy — NOT the
      // unknown-glyph mid-gray fallback); max_coverage (the normalize_ink
      // anchor) tracks the table, so absolute SSIM is only comparable
      // within one table choice.
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
// = §6 downscale-SSIM: source resampled to rendered dims through auto-ascii-core's
// own Resampler (same box-average semantics as the player), then ssim.
// Pass the viewport-cropped raster (GrayImage::crop) — pads are not scored.

// edge.rs — NEW at M3: §6 edge F1 vs SOURCE Canny at grid resolution
// (ground truth is never the factory's own planes — the driver streams the
// raw fps-normalized gray source; imageproc canny; auto-ascii-eval gained the
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
    // source luma → auto-ascii-core Resampler box-average downscale to the
    // viewport grid (same semantics as the player) → imageproc canny.
    // "grid resolution" = viewport cells (anisotropic ~1:2) — the metric is
    // cell-level by definition
pub fn edge_cells_from_layers(&Grid<u8>, &Viewport) -> EdgeMask; // crop pads,
    // select auto_ascii_core::compose::layer::EDGE
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

// stats.rs — damage/bytes aggregation from auto-ascii-term FrameStats + stage timers.
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
// corpus). Pure integer plane generators → AsciiWriter in memory (no ffmpeg,
// no files, no floats). Test support: invalid assets PANIC (not Result).
pub const FIXTURE_BASE_W/H: u16 = 192/108;   // 16:9, C plane 96×54 RGB565
pub const FIXTURE_FRAMES: u32 = 72;          // 30 fps, keyframes every 24
pub const FIXTURE_KEYFRAME_IVL: u8 = 24;
pub const HARD_CUT_FRAME: u32 = 36;          // mid-GOP shot boundary
pub enum Fixture { GradientMotion, HardCut, CheckerDrift }  // + ALL, name()
pub fn luma_plane/chroma_plane(Fixture, frame: u32) -> Vec<u8>;  // pure
pub fn shot_records(Fixture) -> Vec<ShotRecord>;  // HardCut: 2 shots, CUT
pub fn build_fixture(Fixture) -> Vec<u8>;    // full ASCI v1 (Y+C, delta,
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
                                 // auto-ascii/tests/pipeline_parity.rs
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

## auto-ascii — THE public facade (M4 item A; source of truth for the API)

The `auto-ascii` crate is the one crate an outside project depends on; every
`ascii-*` crate is an implementation detail behind it. Public surface —
audited item-by-item against "does a simple embedding project need this?":

```rust
// lib.rs — always available (also under --no-default-features)
pub enum Error;                       // one coherent error (thiserror-style
    // layering, hand-rolled): Io{path,source} | Format{path,source:AsciiError}
    // | Asset(&'static str) | Decode{frame,plane,source:AsciiError}
    // | Config(String) | Terminal(io::Error); #[non_exhaustive];
    // M5 fix 1: Display states THIS layer only; the cause is exposed via
    // source() alone, so anyhow-style chain printers show it exactly once
pub enum PaletteChoice { Auto, Ascii, Unicode, Braille }  // §3.4 charset axis
    // Auto = probed caps (Player) / Unicode blocks (RenderSession);
    // braille NEVER chosen automatically
pub use auto_ascii_core::{Cell, Grid, Rgb};  // what render() hands back — nothing
    // else from auto-ascii-core is re-exported (resampler, palettes, viewport,
    // hysteresis: engine internals a simple project never touches)
pub mod timecode;  // M7 (PLAN-M6-M8 §2): the project's ONE timestamp grammar
    // pub fn parse(&str) -> Result<f64, TimecodeError>   // SS[.f] | MM:SS[.f]
    //     | HH:MM:SS[.f]; fields trimmed, NOT range-checked against the unit
    //     above them (0:90 == 90 s), negatives/inf/NaN rejected
    // pub fn format_mmss(f64) -> String                  // M:SS, H:MM:SS past
    //     the hour — the progress overlay's shape; <0/NaN print 0:00
    // pub enum TimecodeError { TooManyFields, BadField(String),
    //     OutOfRange(String) }   // Display text is user-facing verbatim
    // Core tier: no deps, no features. `auto-ascii-player --seek` and
    // `auto-ascii import --ss/--t` are both this function.

// composition.rs — M8 (PLAN-M6-M8 §3): clips stitched on one timeline.
// Core tier EXCEPT the TOML parser (feature `compose`, default-on via
// `terminal`); nothing here re-encodes anything.
pub const SCHEMA_VERSION: i64 = 1;    // the only `schema` this build reads
pub struct Clip { pub name: String, pub path: PathBuf, pub in_secs: f64,
                  pub out_secs: Option<f64>, pub at_secs: Option<f64> }
    // as written in the file; `Clip::new(path)` = untrimmed, sequential
pub struct ClipSpan { pub start_frame, end_frame: u32,
                      pub start_secs, end_secs, in_secs, out_secs: f64,
                      pub fps_num, fps_den: u16, pub frame_count: u32,
                      pub base_w, base_h, aspect_num, aspect_den: u16 }
    // + fps() / len_frames() / len_secs() / source_secs() / planes()
    // -> &[u8] / contains_frame(u32)
    // one per clip, parallel to clips(): the resolved `[start, end)` table
    // `compose show` prints and `export` validates. The timeline is FRAMES
    // at the composition rate and the seconds are derived from them —
    // seconds cannot express a boundary (a 0.3 s slice is
    // 0.30000000000000004), which used to hand the frame at exactly 0.3 s
    // to the wrong clip at an already-trimmed source frame
pub struct Located { pub clip_idx: usize, pub local_frame: u32 }
pub struct Span { pub start_frame, end_frame: u32,
                  pub start_secs, end_secs: f64 }   // + len_frames()
pub struct Overlap { pub span: Span, pub under: usize, pub over: usize }
pub enum ClipMark { Clear, Partial, Hidden }   // `compose show`'s verdict
pub struct Composition;
impl Composition {
  pub fn single(path: impl Into<PathBuf>) -> Composition;     // one clip, no I/O
  pub fn from_clips(name: impl Into<String>, Vec<Clip>) -> Composition;
      // `auto-ascii cut` = this with one trimmed clip, exported
  pub fn from_toml_str(&str, base_dir: &Path, library_dir: Option<&Path>)
      -> Result<Composition, Error>;                          // feature `compose`
  pub fn from_toml_file(impl AsRef<Path>, library_dir: Option<&Path>)
      -> Result<Composition, Error>;                          // feature `compose`
      // `asset` resolves as an existing path relative to the FILE, else
      // <library>/<asset>.ascii. Unknown keys, a wrong `schema`, a bad time
      // and an asset that resolves nowhere are Error::Config — every
      // clip-level message names the CLIP INDEX (hand-rolled over
      // toml::Table for exactly that reason; serde is not a direct dep)
  pub fn default_library_dir() -> Option<PathBuf>;
      // $AUTO_ASCII_HOME/library, else ~/auto-ascii/library, only if it exists
  pub fn resolve(&mut self) -> Result<(), Error>;
      // THE I/O step: OPENS each clip as a container through a mapping
      // (`AsciiReader::open` — header, TRLR anchor, chunk roll, FIDX; a
      // truncated asset must fail here, not once the session is up), takes
      // fps/frames/base res/aspect/plane ids from its header, validates
      // 0 <= in < out <= asset duration (with half a frame of slack at the
      // end, then clamped — the duration the tools PRINT must be usable as
      // `out`) and rejects an empty composition, a zero-frame or luma-less
      // clip — every error names the clip index. Idempotent; everything
      // below is defined only after it succeeds (unresolved reports zeros)
  pub fn is_resolved(&self) -> bool;
  pub fn name(&self) -> &str;              pub fn clips(&self) -> &[Clip];
  pub fn timeline(&self) -> &[ClipSpan];   pub fn aspect(&self) -> f64;
  pub fn fps(&self) -> f64;                // the HIGHEST clip fps
  pub fn fps_ratio(&self) -> (u16, u16);   // that clip's exact header rational
  pub fn duration_secs(&self) -> f64;      // the latest clip end
  pub fn frame_count(&self) -> u32;        // duration × fps, snapped
  pub fn locate(&self, t_secs: f64) -> Option<Located>;   // = locate_frame(t·fps)
  pub fn locate_frame(&self, frame_idx: u32) -> Option<Located>;
      // THE time→frame function (§3 semantics): file order, `at` overrides,
      // ends EXCLUSIVE, overlap → the LATER-listed clip, gap → None (black,
      // not an error). Pure integer over the frame grid; float positions
      // are snapped to the exact integer within 1e-6 frames (absolute), so
      // a single asset maps frame f to frame f and a boundary frame has
      // exactly one owner
  pub fn is_toml_path(path: &Path) -> bool;   // `.toml`, case-insensitive:
      // the one rule for "clip or composition?" (player bin, examples, CLI)
  pub fn frame_after(&self, base_frame: u64, elapsed_secs: f64) -> u64;
      // the run loop's one pacing expression, unchanged from M5
  pub fn frame_at_secs(&self, secs: f64) -> Result<u32, Error>;
      // THE `--seek` bound check (PlayerBuilder, --sim, --bench-seek): one
      // copy, and the past-the-end message names the composition or the
      // asset per is_stitch()
  pub fn is_stitch(&self) -> bool;   // a real stitch, vs one asset wrapped
      // by single() — what messages call it, and whether the progress row
      // prints ` c/N `
  pub fn gaps(&self) -> Vec<Span>;               // stretches no clip covers
  pub fn overlaps(&self) -> Vec<Overlap>;        // every sharing PAIR,
      // later-listed clip `over`, timeline order
  pub fn mark_for(&self, clip_idx: usize) -> Option<ClipMark>;
      // Clear / Partial / Hidden (whole span covered by later clips)
      // All three are computed on the FRAME grid, so abutting clips can
      // never report a sliver gap or a one-ULP overlap — which is what a
      // float analysis of the same timeline does (`GAP 0.20 0.20`,
      // `end_secs: 0.30000000000000004`). `compose show` is a lookup.
}

// compose.rs — M8 export (always available; no TOML needed)
pub mod compose {
  pub struct ExportOptions { pub keyframe_ivl: u8, pub zstd_level: i32 }
      // Default = params.toml [build]: 60 / 15 (NOT auto-ascii-format's 19)
  pub struct ExportReport { pub frames: u32, pub fps: f64, pub bytes: u64,
                            pub shots: u32, pub cuts: u32 }
  pub fn export(&Composition, out: &Path, &ExportOptions)
      -> Result<ExportReport, Error>;
      // ATOMIC (as the factory's build pass is): frames go to `<out>.part`,
      // renamed over `out` only once FIDX + TRLR + the patched header are
      // down; any failure removes the part and leaves an existing `out`
      // untouched. One mapping per clip, but decoders (plane buffers +
      // FIDX) are opened on demand and capped LRU at MAX_LIVE_SOURCES (4),
      // so memory tracks live clips, not clip count; the NORM pre-pass
      // copies shot tables and drops its readers.
      // Flatten to one asset: every clip must share ONE base resolution and
      // ONE plane registry (the error names the offending clip and suggests
      // `import --res`). Planes are copied verbatim from the top clip
      // (sequential roll, else FIDX seek); gaps write black planes; NORM is
      // one pre-pass — a record per (clip slice ∩ source shot), rebased to
      // output frames, CUT at every clip boundary and gap edge (record 0
      // keeps the source's own flag), gap records identity levels
}

// session.rs — the terminal-free embedder entry (always available)
pub struct RenderSession;   // owns the mmap + decode state + hysteresis
impl RenderSession {
  pub fn open(path: impl AsRef<Path>) -> Result<RenderSession, Error>;
      // = from_composition(Composition::single(path)): ONE path, and
      // resolving opens the container (so a truncated asset fails here,
      // not at the first frame). Defaults: Unicode palette, truecolor
      // cells (embedder owns quantization), cell aspect 2.0.
      // Internally a one-clip `deck::ClipDeck` (M8) holding
      // Player<'static> over the owned map (encapsulated self-reference;
      // SAFETY comment in deck.rs — drop order pins the borrow, the fake
      // 'static never escapes that module)
  pub fn open_composition(impl AsRef<Path>, library_dir: Option<&Path>)
      -> Result<RenderSession, Error>;                    // M8, feature `compose`
  pub fn from_composition(Composition) -> Result<RenderSession, Error>;  // M8
      // Same render() contract on the COMPOSITION's timeline: frame_idx
      // counts its frames at its fps(), one mmap + one decode pipeline per
      // clip built on first use, a clip switch resets temporal state (each
      // clip has its own, so the switch starts fresh by construction — the
      // reset is for RETURNING to a clip whose state is as stale as the
      // time away), a gap renders an all-blank grid. Resolves the
      // composition if the caller has not. Single-asset sessions are
      // untouched: frame_idx maps to itself, no timeline in the way
  pub fn render(&mut self, frame_idx: u32, cols: u16, rows: u16)
      -> Result<&Grid<Cell>, Error>;
      // letterboxed compose at cols×rows; <32×9 renders the enlarge card.
      // TEMPORAL-STATE CONTRACT (documented on the type): monotonic
      // frame_idx advance (skips fine) = full hysteresis quality;
      // BACKWARD jump = automatic full temporal reset (no pre-seek
      // ghosting, landing frame == cold start); grid size change =
      // realloc+reset (same as terminal resize)
  pub fn fps(&self) -> f64;           // drive your clock: (t·fps) as u32
  pub fn frame_count(&self) -> u32;   // > 0, enforced at open
  pub fn aspect(&self) -> f64;        // asset picture aspect (w/h, ≈1.778);
      // since M5 fix 2 this IS the letterbox target ratio render() uses
  pub fn set_palette(&mut self, PaletteChoice);            // resets temporal
  pub fn set_cell_aspect(&mut self, f64) -> Result<(), Error>; // §3.2 knob
      // (1.0 for square cells in an embedder's own renderer)
  pub fn set_font_table(&mut self, Option<&str>) -> Result<(), Error>;
      // M5 item B (§3.4 --font-table): builtin NAME | PATH to a generator
      // TOML | None to clear. The table's repertoire VETOES the palette
      // choice via FontTable::veto_tier (braille→unicode→ascii) — resets
      // temporal state like set_palette; bad specs are Error::Config and
      // leave the session untouched
}

// player.rs — feature "terminal" (in the default set via "bin")
pub enum RepaintMode { Full /*default*/, Diff }
pub struct PlayerBuilder;   // Default; #[must_use]
impl PlayerBuilder {        // the spec'd builder (§7 M4) + escape hatches
  pub fn asset(self, impl Into<PathBuf>) -> Self;          // REQUIRED
  pub fn composition(self, impl Into<PathBuf>) -> Self;    // M8: …or this,
      // a composition .toml — mutually exclusive with asset(). The whole
      // run loop then counts COMPOSITION frames: --seek, 0-9, the arrows,
      // --loop, --fps-cap and the progress row. Bare library names inside
      // the file resolve through Composition::default_library_dir()
  pub fn palette(self, PaletteChoice) -> Self;             // default Auto
  pub fn tier(self, Option<ColorTier>) -> Self;   // Some = force + skip volley
  pub fn repaint(self, RepaintMode) -> Self;
  pub fn fps_cap(self, f64) -> Self;    // >= MIN_FPS_CAP (1.0) checked at
      // build (M5 fix 3: tiny caps used to panic/hang in run() AFTER the
      // session started); pub const MIN_FPS_CAP: f64 = 1.0 is exported
  pub fn looping(self, bool) -> Self;
  pub fn cell_aspect(self, f64) -> Self;          // finite >0 checked at build
  pub fn seek_secs(self, f64) -> Self;            // FIDX seek; bounds at build
  pub fn duration_secs(self, f64) -> Self;        // stop after N s wall clock
  pub fn no_query(self, bool) -> Self;            // probe escape hatches
  pub fn no_cache(self, bool) -> Self;            //   (PLAN §3.1)
  pub fn no_quirks(self, bool) -> Self;           // M5 item C: skip the
      // identity-keyed quirk table (auto-ascii-term quirks.rs) post-probe
  pub fn font_table(self, impl Into<String>) -> Self;  // M5 item B (§3.4):
      // builtin NAME | PATH; parsed+validated at build(); run() applies the
      // repertoire veto AFTER resolve_for_caps (user-asserted font truth
      // degrades the probed/forced tier, never upgrades it)
  pub fn build(self) -> Result<Player, Error>;    // opens+validates the asset;
      // does NOT touch the terminal — bad path/file fails before any
      // screen state changes (incl. font-table resolution)
}
pub struct Player;          // asset open+validated, terminal untouched
impl Player {
  pub fn builder() -> PlayerBuilder;
  pub fn run(self) -> Result<(), Error>;  // BLOCKING: probe (§3.1) →
      // AnsiBackend session (restore hooks armed first) → the §3.6
      // wall-clock loop (latest-frame-wins, digit jumps, resize reflow) →
      // shutdown/restore. Consumes self; the M0–M3 machinery verbatim
      // (moved from the old auto-ascii-player main.rs — no logic fork with the
      // bin, which is now a pure argv shim)
}
pub use auto_ascii_term::ColorTier;   // the tier(..) argument type — the ONLY
    // auto-ascii-term re-export; Caps deliberately NOT re-exported (probing is
    // run()'s internal business; audit: a simple project never needs it)
```

Deliberately `#[doc(hidden)]` (workspace harness contract, semver-exempt):
`auto_ascii::pipeline` (below), `PaletteChoice::resolve_for_caps(&Caps)`
and `auto_ascii::load_font_table(&str) -> Result<auto_ascii_core::FontTable,
Error>` (M5: the bin's `--sim` path applies the same repertoire veto as
run(); embedders use the wrapped forms above) — CLI/--sim plumbing — plus
`auto_ascii::deck` (M8): `ClipDeck::new(Vec<PathBuf>, DeckConfig)` and the
render/overlay forwarders `activate`/`render_grid`/`render_present`/
`drain_events`/`set_*`/`stage`/`layer_mask`, plus `MAX_LIVE_CLIPS` (8) and
`live_clips()`. The render surface is exactly three calls —
`render_at(Option<Located>)`, `present_at(backend, Option<Located>)` and
`showing()` — so the locate→activate→render dispatch exists ONCE rather
than at each of the four call sites; `stage()` carries evicted clips' time
and times gap presents, so a composition's budget still adds up. It is the ONE clip-switch
implementation — `RenderSession`, the terminal `Player` and `--sim` all
drive it — and it never hands out the `Player<'static>` it holds, which is
what keeps the fake `'static` inside the module. At most `MAX_LIVE_CLIPS`
pipelines are resident: opening one past the cap evicts the least recently
fronted clip, clearing its player BEFORE its mapping (same drop-order
argument), and re-opening is the lazy path again — cold, which is the
temporal reset a return to a clip wants anyway.

## auto_ascii::pipeline — the hidden engine room (ex auto-ascii-player lib)

Extracted to a lib at M2 so `auto-ascii-factory eval` drives the EXACT player
frame pipeline headlessly (metrics must measure the real renderer, not a
reimplementation — note 14); M4 moved it verbatim from `auto_ascii_player::` to
`auto_ascii::` and hid it from the public docs. Consumers: the auto-ascii-player
bin (--sim), factory eval, resize fuzz, perf benches, parity goldens.

M4 signature changes: all `anyhow::Result` became `Result<_, auto_ascii::Error>`
(same coherent type as the facade; factory's `?` still works — Error is a
std error). New: the backend seam is split so RenderSession stays
terminal-free — `reflow_grid(cols, rows)` (everything but backend
resize/invalidate) and `render_grid(frame_idx)` (everything but present);
`reflow`/`render_present` are now thin wrappers over them + the backend
calls (call order and bytes IDENTICAL to M3 — verified by the pre/post
sim-dump sha256 pin at M4). Also new: `reset_temporal_state()` (the §3.5
discontinuity reset, used by RenderSession backward jumps),
`set_glyph_tier(GlyphTier)` + `set_cell_aspect(f64)` (take effect at next
reflow; RenderSession setters).

```rust
// pipeline.rs — M3: the full §3.5 three-layer path (integrator; note 20).
pub struct StageNs { pub decode, resample, compose, present: u64 } // ns, Copy
pub struct Drained { pub quit: bool, pub jump_digit: Option<u8>,
                     pub seek_steps: i32 }  // M5 scrub UX: net Left(−1)/
    // Right(+1) presses, coalesced per drain; caller converts to ±5 s.
    // Nonzero ⇒ hysteresis already reset (same rule as jump_digit).
    // M3 review fix (medium, seek ghosting): when jump_digit is Some,
    // drain_events has ALREADY reset all hysteresis state — a digit seek is
    // a temporal discontinuity (same class as the §3.5 cut/resize resets;
    // update_levels only covers jumps that cross a shot boundary, so a
    // same-shot jump used to ghost pre-seek was_edge/idx into the landing
    // frame). Callers just repoint their clock and render (regression:
    // auto-ascii/tests/m3_layers.rs digit_jump_seek_resets_hysteresis_state).
pub fn glyph_tier_from_caps(&Caps) -> GlyphTier;  // AsciiOnly/Cp437→Ascii,
      // UnicodeCore→UnicodeBlocks, UnicodeFull→UnicodeBlocks unless
      // Caps.glyphs has BRAILLE (verified-only) → BrailleVerified
pub fn color_depth(ColorTier) -> ColorDepth;      // 1:1 variant map
pub struct Player<'a>;   // decode → resample → NORM LUT → compose → present
impl<'a> Player<'a> {
  pub fn new(reader: AsciiReader<'a>, cell_aspect: f64, repaint_full: bool,
             color: ColorDepth, glyph_tier: GlyphTier)
      -> Result<Player<'a>, Error>;   // M4: facade Error
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
  pub fn set_progress_overlay(&mut self, visible: bool);  // M5 scrub UX:
      // 1-line bottom-row progress bar drawn over the composed grid by
      // render_grid; visible→hidden schedules a one-shot backend invalidate
      // consumed by the next render_present (the diff baseline can never
      // keep describing overlay cells). Presentation-only: hysteresis and
      // the LayerMask are untouched; eval never enables it. Tested by
      // auto-ascii/tests/scrub_overlay.rs (strict escape-stream replay).
  pub fn render_present<B: Backend>(&mut self, backend: &mut B, frame_idx: u32)
      -> Result<FrameStats, Error>;  // M4: facade Error
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
pub fn draw_progress_overlay(grid: &mut Grid<Cell>, frame: u32,
                             frame_count: u32, fps: f64);  // M5 scrub UX:
    // the bottom-row bar (" M:SS / M:SS [====>....] NN% ", pure ASCII,
    // byte-deterministic — unchanged rows cost zero diff damage)
// M8 (PLAN-M6-M8 §3) additions:
pub struct ProgressContext { pub frame, frame_count: u32,
                             pub fps_num, fps_den: u16,
                             pub clip: Option<(usize, usize)> }  // + fps()
pub fn Player::set_progress_context(&mut self, Option<ProgressContext>);
    // the row reports the COMPOSITION's position instead of this clip's own
    // frame counter; None (every single-asset path) is the M6 row verbatim
pub fn draw_progress_overlay_clips(grid, frame, frame_count, fps,
                                   clip: Option<(usize, usize)>);
    // draw_progress_overlay + " c/N " right after the time block, dropped
    // below PROGRESS_HINT_MIN_COLS (64) and for single-clip compositions —
    // so `draw_progress_overlay` is literally this with clip = None
pub fn drain_backend_events<B: Backend>(&mut B) -> (Drained, Option<(u16,u16)>);
    // the key mapping, coalesced, with NO player state touched (Quit still
    // wins and stops the drain). Player::drain_events is this plus the
    // reflow/reset; deck::ClipDeck::drain_events is this plus its own —
    // a gap frame has no clip player to drain through
// REMOVED at M3: compose_cells (the M1 base-only compositor) — the §3.5
// path replaced it wholesale; keeping it invited silent drift between the
// shipping renderer and the golden harness.
```

(M4 additions to this registry — `reflow_grid`/`render_grid`/
`reset_temporal_state`/`set_glyph_tier`/`set_cell_aspect` — are described in
the facade section above. Nothing else in the crate is `pub` outside the
facade surface + this hidden module.)

## Binaries

- `auto-ascii-factory` (PLAN §5), CLI as of M3 (+ M5 item B):
  `build <in> -o <out> [--ss T] [--t T] [--fps N] [--res WxH] [--params F]`,
  `inspect <asset> [--dump-planes DIR] [--frame N]...` (M3: per-plane value
  stats over sampled frames + optional PGM/PPM plane dumps for eyeballing),
  `params --dump [--params F]`,
  `font-table <font.ttf> -o T.toml [--name N] | font-table --conservative
  -o T.toml` (M5 item B: rasterizes `all_palette_glyphs()` at 64×128 via
  ab_glyph — advance-fitted, ink-box centered+clipped — into a
  deterministic TOML coverage table; same font bytes ⇒ byte-identical
  output, unit-tested; missing glyphs get coverage 0 + `missing` entry +
  stderr WARN; the five committed `fonts/*.toml` are its artifacts, see
  fonts/README.md),
  `eval --corpus <dir> [--params F] [--baseline B.json] --out X.json
  [--html X.html] [--reel R.html] [--cache-dir D] [--font-table NAME|PATH]`
  (M5: `--font-table` swaps the SSIM rasterizer's ink model — builtin name
  or generator TOML path; default `conservative` = the committed baseline's
  table; sweep always scores conservative),
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
  edge_white_cut_q8/halfblock_min_delta/edge_strong/quad_e_on/quad_e_off`
  (M3 integrator + M4 review quadrant noise floor, note
  20: RENDERER knobs — mapped onto `auto_ascii_core::ComposeParams` and handed to
  the Player by the eval driver; deliberately EXCLUDED from
  `build_fingerprint`, so compose sweeps never rebuild assets; defaults
  pinned to `ComposeParams::default()` by unit test),
  `[eval] grid_cols/grid_rows/max_frames/ssim_every/contact_frames` +
  `[eval.tolerances]` (auto-ascii-eval `Tolerances` subset). `build.keyframe_ivl`
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
  auto-ascii-factory + auto-ascii-format `src/*.rs`, emitted by build.rs — M2 review
  fix 4d: factory/format code changes must invalidate cached corpus
  assets) → edge-F1 ground-truth pass (M3: one streaming ffmpeg gray decode
  of the source through the identical scale/fps chain; Canny masks at the
  `ssim_every` cadence + reel timestamps, see auto-ascii-eval edge.rs) → three
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
- `auto-ascii-player` (PLAN §3) — M4: now built from `crates/auto-ascii`
  (`[[bin]]` behind the default-on `bin` feature, so `cargo install
  auto-ascii` ships it; `required-features` keeps embedder builds
  binary-free). The bin is a thin argv shim: interactive flags map 1:1 onto
  `PlayerBuilder` and `run()` (no logic fork); `--sim` drives
  `auto_ascii::pipeline` directly. CLI unchanged since M3 and byte-identical
  in behavior (sim-dump sha256 pinned pre/post move). CLI as of M3 (see
  notes 9, 11 and 20):
  `<asset> [--repaint full|diff] [--loop] [--fps-cap FPS] [--cell-aspect F]
  [--duration-secs N] [--seek TIMESTAMP] [--tier TIER] [--no-query]
  [--no-cache] [--no-quirks] [--palette auto|ascii|unicode|braille]
  [--bench-seek N]
  [--font-table NAME|PATH] [--sim COLSxROWS:NFRAMES] [--sim-tier TIER]
  [--sim-dump PATH] [--sim-resize [COLSxROWS]]`.
  M8: the positional argument may be a composition `.toml` instead of an
  asset (decided by the extension, the same rule `auto-ascii play` follows)
  — interactively it becomes `PlayerBuilder::composition`, and `--sim` /
  `--bench-seek` walk the composition timeline through `deck::ClipDeck`.
  Every flag keeps its meaning on that timeline; bare library names inside
  the file resolve through `Composition::default_library_dir()`.
  M5 item B: `--font-table` maps onto `PlayerBuilder::font_table`
  (interactive) and applies the identical repertoire veto on the `--sim`
  path via the hidden `auto_ascii::load_font_table`.
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
- `auto-ascii` (PLAN-M6-M8 §2) — M7, the agent-first CLI, built from
  `crates/auto-ascii-cli` (package `auto-ascii-cli`, binary `auto-ascii`;
  unpublished, like the factory it depends on). CLI as of M8:
  `[--json] import <video> [--name N] [--ss T] [--t T] [--fps N]
  [--res WxH] [--force] | list | info <clip> |
  cut <clip> --in T --out T [--name N] [--force] |
  compose new <name> | compose add <name> <clip> [--in T] [--out T] [--at T] |
  compose show <name> | compose play <name> |
  compose export <name> [-o path] [--force] |
  play <clip | composition> | agent-guide | home`. `--json` is global
  (accepted before or after the subcommand, at any subcommand depth).
  **Home folder:** `~/auto-ascii`, or `$AUTO_ASCII_HOME` when set and
  non-empty, with `library/` + `compositions/` + `exports/` created on
  demand by `home` and by `import` (NOT by `agent-guide`, which resolves no
  home at all and works with `HOME` unset). **`<clip>` resolves** as an
  existing path first, else `library/<clip>.ascii`, else
  `library/<kebab(clip)>.ascii`. **Names are kebab-case on the WAY IN:**
  lowercase, every run of non-ASCII-alphanumerics collapsed to one `-`,
  ends trimmed — applied by `import` to `--name` as well as to the derived
  file stem, which also makes a name incapable of holding a path separator
  or `..`. Readers (`list`, `info`) report the file stem VERBATIM instead,
  so they cannot disagree about a file a human dropped in by hand; the
  kebab step in `resolve_clip` is the bridge between the two.
  **Timestamps** (`--ss`, `--t`) go through `auto_ascii::timecode::parse`,
  the facade's one grammar (`SS[.f]`, `MM:SS[.f]`, `HH:MM:SS[.f]`), shared
  with `auto-ascii-player --seek`; they are parsed by the command, not by a
  clap `value_parser`, so a bad one is an `auto-ascii` error in the chosen
  output mode rather than a clap usage error.
  **Output contract:** humans get aligned text on stdout (the factory's
  `inspect` column style: two spaces, a 14-wide label); `--json` puts
  EXACTLY one JSON value there and nothing else, and every error becomes
  `{"error": "..."}` on stderr with exit code 1 — INCLUDING clap's own
  usage errors (M8 review), which the binary takes from `try_parse` and
  re-renders through that one path, because an agent that typo'd a flag can
  read an object and cannot read a usage block. `--json` is looked for in
  argv rather than in the parsed `Cli`, which a usage error means there is
  none of. `--help`/`--version` stay OUTPUT (clap's own stream, exit 0) and
  a usage error WITHOUT `--json` keeps clap's rendering and clap's exit
  code 2, so "you typed it wrong" stays distinguishable from "it ran and
  failed". `play` is the one command
  that REFUSES `--json` (`{"error": "play is interactive; run it without
  --json"}`, exit 1, checked before the home folder is even resolved): the
  player owns stdout for its whole run, so no value printed around it could
  be the only one there. Everything chatty —
  ffmpeg progress, the factory's `input:`/`pass 1/2:`/`wrote` lines —
  goes to stderr in BOTH modes, which is what makes that keepable.
  **Every byte of output goes through one `emit`/`outln!` pair** (M8
  review) that writes to a locked handle and treats `BrokenPipe` as the
  end of the job: `auto-ascii list | head -1` exits 0 quietly where
  `println!` would have panicked with exit 101 (the same rule as
  `examples/headless-dump.rs`, M5 fix 7).
  **The sidecar** (`library/<name>.json`) is the one JSON shape: `import`
  and `cut` print exactly what they wrote, `info` prints one, `list` prints
  an array of them —
  `{"name", "source": {"path","sha256","bytes"} | null,
  "asset": {"path","bytes","frames","fps","duration_secs","base_w",
  "base_h"} | null, "created_unix": u64|null, "created": RFC-3339-UTC|null,
  "error": String (ABSENT when the clip read cleanly)}`.
  **Provenance is retired only once the new bytes are in place** (M8
  review): both writers validate, build (or export), and only THEN delete
  the old sidecar and write the new one — so a rebuild that fails leaves
  the clip it did not replace fully described, and a failure between the
  rename and the sidecar leaves a clip visibly unrecorded (which `list`
  says) rather than one described by bytes that were never written. The
  four steps `import` and `cut` share — name, refuse-existing, retire +
  record, print — are one set of functions in `main.rs`; only what they put
  in the library differs.
  The `asset` block is re-read from the ASCI header (mmap + `AsciiReader`)
  on every `list`/`info`, so it cannot go stale; only provenance comes from
  the file, and an asset with no sidecar still lists, with the three
  nullable fields `null`. Sidecars are parsed through a provenance-only
  struct (source + created fields, all optional, unknown keys ignored), so
  a hand-written `{"source": {...}}` loads, and INSIDE `source` only a
  video's `path` is required (M8 review): `"kind": "cut"` makes it a cut
  with `from`/`in`/`out` optional, anything else is a video source with
  `sha256`/`bytes` optional, and what is missing reads back as `null` or
  prints as `(unknown)` rather than sinking the whole sidecar. A `source`
  that is neither — no `path`, no `kind` — is the malformed case, and the
  message names the file it is in. `list` NEVER aborts on one bad
  entry — a truncated file, a directory, a non-UTF-8 name or an unparseable
  sidecar becomes an entry carrying `error` (and `?` columns in the human
  table), exit 0 — while `info`/`play`, which name ONE clip, stay strict. `home --json` prints
  `{"home","library","compositions","exports"}`. `agent-guide` prints
  `docs/AGENT-GUIDE.md` via `include_str!`, so the command and the file
  cannot drift (`--json`: `{"guide": "..."}`).
  **`cut`** (M8) is an `auto_ascii::compose::export` of a ONE-CLIP
  composition — no ffmpeg, planes copied not re-derived — into
  `library/<name>.ascii`, default name `<clip>-<in>-<out>` with the
  timestamps made file-name-safe (`apple-1984-0m05s-0m20s`: `format_mmss`
  with its colons spelled `h`/`m` and a trailing `s`, so a sub-second
  distinction collides by name and wants `--name`). `export` is itself
  atomic (note 29(i)), so it points straight at `library/` and a failed
  `--force` leaves the clip already there intact; the slice is validated
  (`Composition::resolve`) BEFORE anything on disk moves and the old
  sidecar goes only once the new bytes have landed, so a rejected `--out`
  leaves the old clip whole, provenance included. Its encode knobs are the
  factory's `params.toml` `[build]` pair read through `effective_params`,
  shared with `compose export`, so a slice is the kind of asset `import`
  writes rather than a third profile. Its sidecar is the same object with a
  different `source`: `{"kind": "cut", "from": <library name or path>,
  "in": secs, "out": secs}`. The `Source` enum is UNTAGGED — a
  video source carries no `kind` and never did, so every sidecar `import`
  has written still loads — and the `asset` block is read back off the
  header, like `list`/`info` read it.
  **`compose …`** (M8) edits the same TOML an agent would write by hand,
  and only ever by appending: `new <name>` creates
  `compositions/<name>.toml` (`create_new`, so a collision is an error)
  holding `schema = 1`, `name` and a comment block naming the four clip
  keys; `add <name> <clip> [--in T] [--out T] [--at T]` appends one
  `[[clip]]` table whose `asset` is the LIBRARY NAME when the clip lives
  in `library/` and its absolute path otherwise, with the timestamps
  written back VERBATIM (the schema takes a string anywhere it takes
  seconds). The composition the file WOULD become is parsed and RESOLVED
  first (M8 review), so a table above that no longer loads, a trim the
  timeline rejects or a `<clip>` that is not an ASCI asset fails with the
  file byte for byte as it was; the append itself is one `O_APPEND` write,
  so two agents adding at once both land where a read-modify-write would
  have lost one;
  `show <name>` prints the resolved timeline; `play <name>` runs the
  player on it and refuses `--json` for the same reason `play` does;
  `export <name> [-o path] [--force]` flattens it to
  `exports/<name>.ascii`. **A `<name>` resolves** like a `<clip>` does —
  an existing path (a `.toml` anywhere), else `compositions/<name>.toml`,
  else the kebab form — and everywhere a name meets its folder the
  extension THAT folder adds is stripped from the argument first (both
  resolvers and `compose new`: `demo.toml` and `demo` are one composition,
  `clip.ascii` and `clip` one clip; never `demo.toml.toml`), which is also
  the name the "does not exist" error suggests. `play` resolves EITHER kind, clips first (so every M7 spelling
  still means what it meant), with a `.toml` path always a composition.
  **`compose show --json`:** `{"name", "fps", "duration_secs",
  "frame_count", "clips": [{"index","asset","path","in_secs","out_secs",
  "at_secs","start_secs","end_secs","fps"}], "gaps":
  [{"start_secs","end_secs"}], "overlaps":
  [{"start_secs","end_secs","under","over"}]}` — `at_secs` is the `at` AS
  WRITTEN (null when the clip simply follows the one before it), while
  `in_secs`/`out_secs` are RESOLVED (an absent `out` reports the asset's
  own end); `gaps` and `overlaps` are `Composition::gaps()` /
  `Composition::overlaps()` verbatim (the FRAME GRID, so abutting clips
  cannot report a 1e-16 s sliver either way), with `over` the later-listed
  clip that plays there. The human table is those rows in
  TIMELINE order with `GAP` rows interleaved and BOTH sides of every
  overlap marked: `OVERLAP #k` on the covering clip, and on the covered
  one `UNDER #k` or `HIDDEN #k` — `Composition::mark_for` decides which by
  summing covered FRAMES, so a clip hidden by two later clips between them
  is `HIDDEN` even though neither covers it alone. A clip not one frame of
  which ever plays is exactly what start/end columns hide.
  **`compose export --json`** is the `ExportReport` plus where it landed:
  `{"path","frames","fps","bytes","shots","cuts"}`.

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
   format break). `auto-ascii-eval` was absent M0/M1; created at M2 (item A,
   note 12).
5. **Implemented now** (beyond skeletons): viewport math + worked-example
   tests, ramps, Grid/Cell, header/chunk byte codecs + layout-freezing tests,
   EventQueue, SimBackend construction/throttle/capture plumbing,
   **Resampler build/apply + `compose_luma` (auto-ascii-core complete for M0)**.
   `todo!()`: both `present`/`resize` paths, AnsiBackend session setup,
   restore hooks, AsciiWriter/AsciiReader streaming bodies, both bin mains.
6. **`compose_luma` added** (not in the original freeze; mandated by the
   auto-ascii-core task item e): `compose::compose_luma(luma, &Viewport, ramp,
   &mut Grid<Cell>)`, re-exported at crate root. L0-only M0 compositor —
   ramp glyph + gray fg from the same luma sample, BLANK pads, zero
   allocation.
7. **auto-ascii-format M1 upgrade** (M1 format agent): full ASCI v1 per the section
   above. Deliberate format changes: `VERSION_MINOR` 0→1;
   `WriterOptions::default()` filter is now `TEMPORAL_DELTA`; the committed
   byte golden (`GOLDEN_SHA256` in `tests/container.rs`) was re-baselined.
   Back-compat: minor-0 intra (M0) assets still open/decode/verify —
   confirmed against the committed `assets/*.ascii`. (`auto-ascii-factory build`
   briefly pinned the intra profile here; superseded by the M1 factory
   upgrade, note 10 — it now emits the full v1 profile.) Adversarial-review
   fixes: zero fps and zero base dims are rejected by both writer and reader
   (regression-tested in `tests/container.rs` + `tests/m1_format.rs`), and
   the player's fps guard in `main()` now covers `fps_num == 0` too.
   Measured on the synthetic coherent sequence (tests/m1_format.rs):
   delta+zstd-19 is ~16.8× smaller than intra+zstd-19.
8. **auto-ascii-term M1 upgrade** (M1 term agent): caps probe (`probe.rs`),
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
10. **auto-ascii-factory M1 upgrade** (M1 factory agent): full v1 pipeline per
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
11. **auto-ascii-player M1 integration** (M1 integrator): CLI per the Binaries
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
    normalized luma + chroma/gray fg, BLANK pads) — auto-ascii-core's M0
    `compose_luma` is unchanged. Player integration tests:
    `tests/m1_sim.rs` (tier byte checks via `--sim-dump`,
    seek-vs-sequential byte identity, runtime NORM per shot, probe no-hang).
12. **auto-ascii-eval created** (M2 item A agent): metrics library per its section
    above — nothing else in the workspace consumes it yet (the
    `auto-ascii-factory eval` wiring is M2 item B). Decisions recorded:
    (a) built-in coverage constants derived from DejaVu Sans Mono (the
    conservative default; per-font tables M5) via the committed
    `crates/auto-ascii-eval/tools/derive_coverage.py` — constants are the
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
    (e) deps: auto-ascii-core + auto-ascii-term (FrameStats is consumed directly, no
    mirror type) + serde/serde_json. insta/proptest/criterion arrive with
    M2 items C/D/E, not here.
13. **M2 items C/D landed** (goldens + resize fuzzing agent). New surface:
    `auto_ascii_eval::fixtures` (see the auto-ascii-eval section) — three deterministic
    synthetic fixture assets (gradient-motion / hard-cut / checker-drift,
    192×108 Y+C, 72 frames, keyframe 24, production delta+zstd-19+CRC
    profile) and a player-pipeline-parity renderer used by every committed
    golden and the fuzzer. Committed goldens (all corpus-free):
    (a) 27 insta cell-grid snapshots at
    `crates/auto-ascii-eval/tests/snapshots/` — 3 fixtures × 80×24 / 206×58 /
    320×90 × ascii-coarse / ascii-fine / mono-glyph-only; serialization =
    glyph grid verbatim + per-row FNV-1a 64 fg digest; re-bless with
    `INSTA_UPDATE=always cargo test -p auto-ascii-eval --test golden_grids`;
    (b) per-tier escape-stream byte goldens at
    `crates/auto-ascii-term/tests/goldens/gradient_f10_48x12_{truecolor,256,16,
    mono}.ansi` (one fixture frame, 48×12, sync_2026 wrap on, byte-exact;
    re-bless with `ASCII_UPDATE_GOLDENS=1`); auto-ascii-term gained a
    DEV-dependency on auto-ascii-eval for this (a legal dev-dep cycle — dev-deps
    sit outside the package's own dep graph).
    Resize fuzzing (§6 invariant set as explicit assertions; MOVED to
    `crates/auto-ascii/tests/resize_fuzz.rs` against the real `Player`
    by review fix 4c, note 17):
    originally `crates/auto-ascii-eval/tests/resize_fuzz.rs` — random
    1×1..=1000×1000 resize
    storms through `SimBackend::push_event` + player-style coalescing drain,
    asserting viewport ⊆ terminal, aspect error minimal-among-candidates
    (spec formula recomputed), pads symmetric ±1, backend/painter/resampler/
    grid realloc'd consistently, tap rebuild < 1 ms (min-of-3, worst
    observed 0.038 ms), and full-frame present after every resize; 256 cases
    under plain `cargo test`, `PROPTEST_CASES=10000` in scripts (measured
    84 s wall on this box). The same letterbox/aspect invariants also run
    directly on `compute_viewport` in
    `crates/auto-ascii-core/tests/viewport_props.rs` (proptest, incl. full-u16
    dims and degenerate aspects). Workspace: `insta` + `proptest` added to
    `[workspace.dependencies]`; `[profile.dev.package.*] opt-level = 3` for
    the four libs + zstd so goldens/fuzz stay fast under `cargo test`
    (debug-assertions unchanged). No existing `pub` signature changed.
14. **M2 item B + review fix 1 landed** (params/eval agent). Decisions
    recorded:
    (a) **pipeline extraction over binary-shelling**: `auto-ascii-player` gained
    a lib target (`pipeline` module, section above) and `auto-ascii-factory
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
    enforced writer + reader + factory (see auto-ascii-format section); player
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
    (g) eval cache under `runs/cache/` (gitignored via `*.ascii`), key
    `(input sha256, build-params sha256)` with a hand-rolled tested SHA-256
    (`sha256.rs`; INCREMENTAL as of the M8 review — a `Sha256` of eight
    words plus a <64-byte tail, full blocks compressed straight out of the
    caller's slice, and `sha256_file` streaming 64 KiB at a time, so
    hashing a 4 GB source costs 64 KiB rather than twice the file) — no new
    hashing dependency; PNGs for the contact sheet
    come from the ffmpeg subprocess (rawvideo→png and
    scale/fps/select→png), so no image crate either; `toml` is the one new
    workspace dependency.
15. **M2 item E + review fixes 2/3 landed** (perf-gate agent). No `pub`
    signature changed. Perf gates (PLAN §6): criterion benches at
    `crates/auto-ascii/benches/pipeline.rs` over the REAL pipeline —
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
    gate is a plain test, `crates/auto-ascii/tests/perf_fps.rs`
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
    `auto-ascii-factory eval` against `runs/base.json` writing
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
    moved to `crates/auto-ascii/tests/resize_fuzz.rs` and now drives
    the real `Player` through `drain_events`/`reflow`/`render_present`
    (new read-only accessor `Player::resampler_dims`); new
    `crates/auto-ascii/tests/pipeline_parity.rs` pins FixtureRenderer
    to Player cell-for-cell (3 fixtures × grid sweep incl. all golden
    sizes + 48×12 tier-golden size × color/mono × seq/seek/cut frames ×
    mid-run reflows), so the 27 insta goldens + 4 tier goldens
    transitively cover the shipping renderer (mutation-tested: dropping
    reflow's ramp update fails parity). auto-ascii-player gained dev-deps
    auto-ascii-eval + proptest; auto-ascii-eval dropped its proptest dev-dep;
    scripts/eval.sh fuzz section now targets it (M4: crate renamed auto-ascii).
    (4d) **eval cache staleness** [medium]: the eval asset cache key
    gained a third component — `ASCII_PIPELINE_FINGERPRINT`, an FNV-1a 64
    over every `.rs` in auto-ascii-factory/src + auto-ascii-format/src emitted by
    the new `crates/auto-ascii-factory/build.rs` — so pipeline code changes
    invalidate cached corpus assets (over-invalidation by eval-driver
    edits is accepted as the safe direction). Existing runs/cache entries
    were migrated to the new names after the grass byte-identity guard
    proved output unchanged.
18. **M3 factory plane extraction landed** (factory agent; PLAN §5 stages
    3–4). `auto-ascii-factory build` now writes all six §4 planes — see the
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
    raw, pools EMA'd). Deliberate re-pins: `FIXTURE_ASSET_SHA` in
    tests/m2_params_eval.rs (new pipeline = new default-build bytes);
    `assets/*.ascii` are still M1-era Y+C and must be REBUILT at M3
    integration (the `#[ignore]`d grass byte-identity guard fails until
    then, by design). Memory: extraction state is O(plane), ~4 MB fixed
    (features.rs memory note); planes stream to the writer.
19. **M3 edge-F1 metric + review reel landed** (edge-F1/reel agent; PLAN
    §6 "Edge F1 vs source Canny", §7 M3 review-reel gate). Decisions:
    (a) **ground truth** = imageproc Canny on the RAW source (one streaming
    ffmpeg gray decode per clip through the identical
    `scale=W:H:flags=area,fps=N` ingest chain — independent of every
    factory tunable, same no-self-grading posture as the SSIM reference),
    downscaled to viewport-cell resolution through auto-ascii-core's own
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
    additive auto-ascii-core API (`compose::layer`, `compose_cell_layer`,
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
    auto-ascii-eval, `gif` for auto-ascii-factory — PNG I/O stays with the ffmpeg
    subprocess.
20. **M3 pipeline integration landed** (integrator). The player runs the
    full §3.5 path — see the auto_ascii::pipeline section for the surface.
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
21. **M3 Tune landed** (tune agent): `auto-ascii-factory sweep` per the Binaries
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
    constant): `auto_ascii_core::hysteresis_idx` gained a `hyst_q8` parameter,
    `ComposeParams`/params.toml `[compose]` gained `idx_hyst_q8`
    (default 90 = the spec value; `IDX_HYST_Q8` remains as the documented
    default constant) — axis 3 of the mandated sweep plan trades cell
    stickiness against responsiveness with zero asset rebuilds.
22. **M3 Tune finish + M2-low fixes** (fix agent). Tuning (renderer-only —
    zero factory/asset changes; the ASCI byte pins and assets/ stay valid):
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
    see the auto-ascii-term probe section — the 2 s disarm window and the
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

23. **M4 items D + E landed** (terminal-matrix agent; PLAN §7 M4 "local
    terminals verified", Scope-amendment audit). No facade API change; two
    probe *readings* corrected against researched terminal behavior, one new
    `pub` helper (`ProbeReplies::sync_supported`), one new harness mode
    (`caps`) and two extra fields on the harness PROBE-DONE line
    (`support=`, `glyphs=`).
    (a) **Per-terminal pty identity fixtures**
    (`crates/auto-ascii-term/tests/terminal_identity.rs`, pty plumbing extracted to
    `tests/common/mod.rs` and shared with `pty_probe.rs`): kitty, alacritty,
    wezterm, gnome-terminal (VTE), xterm, xterm-direct and the Linux console
    are each replayed through the real `probe_caps` on a real pty — that
    terminal's env (`TERM`/`COLORTERM`/`TERM_PROGRAM`/locale), its
    `TIOCGWINSZ` (with or without pixel fields) and its canned reply stream —
    and the resulting `Caps` (color tier, sync_2026, cell_px, glyph support
    tier, glyph flags, zero stray bytes) asserted. Every stream is derived
    from that terminal's own source, cited inline (kitty screen.c/terminfo.py/
    window.py; alacritty term/mod.rs + CHANGELOG; wezterm terminalstate/mod.rs;
    vte vteseq.cc/modes.py/pty.cc; xterm ctlseqs + misc.c; console_codes(4)).
    (b) **Two probe readings fixed by that research.** DECRPM 2026 now counts
    only Ps ∈ {1,2}: 3/4 mean "permanently set/reset" (not support), and VTE
    answers **4**, so gnome-terminal no longer gets `?2026h…l` wraps it will
    never honor. XTGETTCAP `RGB` is now read BY VALUE: xterm answers the
    *valid* `1+r524742=`hex("-1") form when not in direct-color mode, which
    the old prefix test promoted to truecolor — plain xterm is C256,
    `xterm-direct` (value "8") and wezterm (value "8/8/8") are True. Kitty
    answers `0+r` (no RGB cap in its tables) and reaches truecolor via
    COLORTERM. Unit tests (`sync_supported_only_for_settable_modes`,
    `hex_decode_pairs_and_rejects_malformed`), parser cases
    (`xtgettcap_rgb_is_read_by_value`) and the kitty/xterm/vte transcripts in
    `tests/probe_parser.rs` re-pointed at the researched truth.
    (c) **`TERM=linux` legibility floor**
    (`crates/auto-ascii/tests/linux_console_golden.rs` + committed
    `tests/goldens/linux_console_80x24_f10.txt`): the fixture frame rendered
    through the REAL `pipeline::Player` at console caps (C16 + Cp437 →
    `PaletteChoice::Auto` resolves the ASCII floor, palette 8 ramp, aspect
    fallback 2.0) — glyph-grid golden plus assertions no golden can express:
    every glyph CP437-safe (0x20..=0x7E), > 50 % non-blank coverage, ≥ 5
    distinct glyphs, blank letterbox pad, and a present() stream free of
    `38;2`/`38;5`/`?2026`.
    (d) **Owner doc**: `docs/TERMINAL-CHECKLIST.md` — one command per real
    terminal, expected visuals, known quirks (kitty's missing RGB cap, VTE's
    permanent-reset 2026 ⇒ expected tearing, xterm's correct 256-color
    banding + `-direct2` path, alacritty's winsize-only cell size), the
    `auto-ascii-term-harness caps` diagnostic, the escape-hatch table, and what the
    fixtures do/don't cover.
    (e) **No-connectivity audit** (command + result in
    `docs/TERMINAL-CHECKLIST.md` §5): zero connectivity code paths. The only
    hits are the multiplexer flag inside the probe's *cache key*, now pinned
    inert by `multiplexer_flag_only_partitions_the_cache` (identical `Caps`
    with the flag set/unset, different cache slot). Stale prose about a
    ConPTY backend and the descoped throughput governor removed from
    auto-ascii-term docs. No bench, `perf/thresholds.toml`, `runs/` or params file
    was touched.

24. **M4 review fixes landed** (review-fix agent). Three confirmed findings,
    each with a regression test that can actually see the defect.
    (a) **ASCII tier is now genuinely ASCII.** `SUBPOS_GLYPHS` top slot
    `‾` U+203E → `"`. U+203E is not a CP437 code point (CP437 0xEE is
    U+00AF MACRON), so the shipping ASCII/CP437 path drew a missing-glyph box
    on the Linux console — or three mojibake bytes outside UTF-8 mode —
    exactly the failure `docs/TERMINAL-CHECKLIST.md` tells the owner to watch
    for. `"` also fixes an eval-side accident: U+203E was absent from
    `CONSERVATIVE_COVERAGE` and rasterized through the `max·0.5` ≈ 0.13
    fallback, ~2× its real ink; `"` measures 0.064 against `_`'s 0.055, so
    the top/bottom subposition pair is now ink-matched. Pinned by
    `auto_ascii_core::palette::every_ascii_tier_glyph_is_ascii` (enumerates the
    whole ASCII PaletteSet surface across both densities × all four color
    depths — data-side, unconditional), by the ascii-render sweep in
    `auto-ascii-eval` `golden_frames_are_meaningful`, and by
    `linux_console_golden.rs::every_glyph_is_console_printable`, which now
    sweeps 3 fixtures × 40 frames **and asserts it actually reached the
    subposition branch** (the single-frame version was vacuous — it passed
    only because that one gradient frame had no subposition cells).
    **Goldens re-pinned:** 6 checker-drift ascii/mono snapshots, 174 lines,
    diff verified to be the single substitution `‾`→`"` and nothing else.
    (b) **Quadrant noise floor re-tuned + made dither-stable.** The M3-review
    floor used `edge_t_off` (16), which disabled quadrant refinement across
    the whole E ∈ [2,15] band it exists to serve: resampled E is diluted by
    the cell box average (corpus source E mean 3.6, 8.95 % nonzero), so a
    genuine fine diagonal lands near cell E 6–11. New `ComposeParams`
    `quad_e_on`/`quad_e_off` = 2/1 — a *noise* floor (1-LSB resample noise
    cannot exceed E = 1), run through the same `edge_gate()` dual threshold
    as the edge layer with its own `cell_flags::WAS_QUADRANT` memory, so a
    cell dithering across the floor cannot alternate quadrant/half-block
    every frame. Both new fields are `params.toml` `[compose]` knobs
    (validated `quad_e_off <= quad_e_on`, pinned to the core defaults by the
    existing single-source-of-truth test). CI could not see any of this — all
    committed goldens contain zero quadrant glyphs and `auto-ascii-factory eval`
    renders at `GlyphTier::Ascii`, where `quadrant: false` — so the coverage
    is three `auto-ascii-core` unit tests instead:
    `lsb_noise_orientation_never_picks_quadrant` (floor holds),
    `fine_diagonal_band_still_refines_to_quadrants` (E 3..=15, both diagonal
    classes — this is the test the `edge_t_off` floor would fail), and
    `quadrant_floor_is_dither_stable` (arm/hold/re-arm + scene-cut reset).
    (c) **Doctests are green in the pure-embedder configuration.** The
    crate-level quickstart's first fence is now
    `#![cfg_attr(not(feature = "terminal"), doc = "```no_run,ignore")]`, so
    `cargo test -p auto-ascii --no-default-features --doc` passes (2 passed,
    1 ignored) instead of failing on a `Player` that is configured out;
    docs.rs builds with default features and still shows the runnable form.
    Same-config rot fixed alongside: `tests/m1_sim.rs` and `tests/sim_e2e.rs`
    carry `#![cfg(feature = "bin")]`, so they no longer silently exercise a
    stale `target/debug/auto-ascii-player` left by an earlier default-feature
    build. No public signature changed in (c).

25. **M5 item B landed** (font-tables agent; PLAN §3.4 "coverage tables for
    4 common monospace fonts plus one conservative default" + `--font-table`).
    New surface per the sections above: `auto_ascii_core::palette::{tier_glyphs,
    all_palette_glyphs}` (palette-data-driven glyph enumeration),
    `auto_ascii_core::font_table` (`FontTable` parse/builtin/veto_tier,
    `BUILTIN_FONT_TABLES`), `auto_ascii_eval::CoverageTable::from_font_table`,
    facade `PlayerBuilder::font_table` + `RenderSession::set_font_table` +
    hidden `auto_ascii::load_font_table`, factory `font-table` subcommand +
    `eval --font-table`, player `--font-table`. Decisions recorded:
    (a) **Generator = `auto-ascii-factory font-table`** (ab_glyph — already in
    the tree via imageproc; new direct workspace dep). Cell model: font
    scaled so the monospace ADVANCE = 64 px (terminals size by advance,
    not em), ink box centered and clipped to the 64×128 cell; coverage =
    antialiased-ink integral (same model as the M2 derive_coverage.py
    reference — DejaVu `@` agrees within 1.5%). Deterministic TOML emitter
    (fixed field order/precision, basename+sha256 provenance only):
    byte-identity unit-tested AND the five committed tables reproduce
    byte-for-byte from the system fonts (fonts-dejavu-core,
    fonts-liberation, fonts-ubuntu, fonts-noto-mono).
    (b) **Tables committed at repo-root `fonts/*.toml`** and embedded into
    auto-ascii-core via `include_str!` (`FontTable::builtin`) — auto-ascii-core is not
    on the `cargo package -p auto-ascii` path, so item F is unaffected; the
    facade embeds nothing.
    (c) **Repertoire findings (cmap-verified, cited in fonts/README.md):**
    no common monospace font ships palette-7 braille (DejaVu's braille is
    in the Sans face); Liberation Mono lacks `╱╲`+corner quadrants; Ubuntu
    Mono also lacks `‾` and all half/quadrant blocks. veto_tier therefore
    degrades braille→unicode for DejaVu/Noto and unicode→ascii for
    Liberation/Ubuntu — pinned by auto-ascii-core/facade tests
    (`builtins_load_and_veto_as_researched`,
    `font_table_repertoire_vetoes_palette_tier`). The veto was wired (it
    was trivial on top of the repertoire data), satisfying the §3.4
    "palette selection can veto" clause.
    (d) **Comparison note (fonts/README.md):** mean per-glyph ΔL* across
    the four fonts 5.0, max ΔL* 19.6 (`▒`, DejaVu much denser); ramp-
    ordering INVERSIONS exist under every font (e.g. `:`→`-` and `=`→`+`
    in palette 1 everywhere; `f`→`L` in palette 2 everywhere; `+`→`*` in
    palette 4 under DejaVu/Liberation/conservative); palettes 5 and 8 are
    monotone under all fonts. Ramps deliberately NOT retuned at M5 (task
    directive) — the tables are the input for that follow-up.
    (e) **Eval default unchanged:** `eval` without `--font-table` scores
    through the conservative constants exactly as before (runs/base.json
    untouched); per-font SSIM is a new mode whose normalization anchor
    moves with the table (documented on from_font_table).

26. **M5 items C + D + E + F landed** (scrub/ship agent). PLAN §7 M5 minus
    the soak (A) and font tables (B), which landed separately (note 25).
    (a) **Quirk table keyed on queried identity** (item C, PLAN §3.1):
    `auto_ascii_term::quirks` (session-gated) — a static `QUIRKS: &[Quirk]`
    matched on the XTVERSION reply prefix plus the XTGETTCAP-RGB reading,
    applied by `probe_caps` post-volley and pre-`--tier`, NEVER on cache
    hits/`--no-query` (no queried identity there). Two sourced entries:
    `kitty-rgbless-xtgettcap` (kitty's XTGETTCAP tables carry `Tc` but no
    `RGB` → `0+r`; kitty is unconditionally truecolor, so a
    COLORTERM-stripped kitty is upgraded C256→True; kitty/terminfo.py) and
    `xterm-no-direct-color` (xterm answers the valid RGB form with "-1" =
    no direct color and approximates SGR 38;2 into its 256 palette;
    xterm/misc.c + ctlseqs — a .bashrc `COLORTERM=truecolor` lie is clamped
    True→C256). Escape hatch `--no-quirks` end-to-end
    (`ProbeOptions.no_quirks` / `PlayerBuilder::no_quirks` / CLI); no-quirks
    bypasses the probe cache both ways — never stored (cache hits cannot
    re-quirk) and never read (cached entries embed quirk adjustments) — so
    it always re-volleys. Cache entries record downgrade-direction quirk
    clamps (`quirk_clamped`, cache v3) so a warm-cache hit re-applies the
    clamp over the same passive evidence instead of losing it to the
    upgrade-only merge. Tests: quirks.rs unit suite + pty identity fixtures
    (KITTY_STRIPPED, XTERM_COLORTERM_LIE) each asserted quirked AND via the
    harness mode `probe-reply-noquirks`, plus cache×quirk pty regressions
    (`probe-cached[-noquirks]` modes: clamp survives a cache hit; no-quirks
    ignores the cached quirked entry).
    (b) **Scrub UX** (item D): `Key::{Left,Right}` (auto-ascii-term) → ±5 s
    (`auto_ascii::SCRUB_STEP_SECS`), coalesced per drain
    (`Drained.seek_steps`), hysteresis reset exactly like digit jumps;
    digits keep their 0–90% bindings. Transient bottom-row progress overlay
    (`set_progress_overlay`/`draw_progress_overlay`), auto-hidden by the
    facade loop after ~1 s (`OVERLAY_HIDE_AFTER`); hide schedules a one-shot
    backend invalidate so the diff baseline is rebuilt — proven by
    tests/scrub_overlay.rs, which replays the diff-mode escape stream
    through a strict screen model (parse failure = corruption) and pins
    with-overlay vs no-overlay screens identical after hide + the full
    repaint on the hide frame. Scrub latency instrumented by the new
    `auto-ascii-player --bench-seek N` (reset + FIDX seek + decode + resample +
    compose + present @300×80, seeded xorshift frame sequence): on the
    856 MB sheep asset, 100 seeks → p50 8.6 ms / p95 20.3 ms / max 32.3 ms
    (accept < 50 ms).
    (c) **Ship** (item E): `scripts/release.sh` — native gnu + musl
    (static-pie, `ldd` "statically linked", gated) + windows-gnu cross
    (mingw-w64), all stripped and gated < 5 MB (measured 1.66 / 1.77 /
    2.99 MiB; factory native 3.61 MiB, informational); wine smoke only if
    wine exists, else the .exe ships documented as UNTESTED-CROSS. The
    windows-gnu build required cfg-splitting auto-ascii-term's session layer:
    libc is now a `[target.'cfg(unix)']` dependency; windows halves of
    ansi/restore/probe go through crossterm's WinAPI layer (raw mode,
    execute!-entered alt screen, `std::io` writes), probe is passive-only
    (`can_query=false`, no volley, no cache, quirks inert), restore is
    panic-hook + Drop (Ctrl-C arrives as a key event in raw mode). Unix
    behavior is byte-identical (pure cfg split). Makefile (build/dist/test/
    eval + the macOS build-on-mac section) and README "Install" added;
    measured: binary path 0.2 s copy→first frame (pty, probe deadline
    included), clean `git archive HEAD` source build 28.7 s wall.
    (d) **Publish hygiene** (item F): workspace path deps carry version
    reqs; auto-ascii-eval stays path-only ON PURPOSE (dev-dep cycle with
    auto-ascii-term — cargo strips path-only dev-deps when packaging). Manifest
    check: `cargo package -p auto-ascii-core -p auto-ascii-format -p auto-ascii-term
    -p auto-ascii --no-verify` passes (the closure is packaged together
    because the deps are unpublished; `--no-verify` skips the rebuild,
    nothing is published; on a dirty tree add `--allow-dirty`).

27. **M6 landed** (key-hints agent; PLAN-M6-M8 §1 — "the player should show,
    terse but clear, what the keys do"). No always-on chrome: the hints are
    event-driven like the M5 overlay, so every headless grid is untouched.
    (a) **Progress row gains an arrow-hint block** at the far left —
    `" <- 5s -> "` ahead of the timecode — only when the row is at least
    `PROGRESS_HINT_MIN_COLS` (64) wide; below that the bytes are the M5
    layout exactly, so a narrow terminal spends its columns on the timecode
    and the bar. The `5` is `SCRUB_STEP_SECS`, which MOVED from
    `crate::player` to `crate::pipeline` for this (the overlays print it and
    that module builds without the `terminal` feature); the public name
    `auto_ascii::SCRUB_STEP_SECS` is unchanged — `player` re-exports it.
    (b) **New key-hints row** on `rows-2`, in the progress row's colors:
    `q quit   0-9 jump   <- -> 5s   d dial   [ ] adjust   ? keys`, with whole
    items dropped in `HINT_DROP_ORDER` until the list fits `cols` — `[ ]
    adjust`, then `d dial`, then the arrows, then `0-9`, then `q quit`, with
    `? keys` the LAST to go, since how to summon the legend back is what a
    cramped screen must still say (80/64 → all six, 40 → four, 32 → three).
    The remainder is painted in the same background, so no picture cell
    survives underneath, and the row is gated on the viewport so it never
    lands on the enlarge card (`rows-2` is where that card's second line sits
    on a 4-row screen). New pipeline surface:
    `Player::set_hint_overlay(bool)` and `pub fn draw_hint_overlay(grid: &mut
    Grid<Cell>)`, plus `Drained.toggle_hints: bool` (`?` or `h`, collapsed to
    one flag per drain — a held key must not flicker the row). Printable
    ASCII only, so the arrows are `<-`/`->` (PLAN-M6-M8 §0.6); `auto-ascii-term`
    needed no change, since both keys already arrive as `Key::Char`.
    (c) **Visibility is run-loop policy, never the pipeline's.** `player.rs`
    gains `HINT_STARTUP_SHOW_FOR` (3 s) and a private `HintState`: the row
    rides with whichever transient overlay is up, shows for the start-up
    window, and is pinned by `?`/`h` until the next press. The pin toggles
    against what is ON SCREEN, not against the flag alone — a press inside
    the start-up window (or during an overlay's ride-along) dismisses the row
    and ends the window, instead of silently pinning it for the rest of
    playback and leaving the next press reading inverted. `--sim`,
    `RenderSession` and the eval harness never call the setter, so the parity
    grid, the console goldens and the insta snapshots pass unblessed. Every
    hide — progress, dial or hints — still goes through `overlay_hide_pending`
    → `invalidate()`.
    (d) **Tests:** auto-ascii/tests/scrub_overlay.rs extended to the new
    contract — the two rows hide on SEPARATE frames so each hide path is
    asserted to damage `cols*rows` on its own, and while they are up every
    row above the bottom TWO matches the no-overlay reference — plus hint
    content at 80/64/40 columns, the 64-column arrow-block threshold (and its
    absence at 63) and the `?`/`h` toggle through the real event queue. The
    TIMING rules are unit-tested in player.rs with synthetic instants
    (`hint_row_shows_at_start_up_then_rides_the_overlays`): `run()` owns the
    only clock and hard-wires `AnsiBackend`, so there is no seam to hand a
    SimBackend or a fake clock, and cutting one was out of this milestone's
    scope.
    (e) **Dial polish** (review fix, same seam): the first `d` now REVEALS
    the readout on the dial already selected instead of cycling past shadow
    lift — `dial_after_cycle(idx, presses, readout_up)` in player.rs, unit
    tested — and every `d` while the readout is up cycles as before.
    (f) **Space pauses** (added after M8 landed). `Drained.toggle_pause`
    (`Key::Char(' ')`, one flag per drain); `pipeline::Player::set_paused` /
    `ClipDeck::set_paused` / `draw_progress_overlay_paused`, and
    `draw_progress_overlay_clips` gained a trailing `paused: bool` — the row
    then prints ` PAUSED ` where the percentage goes and `|` where the bar
    head goes, printable ASCII under the same width rules. Two run-loop
    helpers carry the policy, both unit-tested with synthetic instants:
    `Transport` (asset time = `base_frame` + wall time since `clock`; a pause
    parks the frozen frame in `base_frame` and stops the second term, so
    resuming only repoints `clock` and playback continues from the frame on
    screen instead of skipping the pause's length — seeks repoint both and
    leave `paused` alone, which is why a jump while frozen just moves the
    frozen frame), and `ProgressTimer` (a seek or a resume restarts
    `OVERLAY_HIDE_AFTER`; a pause SUSPENDS it, so the row and the hints row
    riding on it stay up for the whole freeze). `duration_secs` is unchanged
    and deliberately still WALL clock: a pause spends the budget like
    playback does, now documented on the builder. The hints row gained
    `space pause` in second place, dropped third (after `[ ] adjust` and
    `d dial`) for its eleven columns; `? keys` is still last to go.
28. **M7 landed** (agent-CLI agent; PLAN-M6-M8 §2 — "an agent-first CLI
    should take a video from anywhere on the desktop, process it, and land
    it in the folder where the user's processed videos live"). The shape of
    the new binary is in the Binaries section above; these are the
    decisions behind it.
    (a) **The factory became lib + thin bin.** `auto-ascii-factory/src/lib.rs`
    owns all sixteen modules (every one `pub` — the crate is unpublished
    workspace plumbing, and making them private would have turned
    cross-module helpers into dead code) and adds the supported entry
    points: `build(&BuildRequest, &mut dyn Write) -> Result<BuildReport>`,
    `effective_params`, `parse_res` and the re-exported `sha256*`.
    `main.rs` kept the clap surface and `inspect`. The `Write` argument is
    the whole point of the split: `build::run` no longer `eprintln!`s its
    `input:`/`pass 1/2:`/`wrote` lines, it writes them to a caller's sink,
    so the factory bin passes `stderr` (bytes unchanged) and the CLI passes
    stderr too while keeping stdout for one JSON value. The indicatif bars
    stayed on stderr, where they always were. Proof the split moved
    nothing: `tests/build_e2e.rs` and `tests/m2_params_eval.rs` pass
    untouched, including both byte pins.
    (b) **One timestamp grammar**, `auto_ascii::timecode` (core tier, no
    deps): `parse` + `format_mmss` + `TimecodeError`. The player binary's
    private `parse_timestamp` is gone; its unit test now calls the shared
    parser and pins that `--seek` accepts exactly the same set of strings.
    `format_mmss` matches the progress overlay's shape, which keeps its own
    closure (M6 pins those bytes; nothing in `pipeline.rs` was touched).
    (c) **The AVI fixture writer moved** to
    `auto_ascii_eval::fixtures::write_bgr24_avi(path, w, h, fps, frames)` —
    frames are DIB-order (rows bottom-up, pixels B,G,R), `w` must be a
    multiple of 4 so no row or chunk needs padding. The factory's pin test
    kept its pattern generator and calls the shared writer;
    `FIXTURE_AVI_SHA` did not move, which is the proof the container bytes
    are identical. This is the one file-writing helper in an otherwise
    I/O-free crate, and it exists so the determinism guard and the CLI's
    tests cannot drift apart.
    (d) **Nullable provenance, authoritative headers.** `list`/`info`
    rebuild the `asset` block from the ASCI header every time (mmap +
    `AsciiReader`) and take only `source`/`created_unix`/`created` from the
    sidecar, so a stale or absent sidecar can never misreport what will
    play. An asset with no sidecar lists with those three fields `null` —
    never a missing key, which is what an agent's schema check needs. A
    clip that cannot be read at all keeps its row too, with `asset` null
    and an `error` string; the listing's job is to show the folder, and one
    bad file hiding the other twenty is the worse failure. `--force`
    deleted the old sidecar BEFORE the rebuild started — REVERSED at M8
    review (note 29(j)): the delete now follows the successful build, so
    the clip a failed rebuild did not replace keeps its provenance, while
    a failure between the rename and the sidecar still leaves the clip
    visibly unrecorded and names the orphaned path in its message.
    (e) **Kebab-casing is applied to `--name`, not just to the default —
    on the way IN only.** It is the documented naming rule (agent guide
    rule 1) and it doubles as the containment check: a kebab name cannot
    hold `/` or `..`, so no `--name` can write outside `library/`. Readers
    report the file stem verbatim, and `resolve_clip` falls back to the
    kebab form, so an agent can ask for `My Clip` and get `my-clip`.
    (f) **Tests:** `crates/auto-ascii-cli/tests/cli.rs` runs the real binary
    with `AUTO_ASCII_HOME` pointed at a per-test temp dir — `home` creates
    the three folders; `import` of a 12-frame 160x90 30 fps AVI produces the
    clip and a sidecar with every field checked (human AND `--json`, with
    the printed object asserted equal to the file on disk); the name
    collision fails before ffmpeg runs and `--force` rebuilds
    byte-identically; `list --json` covers a sidecar-less asset; `info`
    resolves by name and by path; `agent-guide` is asserted equal to the
    committed file; a bad `--ss` exits 1 with `{"error": ...}` on stderr,
    empty stdout and no asset written.
29. **M8 landed** (compositions agent; PLAN-M6-M8 §3 — "`.ascii` files are
    the clips, and a *composition* stitches an unbounded number of them,
    each placed at a chosen point on the timeline with its start and end
    trimmed"). The library half: the type, the player, the export. The
    `auto-ascii compose …`/`cut` subcommands are the CLI half and land
    beside it.
    (a) **One timeline function, and single assets go through it.**
    `Composition::locate(t_secs) -> Option<Located>` is the only place §3's
    semantics live (file order, `at` overrides, ends EXCLUSIVE, later-listed
    clip on top of an overlap, gap → `None`), and `locate_frame(f)` is
    `locate(f / fps)`. A plain asset is `Composition::single(path)` — one
    clip, no trim — so the run loop's two `base + elapsed·fps` copies became
    one `Composition::frame_after`, and `RenderSession`, `Player`, `--sim`
    and `--bench-seek` all map frames the same way. Float frame positions
    are SNAPPED to the exact integer within 1e-6 (a frame becomes a time
    and back, which lands a few ULPs either side); without that, `floor`
    dropped a frame at every boundary, and with it a one-clip composition
    provably maps frame f to frame f at every f — unit-tested over a whole
    fixture, which is what makes routing single assets through the
    composition path safe.
    (b) **`resolve()` is the one I/O step,** reading each clip's 64-byte
    header through a mapping (fps/frames/base res/aspect/plane ids) rather
    than an `AsciiReader::open` that would build a FIDX per clip. It also
    took over the unplayable-asset checks (`zero frames`, `no Y plane`) so
    `PlayerBuilder::build` still rejects a bad asset before the terminal is
    touched, now for every clip. Everything below `resolve` is defined only
    after it succeeds; an unresolved composition reports zeros and locates
    nothing, and `export` says so rather than writing an empty file.
    (c) **`deck::ClipDeck` is the one clip-switch implementation** (hidden,
    semver-exempt, like `pipeline`): one mmap + one `pipeline::Player` per
    clip, both built on first use, plus the presentation state — overlays,
    dial readout, compose params, layer mask, progress context — re-applied
    to whichever clip is fronted, so a switch never drops an overlay or a
    turned dial. A switch resets that clip's temporal state and schedules
    `invalidate()`; each clip owning its own state means the switch is
    already cold by construction, and the explicit reset is for RETURNING
    to a clip whose remembered ramp indices describe the frame it showed
    before we left. Gap frames are a blank `Grid` with the overlays drawn
    on top (the enlarge card below 32×9), so a gap still answers the
    keyboard. `pipeline::drain_events` split: `drain_backend_events` is the
    key mapping with no player state, which the deck needs because a gap
    frame has no player to drain through — `Player::drain_events` is that
    plus the reflow/reset it always did.
    (d) **Single-asset output is untouched,** deliberately and by test:
    `RenderSession::render` keeps the identity mapping for a plain asset
    (no timeline in the way), the progress row only reports composition
    time when the player was built from a `.toml`, and ` c/N ` needs both
    ≥64 columns and more than one clip — so `draw_progress_overlay` is
    `draw_progress_overlay_clips(.., None)` and the M6 bytes stand.
    `render_session.rs`, `scrub_overlay.rs`, `pipeline_parity.rs`,
    `linux_console_golden.rs`, the tier goldens and the insta snapshots all
    pass unblessed.
    (e) **Export copies planes, never re-derives them.** Every clip must
    share one base resolution and one plane registry (the error names the
    offending clip and suggests `import --res`); the top clip's planes are
    decoded (sequential roll where the walk is sequential, FIDX seek
    otherwise) and handed to `write_frame` unchanged, gaps write black
    planes, and NORM is one pre-pass over the clips' own shot tables with
    no decoding: a record per (clip slice ∩ source shot), rebased to output
    frames, CUT at every clip boundary and gap edge, identity levels for
    gaps, and record 0 keeping the source's own flag (nothing precedes
    frame 0 to cut away from). `ExportOptions` defaults to params.toml's
    `[build]` 60/15, NOT `auto-ascii-format`'s 19 — the format crate's
    default is not the factory's policy, and a flattened composition is the
    same kind of asset the factory writes.
    (f) **No `auto-ascii-format` change was needed.** The task allowed for
    a missing reader accessor; `shots()`, `shot_for_frame()` and
    `plane_index()` already cover the NORM pre-pass, so the container crate
    is untouched at M8.
    (g) **`toml` is the only new dependency, under the new default-on
    `compose` feature** (`terminal` enables it — the player takes a
    composition file). `serde` is NOT a direct dep: the parse is hand-rolled
    over `toml::Table` so every rejection can name the clip INDEX, which a
    `deny_unknown_fields` derive cannot. `cargo check -p auto-ascii
    --no-default-features` still carries no toml/clap/crossterm.
    (h) **Tests:** `locate` unit-tested in `composition.rs` (sequential
    default, explicit `at` + gap, overlap → later clip, in/out trims, mixed
    fps via a 15 fps clip, exclusive ends, empty/inverted/past-the-end
    rejections naming the clip index, and the TOML error surface);
    `auto-ascii/tests/composition.rs` builds two fixture clips in a temp
    dir and stitches them with a trim and an `at` that leaves a 1.6 s gap,
    then asserts the frame before the gap equals clip A played alone at the
    same point, the frame after equals clip B started cold at its `in`
    frame, gap frames are exactly `Cell::BLANK`, and an export reopens with
    the same frame count, the NORM table the report describes (4 records, 3
    cuts, at frames 0/72/120/141) and the same pictures; a trimmed one-clip
    export is the source's frames at the offset (the `cut` round trip); the
    real binary prints the stats line for `--sim 120x40:60` on a `.toml`,
    and `--seek 0:04.5` on the composition produces a sim-dump byte-
    identical to seeking clip B alone to `0:01`.
    (i) **Review fixes.** Export is ATOMIC (`<out>.part` + rename, the
    factory's pattern) — a failed export no longer replaces a good asset
    with a headerless stub, and it leaves no debris; tested by corrupting a
    clip's frame payload after `resolve` and asserting the previous `out`
    survives byte for byte. The deck caps residency at `MAX_LIVE_CLIPS`
    (8) with LRU eviction, so "unbounded clips" costs bounded memory;
    eviction clears the player before the mapping, and a 12-clip walk
    asserts the cap holds and that a re-opened clip renders what it did
    before. A gap below 32×9 now draws the enlarge card AND keeps the
    progress/dial rows (the hints row stays off, since `rows-2` is the
    card's second line) — being stuck in a gap on a small terminal with no
    scrub bar is exactly the case that needs one. `headless-dump` takes a
    `.toml` too, and the seek error says "composition" when the source is
    one.
    (j) **The CLI half** — `auto-ascii cut` and `compose new/add/show/play/
    export`, whose shapes are in the Binaries section above. `cut` is an
    export of a one-clip composition, so slicing needs no ffmpeg and the
    slice is the source's own planes, and it points straight at
    `library/` because `export` is itself atomic (sub-item (i)), so a
    failed `--force` leaves the clip already there intact. The `compose` subcommands are TEXT operations on
    the TOML (`crates/auto-ascii-cli/src/composition.rs`): `new` writes a
    header and a commented clip table, `add` appends one `[[clip]]` and
    nothing re-serializes the file, so an agent's comments and ordering
    survive every edit — the file is the source of truth (§0.3), and these
    are conveniences over it, not a model of it. Reading is the facade's
    `Composition::from_toml_file`, resolved against the home `library/`
    rather than the environment sniff `default_library_dir` does, because
    the CLI always knows which home it is in. `compose show` adds the only
    two things a timeline has that a clip list does not, both computed off
    the facade: `gaps()`, `overlaps()` and `mark_for()` on the frame
    grid, so a clip 0.3 s long (0.30000000000000004 in binary) abutting one
    placed at 0.3 shows neither a gap nor an overlap — the CLI's own float
    versions were deleted when that surface landed. The sidecar's `source` became an untagged enum so a
    cut's provenance shares the shape without invalidating one sidecar
    `import` had already written, and `play` now takes either kind of
    argument. `docs/AGENT-GUIDE.md` grew to 79 lines for the new commands
    and the two-clip-with-a-gap example; its pinned cap moved 60 → 80.
    **Review fixes (same round).** clap's usage errors go through
    `try_parse` and the one `{"error": ...}` path (Binaries above);
    provenance is retired only after the build/export succeeds, reversing
    note 28(d) and keeping a failed `--force` non-destructive; `play`
    checks the HEADER only, so a clip with a truncated sidecar still plays
    while `info`, which is about the sidecar, still refuses; an EMPTY
    `HOME`/`USERPROFILE` is no home rather than a relative `auto-ascii/` in
    the working directory; the `.toml` extension test ignores ASCII case
    (`Demo.TOML` is a composition on the file systems this ships to); the
    export knobs come from `auto_ascii_factory::effective_params` instead
    of a third hard-coded 60/15; and the four steps `import` and `cut`
    share are one set of functions rather than two copies.
    **Tests:** seventeen cases added to `crates/auto-ascii-cli/tests/cli.rs`
    (31 in the suite), over
    `auto_ascii_eval` fixture clips written straight into `library/` with
    no sidecars (no ffmpeg anywhere in the M8 half) — `cut` writes frames =
    round((out−in)·fps) with the cut provenance and no `.part` left behind;
    `cut --json` equals the file it wrote, collides, and rebuilds
    byte-identically under `--force`; `new` + two `add`s are pinned to the
    exact TOML tail and an agent's own comment survives the next `add`;
    `show --json` reports the 1.6 s gap of the §3 example and a second case
    marks `OVERLAP #0`; `export`'s `frames` equals `show`'s `frame_count`
    and its file re-enters `list` header-first; a `.toml` outside the home
    folder resolves and exports; and every error path — unknown clip,
    missing composition, `new` on an existing name, `cut` with `out <= in`,
    a clipless `show` — is one `{"error": ...}` on stderr with empty stdout
    and exit 1. The review round added: the four usage-error kinds as JSON
    objects (with `--help`/`--version` still exit 0) and clap's own text at
    exit 2 without `--json`; a failed `--force` rebuild keeping its
    provenance and a successful one replacing it; `play` over a truncated
    sidecar; an empty `HOME`; an uppercase `.TOML` path resolving as a
    composition; and a unit test tying the export knobs to the factory's
    params.
    (j) **Code-review fixes.** (1) The timeline is INTEGER frames at the
    composition rate — `ClipSpan { start_frame, end_frame }`, `locate_frame`
    pure integer, `locate(t)` = `locate_frame(t·fps)`, seconds derived from
    the frames. Seconds could not express a boundary: a 0.3 s slice ends at
    0.30000000000000004, so the frame at exactly 0.3 s went to the FIRST
    clip at a source frame its own `out` had trimmed away, and the second
    clip never showed its frame 0 (swept over every one-decimal placement
    at 30 fps now). Derived seconds also kill the 1e-16 s "gaps" `compose
    show` would otherwise print between abutting clips. (2) `out` gets half
    a frame of slack and is then clamped, so the duration the tools PRINT
    (`4.17s` for 100 frames at 24 fps) is a usable `out` instead of a
    self-contradicting rejection; past that the message speaks the same
    `{:.2}s`. (3) `resolve` runs `AsciiReader::open` per clip, not a header
    parse, so `PlayerBuilder::build` keeps its documented promise to reject
    a corrupt asset before the terminal is touched (truncated-asset test);
    its unplayable-asset messages now name the clip index like every other
    resolve error. (4) `export` opens clip decoders lazily, LRU-capped at
    `MAX_LIVE_SOURCES` (4), and the NORM pre-pass keeps shot tables rather
    than readers — memory tracks live clips, not clip count. (5)
    `RenderSession::render` records the frame cursor only after a
    successful render, so a failed frame cannot corrupt backward-jump
    detection. (6) `ClipDeck::stage()` carries evicted players' time and
    times gap presents; the dispatch collapsed into `render_at`/
    `present_at`/`showing` (four pasted copies gone, `activate` private,
    `clip_frame_count`/`active` deleted). (7) The progress row prints
    through `timecode::format_mmss` instead of its own closure, and
    `Composition::is_toml_path` is the one "clip or composition?" rule.
    (k) **Addenda to the review batch.** (1) `Composition::gaps()`,
    `overlaps()` and `mark_for(clip_idx)` expose the timeline analysis
    `compose show` was doing in floats: gaps as `Span`s, overlaps as PAIRS
    (`under`/`over`, later-listed on top, so three-deep coverage is three
    facts rather than a special case) and a per-clip `ClipMark`
    (Clear/Partial/Hidden, Hidden = every frame covered). All on the frame
    grid, so abutting clips report neither a sliver gap nor a one-ULP
    overlap — the two cases the float version got wrong. (2) A paused run
    loop no longer re-composes and re-presents the frozen frame every tick:
    `RepaintGate` paints only when something changed (key, seek, resize,
    overlay edge including a timeout) and the loop idles otherwise, which
    is most of a core and tens of MB/s of escape stream saved on a picture
    that is not moving. (3) Space freezes on the frame last PRESENTED
    (`freeze_target`), not on where the clock has reached — at `--fps-cap
    1` on a 30 fps asset those were ~30 frames apart, so the picture jumped
    forward a second at the moment it was asked to stop. (4) `HintState`
    reads the PIN before the start-up window: pinned → unpin, start-up
    freebie → dismiss, anything else (including an overlay's ride-along) →
    pin. Under the old "toggle against what is on screen" rule a pause held
    the progress row up forever, so `?` could never pin the legend and
    would silently unpin one that was. (5) `RenderSession::open` is
    `from_composition(Composition::single(path))` — no second mapping path,
    no cached fps/aspect/frame_count, one 16:9 degenerate-aspect fallback
    (in `resolve`); single-asset output is unchanged (goldens, parity and
    the render-session suite unblessed).
