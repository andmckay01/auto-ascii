//! The frame pipeline: decode → resample → NORM levels → compose → present
//! (PLAN §3.4–§3.6). Extracted from the binary at M2 (item B decision) so
//! `sleepy-factory eval` drives the EXACT player code path headlessly
//! against `SimBackend` — metrics measure the real renderer, not a
//! reimplementation. The binary's event loop, pacing and CLI stay in
//! `main.rs`; nothing here touches a clock or a tty.

use std::time::Instant;

use anyhow::{Context, Result, bail};
use slpy_core::ramp::{base_ramp_for_cols, ramp_glyph};
use slpy_core::{Cell, Grid, Resampler, Rgb, Viewport, compute_viewport};
use slpy_format::header::plane_id;
use slpy_format::{PlaneLevels, SlpyReader};
use slpy_term::{Backend, Event, FrameStats, Key};

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
    cell_aspect: f64,
    repaint_full: bool,
    ramp: &'static [char],
    vp: Option<Viewport>,
    resampler: Option<Resampler>,
    chroma_resampler: Option<Resampler>,
    /// Decoded Y plane, `src_w × src_h` — the standing delta double buffer.
    luma_src: Vec<u8>,
    /// Resampled luma, `vp.cols × vp.rows`.
    luma_dst: Vec<u8>,
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
    /// Frame currently decoded in `luma_src`/`chroma_src` — drives the
    /// sequential-roll vs FIDX-seek decode policy on delta assets.
    loaded: Option<u32>,
    /// Full terminal grid (viewport + letterbox pads).
    grid: Grid<Cell>,
    stage: StageNs,
}

impl<'a> Player<'a> {
    pub fn new(
        reader: SlpyReader<'a>,
        cell_aspect: f64,
        repaint_full: bool,
        want_color: bool,
    ) -> Result<Player<'a>> {
        let (src_w, src_h) = reader
            .plane_dims(plane_id::Y)
            .context("asset has no Y (luma) plane")?;
        let frame_count = reader.frame_count();
        if frame_count == 0 {
            bail!("asset has zero frames");
        }
        let chroma_dims = reader.plane_dims(plane_id::C);
        let use_chroma = want_color && chroma_dims.is_some();
        let chroma_len = chroma_dims.map_or(0, |(w, h)| w as usize * h as usize);
        let mut levels_lut = [0u8; 256];
        build_levels_lut(&mut levels_lut, None); // identity until NORM says otherwise
        Ok(Player {
            reader,
            frame_count,
            src_w,
            src_h,
            chroma_dims,
            use_chroma,
            cell_aspect,
            repaint_full,
            ramp: base_ramp_for_cols(0),
            vp: None,
            resampler: None,
            chroma_resampler: None,
            luma_src: vec![0; src_w as usize * src_h as usize],
            luma_dst: Vec::new(),
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
            stage: StageNs::default(),
        })
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

    /// `(src_dims, dst_dims)` of the current luma resampler (`None` below
    /// the viewport minimum) — the §6 resize-fuzz invariant "tap tables
    /// realloc'd consistently" asserts this against the viewport (M2 review
    /// fix: the fuzzer drives THIS player, so the accessor lives here).
    pub fn resampler_dims(&self) -> Option<((u16, u16), (u16, u16))> {
        self.resampler.as_ref().map(|r| (r.src_dims(), r.dst_dims()))
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
    /// recompute, resampler tap rebuilds, invalidate. The next rendered frame
    /// lands on the new grid (M0 acceptance 2).
    pub fn reflow<B: Backend>(&mut self, backend: &mut B, cols: u16, rows: u16) {
        backend.resize(cols, rows);
        self.grid.resize(cols, rows);
        self.vp = compute_viewport(cols, rows, self.cell_aspect);
        if let Some(vp) = self.vp {
            self.ramp = base_ramp_for_cols(vp.cols);
            self.resampler = Some(Resampler::build(self.src_w, self.src_h, vp.cols, vp.rows));
            let cells = vp.cols as usize * vp.rows as usize;
            self.luma_dst.resize(cells, 0);
            if self.use_chroma {
                let (cw, ch) = self.chroma_dims.expect("use_chroma implies C dims");
                self.chroma_resampler = Some(Resampler::build(cw, ch, vp.cols, vp.rows));
                self.cr_dst.resize(cells, 0);
                self.cg_dst.resize(cells, 0);
                self.cb_dst.resize(cells, 0);
            }
        } else {
            // Below 32x9 (PLAN §3.2): "enlarge terminal" card until regrown.
            self.resampler = None;
            self.chroma_resampler = None;
        }
        backend.invalidate();
    }

    /// Drain the event queue (PLAN §3.6 step 1): Quit wins, resizes coalesce
    /// to the latest and trigger one reflow, digit keys report a jump.
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
            self.reflow(backend, c, r);
        }
        Drained { quit: false, jump_digit }
    }

    /// Bring `luma_src` (+ `chroma_src`) to `frame_idx`. Sequential
    /// successors roll one delta via `decode_plane_into` (the standing
    /// double buffer, PLAN §3.6 step 3); anything else — startup, `--seek`,
    /// digit jumps, latest-frame-wins skips, loop wrap — goes through
    /// `seek_plane_into` (FIDX keyframe bsearch + delta rolls, PLAN §4).
    /// Frame-skipping on delta assets MUST NOT use plain decode (INTERFACES).
    fn load_frame(&mut self, frame_idx: u32) -> Result<()> {
        if self.loaded == Some(frame_idx) {
            return Ok(()); // paced repeat: planes already current
        }
        let sequential = frame_idx > 0 && self.loaded == Some(frame_idx - 1);
        if sequential {
            self.reader
                .decode_plane_into(frame_idx, plane_id::Y, &mut self.luma_src)
                .with_context(|| format!("decoding frame {frame_idx}"))?;
            if self.use_chroma {
                self.reader
                    .decode_plane_into(frame_idx, plane_id::C, &mut self.chroma_src)
                    .with_context(|| format!("decoding chroma of frame {frame_idx}"))?;
            }
        } else {
            self.reader
                .seek_plane_into(frame_idx, plane_id::Y, &mut self.luma_src)
                .with_context(|| format!("seeking to frame {frame_idx}"))?;
            if self.use_chroma {
                self.reader
                    .seek_plane_into(frame_idx, plane_id::C, &mut self.chroma_src)
                    .with_context(|| format!("seeking chroma to frame {frame_idx}"))?;
            }
        }
        self.loaded = Some(frame_idx);
        Ok(())
    }

    /// Rebuild the levels LUT iff `frame_idx` entered a different shot
    /// (PLAN §3.5 per-shot auto-levels from NORM — per-frame rebuilds would
    /// pump; per-shot is the contract). Assets without NORM keep identity.
    fn update_levels(&mut self, frame_idx: u32) {
        let shot = self.reader.shot_for_frame(frame_idx).map(|s| s.first_frame);
        if shot != self.lut_shot {
            build_levels_lut(&mut self.levels_lut, self.reader.norm_levels(frame_idx, plane_id::Y));
            self.lut_shot = shot;
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
            resampler.apply(&self.luma_src, &mut self.luma_dst);
            // Runtime per-shot normalization (M1: replaces M0's baked-in
            // stretch). Applied post-resample: the map is monotone linear,
            // so order is equivalent — and 24k lookups beat 130k.
            for v in &mut self.luma_dst {
                *v = self.levels_lut[*v as usize];
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
            let chroma = self
                .use_chroma
                .then(|| (&self.cr_dst[..], &self.cg_dst[..], &self.cb_dst[..]));
            compose_cells(&self.luma_dst, chroma, &vp, self.ramp, &mut self.grid);
            self.stage.compose += t.elapsed().as_nanos() as u64;
        } else {
            draw_enlarge_card(&mut self.grid);
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

/// Fill `out` from normalized luma + optional resampled chroma channels,
/// letterboxed per `vp` (PLAN §3.4/§3.5): base ramp glyph from luma, fg
/// sampled from the chroma plane (area-resampled) on color tiers, gray fg
/// fallback for luma-only assets / mono. [`Cell::BLANK`] pads. Never
/// allocates; `out` must already be term-grid-sized (PLAN §6 discipline).
pub fn compose_cells(
    luma: &[u8],
    chroma: Option<(&[u8], &[u8], &[u8])>,
    vp: &Viewport,
    ramp: &[char],
    out: &mut Grid<Cell>,
) {
    let vc = vp.cols as usize;
    let vr = vp.rows as usize;
    assert_eq!(out.cols(), vp.cols + vp.pad_left + vp.pad_right, "grid cols != viewport + pads");
    assert_eq!(out.rows(), vp.rows + vp.pad_top + vp.pad_bottom, "grid rows != viewport + pads");
    assert!(luma.len() >= vc * vr, "luma plane smaller than viewport");
    if let Some((r, g, b)) = chroma {
        assert!(r.len() >= vc * vr && g.len() >= vc * vr && b.len() >= vc * vr);
    }
    assert!(!ramp.is_empty(), "empty ramp");

    out.fill(Cell::BLANK);
    let pad_left = vp.pad_left as usize;
    for row in 0..vr {
        let base = row * vc;
        let src = &luma[base..base + vc];
        let drow = &mut out.row_mut(vp.pad_top + row as u16)[pad_left..pad_left + vc];
        for (i, (cell, &n)) in drow.iter_mut().zip(src).enumerate() {
            let fg = match chroma {
                Some((r, g, b)) => Rgb::new(r[base + i], g[base + i], b[base + i]),
                None => Rgb::gray(n),
            };
            *cell = Cell::new(ramp_glyph(ramp, n), fg, Rgb::BLACK);
        }
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
    fn compose_cells_chroma_fg_and_blank_pads() {
        let vp = compute_viewport(213, 58, 2.0).unwrap(); // 206x58, pads 3/4
        let cells = vp.cols as usize * vp.rows as usize;
        let luma = vec![128u8; cells];
        let (r, g, b) = (vec![200u8; cells], vec![10u8; cells], vec![30u8; cells]);
        let mut grid: Grid<Cell> = Grid::new(213, 58);
        compose_cells(
            &luma,
            Some((&r, &g, &b)),
            &vp,
            base_ramp_for_cols(vp.cols),
            &mut grid,
        );
        assert_eq!(grid.get(0, 0), Cell::BLANK, "left pad blank");
        let c = grid.get(vp.pad_left, 0);
        assert_eq!(c.fg, Rgb::new(200, 10, 30), "fg from chroma, not gray");
        assert_eq!(c.bg, Rgb::BLACK);
        // Luma-only fallback keeps the M0 gray fg.
        let mut grid2: Grid<Cell> = Grid::new(213, 58);
        compose_cells(&luma, None, &vp, base_ramp_for_cols(vp.cols), &mut grid2);
        assert_eq!(grid2.get(vp.pad_left, 0).fg, Rgb::gray(128));
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
