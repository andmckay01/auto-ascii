//! The frame pipeline: decode → resample → NORM levels → compose → present
//! (PLAN §3.4–§3.6). Extracted from the binary at M2 (item B decision) so
//! `auto-ascii-factory eval` drives the EXACT player code path headlessly
//! against `SimBackend` — metrics measure the real renderer, not a
//! reimplementation. The event loop, pacing and CLI stay above (`Player`
//! / the `auto-ascii-player` bin); nothing here touches a clock or a tty.
//!
//! M4: re-homed from `auto-ascii-player` into the `auto-ascii` facade and split
//! along the backend seam — [`Player::reflow_grid`]/[`Player::render_grid`]
//! carry everything up to the composed [`Grid<Cell>`] with NO backend in
//! sight (the terminal-free [`crate::RenderSession`] path), and
//! [`Player::reflow`]/[`Player::render_present`] wrap them with the
//! `Backend` resize/invalidate/present calls (identical behavior to M3 —
//! same call order, same bytes). This module is `#[doc(hidden)]`: it is the
//! workspace harness contract (factory eval, fuzz, benches, goldens), not
//! the embedding API, and is exempt from facade semver.
//!
//! M3: the full §3.5 three-layer path. Luma is resampled at Vc×2Vr (§3.3 —
//! ONE tap-table build at 2× vertical through the same separable code path);
//! E/Ex/Ey/H are decoded when the plane registry carries them (absent planes
//! auto-disable their layers, so M1-era Y+C assets still play — PLAN §4
//! back-compat) and resampled at Vc×Vr; coherence is computed post-resample
//! inside `compose_cell` from the resampled doubled-angle field; the
//! per-shot NORM LUT feeds `compose_frame` directly (taps are normalized
//! per §3.5 line 2); all temporal state lives in a [`HysteresisState`]
//! reset on shot change and realloc+reset on resize.

use std::time::Instant;

use auto_ascii_core::{
    Cell, ColorDepth, ComposeParams, FramePlanes, GlyphTier, Grid, HysteresisState, PaletteSet,
    Resampler, Rgb, Viewport, compose_frame, compose_frame_masked, compute_viewport_for,
    select_palettes,
};
use auto_ascii_format::header::plane_id;
use auto_ascii_format::{PlaneLevels, AsciiReader};
use auto_ascii_term::{Backend, Caps, ColorTier, Event, FrameStats, GlyphFlags, GlyphSupportTier, Key};

use crate::error::Error;

/// Module-local result over the facade [`Error`] (the pipeline reports
/// through the same coherent type as the public API).
type Result<T> = std::result::Result<T, Error>;

/// H-mask box-average thresholds (integrator decision, M3): the H plane is
/// bitflags, so each bit is expanded to a 0/255 mask at source resolution,
/// box-averaged through the shared feature resampler, and re-thresholded per
/// cell. Highlights are sparse accents — a quarter of the cell's ink is
/// enough to keep a star alive at fine grids without letting single-pixel
/// noise own a coarse cell; deep shadow is an area feature — half the cell.
const H_HIGHLIGHT_MIN: u8 = 64;
const H_SHADOW_MIN: u8 = 128;

/// Seconds of asset time one Left/Right arrow press scrubs (M5 scrub UX,
/// PLAN §7 M5). Presses coalesced within one event drain add up (holding the
/// key nets one bigger jump); digits 0–9 still jump to 0–90%. It lives here
/// rather than beside the run loop because the overlays PRINT it (M6 key
/// hints, PLAN-M6-M8 §1) and this module builds without the `terminal`
/// feature; `crate::player` re-exports it under its documented name.
pub const SCRUB_STEP_SECS: f64 = 5.0;

/// Narrowest row that still gets the progress overlay's arrow-hint block
/// (M6, PLAN-M6-M8 §1). Below it the row is exactly the M5 layout — the
/// timecode and the bar are worth more than the hint on a narrow terminal.
const PROGRESS_HINT_MIN_COLS: u16 = 64;

/// Gap between key-hint items (M6, PLAN-M6-M8 §1): wide enough that `[ ]`
/// and `0-9` read as one item each at a glance.
const HINT_SEP: &str = "   ";

/// Drop order for [`hint_line`] (M6 review fix), as indices into the display
/// list — first to go first. `v controls` is deliberately absent: how to
/// summon the legend back is the one hint a narrow terminal must keep, so it
/// is the last item standing. `space pause` goes early for its width: eleven
/// columns is the most any one hint costs, and space is the binding people
/// try without being told.
const HINT_DROP_ORDER: [usize; 6] = [5, 4, 1, 3, 2, 0];

/// Map the probed terminal capabilities to the palette-selection charset
/// tier (PLAN §3.4: `Caps.glyph_support` records the trusted repertoire).
/// Braille requires *verified* support (never set from passive hints).
pub fn glyph_tier_from_caps(caps: &Caps) -> GlyphTier {
    match caps.glyph_support {
        GlyphSupportTier::AsciiOnly | GlyphSupportTier::Cp437 => GlyphTier::Ascii,
        GlyphSupportTier::UnicodeCore => GlyphTier::UnicodeBlocks,
        GlyphSupportTier::UnicodeFull => {
            if caps.glyphs.contains(GlyphFlags::BRAILLE) {
                GlyphTier::BrailleVerified
            } else {
                GlyphTier::UnicodeBlocks
            }
        }
    }
}

/// Map a terminal color tier to the palette-selection color depth
/// (auto-ascii-core mirrors the variants without depending on auto-ascii-term).
pub fn color_depth(tier: ColorTier) -> ColorDepth {
    match tier {
        ColorTier::True => ColorDepth::True,
        ColorTier::C256 => ColorDepth::C256,
        ColorTier::C16 => ColorDepth::C16,
        ColorTier::Mono => ColorDepth::Mono,
    }
}

/// Per-stage wall-time accumulators (ns) — the §3.6 stage split reported by
/// `--sim` JSON and consumed by the eval driver (per-frame deltas).
#[derive(Clone, Copy, Debug, Default)]
pub struct StageNs {
    pub decode: u64,
    pub resample: u64,
    pub compose: u64,
    pub present: u64,
}

/// Result of one event-queue drain.
pub struct Drained {
    pub quit: bool,
    /// Digit key 0–9 → jump to that ×10% of the asset (interactive seek).
    /// When set, `drain_events` has ALREADY reset all per-cell hysteresis
    /// state (M3 review fix): a seek is a temporal discontinuity, so the
    /// caller just repoints its clock and renders the landing frame.
    pub jump_digit: Option<u8>,
    /// Net arrow-key scrub steps this drain (M5 scrub UX): each `Right` is
    /// +1, each `Left` −1; the caller converts steps to ±5 s
    /// (`auto_ascii::SCRUB_STEP_SECS`) of asset time and repoints its clock.
    /// When nonzero, hysteresis state has ALREADY been reset (same temporal-
    /// discontinuity rule as `jump_digit`).
    pub seek_steps: i32,
    /// `Tab` presses this drain — each advances the live-dial selection by one
    /// (wrapping). Dials retune the renderer during playback; see
    /// [`Dial`](crate::Dial).
    pub dial_cycle: u32,
    /// Net `[`/`]` steps this drain: each `]` is +1, each `[` −1. The caller
    /// scales this by the selected dial's step size and hands the result to
    /// [`Player::set_compose_params`], which is where a turn that actually
    /// moves resets the per-cell hysteresis memory. A renderer change — no
    /// asset is touched.
    pub dial_delta: i32,
    /// `v` pressed this drain (M6 key hints, PLAN-M6-M8 §1) — the
    /// caller flips the sticky key-hints row. Coalesced to a flag like the
    /// arrows: holding the key must not race the row on and off.
    pub toggle_hints: bool,
    /// Space pressed this drain (M6 pause) — the caller freezes or resumes
    /// playback. Coalesced to a flag for the same reason as `toggle_hints`:
    /// a key repeat is one intent, not a pause/resume stutter.
    pub toggle_pause: bool,
}

/// What the progress overlay prints instead of this clip's own numbers
/// while a COMPOSITION is playing (M8, PLAN-M6-M8 §3): the composition's
/// position, and which clip of how many is on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProgressContext {
    /// Frame on the composition timeline.
    pub frame: u32,
    /// Frames on the composition timeline.
    pub frame_count: u32,
    /// Composition fps numerator (kept as the exact header rational so the
    /// row's timecode cannot drift from the timeline's own arithmetic).
    pub fps_num: u16,
    /// Composition fps denominator.
    pub fps_den: u16,
    /// `(current clip, clip count)`, both 1-based counts — the ` c/N `
    /// block. Suppressed below [`PROGRESS_HINT_MIN_COLS`] and for
    /// single-clip compositions.
    pub clip: Option<(usize, usize)>,
}

impl ProgressContext {
    /// The composition frame rate the timecode is printed from.
    pub fn fps(&self) -> f64 {
        (f64::from(self.fps_num) / f64::from(self.fps_den.max(1))).max(1e-9)
    }
}

/// Coalesce a backend's event queue into a [`Drained`] plus the latest
/// resize, touching no player state.
///
/// Split out of [`Player::drain_events`] at M8 (PLAN-M6-M8 §3): a
/// composition's gap frames have no active clip player, and the key mapping
/// must not exist in two copies. Quit wins and stops the drain, exactly as
/// before.
pub fn drain_backend_events<B: Backend>(backend: &mut B) -> (Drained, Option<(u16, u16)>) {
    let mut resize: Option<(u16, u16)> = None;
    let mut jump_digit = None;
    let mut seek_steps: i32 = 0;
    let mut dial_cycle: u32 = 0;
    let mut dial_delta: i32 = 0;
    let mut toggle_hints = false;
    let mut toggle_pause = false;
    while let Some(ev) = backend.events().pop() {
        match ev {
            Event::Quit => {
                let quit = Drained {
                    quit: true,
                    jump_digit: None,
                    seek_steps: 0,
                    dial_cycle: 0,
                    dial_delta: 0,
                    toggle_hints: false,
                    toggle_pause: false,
                };
                return (quit, None);
            }
            Event::Resize(c, r) => resize = Some((c, r)),
            Event::Key(Key::Char(c @ '0'..='9')) => jump_digit = Some(c as u8 - b'0'),
            // M5 scrub UX: ±5 s per arrow press, coalesced per drain
            // (holding the key nets one bigger jump, not N renders).
            Event::Key(Key::Left) => seek_steps = seek_steps.saturating_sub(1),
            Event::Key(Key::Right) => seek_steps = seek_steps.saturating_add(1),
            // Live dials: `d` selects which one, `[`/`]` turn it. Coalesced
            // per drain like the arrows, so holding a key nets one bigger
            // move rather than N renders. Digits, arrows and q/Esc are
            // already spoken for; these three are not.
            Event::Key(Key::Char('d')) => dial_cycle = dial_cycle.saturating_add(1),
            Event::Key(Key::Char('[')) => dial_delta = dial_delta.saturating_sub(1),
            Event::Key(Key::Char(']')) => dial_delta = dial_delta.saturating_add(1),
            // M6 key hints (PLAN-M6-M8 §1): `v` for the controls legend —
            // one unshifted key, and the row itself names it, so there is
            // nothing to guess. Collapsed to a flag, not counted — two
            // presses in one drain are a key repeat, not a request to
            // flicker the row.
            Event::Key(Key::Char('v')) => toggle_hints = true,
            // M6 pause: space is the one binding every player shares, and
            // it is the last printable key not already spoken for.
            Event::Key(Key::Char(' ')) => toggle_pause = true,
            Event::Key(_) => {}
        }
    }
    let drained = Drained {
        quit: false,
        jump_digit,
        seek_steps,
        dial_cycle,
        dial_delta,
        toggle_hints,
        toggle_pause,
    };
    (drained, resize)
}

/// The frame pipeline: decode → resample → NORM levels → compose into a
/// term-sized grid. All buffers are (re)allocated only in `new`/`reflow` —
/// the hot loop is allocation-free (PLAN §6 discipline).
pub struct Player<'a> {
    reader: AsciiReader<'a>,
    frame_count: u32,
    src_w: u16,
    src_h: u16,
    /// C plane dims when the asset has chroma (half res, PLAN §4).
    chroma_dims: Option<(u16, u16)>,
    /// Decode + composite chroma (asset has C and the tier shows color;
    /// mono keeps glyph-only and skips the C subblocks entirely, PLAN §4).
    use_chroma: bool,
    /// Plane-registry detection (PLAN §4 back-compat): the edge layer needs
    /// all of E/Ex/Ey; H is independent. Absent planes are never decoded and
    /// their layers compose disabled (M1-era Y+C assets play unchanged).
    has_edges: bool,
    has_h: bool,
    /// The asset's picture aspect from the header (`aspect_num/den`, PLAN
    /// §4) — the viewport's target ratio (M5 fix 2: the letterbox targets
    /// the ASSET's aspect, not a hard-coded 16:9). Degenerate (zero) header
    /// fields are normalized to 16:9 at `new`.
    aspect_num: u16,
    aspect_den: u16,
    cell_aspect: f64,
    repaint_full: bool,
    /// Palette-selection inputs derived from Caps by the caller (§3.4 key:
    /// charset tier × color depth; density comes from the viewport).
    glyph_tier: GlyphTier,
    color: ColorDepth,
    /// The selected §3.4 palette configuration (rebuilt on reflow — density
    /// band depends on viewport cols).
    palette: Option<PaletteSet>,
    /// §3.5 compositor tunables (defaults = the untuned M3 baseline; the
    /// eval driver overrides from params.toml `[compose]`).
    compose_params: ComposeParams,
    /// Per-cell temporal state (§3.5): ramp-index hysteresis, edge on/off
    /// memory, orientation bin. Reset on shot change, realloc+reset on
    /// resize.
    state: HysteresisState,
    vp: Option<Viewport>,
    /// Luma resampler, built at Vc × 2Vr (§3.3: two vertical taps per cell
    /// through the ONE separable path — no second luma resampler).
    resampler: Option<Resampler>,
    /// Feature-plane resampler (E/Ex/Ey + H masks) at Vc × Vr.
    feat_resampler: Option<Resampler>,
    chroma_resampler: Option<Resampler>,
    /// Decoded planes at source res — the standing delta double buffers.
    luma_src: Vec<u8>,
    e_src: Vec<u8>,
    ex_src: Vec<u8>,
    ey_src: Vec<u8>,
    h_src: Vec<u8>,
    /// H bits expanded to 0/255 masks at source res (resampler inputs).
    hl_mask: Vec<u8>,
    sh_mask: Vec<u8>,
    /// Resampled planes at viewport res (`luma2_dst` at Vc × 2Vr).
    luma2_dst: Vec<u8>,
    e_dst: Vec<u8>,
    ex_dst: Vec<u8>,
    ey_dst: Vec<u8>,
    hl_dst: Vec<u8>,
    sh_dst: Vec<u8>,
    /// Re-thresholded H flags at cell res (see `H_HIGHLIGHT_MIN`).
    h_dst: Vec<u8>,
    /// Decoded C plane (RGB565 LE) — the chroma delta double buffer.
    chroma_src: Vec<u8>,
    /// C unpacked to 8-bit channels at chroma res (resampler inputs).
    cr_src: Vec<u8>,
    cg_src: Vec<u8>,
    cb_src: Vec<u8>,
    /// Resampled chroma channels, `vp.cols × vp.rows`.
    cr_dst: Vec<u8>,
    cg_dst: Vec<u8>,
    cb_dst: Vec<u8>,
    /// Per-shot NORM levels folded into one LUT (PLAN §3.5: n =
    /// clamp((L − shot_lo) · shot_inv_range)); rebuilt only on shot change.
    /// Transient live-dial readout for the bottom row, `(label, value, max)`.
    /// Shares the row with the progress overlay and takes precedence while up.
    dial_overlay: Option<(&'static str, u8, u8)>,
    levels_lut: [u8; 256],
    /// What `levels_lut` was built for: the shot's `first_frame` (`None` on
    /// assets without NORM, where the window is the identity) and the
    /// `shadow_lift` folded into it. `None` = not built yet. Keyed on both
    /// so a lift turned on a NORM-less asset rebuilds too — keyed on the
    /// shot alone, `None == None` there and the dial did nothing.
    lut_key: Option<(Option<u32>, u8)>,
    /// Frame currently decoded in the source buffers — drives the
    /// sequential-roll vs FIDX-seek decode policy on delta assets.
    loaded: Option<u32>,
    /// Full terminal grid (viewport + letterbox pads).
    grid: Grid<Cell>,
    /// Winning-layer render metadata (`auto_ascii_core::compose::layer` ids, same
    /// dims as `grid`), collected only when the eval driver asks
    /// ([`enable_layer_mask`](Player::enable_layer_mask)) — `None` keeps the
    /// interactive hot path untouched. M3: an enabled mask is filled by
    /// `compose_frame_masked` (byte-identical cells, tags observable) — the
    /// §6 edge-F1 prediction side.
    layer_mask: Option<Grid<u8>>,
    stage: StageNs,
    /// M5 scrub UX: the transient 1-line progress overlay (bottom terminal
    /// row). Drawn OVER the composed grid in `render_grid` when visible; it
    /// never touches hysteresis or the layer mask — presentation only.
    overlay_visible: bool,
    /// M8: what the progress overlay reports while a composition plays —
    /// the composition's position and ` c/N `, not this clip's own frame
    /// counter. `None` (the default, and every single-asset path) leaves
    /// the M6 row byte for byte.
    progress_ctx: Option<ProgressContext>,
    /// M6 key hints (PLAN-M6-M8 §1): the one-line key legend on `rows-2`.
    /// Driven entirely by the run loop (it rides with the transient
    /// overlays, the start-up window and `v`), so `--sim`, the eval
    /// harness and `RenderSession` never raise it and their grids are
    /// untouched. Presentation only, exactly like `overlay_visible`.
    hint_visible: bool,
    /// M6 pause: the run loop has frozen the picture, so the progress row
    /// reads ` PAUSED ` with a `|` bar head instead of a percentage. Purely
    /// how the row PRINTS — the pipeline never decides what frame to show.
    paused: bool,
    /// Set on the visible→hidden transition; consumed by `render_present`,
    /// which invalidates the backend so the row under the overlay is
    /// repainted from a clean baseline (the diff state can never be left
    /// describing overlay cells that are no longer drawn).
    overlay_hide_pending: bool,
    /// Asset fps from the header (`fps_num/fps_den`) — the overlay's
    /// frame→seconds conversion. The reader rejects zero fps at open.
    fps: f64,
}

impl<'a> Player<'a> {
    pub fn new(
        reader: AsciiReader<'a>,
        cell_aspect: f64,
        repaint_full: bool,
        color: ColorDepth,
        glyph_tier: GlyphTier,
    ) -> Result<Player<'a>> {
        let (src_w, src_h) = reader
            .plane_dims(plane_id::Y)
            .ok_or(Error::Asset("asset has no Y (luma) plane"))?;
        let frame_count = reader.frame_count();
        if frame_count == 0 {
            return Err(Error::Asset("asset has zero frames"));
        }
        let chroma_dims = reader.plane_dims(plane_id::C);
        let use_chroma = color != ColorDepth::Mono && chroma_dims.is_some();
        let chroma_len = chroma_dims.map_or(0, |(w, h)| w as usize * h as usize);
        // Plane-registry detection (PLAN §4): the edge layer runs only when
        // E, Ex AND Ey all travel; a partial set is auto-disabled too.
        let has_edges = reader.plane_dims(plane_id::E).is_some()
            && reader.plane_dims(plane_id::EX).is_some()
            && reader.plane_dims(plane_id::EY).is_some();
        let has_h = reader.plane_dims(plane_id::H).is_some();
        // Viewport target ratio = the asset's header aspect; zero fields
        // (degenerate headers) fall back to 16:9, matching RenderSession's
        // documented `aspect()` fallback.
        let header = reader.header();
        let (aspect_num, aspect_den) = if header.aspect_num == 0 || header.aspect_den == 0 {
            (16, 9)
        } else {
            (header.aspect_num, header.aspect_den)
        };
        let src_len = src_w as usize * src_h as usize;
        // Overlay timekeeping (M5 scrub UX). Zero fps is rejected by
        // AsciiReader::open since M1; the max(ε) is belt-and-braces only.
        let fps = (f64::from(header.fps_num) / f64::from(header.fps_den.max(1))).max(1e-9);
        let mut levels_lut = [0u8; 256];
        build_levels_lut(&mut levels_lut, None); // identity until NORM says otherwise
        Ok(Player {
            reader,
            frame_count,
            src_w,
            src_h,
            chroma_dims,
            use_chroma,
            has_edges,
            has_h,
            aspect_num,
            aspect_den,
            cell_aspect,
            repaint_full,
            glyph_tier,
            color,
            palette: None,
            compose_params: ComposeParams::default(),
            state: HysteresisState::new(0, 0),
            vp: None,
            resampler: None,
            feat_resampler: None,
            chroma_resampler: None,
            luma_src: vec![0; src_len],
            e_src: vec![0; if has_edges { src_len } else { 0 }],
            ex_src: vec![0; if has_edges { src_len } else { 0 }],
            ey_src: vec![0; if has_edges { src_len } else { 0 }],
            h_src: vec![0; if has_h { src_len } else { 0 }],
            hl_mask: vec![0; if has_h { src_len } else { 0 }],
            sh_mask: vec![0; if has_h { src_len } else { 0 }],
            luma2_dst: Vec::new(),
            e_dst: Vec::new(),
            ex_dst: Vec::new(),
            ey_dst: Vec::new(),
            hl_dst: Vec::new(),
            sh_dst: Vec::new(),
            h_dst: Vec::new(),
            chroma_src: vec![0; if use_chroma { chroma_len * 2 } else { 0 }],
            cr_src: vec![0; if use_chroma { chroma_len } else { 0 }],
            cg_src: vec![0; if use_chroma { chroma_len } else { 0 }],
            cb_src: vec![0; if use_chroma { chroma_len } else { 0 }],
            cr_dst: Vec::new(),
            cg_dst: Vec::new(),
            cb_dst: Vec::new(),
            dial_overlay: None,
            levels_lut,
            lut_key: None,
            loaded: None,
            grid: Grid::new(0, 0),
            layer_mask: None,
            stage: StageNs::default(),
            overlay_visible: false,
            progress_ctx: None,
            hint_visible: false,
            paused: false,
            overlay_hide_pending: false,
            fps,
        })
    }

    /// The compositor tunables currently in force — the starting position for
    /// the interactive dials.
    pub fn compose_params(&self) -> ComposeParams {
        self.compose_params
    }

    /// Override the §3.5 compositor tunables (the eval driver wires
    /// params.toml `[compose]` here; interactive playback starts from the
    /// defaults, which are pinned to the committed params.toml by test, and
    /// turns them with the live dials). A change resets ALL per-cell
    /// hysteresis state, so the next frame is a cold start at the new
    /// position; an equal value is a no-op.
    pub fn set_compose_params(&mut self, params: ComposeParams) {
        if params == self.compose_params {
            return; // a press on a dial's stop moves nothing, so resets nothing
        }
        self.compose_params = params;
        // A change of rules is a discontinuity for every remembered ramp
        // index, `was_edge` bit and orientation bin: each was decided under
        // the OLD thresholds and is stale by construction — the same
        // reasoning as the reset on a LUT rebuild in `update_levels`.
        // Without this the picture moved only when a turn cleared the
        // hysteresis band, which is direction-dependent: lowering T_on armed
        // edges that T_off then held whatever the dial did next, and a
        // lifted ramp index stayed held after the lift came off — the dial
        // worked one way and not the other. The lift itself reaches the
        // picture through the LUT key in `update_levels` on the next render.
        self.state.reset();
    }

    /// Start collecting the per-cell winning-layer mask (render metadata for
    /// the eval harness — §6 edge-F1 prediction side). Costs one `Grid<u8>`
    /// kept in step with the terminal grid; interactive playback never calls
    /// this.
    pub fn enable_layer_mask(&mut self) {
        let mut mask = Grid::new(self.grid.cols(), self.grid.rows());
        mask.fill(auto_ascii_core::layer::BASE);
        self.layer_mask = Some(mask);
    }

    /// The layer mask for the last rendered frame (`None` unless
    /// [`enable_layer_mask`](Player::enable_layer_mask) was called). Values
    /// are `auto_ascii_core::compose::layer` ids at full terminal dims; pads are
    /// `layer::BASE`.
    pub fn layer_mask(&self) -> Option<&Grid<u8>> {
        self.layer_mask.as_ref()
    }

    /// Frames in the asset (> 0 — enforced at `new`).
    pub fn frame_count(&self) -> u32 {
        self.frame_count
    }

    /// The composed full terminal grid (viewport + letterbox pads) as of the
    /// last [`render_present`](Player::render_present) — the eval driver
    /// rasterizes this for downscale-SSIM and feeds it to the flicker
    /// accumulator (PLAN §6).
    pub fn grid(&self) -> &Grid<Cell> {
        &self.grid
    }

    /// Current viewport (`None` below the 32×9 minimum).
    pub fn viewport(&self) -> Option<Viewport> {
        self.vp
    }

    /// `(src_dims, dst_dims)` of the current LUMA resampler (`None` below
    /// the viewport minimum). M3: dst is `(Vc, 2·Vr)` — the §3.3 two-taps-
    /// per-cell plane; the resize fuzz asserts exactly that.
    pub fn resampler_dims(&self) -> Option<((u16, u16), (u16, u16))> {
        self.resampler.as_ref().map(|r| (r.src_dims(), r.dst_dims()))
    }

    /// Dimensions of the per-cell hysteresis state (viewport cells) — the
    /// §6 fuzz invariant "hysteresis buffers realloc'd to the new grid".
    pub fn hysteresis_dims(&self) -> (u16, u16) {
        (self.state.cols(), self.state.rows())
    }

    /// Decoded source Y plane (`src_w × src_h` L\* bytes) for the frame last
    /// passed to [`render_present`](Player::render_present) — the SSIM
    /// source-side input.
    pub fn luma_src(&self) -> &[u8] {
        &self.luma_src
    }

    /// The active per-shot NORM levels LUT (identity when the asset has no
    /// NORM). Applying it to [`luma_src`](Player::luma_src) reproduces the
    /// normalized luma the compositor consumed.
    pub fn levels_lut(&self) -> &[u8; 256] {
        &self.levels_lut
    }

    /// Cumulative per-stage wall times since construction.
    pub fn stage(&self) -> StageNs {
        self.stage
    }

    /// Resize path (PLAN §3.6 step 1): backend + grid realloc, viewport
    /// recompute, resampler tap rebuilds, palette reselection (density band
    /// tracks viewport cols), hysteresis realloc+reset (§3.5 graft from C),
    /// invalidate. The next rendered frame lands on the new grid.
    pub fn reflow<B: Backend>(&mut self, backend: &mut B, cols: u16, rows: u16) {
        backend.resize(cols, rows);
        self.reflow_grid(cols, rows);
        backend.invalidate();
    }

    /// Backend-free reflow (M4): everything [`reflow`](Player::reflow) does
    /// except the backend `resize`/`invalidate` calls — grid realloc,
    /// viewport, tap tables, palette, hysteresis realloc+reset. The
    /// terminal-free [`crate::RenderSession`] resize path.
    pub fn reflow_grid(&mut self, cols: u16, rows: u16) {
        self.grid.resize(cols, rows);
        if let Some(mask) = &mut self.layer_mask {
            mask.resize(cols, rows); // realloc + reset with the grid (§6 discipline)
        }
        // Letterbox to the ASSET's header aspect (M5 fix 2) — 16:9 assets
        // take the exact pre-M5 code path bit-for-bit.
        self.vp =
            compute_viewport_for(cols, rows, self.cell_aspect, self.aspect_num, self.aspect_den);
        if let Some(vp) = self.vp {
            self.palette = Some(select_palettes(self.glyph_tier, self.color, vp.cols));
            // §3.3: luma at Vc × 2Vr — one build with doubled vertical dst.
            self.resampler = Some(Resampler::build(self.src_w, self.src_h, vp.cols, 2 * vp.rows));
            self.state.resize(vp.cols, vp.rows);
            let cells = vp.cols as usize * vp.rows as usize;
            self.luma2_dst.resize(cells * 2, 0);
            if self.has_edges || self.has_h {
                self.feat_resampler =
                    Some(Resampler::build(self.src_w, self.src_h, vp.cols, vp.rows));
            }
            if self.has_edges {
                self.e_dst.resize(cells, 0);
                self.ex_dst.resize(cells, 0);
                self.ey_dst.resize(cells, 0);
            }
            if self.has_h {
                self.hl_dst.resize(cells, 0);
                self.sh_dst.resize(cells, 0);
                self.h_dst.resize(cells, 0);
            }
            if self.use_chroma {
                let (cw, ch) = self.chroma_dims.expect("use_chroma implies C dims");
                self.chroma_resampler = Some(Resampler::build(cw, ch, vp.cols, vp.rows));
                self.cr_dst.resize(cells, 0);
                self.cg_dst.resize(cells, 0);
                self.cb_dst.resize(cells, 0);
            }
        } else {
            // Below 32x9 (PLAN §3.2): "enlarge terminal" card until regrown.
            self.palette = None;
            self.resampler = None;
            self.feat_resampler = None;
            self.chroma_resampler = None;
            self.state.resize(0, 0);
        }
    }

    /// Drain the event queue (PLAN §3.6 step 1): Quit wins, resizes coalesce
    /// to the latest and trigger one reflow, digit keys report a jump.
    ///
    /// M3 review fix (seek ghosting): a digit jump is an explicit temporal
    /// discontinuity — the landing frame has no relation to what is on
    /// screen — so ALL per-cell hysteresis memory (`was_edge`, held ramp
    /// idx, orientation bin) is reset HERE, before the caller renders the
    /// landing frame. Same class as the §3.5 cut and resize resets; the
    /// shot-change reset in `update_levels` only covers jumps that cross a
    /// shot boundary — a same-shot jump would otherwise render edge glyphs
    /// a cold start at that frame would not (m3_layers regression test).
    pub fn drain_events<B: Backend>(&mut self, backend: &mut B) -> Drained {
        let (drained, resize) = drain_backend_events(backend);
        if let Some((c, r)) = resize {
            self.reflow(backend, c, r); // resize path resets state too (realloc)
        }
        if drained.jump_digit.is_some() || drained.seek_steps != 0 {
            self.state.reset(); // seek discontinuity: no pre-seek ghosting
        }
        // A dial move resets hysteresis too, but not here: the drain only
        // counts presses, and a press on a dial's stop moves nothing. The
        // reset rides the actual change, in `set_compose_params`.
        drained
    }

    /// Sequential-roll or FIDX-seek one plane into its standing buffer.
    fn load_plane(
        reader: &mut AsciiReader<'a>,
        sequential: bool,
        frame_idx: u32,
        id: u8,
        dst: &mut [u8],
    ) -> Result<()> {
        if sequential {
            reader.decode_plane_into(frame_idx, id, dst)
        } else {
            reader.seek_plane_into(frame_idx, id, dst)
        }
        .map_err(|source| Error::Decode { frame: frame_idx, plane: id, source })?;
        Ok(())
    }

    /// Bring every decoded plane to `frame_idx`. Sequential successors roll
    /// one delta via `decode_plane_into` (the standing double buffer, PLAN
    /// §3.6 step 3); anything else — startup, `--seek`, digit jumps,
    /// latest-frame-wins skips, loop wrap — goes through `seek_plane_into`
    /// (FIDX keyframe bsearch + delta rolls, PLAN §4). Frame-skipping on
    /// delta assets MUST NOT use plain decode (INTERFACES).
    fn load_frame(&mut self, frame_idx: u32) -> Result<()> {
        if self.loaded == Some(frame_idx) {
            return Ok(()); // paced repeat: planes already current
        }
        let sequential = frame_idx > 0 && self.loaded == Some(frame_idx - 1);
        Self::load_plane(&mut self.reader, sequential, frame_idx, plane_id::Y, &mut self.luma_src)?;
        if self.has_edges {
            Self::load_plane(&mut self.reader, sequential, frame_idx, plane_id::E, &mut self.e_src)?;
            Self::load_plane(&mut self.reader, sequential, frame_idx, plane_id::EX, &mut self.ex_src)?;
            Self::load_plane(&mut self.reader, sequential, frame_idx, plane_id::EY, &mut self.ey_src)?;
        }
        if self.has_h {
            Self::load_plane(&mut self.reader, sequential, frame_idx, plane_id::H, &mut self.h_src)?;
        }
        if self.use_chroma {
            Self::load_plane(&mut self.reader, sequential, frame_idx, plane_id::C, &mut self.chroma_src)?;
        }
        self.loaded = Some(frame_idx);
        Ok(())
    }

    /// Rebuild the levels LUT iff `frame_idx` entered a different shot
    /// (PLAN §3.5 per-shot auto-levels from NORM — per-frame rebuilds would
    /// pump; per-shot is the contract) or the shadow lift folded into it
    /// moved. Assets without NORM keep the identity window, lifted or not.
    ///
    /// M3: a shot change also resets ALL hysteresis state. This is a
    /// deliberate superset of the §3.5 "cut flags reset hysteresis" rule:
    /// when the LUT changes, every remembered ramp index refers to the OLD
    /// normalization and is stale by construction — and every CUT-flagged
    /// boundary is a shot change, so the spec case is covered exactly.
    fn update_levels(&mut self, frame_idx: u32) {
        let shot = self.reader.shot_for_frame(frame_idx).map(|s| s.first_frame);
        let key = (shot, self.compose_params.shadow_lift);
        if self.lut_key != Some(key) {
            build_levels_lut_lifted(
                &mut self.levels_lut,
                self.reader.norm_levels(frame_idx, plane_id::Y),
                key.1,
            );
            self.lut_key = Some(key);
            self.state.reset(); // scene-cut / shot-change reset (§3.5)
        }
    }

    /// Show/hide the transient bottom-row progress overlay (M5 scrub UX).
    /// The overlay is drawn over the composed grid by `render_grid`; hiding
    /// it schedules a one-shot backend invalidate consumed by the next
    /// [`render_present`](Player::render_present), so the diff baseline is
    /// rebuilt from a full repaint and can never keep describing overlay
    /// cells that are no longer drawn. Presentation-only: temporal state,
    /// the layer mask and the composed viewport are untouched.
    /// Show (or clear) the live-dial readout on the bottom row. Same
    /// hide-repaint contract as [`set_progress_overlay`](Self::set_progress_overlay):
    /// clearing it schedules the invalidate that repaints the row underneath.
    pub fn set_dial_overlay(&mut self, dial: Option<(&'static str, u8, u8)>) {
        if self.dial_overlay.is_some() && dial.is_none() {
            self.overlay_hide_pending = true;
        }
        self.dial_overlay = dial;
    }

    pub fn set_progress_overlay(&mut self, visible: bool) {
        if self.overlay_visible && !visible {
            self.overlay_hide_pending = true;
        }
        self.overlay_visible = visible;
    }

    /// Make the progress overlay report a COMPOSITION's position instead of
    /// this clip's own (M8, PLAN-M6-M8 §3). `None` restores the asset
    /// reading, which is what every single-asset path keeps — the M6 row is
    /// unchanged to the byte.
    pub fn set_progress_context(&mut self, ctx: Option<ProgressContext>) {
        self.progress_ctx = ctx;
    }

    /// Tell the progress row that playback is frozen (M6 pause): the bar
    /// head becomes `|` and the percent block reads ` PAUSED `. Presentation
    /// only — pausing is the run loop's business, and this just reports it.
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// Show/hide the key-hints row on `rows-2` (M6, PLAN-M6-M8 §1). Same
    /// hide-repaint contract as [`set_progress_overlay`](Self::set_progress_overlay):
    /// the visible→hidden edge schedules the invalidate that repaints the row
    /// underneath, so the diff baseline never keeps describing hint cells.
    /// The caller owns WHEN it shows — timers and stickiness are run-loop
    /// policy (`crate::Player`), never something the pipeline decides.
    pub fn set_hint_overlay(&mut self, visible: bool) {
        if self.hint_visible && !visible {
            self.overlay_hide_pending = true;
        }
        self.hint_visible = visible;
    }

    /// Decode → resample → NORM levels → compose → present one asset frame
    /// (PLAN §3.6 steps 3–6). Renders the "enlarge terminal" card when the
    /// terminal is below the 32x9 minimum.
    pub fn render_present<B: Backend>(
        &mut self,
        backend: &mut B,
        frame_idx: u32,
    ) -> Result<FrameStats> {
        self.render_grid(frame_idx)?;
        // Full-repaint mode, or the overlay just hid (M5 scrub UX): the diff
        // baseline must not survive an overlay transition.
        if self.repaint_full || std::mem::take(&mut self.overlay_hide_pending) {
            backend.invalidate();
        }
        let t = Instant::now();
        let stats = backend.present(&self.grid);
        self.stage.present += t.elapsed().as_nanos() as u64;
        Ok(stats)
    }

    /// Backend-free frame render (M4): decode → resample → NORM levels →
    /// compose into [`grid`](Player::grid), stopping short of `present` —
    /// the terminal-free [`crate::RenderSession`] frame path. Identical
    /// composition (and identical temporal-state mutations) to
    /// [`render_present`](Player::render_present).
    pub fn render_grid(&mut self, frame_idx: u32) -> Result<()> {
        if self.vp.is_some() && self.resampler.is_some() {
            let t = Instant::now();
            self.load_frame(frame_idx)?;
            self.stage.decode += t.elapsed().as_nanos() as u64;

            let t = Instant::now();
            self.update_levels(frame_idx);
            let resampler = self.resampler.as_mut().expect("checked above");
            resampler.apply(&self.luma_src, &mut self.luma2_dst);
            if self.has_edges || self.has_h {
                let feat = self.feat_resampler.as_mut().expect("features imply resampler");
                if self.has_edges {
                    feat.apply(&self.e_src, &mut self.e_dst);
                    feat.apply(&self.ex_src, &mut self.ex_dst);
                    feat.apply(&self.ey_src, &mut self.ey_dst);
                }
                if self.has_h {
                    // Bitflags don't box-average: expand each bit to a 0/255
                    // mask, resample, re-threshold (see H_HIGHLIGHT_MIN).
                    for (i, &h) in self.h_src.iter().enumerate() {
                        self.hl_mask[i] = if h & auto_ascii_core::h_flags::HIGHLIGHT != 0 { 255 } else { 0 };
                        self.sh_mask[i] = if h & auto_ascii_core::h_flags::DEEP_SHADOW != 0 { 255 } else { 0 };
                    }
                    feat.apply(&self.hl_mask, &mut self.hl_dst);
                    feat.apply(&self.sh_mask, &mut self.sh_dst);
                    for i in 0..self.h_dst.len() {
                        self.h_dst[i] = u8::from(self.hl_dst[i] >= H_HIGHLIGHT_MIN)
                            | (u8::from(self.sh_dst[i] >= H_SHADOW_MIN) << 1);
                    }
                }
            }
            if self.use_chroma {
                unpack_rgb565(&self.chroma_src, &mut self.cr_src, &mut self.cg_src, &mut self.cb_src);
                let cres = self.chroma_resampler.as_mut().expect("use_chroma implies resampler");
                cres.apply(&self.cr_src, &mut self.cr_dst);
                cres.apply(&self.cg_src, &mut self.cg_dst);
                cres.apply(&self.cb_src, &mut self.cb_dst);
            }
            self.stage.resample += t.elapsed().as_nanos() as u64;

            let t = Instant::now();
            let vp = self.vp.expect("checked above");
            let set = self.palette.as_ref().expect("viewport implies palette");
            let planes = FramePlanes {
                luma2: &self.luma2_dst,
                e: self.has_edges.then_some(&self.e_dst[..]),
                ex: self.has_edges.then_some(&self.ex_dst[..]),
                ey: self.has_edges.then_some(&self.ey_dst[..]),
                h: self.has_h.then_some(&self.h_dst[..]),
                chroma: self
                    .use_chroma
                    .then(|| (&self.cr_dst[..], &self.cg_dst[..], &self.cb_dst[..])),
            };
            match &mut self.layer_mask {
                Some(mask) => compose_frame_masked(
                    &planes,
                    &vp,
                    &self.levels_lut,
                    set,
                    &self.compose_params,
                    &mut self.state,
                    &mut self.grid,
                    mask,
                ),
                None => compose_frame(
                    &planes,
                    &vp,
                    &self.levels_lut,
                    set,
                    &self.compose_params,
                    &mut self.state,
                    &mut self.grid,
                ),
            }
            self.stage.compose += t.elapsed().as_nanos() as u64;
        } else {
            draw_enlarge_card(&mut self.grid);
            if let Some(mask) = &mut self.layer_mask {
                mask.fill(auto_ascii_core::layer::BASE);
            }
        }
        if self.overlay_visible {
            // Drawn last, over pads/viewport alike (bottom row only); the
            // layer mask is deliberately NOT updated — the overlay is
            // presentation, not composition (eval never enables it).
            match self.progress_ctx {
                Some(ctx) => draw_progress_overlay_clips(
                    &mut self.grid,
                    ctx.frame,
                    ctx.frame_count,
                    ctx.fps(),
                    ctx.clip,
                    self.paused,
                ),
                None => draw_progress_overlay_paused(
                    &mut self.grid,
                    frame_idx,
                    self.frame_count,
                    self.fps,
                    self.paused,
                ),
            }
        }
        if let Some((label, value, max)) = self.dial_overlay {
            draw_dial_overlay(&mut self.grid, label, value, max);
        }
        // One row above the progress/dial row, same presentation-only rules:
        // no layer mask, no temporal state (M6, PLAN-M6-M8 §1). Gated on the
        // viewport so it never lands on the enlarge card — when the terminal
        // is too small to play, "enlarge terminal" is the only message that
        // matters, and `rows-2` is exactly where the card's second line sits
        // on a 4-row screen.
        if self.hint_visible && self.vp.is_some() {
            draw_hint_overlay(&mut self.grid);
        }
        Ok(())
    }

    /// Reset ALL per-cell temporal state (ramp-index hysteresis, edge
    /// on/off memory, orientation bins) — the §3.5 discontinuity reset.
    /// [`crate::RenderSession`] calls this on a backward frame jump; the
    /// interactive digit-seek path resets through
    /// [`drain_events`](Player::drain_events).
    pub fn reset_temporal_state(&mut self) {
        self.state.reset();
    }

    /// Re-key palette selection (PLAN §3.4 charset tier axis). Takes effect
    /// at the next [`reflow_grid`](Player::reflow_grid)/[`reflow`](Player::reflow)
    /// — palette objects are (re)built there, keyed on the viewport density.
    pub fn set_glyph_tier(&mut self, glyph_tier: GlyphTier) {
        self.glyph_tier = glyph_tier;
    }

    /// Override the §3.2 cell aspect (`cell_h_px / cell_w_px`). Takes effect
    /// at the next reflow (viewport math is recomputed there).
    pub fn set_cell_aspect(&mut self, cell_aspect: f64) {
        self.cell_aspect = cell_aspect;
    }
}

/// Fold per-shot p2/p98 NORM levels into a 256-entry LUT (PLAN §3.5:
/// `n = clamp((L − shot_lo) · shot_inv_range)`, rounded). `None` levels or a
/// degenerate span (p98 ≤ p2 — flat shot, or the (0,0) rows of unused plane
/// slots / NORM-less assets) → identity, keeping M0 assets byte-identical.
pub fn build_levels_lut(lut: &mut [u8; 256], levels: Option<PlaneLevels>) {
    build_levels_lut_lifted(lut, levels, 0);
}

/// [`build_levels_lut`] with a shadow lift applied on top of the linear window
/// ([`auto_ascii_core::ComposeParams::shadow_lift`]). `shadow_lift == 0` reproduces
/// `build_levels_lut` byte for byte.
///
/// The lift blends the normalized value `n` toward `sqrt(n · 255)` — the
/// classic shadow-opening curve — weighted by `lift/255`:
///
/// ```text
/// out = n + (isqrt(n · 255) − n) · lift / 255
/// ```
///
/// **Integer by construction, deliberately.** A `powf` gamma is the textbook
/// form, but float results are not guaranteed bit-identical across platforms
/// and the render goldens here are byte-compared. Both terms are monotonic
/// non-decreasing in `n` and the blend weights are fixed, so the curve is
/// monotonic — the ramp can never invert — and identical on every target.
/// `0` and `255` are fixed points, so this opens the shadows without raising
/// black or clipping white. At full strength a mid-shadow 64 lands at 127: a
/// two-to-three step move on an 8–16 step ramp, which is the entire point.
pub fn build_levels_lut_lifted(lut: &mut [u8; 256], levels: Option<PlaneLevels>, shadow_lift: u8) {
    match levels {
        Some(PlaneLevels { p2, p98 }) if p98 > p2 => {
            let lo = u32::from(p2);
            let span = u32::from(p98) - lo;
            for (v, out) in lut.iter_mut().enumerate() {
                let v = v as u32;
                *out = if v <= lo {
                    0
                } else if v >= lo + span {
                    255
                } else {
                    (((v - lo) * 255 + span / 2) / span) as u8
                };
            }
        }
        _ => {
            for (v, out) in lut.iter_mut().enumerate() {
                *out = v as u8;
            }
        }
    }
    apply_shadow_lift(lut, shadow_lift);
}

/// Bend an already-built levels LUT toward the shadows, in place. `lift == 0`
/// returns immediately, so the un-lifted path stays byte-identical.
fn apply_shadow_lift(lut: &mut [u8; 256], lift: u8) {
    if lift == 0 {
        return;
    }
    let lift = u32::from(lift);
    for out in lut.iter_mut() {
        let n = u32::from(*out);
        // isqrt(n · 255) — exact integer square root, no float anywhere.
        // curved >= n for every n in 0..=255, so this only ever lifts.
        let curved = (n * 255).isqrt();
        *out = (n + (curved - n) * lift / 255) as u8;
    }
}

/// Unpack little-endian RGB565 (factory C plane contract, PLAN §4) into
/// three 8-bit channel planes, expanding with bit replication
/// (`r8 = r5<<3 | r5>>2` etc. — 0x1f → 255, canonical).
pub fn unpack_rgb565(src: &[u8], r: &mut [u8], g: &mut [u8], b: &mut [u8]) {
    for (i, px) in src.chunks_exact(2).enumerate() {
        let v = u16::from_le_bytes([px[0], px[1]]);
        let r5 = (v >> 11) as u8;
        let g6 = ((v >> 5) & 0x3f) as u8;
        let b5 = (v & 0x1f) as u8;
        r[i] = (r5 << 3) | (r5 >> 2);
        g[i] = (g6 << 2) | (g6 >> 4);
        b[i] = (b5 << 3) | (b5 >> 2);
    }
}

/// Centered "enlarge terminal" card (PLAN §3.2, below 32x9).
pub fn draw_enlarge_card(grid: &mut Grid<Cell>) {
    grid.fill(Cell::BLANK);
    let (cols, rows) = (grid.cols(), grid.rows());
    if cols == 0 || rows == 0 {
        return;
    }
    let lines: [&str; 2] = ["AUTO-ASCII", "enlarge terminal (min 32x9)"];
    let top = rows.saturating_sub(lines.len() as u16) / 2;
    for (i, line) in lines.iter().enumerate() {
        let row = top + i as u16;
        if row >= rows {
            break;
        }
        let n = (line.len() as u16).min(cols); // ASCII-only card text
        let left = (cols - n) / 2;
        for (j, ch) in line.chars().take(n as usize).enumerate() {
            grid.set(left + j as u16, row, Cell::new(ch, Rgb::gray(220), Rgb::BLACK));
        }
    }
}

/// The transient 1-line progress overlay (M5 scrub UX): bottom terminal row,
/// `_MM:SS_/_MM:SS_[====>....]_NN%_` in pure ASCII (palette/tier-agnostic —
/// mono quantizes the colors away and the glyphs still carry everything).
/// Pure function of `(frame, frame_count, fps, cols)` — byte-deterministic,
/// so diff-mode presents of an unchanged overlay row cost zero damage.
pub fn draw_progress_overlay(grid: &mut Grid<Cell>, frame: u32, frame_count: u32, fps: f64) {
    draw_progress_overlay_clips(grid, frame, frame_count, fps, None, false);
}

/// [`draw_progress_overlay`] with the M6 pause reading: `paused` swaps the
/// bar head for `|` and the percent block for ` PAUSED `, so a frozen
/// picture never reads as a stalled one.
pub fn draw_progress_overlay_paused(
    grid: &mut Grid<Cell>,
    frame: u32,
    frame_count: u32,
    fps: f64,
    paused: bool,
) {
    draw_progress_overlay_clips(grid, frame, frame_count, fps, None, paused);
}

/// [`draw_progress_overlay`] with the M8 clip block: `clip = Some((c, n))`
/// adds ` c/N ` right after the timecode while a composition plays
/// (PLAN-M6-M8 §3). The block is dropped below [`PROGRESS_HINT_MIN_COLS`]
/// and for single-clip compositions, so `None` — every single-asset path —
/// prints the M6 row byte for byte.
pub fn draw_progress_overlay_clips(
    grid: &mut Grid<Cell>,
    frame: u32,
    frame_count: u32,
    fps: f64,
    clip: Option<(usize, usize)>,
    paused: bool,
) {
    let (cols, rows) = (grid.cols(), grid.rows());
    if cols == 0 || rows == 0 {
        return;
    }
    let row = rows - 1;
    let fg = Rgb::gray(235);
    let bg = Rgb::new(24, 24, 40);

    // The project's one timecode shape (M7, core tier): the row and
    // `auto-ascii list` cannot drift apart because there is one formatter.
    let mmss = crate::timecode::format_mmss;
    let pos = f64::from(frame) / fps;
    let total = f64::from(frame_count) / fps;
    // frame_count > 0 is enforced at Player::new; percent of the LAST frame
    // reads 100 (frame_count−1 maps to the full bar).
    let pct = if frame_count > 1 {
        (u64::from(frame) * 100) / u64::from(frame_count - 1)
    } else {
        100
    };
    // M6 (PLAN-M6-M8 §1): the arrow-key hint rides at the far left when the
    // row is wide enough for it; below the threshold this is empty and the
    // bytes are exactly the M5 row.
    let hint = if cols >= PROGRESS_HINT_MIN_COLS {
        format!(" <- {} -> ", scrub_step_label())
    } else {
        String::new()
    };
    let mut left = format!(" {} / {} ", mmss(pos), mmss(total));
    // M8 (PLAN-M6-M8 §3): which clip of how many, right after the time
    // block — only for a real stitch, and only where the row can spare it.
    if let Some((c, n)) = clip
        && n > 1
        && cols >= PROGRESS_HINT_MIN_COLS
    {
        left.push_str(&format!("{c}/{n} "));
    }
    // M6 pause: the percent block says PAUSED instead, under the same width
    // rules — the bar simply gets two columns fewer. Printable ASCII, so the
    // strict overlay parser and every palette keep reading it.
    let right = if paused { " PAUSED ".to_string() } else { format!(" {pct:>3}% ") };

    // Bar fills whatever remains between the text blocks; on absurdly small
    // grids the texts alone are truncated to the row.
    let mut line = String::with_capacity(cols as usize);
    line.push_str(&hint);
    line.push_str(&left);
    let fixed = hint.chars().count() + left.chars().count() + right.chars().count() + 2; // []
    if (cols as usize) > fixed {
        let span = cols as usize - fixed;
        let filled = if frame_count > 1 {
            (u64::from(frame) * span as u64) / u64::from(frame_count - 1)
        } else {
            span as u64
        } as usize;
        line.push('[');
        for i in 0..span {
            // The head reads the transport: `>` playing, `|` frozen.
            let head = if paused { '|' } else { '>' };
            line.push(if i < filled {
                '='
            } else if i == filled {
                head
            } else {
                '.'
            });
        }
        line.push(']');
    }
    line.push_str(&right);

    let mut chars = line.chars();
    for col in 0..cols {
        let ch = chars.next().unwrap_or(' ');
        grid.set(col, row, Cell::new(ch, fg, bg));
    }
}

/// The transient 1-line live-dial readout: bottom terminal row,
/// `_<label>_[####----]_NNN/MMM_` in pure ASCII, so it reads on every palette
/// and color tier exactly like the progress overlay. Pure function of
/// `(label, value, max, cols)` — byte-deterministic, so an unchanged row costs
/// zero damage in diff mode.
pub fn draw_dial_overlay(grid: &mut Grid<Cell>, label: &str, value: u8, max: u8) {
    let (cols, rows) = (grid.cols(), grid.rows());
    if cols == 0 || rows == 0 {
        return;
    }
    let row = rows - 1;
    // Warmer than the progress overlay's blue so the two are never confused
    // at a glance while both are being driven from the keyboard.
    let fg = Rgb::gray(245);
    let bg = Rgb::new(46, 32, 16);

    let left = format!(" {label} ");
    let right = format!(" {value:>3}/{max:<3} ");
    let mut line = String::with_capacity(cols as usize);
    line.push_str(&left);
    let fixed = left.chars().count() + right.chars().count() + 2; // "[" + "]"
    if (cols as usize) > fixed {
        let span = cols as usize - fixed;
        // Fill proportional to value/max; max == 0 would be a degenerate dial.
        let filled = if max > 0 {
            (usize::from(value) * span) / usize::from(max)
        } else {
            0
        };
        line.push('[');
        for i in 0..span {
            line.push(if i < filled { '#' } else { '-' });
        }
        line.push(']');
    }
    line.push_str(&right);

    let mut chars = line.chars();
    for col in 0..cols {
        let ch = chars.next().unwrap_or(' ');
        grid.set(col, row, Cell::new(ch, fg, bg));
    }
}

/// [`SCRUB_STEP_SECS`] as the overlays print it: whole seconds carry no
/// trailing `.0` (`5s`, not `5.0s`), which is all a one-line row has room to
/// say. Derived, never a literal — the two rows and the constant cannot drift.
fn scrub_step_label() -> String {
    format!("{SCRUB_STEP_SECS}s")
}

/// The key-hints line for a `cols`-wide row (M6, PLAN-M6-M8 §1). Items are
/// dropped WHOLE, in [`HINT_DROP_ORDER`], until the list fits — so a narrow
/// terminal shows fewer hints rather than a word cut in half, and the
/// survivors keep their reading order. One leading and trailing space frame
/// the list, matching the progress row's blocks. Returns "" when not even
/// `v controls` fits — the row is still painted, just empty.
fn hint_line(cols: u16) -> String {
    let arrows = format!("<- -> {}", scrub_step_label());
    let items: [&str; 7] =
        ["q quit", "space pause", "0-9 jump", &arrows, "d dial", "[ ] adjust", "v controls"];
    // Framed width of the kept items: the items, the gaps between them and
    // the two framing spaces. Every byte here is ASCII, so byte length is
    // column count (PLAN-M6-M8 §0.6: overlays stay printable ASCII).
    let width = |keep: &[bool; 7]| -> usize {
        let kept = items.iter().zip(keep).filter(|(_, k)| **k);
        let (n, len) = kept.fold((0, 0), |(n, len), (it, _)| (n + 1, len + it.len()));
        if n == 0 { 0 } else { len + (n - 1) * HINT_SEP.len() + 2 }
    };

    let mut keep = [true; 7];
    for i in HINT_DROP_ORDER {
        if width(&keep) <= cols as usize {
            break;
        }
        keep[i] = false;
    }
    if width(&keep) > cols as usize {
        return String::new(); // not even the `v controls` hint fits
    }
    let kept: Vec<&str> =
        items.iter().zip(keep).filter(|(_, k)| *k).map(|(it, _)| *it).collect();
    format!(" {} ", kept.join(HINT_SEP))
}

/// The key-hints row (M6, PLAN-M6-M8 §1): one line on `rows-2` in the
/// progress overlay's colors, listing every bound key. Shown while a
/// transient overlay is up, for a short window at start-up and whenever `v`
/// pins it (all run-loop policy — see `Player::set_hint_overlay`). Pure
/// function of `cols`, so an unchanged row costs zero damage in diff mode.
pub fn draw_hint_overlay(grid: &mut Grid<Cell>) {
    let (cols, rows) = (grid.cols(), grid.rows());
    if cols == 0 || rows < 2 {
        return; // nowhere to put it without evicting the progress row
    }
    let row = rows - 2;
    // Deliberately the progress overlay's palette: one chrome, two rows.
    let fg = Rgb::gray(235);
    let bg = Rgb::new(24, 24, 40);

    let line = hint_line(cols);
    let mut chars = line.chars();
    for col in 0..cols {
        let ch = chars.next().unwrap_or(' ');
        grid.set(col, row, Cell::new(ch, fg, bg));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadow_lift_opens_shadows_monotonically() {
        // Off is byte-identical to the un-lifted builder.
        let (mut plain, mut lifted) = ([0u8; 256], [0u8; 256]);
        build_levels_lut(&mut plain, None);
        build_levels_lut_lifted(&mut lifted, None, 0);
        assert_eq!(plain, lifted, "shadow_lift 0 must not perturb the LUT");

        for lift in [1u8, 64, 128, 255] {
            let mut lut = [0u8; 256];
            build_levels_lut_lifted(&mut lut, None, lift);
            // Endpoints are fixed points: black stays black, white stays white.
            assert_eq!(lut[0], 0, "lift {lift} raised black");
            assert_eq!(lut[255], 255, "lift {lift} clipped white");
            // Monotonic — a non-monotonic tone curve would invert the ramp.
            assert!(lut.windows(2).all(|w| w[0] <= w[1]), "lift {lift} is not monotonic");
            // It only ever lifts, never darkens.
            assert!(
                lut.iter().enumerate().all(|(i, &v)| v as usize >= i),
                "lift {lift} darkened a value"
            );
        }

        // Full strength is the sqrt curve: a mid-shadow 64 must clear the
        // first ramp steps rather than sitting at the same glyph as black.
        let mut full = [0u8; 256];
        build_levels_lut_lifted(&mut full, None, 255);
        assert_eq!(full[64], 127, "full lift should take 64 to the sqrt curve");
        assert!(full[32] > 2 * 32, "full lift should more than double deep shadow");
    }

    #[test]
    fn levels_lut_identity_without_norm() {
        let mut lut = [0u8; 256];
        build_levels_lut(&mut lut, None);
        assert!(lut.iter().enumerate().all(|(i, &v)| v as usize == i));
        // Degenerate spans are identity too (unused (0,0) slots, flat shots).
        build_levels_lut(&mut lut, Some(PlaneLevels { p2: 0, p98: 0 }));
        assert!(lut.iter().enumerate().all(|(i, &v)| v as usize == i));
        build_levels_lut(&mut lut, Some(PlaneLevels { p2: 200, p98: 100 }));
        assert!(lut.iter().enumerate().all(|(i, &v)| v as usize == i));
    }

    #[test]
    fn levels_lut_stretches_and_clamps() {
        let mut lut = [0u8; 256];
        build_levels_lut(&mut lut, Some(PlaneLevels { p2: 50, p98: 200 }));
        assert_eq!(lut[0], 0);
        assert_eq!(lut[50], 0);
        assert_eq!(lut[200], 255);
        assert_eq!(lut[255], 255);
        assert_eq!(lut[125], 128); // midpoint → mid gray (rounded)
        // Monotone non-decreasing everywhere.
        assert!(lut.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn rgb565_unpack_is_canonical() {
        // Solid red 0xF800, solid green 0x07E0, solid blue 0x001F, white.
        let src: Vec<u8> = [0xF800u16, 0x07E0, 0x001F, 0xFFFF]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let (mut r, mut g, mut b) = (vec![0u8; 4], vec![0u8; 4], vec![0u8; 4]);
        unpack_rgb565(&src, &mut r, &mut g, &mut b);
        assert_eq!((r[0], g[0], b[0]), (255, 0, 0));
        assert_eq!((r[1], g[1], b[1]), (0, 255, 0));
        assert_eq!((r[2], g[2], b[2]), (0, 0, 255));
        assert_eq!((r[3], g[3], b[3]), (255, 255, 255));
    }

    #[test]
    fn glyph_tier_mapping_from_caps() {
        let mut caps = Caps::default();
        assert_eq!(glyph_tier_from_caps(&caps), GlyphTier::Ascii);
        caps.glyph_support = GlyphSupportTier::Cp437;
        assert_eq!(glyph_tier_from_caps(&caps), GlyphTier::Ascii, "CP437 stays on the ascii floor");
        caps.glyph_support = GlyphSupportTier::UnicodeCore;
        assert_eq!(glyph_tier_from_caps(&caps), GlyphTier::UnicodeBlocks);
        caps.glyph_support = GlyphSupportTier::UnicodeFull;
        assert_eq!(
            glyph_tier_from_caps(&caps),
            GlyphTier::UnicodeBlocks,
            "braille needs the verified flag, not just UnicodeFull"
        );
        caps.glyphs = caps.glyphs.with(GlyphFlags::BRAILLE);
        assert_eq!(glyph_tier_from_caps(&caps), GlyphTier::BrailleVerified);
    }

    #[test]
    fn color_depth_mapping() {
        assert_eq!(color_depth(ColorTier::True), ColorDepth::True);
        assert_eq!(color_depth(ColorTier::C256), ColorDepth::C256);
        assert_eq!(color_depth(ColorTier::C16), ColorDepth::C16);
        assert_eq!(color_depth(ColorTier::Mono), ColorDepth::Mono);
    }

    /// M5 scrub UX: the overlay is a deterministic pure function of
    /// (frame, count, fps, cols) — full-width, ASCII-only, endpoints exact.
    #[test]
    fn progress_overlay_is_deterministic_ascii_and_full_width() {
        let mut g: Grid<Cell> = Grid::new(80, 24);
        draw_progress_overlay(&mut g, 900, 5400, 30.0); // 30 s of 180 s
        let bottom: String = (0..80).map(|c| g.get(c, 23).glyph()).collect();
        assert!(bottom.contains("0:30 / 3:00"), "time text: {bottom:?}");
        assert!(bottom.contains('[') && bottom.contains(']') && bottom.contains('>'));
        assert!(bottom.is_ascii(), "overlay must render on every tier: {bottom:?}");
        // Rows above the overlay are untouched (BLANK from Grid::new).
        assert!((0..80).all(|c| g.get(c, 22) == Cell::BLANK));

        // Endpoints: frame 0 → 0%, empty bar; last frame → 100%, full bar.
        draw_progress_overlay(&mut g, 0, 5400, 30.0);
        let s: String = (0..80).map(|c| g.get(c, 23).glyph()).collect();
        assert!(s.contains("  0%"), "{s:?}");
        assert!(!s.contains('='), "{s:?}");
        draw_progress_overlay(&mut g, 5399, 5400, 30.0);
        let s: String = (0..80).map(|c| g.get(c, 23).glyph()).collect();
        assert!(s.contains("100%"), "{s:?}");
        assert!(!s.contains('.') || !s.contains('>'), "bar full at the end: {s:?}");

        // Determinism: same inputs → byte-identical row (diff-mode presents
        // of an unchanged overlay row must cost zero damage).
        let mut g2: Grid<Cell> = Grid::new(80, 24);
        draw_progress_overlay(&mut g2, 5399, 5400, 30.0);
        assert_eq!(g.row(23), g2.row(23));

        // Never panics on degenerate grids.
        for (c, r) in [(1u16, 1u16), (7, 2), (12, 1), (31, 8)] {
            let mut t: Grid<Cell> = Grid::new(c, r);
            draw_progress_overlay(&mut t, 10, 20, 30.0);
        }
    }

    /// M8 (PLAN-M6-M8 §3): the ` c/N ` clip block rides right after the
    /// time block — but only for a real stitch, and only where the row can
    /// spare it. Everything else is the M6 row to the byte.
    #[test]
    fn progress_overlay_clip_block_is_earned() {
        let row_at = |cols: u16, clip: Option<(usize, usize)>| -> String {
            let mut g: Grid<Cell> = Grid::new(cols, 4);
            draw_progress_overlay_clips(&mut g, 900, 5400, 30.0, clip, false);
            (0..cols).map(|c| g.get(c, 3).glyph()).collect()
        };
        let m6 = |cols: u16| -> String {
            let mut g: Grid<Cell> = Grid::new(cols, 4);
            draw_progress_overlay(&mut g, 900, 5400, 30.0);
            (0..cols).map(|c| g.get(c, 3).glyph()).collect()
        };

        let wide = row_at(80, Some((2, 3)));
        assert!(wide.contains(" 0:30 / 3:00 2/3 ["), "clip block after the time: {wide:?}");
        assert_eq!(wide.len(), 80, "the row is still painted edge to edge");

        // One clip is not a stitch; a narrow row spends its columns on the
        // bar; and no context at all is the M5/M6 row.
        assert_eq!(row_at(80, Some((1, 1))), m6(80), "single-clip compositions say nothing");
        assert_eq!(row_at(63, Some((2, 3))), m6(63), "below 64 columns the block is dropped");
        assert_eq!(row_at(80, None), m6(80), "no context = the M6 row");
        assert_eq!(row_at(64, Some((2, 3))).len(), 64, "the threshold row still fits");
        assert!(wide.is_ascii(), "overlays stay printable ASCII (§0.6)");
    }

    #[test]
    fn enlarge_card_fits_tiny_grids() {
        for (c, r) in [(1u16, 1u16), (10, 2), (31, 8), (80, 24)] {
            let mut g = Grid::new(c, r);
            draw_enlarge_card(&mut g); // must never panic / go OOB
            assert_eq!(g.cols(), c);
        }
        let mut g = Grid::new(40, 9);
        draw_enlarge_card(&mut g);
        let mid: String = (0..40).map(|col| g.get(col, 3).glyph()).collect();
        assert!(mid.contains("AUTO-ASCII"), "card text missing: {mid:?}");
    }
}
