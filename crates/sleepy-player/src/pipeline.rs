//! The frame pipeline: decode → resample → NORM levels → compose → present
//! (PLAN §3.4–§3.6). Extracted from the binary at M2 (item B decision) so
//! `sleepy-factory eval` drives the EXACT player code path headlessly
//! against `SimBackend` — metrics measure the real renderer, not a
//! reimplementation. The binary's event loop, pacing and CLI stay in
//! `main.rs`; nothing here touches a clock or a tty.
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

use anyhow::{Context, Result, bail};
use slpy_core::{
    Cell, ColorDepth, ComposeParams, FramePlanes, GlyphTier, Grid, HysteresisState, PaletteSet,
    Resampler, Rgb, Viewport, compose_frame, compose_frame_masked, compute_viewport,
    select_palettes,
};
use slpy_format::header::plane_id;
use slpy_format::{PlaneLevels, SlpyReader};
use slpy_term::{Backend, Caps, ColorTier, Event, FrameStats, GlyphFlags, GlyphSupportTier, Key};

/// H-mask box-average thresholds (integrator decision, M3): the H plane is
/// bitflags, so each bit is expanded to a 0/255 mask at source resolution,
/// box-averaged through the shared feature resampler, and re-thresholded per
/// cell. Highlights are sparse accents — a quarter of the cell's ink is
/// enough to keep a star alive at fine grids without letting single-pixel
/// noise own a coarse cell; deep shadow is an area feature — half the cell.
const H_HIGHLIGHT_MIN: u8 = 64;
const H_SHADOW_MIN: u8 = 128;

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
/// (slpy-core mirrors the variants without depending on slpy-term).
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
}

/// The frame pipeline: decode → resample → NORM levels → compose into a
/// term-sized grid. All buffers are (re)allocated only in `new`/`reflow` —
/// the hot loop is allocation-free (PLAN §6 discipline).
pub struct Player<'a> {
    reader: SlpyReader<'a>,
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
    levels_lut: [u8; 256],
    /// `first_frame` of the shot `levels_lut` was built for (`None` = the
    /// identity LUT for assets without NORM).
    lut_shot: Option<u32>,
    /// Frame currently decoded in the source buffers — drives the
    /// sequential-roll vs FIDX-seek decode policy on delta assets.
    loaded: Option<u32>,
    /// Full terminal grid (viewport + letterbox pads).
    grid: Grid<Cell>,
    /// Winning-layer render metadata (`slpy_core::compose::layer` ids, same
    /// dims as `grid`), collected only when the eval driver asks
    /// ([`enable_layer_mask`](Player::enable_layer_mask)) — `None` keeps the
    /// interactive hot path untouched. M3: an enabled mask is filled by
    /// `compose_frame_masked` (byte-identical cells, tags observable) — the
    /// §6 edge-F1 prediction side.
    layer_mask: Option<Grid<u8>>,
    stage: StageNs,
}

impl<'a> Player<'a> {
    pub fn new(
        reader: SlpyReader<'a>,
        cell_aspect: f64,
        repaint_full: bool,
        color: ColorDepth,
        glyph_tier: GlyphTier,
    ) -> Result<Player<'a>> {
        let (src_w, src_h) = reader
            .plane_dims(plane_id::Y)
            .context("asset has no Y (luma) plane")?;
        let frame_count = reader.frame_count();
        if frame_count == 0 {
            bail!("asset has zero frames");
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
        let src_len = src_w as usize * src_h as usize;
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
            levels_lut,
            lut_shot: None,
            loaded: None,
            grid: Grid::new(0, 0),
            layer_mask: None,
            stage: StageNs::default(),
        })
    }

    /// Override the §3.5 compositor tunables (the eval driver wires
    /// params.toml `[compose]` here; interactive playback keeps the
    /// defaults, which are pinned to the committed params.toml by test).
    pub fn set_compose_params(&mut self, params: ComposeParams) {
        self.compose_params = params;
    }

    /// Start collecting the per-cell winning-layer mask (render metadata for
    /// the eval harness — §6 edge-F1 prediction side). Costs one `Grid<u8>`
    /// kept in step with the terminal grid; interactive playback never calls
    /// this.
    pub fn enable_layer_mask(&mut self) {
        let mut mask = Grid::new(self.grid.cols(), self.grid.rows());
        mask.fill(slpy_core::layer::BASE);
        self.layer_mask = Some(mask);
    }

    /// The layer mask for the last rendered frame (`None` unless
    /// [`enable_layer_mask`](Player::enable_layer_mask) was called). Values
    /// are `slpy_core::compose::layer` ids at full terminal dims; pads are
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
        self.grid.resize(cols, rows);
        if let Some(mask) = &mut self.layer_mask {
            mask.resize(cols, rows); // realloc + reset with the grid (§6 discipline)
        }
        self.vp = compute_viewport(cols, rows, self.cell_aspect);
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
        backend.invalidate();
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
        let mut resize: Option<(u16, u16)> = None;
        let mut jump_digit = None;
        while let Some(ev) = backend.events().pop() {
            match ev {
                Event::Quit => return Drained { quit: true, jump_digit: None },
                Event::Resize(c, r) => resize = Some((c, r)),
                Event::Key(Key::Char(c @ '0'..='9')) => jump_digit = Some(c as u8 - b'0'),
                Event::Key(_) => {}
            }
        }
        if let Some((c, r)) = resize {
            self.reflow(backend, c, r); // resize path resets state too (realloc)
        }
        if jump_digit.is_some() {
            self.state.reset(); // seek discontinuity: no pre-seek ghosting
        }
        Drained { quit: false, jump_digit }
    }

    /// Sequential-roll or FIDX-seek one plane into its standing buffer.
    fn load_plane(
        reader: &mut SlpyReader<'a>,
        sequential: bool,
        frame_idx: u32,
        id: u8,
        dst: &mut [u8],
    ) -> Result<()> {
        if sequential {
            reader
                .decode_plane_into(frame_idx, id, dst)
                .with_context(|| format!("decoding plane {id} of frame {frame_idx}"))?;
        } else {
            reader
                .seek_plane_into(frame_idx, id, dst)
                .with_context(|| format!("seeking plane {id} to frame {frame_idx}"))?;
        }
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
    /// pump; per-shot is the contract). Assets without NORM keep identity.
    ///
    /// M3: a shot change also resets ALL hysteresis state. This is a
    /// deliberate superset of the §3.5 "cut flags reset hysteresis" rule:
    /// when the LUT changes, every remembered ramp index refers to the OLD
    /// normalization and is stale by construction — and every CUT-flagged
    /// boundary is a shot change, so the spec case is covered exactly.
    fn update_levels(&mut self, frame_idx: u32) {
        let shot = self.reader.shot_for_frame(frame_idx).map(|s| s.first_frame);
        if shot != self.lut_shot {
            build_levels_lut(&mut self.levels_lut, self.reader.norm_levels(frame_idx, plane_id::Y));
            self.lut_shot = shot;
            self.state.reset(); // scene-cut / shot-change reset (§3.5)
        }
    }

    /// Decode → resample → NORM levels → compose → present one asset frame
    /// (PLAN §3.6 steps 3–6). Renders the "enlarge terminal" card when the
    /// terminal is below the 32x9 minimum.
    pub fn render_present<B: Backend>(
        &mut self,
        backend: &mut B,
        frame_idx: u32,
    ) -> Result<FrameStats> {
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
                        self.hl_mask[i] = if h & slpy_core::h_flags::HIGHLIGHT != 0 { 255 } else { 0 };
                        self.sh_mask[i] = if h & slpy_core::h_flags::DEEP_SHADOW != 0 { 255 } else { 0 };
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
                mask.fill(slpy_core::layer::BASE);
            }
        }

        if self.repaint_full {
            backend.invalidate();
        }
        let t = Instant::now();
        let stats = backend.present(&self.grid);
        self.stage.present += t.elapsed().as_nanos() as u64;
        Ok(stats)
    }
}

/// Fold per-shot p2/p98 NORM levels into a 256-entry LUT (PLAN §3.5:
/// `n = clamp((L − shot_lo) · shot_inv_range)`, rounded). `None` levels or a
/// degenerate span (p98 ≤ p2 — flat shot, or the (0,0) rows of unused plane
/// slots / NORM-less assets) → identity, keeping M0 assets byte-identical.
pub fn build_levels_lut(lut: &mut [u8; 256], levels: Option<PlaneLevels>) {
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
    let lines: [&str; 2] = ["SLEEPYTIME", "enlarge terminal (min 32x9)"];
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(mid.contains("SLEEPYTIME"), "card text missing: {mid:?}");
    }
}
