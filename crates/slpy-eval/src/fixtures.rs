//! Deterministic synthetic fixtures + golden render support (PLAN §6, M2
//! items C/D).
//!
//! Everything committed to the repo (insta cell-grid snapshots, per-tier
//! escape-stream goldens, fuzz drivers) must be reproducible WITHOUT the
//! corpus mp4s. This module is that guarantee: pure integer-math plane
//! generators feed [`slpy_format::SlpyWriter`] in memory — no ffmpeg, no
//! files, no floats, byte-identical on every box.
//!
//! Three fixtures (M2 item C):
//! - [`Fixture::GradientMotion`] — smooth diagonal luma gradient drifting
//!   over time (exercises ramps + box-average downscale on smooth content).
//! - [`Fixture::HardCut`] — two visually distinct scenes with a NORM shot
//!   boundary + CUT flag at [`HARD_CUT_FRAME`] and distinct per-shot levels
//!   (exercises runtime NORM and the cut path).
//! - [`Fixture::CheckerDrift`] — 2-px checkerboard drifting 1 px/frame
//!   (high-frequency content: the resampler's box average must gray it out
//!   rather than alias).
//!
//! [`FixtureRenderer`] replays the player's frame pipeline (decode →
//! resample → per-shot NORM LUT → compose) against these assets using only
//! public slpy-core/slpy-format APIs, mirroring `sleepy-player`'s
//! `compose_cells`/`build_levels_lut` semantics. It is pinned cell-for-cell
//! to the REAL `sleepy_player::pipeline::Player` by
//! `sleepy-player/tests/pipeline_parity.rs` (M2 review fix — the committed
//! goldens transitively cover the shipping renderer through that pin; a
//! divergence fails the parity test, not silently the replica alone).
//! [`snapshot`] is the compact text serialization the
//! insta goldens store: the glyph grid verbatim plus one FNV-1a 64 hash of
//! the fg bytes per row (compact, and a mismatch pinpoints the row).

use std::io::Cursor;

use slpy_core::ramp::{ASCII_BASE_COARSE, ASCII_BASE_FINE, base_ramp_for_cols, ramp_glyph};
use slpy_core::{
    Cell, DEFAULT_CELL_ASPECT, Grid, Resampler, Rgb, Viewport, compute_viewport,
};
use slpy_format::header::plane_id;
use slpy_format::{
    Meta, PlaneLevels, PlaneRef, ShotRecord, SlpyReader, SlpyWriter, WriterOptions, norm_flags,
};

/// Fixture base plane width (16:9 like production 480×270, small enough that
/// three fixtures build in well under a second at zstd-19).
pub const FIXTURE_BASE_W: u16 = 192;
/// See [`FIXTURE_BASE_W`].
pub const FIXTURE_BASE_H: u16 = 108;
/// Frames per fixture (2.4 s @ 30 fps — spans three keyframe groups).
pub const FIXTURE_FRAMES: u32 = 72;
/// Keyframe cadence (smaller than the production 60 so seeks cross
/// keyframe boundaries within 72 frames).
pub const FIXTURE_KEYFRAME_IVL: u8 = 24;
/// First frame of scene B in [`Fixture::HardCut`] (mid-GOP: frame 36 is not
/// a keyframe, so the cut also exercises delta decode across a shot change).
pub const HARD_CUT_FRAME: u32 = 36;

/// The three deterministic synthetic fixtures (M2 item C).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fixture {
    GradientMotion,
    HardCut,
    CheckerDrift,
}

impl Fixture {
    pub const ALL: [Fixture; 3] =
        [Fixture::GradientMotion, Fixture::HardCut, Fixture::CheckerDrift];

    /// Stable kebab-case name (snapshot titles, file names).
    pub fn name(self) -> &'static str {
        match self {
            Fixture::GradientMotion => "gradient-motion",
            Fixture::HardCut => "hard-cut",
            Fixture::CheckerDrift => "checker-drift",
        }
    }
}

/// Triangle wave: 0,1,…,255,255,…,1,0 over a period of 512 — smooth (no
/// sawtooth discontinuity) and pure integer.
fn tri(p: u32) -> u8 {
    let m = p % 512;
    if m < 256 { m as u8 } else { (511 - m) as u8 }
}

/// Y plane (`FIXTURE_BASE_W × FIXTURE_BASE_H` u8) of `frame` — pure function.
pub fn luma_plane(fixture: Fixture, frame: u32) -> Vec<u8> {
    let (w, h) = (u32::from(FIXTURE_BASE_W), u32::from(FIXTURE_BASE_H));
    let mut plane = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let v = match fixture {
                // Smooth diagonal bands drifting 6 tri-steps/frame.
                Fixture::GradientMotion => tri(2 * x + 3 * y + 6 * frame),
                Fixture::HardCut => {
                    if frame < HARD_CUT_FRAME {
                        // Scene A: dark vertical gradient, slight drift.
                        (y * 160 / (h - 1) + x / 24 + frame % 8) as u8
                    } else {
                        // Scene B: bright inverse horizontal gradient.
                        (250 - x * 180 / (w - 1) + (frame - HARD_CUT_FRAME) % 6) as u8
                    }
                }
                // 2-px checkerboard drifting 1 px/frame horizontally,
                // 1 px / 3 frames vertically.
                Fixture::CheckerDrift => {
                    let cx = (x + frame) / 2;
                    let cy = (y + frame / 3) / 2;
                    if (cx + cy).is_multiple_of(2) { 235 } else { 20 }
                }
            };
            plane.push(v);
        }
    }
    plane
}

/// C plane (half res, RGB565 little-endian — the factory contract, PLAN §4)
/// of `frame` — pure function.
pub fn chroma_plane(fixture: Fixture, frame: u32) -> Vec<u8> {
    let (cw, ch) = (u32::from(FIXTURE_BASE_W) / 2, u32::from(FIXTURE_BASE_H) / 2);
    let mut plane = Vec::with_capacity((cw * ch * 2) as usize);
    for cy in 0..ch {
        for cx in 0..cw {
            let (r, g, b): (u32, u32, u32) = match fixture {
                Fixture::GradientMotion => (
                    cx * 255 / (cw - 1),
                    cy * 255 / (ch - 1),
                    u32::from(tri(4 * frame + 2 * cx)),
                ),
                Fixture::HardCut => {
                    if frame < HARD_CUT_FRAME {
                        (40, (60 + cy * 3).min(255), 200) // cool scene A
                    } else {
                        (220, 160 - cy.min(53) * 2, 60 + cx / 2) // warm scene B
                    }
                }
                Fixture::CheckerDrift => {
                    // Same parity as the full-res luma checker at (2cx, 2cy).
                    let px = (2 * cx + frame) / 2;
                    let py = (2 * cy + frame / 3) / 2;
                    if (px + py).is_multiple_of(2) { (60, 200, 90) } else { (180, 60, 170) }
                }
            };
            let v = (((r >> 3) as u16) << 11) | (((g >> 2) as u16) << 5) | ((b >> 3) as u16);
            plane.extend_from_slice(&v.to_le_bytes());
        }
    }
    plane
}

/// NORM shot table per fixture (levels position 0 = Y; position 1 = C stays
/// (0,0) — chroma is never stretched, the factory convention).
pub fn shot_records(fixture: Fixture) -> Vec<ShotRecord> {
    let shot = |first_frame: u32, flags: u8, p2: u8, p98: u8| {
        let mut levels = [PlaneLevels::default(); 8];
        levels[0] = PlaneLevels { p2, p98 };
        ShotRecord { first_frame, flags, levels }
    };
    match fixture {
        Fixture::GradientMotion => vec![shot(0, 0, 8, 248)],
        Fixture::HardCut => vec![
            shot(0, 0, 0, 170),
            shot(HARD_CUT_FRAME, norm_flags::CUT, 64, 255),
        ],
        Fixture::CheckerDrift => vec![shot(0, 0, 16, 240)],
    }
}

/// Build the fixture as a complete in-memory SLPY v1 asset (Y + C planes,
/// temporal delta, keyframe every [`FIXTURE_KEYFRAME_IVL`], zstd-19, CRCs,
/// NORM shot table) — the production writer profile at fixture scale.
/// Deterministic: identical bytes on every call, every box.
pub fn build_fixture(fixture: Fixture) -> Vec<u8> {
    let opts = WriterOptions {
        base_w: FIXTURE_BASE_W,
        base_h: FIXTURE_BASE_H,
        plane_ids: vec![plane_id::Y, plane_id::C],
        keyframe_ivl: FIXTURE_KEYFRAME_IVL,
        ..WriterOptions::default()
    };
    let meta = Meta {
        factory_version: "slpy-eval-fixtures".to_owned(),
        source: fixture.name().to_owned(),
        palette_hints: Vec::new(),
    };
    let mut writer = SlpyWriter::new(Cursor::new(Vec::new()), opts, &meta)
        .expect("fixture writer options are valid");
    writer.write_norm(&shot_records(fixture)).expect("fixture NORM is valid");
    for frame in 0..FIXTURE_FRAMES {
        let y = luma_plane(fixture, frame);
        let c = chroma_plane(fixture, frame);
        writer
            .write_frame(&[
                PlaneRef { id: plane_id::Y, data: &y },
                PlaneRef { id: plane_id::C, data: &c },
            ])
            .expect("fixture frame is valid");
    }
    writer.finish().expect("fixture finish").into_inner()
}

/// Golden palette configurations (M2 item C: ascii-coarse / ascii-fine /
/// mono glyph-only). The mono configuration mirrors the player's Mono-tier
/// path: chroma decode skipped, width-selected base ramp, and the snapshot
/// serializes glyphs only (the Mono painter emits no color SGR at all —
/// PLAN §3.4 palette 8 arrives at M3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoldenPalette {
    AsciiCoarse,
    AsciiFine,
    MonoGlyphOnly,
}

impl GoldenPalette {
    pub const ALL: [GoldenPalette; 3] =
        [GoldenPalette::AsciiCoarse, GoldenPalette::AsciiFine, GoldenPalette::MonoGlyphOnly];

    /// Stable kebab-case name (snapshot titles, file names).
    pub fn name(self) -> &'static str {
        match self {
            GoldenPalette::AsciiCoarse => "ascii-coarse",
            GoldenPalette::AsciiFine => "ascii-fine",
            GoldenPalette::MonoGlyphOnly => "mono",
        }
    }

    /// Glyph-only rendering: no chroma decode, no fg in snapshots.
    pub fn is_glyph_only(self) -> bool {
        self == GoldenPalette::MonoGlyphOnly
    }

    fn ramp(self, viewport_cols: u16) -> &'static [char] {
        match self {
            GoldenPalette::AsciiCoarse => ASCII_BASE_COARSE,
            GoldenPalette::AsciiFine => ASCII_BASE_FINE,
            // Player Mono parity: density picks the ramp (PLAN §3.4).
            GoldenPalette::MonoGlyphOnly => base_ramp_for_cols(viewport_cols),
        }
    }
}

/// Replays the player's frame pipeline against a fixture asset using public
/// APIs only: decode (sequential delta roll / FIDX seek) → shared separable
/// resample → per-shot NORM LUT → compose (ramp glyph + chroma or gray fg,
/// BLANK pads). Buffers reallocate only in [`reflow`](FixtureRenderer::reflow)
/// — render loops are allocation-free, like the player's.
///
/// Test support for goldens and fuzzing: invalid fixture assets panic.
pub struct FixtureRenderer<'a> {
    reader: SlpyReader<'a>,
    palette: GoldenPalette,
    frame_count: u32,
    src_w: u16,
    src_h: u16,
    chroma_dims: Option<(u16, u16)>,
    use_chroma: bool,
    vp: Option<Viewport>,
    resampler: Option<Resampler>,
    chroma_resampler: Option<Resampler>,
    luma_src: Vec<u8>,
    luma_dst: Vec<u8>,
    chroma_src: Vec<u8>,
    cr_src: Vec<u8>,
    cg_src: Vec<u8>,
    cb_src: Vec<u8>,
    cr_dst: Vec<u8>,
    cg_dst: Vec<u8>,
    cb_dst: Vec<u8>,
    levels_lut: [u8; 256],
    lut_shot: Option<u32>,
    loaded: Option<u32>,
    grid: Grid<Cell>,
}

impl<'a> FixtureRenderer<'a> {
    /// Open `asset` (SLPY bytes) for rendering with `palette`. Call
    /// [`reflow`](FixtureRenderer::reflow) before the first render.
    pub fn new(asset: &'a [u8], palette: GoldenPalette) -> FixtureRenderer<'a> {
        let reader = SlpyReader::open(asset).expect("fixture asset must be a valid SLPY");
        let (src_w, src_h) = reader.plane_dims(plane_id::Y).expect("fixture has a Y plane");
        let frame_count = reader.frame_count();
        assert!(frame_count > 0, "fixture asset has zero frames");
        let chroma_dims = reader.plane_dims(plane_id::C);
        let use_chroma = !palette.is_glyph_only() && chroma_dims.is_some();
        let chroma_len = chroma_dims.map_or(0, |(w, h)| w as usize * h as usize);
        let mut this = FixtureRenderer {
            reader,
            palette,
            frame_count,
            src_w,
            src_h,
            chroma_dims,
            use_chroma,
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
            levels_lut: [0; 256],
            lut_shot: None,
            loaded: None,
            grid: Grid::new(0, 0),
        };
        build_levels_lut(&mut this.levels_lut, None); // identity until NORM
        this
    }

    /// Resize path (player `reflow` parity, PLAN §3.6 step 1): grid realloc,
    /// viewport recompute at cell aspect 2.0, resampler tap rebuilds. Does
    /// NOT touch any backend — callers pair this with `Backend::resize` +
    /// `invalidate` themselves (the fuzz driver asserts that pairing).
    pub fn reflow(&mut self, cols: u16, rows: u16) {
        self.grid.resize(cols, rows);
        self.vp = compute_viewport(cols, rows, DEFAULT_CELL_ASPECT);
        if let Some(vp) = self.vp {
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
            self.resampler = None;
            self.chroma_resampler = None;
        }
    }

    /// Viewport chosen by the last [`reflow`](FixtureRenderer::reflow)
    /// (`None` below the 32×9 minimum).
    pub fn viewport(&self) -> Option<Viewport> {
        self.vp
    }

    /// `(src_dims, dst_dims)` of the current luma resampler — the fuzz
    /// invariant "tap tables realloc'd consistently" reads this.
    pub fn resampler_dims(&self) -> Option<((u16, u16), (u16, u16))> {
        self.resampler.as_ref().map(|r| (r.src_dims(), r.dst_dims()))
    }

    pub fn frame_count(&self) -> u32 {
        self.frame_count
    }

    /// The term-sized grid of the last render.
    pub fn grid(&self) -> &Grid<Cell> {
        &self.grid
    }

    /// Decode → resample → NORM → compose `frame` into the grid and return
    /// it. Sequential successors roll one delta (`decode_plane_into`);
    /// everything else uses the FIDX seek path (`seek_plane_into`) — the
    /// INTERFACES decode-policy contract for delta assets. Below the 32×9
    /// minimum the grid is all [`Cell::BLANK`] (the enlarge card is player
    /// UI, not pipeline).
    pub fn render(&mut self, frame: u32) -> &Grid<Cell> {
        assert!(frame < self.frame_count, "frame {frame} out of range");
        let (Some(vp), true) = (self.vp, self.resampler.is_some()) else {
            self.grid.fill(Cell::BLANK);
            return &self.grid;
        };
        self.load(frame);
        self.update_levels(frame);

        let resampler = self.resampler.as_mut().expect("checked above");
        resampler.apply(&self.luma_src, &mut self.luma_dst);
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

        // Compose (player `compose_cells` parity): ramp glyph from normalized
        // luma, chroma fg on color palettes / gray fg otherwise, BLANK pads.
        let ramp = self.palette.ramp(vp.cols);
        let (vc, vr) = (vp.cols as usize, vp.rows as usize);
        self.grid.fill(Cell::BLANK);
        let pad_left = vp.pad_left as usize;
        for row in 0..vr {
            let base = row * vc;
            let src = &self.luma_dst[base..base + vc];
            let drow =
                &mut self.grid.row_mut(vp.pad_top + row as u16)[pad_left..pad_left + vc];
            for (i, (cell, &n)) in drow.iter_mut().zip(src).enumerate() {
                let fg = if self.use_chroma {
                    Rgb::new(self.cr_dst[base + i], self.cg_dst[base + i], self.cb_dst[base + i])
                } else {
                    Rgb::gray(n)
                };
                *cell = Cell::new(ramp_glyph(ramp, n), fg, Rgb::BLACK);
            }
        }
        &self.grid
    }

    /// Player `load_frame` parity: sequential → delta roll, else FIDX seek.
    fn load(&mut self, frame: u32) {
        if self.loaded == Some(frame) {
            return;
        }
        let sequential = frame > 0 && self.loaded == Some(frame - 1);
        if sequential {
            self.reader
                .decode_plane_into(frame, plane_id::Y, &mut self.luma_src)
                .expect("fixture decode");
            if self.use_chroma {
                self.reader
                    .decode_plane_into(frame, plane_id::C, &mut self.chroma_src)
                    .expect("fixture chroma decode");
            }
        } else {
            self.reader
                .seek_plane_into(frame, plane_id::Y, &mut self.luma_src)
                .expect("fixture seek");
            if self.use_chroma {
                self.reader
                    .seek_plane_into(frame, plane_id::C, &mut self.chroma_src)
                    .expect("fixture chroma seek");
            }
        }
        self.loaded = Some(frame);
    }

    /// Player `update_levels` parity: rebuild the LUT only on shot change.
    fn update_levels(&mut self, frame: u32) {
        let shot = self.reader.shot_for_frame(frame).map(|s| s.first_frame);
        if shot != self.lut_shot {
            build_levels_lut(&mut self.levels_lut, self.reader.norm_levels(frame, plane_id::Y));
            self.lut_shot = shot;
        }
    }
}

/// Player `build_levels_lut` parity (PLAN §3.5): `n = clamp((L − p2) ·
/// inv_range)`, rounded; `None`/degenerate spans → identity.
fn build_levels_lut(lut: &mut [u8; 256], levels: Option<PlaneLevels>) {
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

/// Player `unpack_rgb565` parity: little-endian RGB565 → 8-bit channel
/// planes with canonical bit-replicating expansion.
fn unpack_rgb565(src: &[u8], r: &mut [u8], g: &mut [u8], b: &mut [u8]) {
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

/// FNV-1a 64 (the classic offset/prime constants) — dependency-free row
/// digest for fg bytes in snapshots.
fn fnv1a64(bytes: impl IntoIterator<Item = u8>) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Compact text serialization for the insta cell-grid goldens (M2 item C):
/// a small header, the glyph grid verbatim (rows framed in `|…|` so
/// trailing-space cells survive editors and diff tools), and — unless the
/// palette is glyph-only — one FNV-1a 64 digest of the row's fg `(r,g,b)`
/// bytes per row. Any fg regression pinpoints its row; the glyph grid stays
/// human-reviewable.
pub fn snapshot(
    title: &str,
    term: (u16, u16),
    palette: GoldenPalette,
    vp: Option<Viewport>,
    grid: &Grid<Cell>,
) -> String {
    let mut s = String::new();
    s.push_str(title);
    s.push('\n');
    match vp {
        Some(v) => {
            s.push_str(&format!(
                "term: {}x{}  viewport: {}x{}  pads: L{} R{} T{} B{}\n",
                term.0, term.1, v.cols, v.rows, v.pad_left, v.pad_right, v.pad_top, v.pad_bottom
            ));
        }
        None => s.push_str(&format!("term: {}x{}  viewport: none\n", term.0, term.1)),
    }
    s.push_str(&format!(
        "palette: {}  fg: {}\n",
        palette.name(),
        if palette.is_glyph_only() { "none (glyph-only)" } else { "chroma" }
    ));
    let frame_line = |s: &mut String| {
        s.push('+');
        for _ in 0..grid.cols() {
            s.push('-');
        }
        s.push_str("+\n");
    };
    frame_line(&mut s);
    for row in 0..grid.rows() {
        s.push('|');
        for cell in grid.row(row) {
            s.push(cell.glyph());
        }
        s.push_str("|\n");
    }
    frame_line(&mut s);
    if !palette.is_glyph_only() {
        s.push_str("fg-fnv64 per row:\n");
        for row in 0..grid.rows() {
            let h = fnv1a64(grid.row(row).iter().flat_map(|c| [c.fg.r, c.fg.g, c.fg.b]));
            s.push_str(&format!("{row:3} {h:016x}\n"));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planes_have_contract_sizes() {
        for f in Fixture::ALL {
            let y = luma_plane(f, 0);
            let c = chroma_plane(f, 0);
            assert_eq!(y.len(), FIXTURE_BASE_W as usize * FIXTURE_BASE_H as usize, "{f:?}");
            assert_eq!(
                c.len(),
                (FIXTURE_BASE_W as usize / 2) * (FIXTURE_BASE_H as usize / 2) * 2,
                "{f:?}"
            );
        }
    }

    /// The committed-golden precondition: building a fixture twice yields
    /// byte-identical SLPY assets (writer determinism is already a §4 gate;
    /// this pins the generators too).
    #[test]
    fn build_is_deterministic() {
        for f in Fixture::ALL {
            assert_eq!(build_fixture(f), build_fixture(f), "{f:?}");
        }
    }

    #[test]
    fn hard_cut_has_cut_flag_and_distinct_scenes() {
        let shots = shot_records(Fixture::HardCut);
        assert_eq!(shots.len(), 2);
        assert!(!shots[0].is_cut());
        assert!(shots[1].is_cut());
        assert_eq!(shots[1].first_frame, HARD_CUT_FRAME);
        // Scenes are visually far apart at plane level.
        let a = luma_plane(Fixture::HardCut, HARD_CUT_FRAME - 1);
        let b = luma_plane(Fixture::HardCut, HARD_CUT_FRAME);
        let mean = |p: &[u8]| p.iter().map(|&v| u32::from(v)).sum::<u32>() / p.len() as u32;
        assert!(mean(&b) > mean(&a) + 60, "cut must be a big luma jump");
    }

    #[test]
    fn renderer_smoke_and_pads() {
        let asset = build_fixture(Fixture::GradientMotion);
        let mut r = FixtureRenderer::new(&asset, GoldenPalette::AsciiCoarse);
        r.reflow(80, 24);
        let vp = r.viewport().expect("80x24 has a viewport");
        assert_eq!((vp.cols, vp.rows), (80, 23)); // PLAN §3.2 worked example
        let grid = r.render(10);
        assert_eq!((grid.cols(), grid.rows()), (80, 24));
        // Bottom pad row is BLANK, viewport rows are not all blank.
        assert!(grid.row(23).iter().all(|c| *c == Cell::BLANK));
        assert!(grid.row(5).iter().any(|c| c.glyph() != ' '));
        // Chroma palette: fg is not the gray of its own luma everywhere.
        assert!(grid.row(5).iter().any(|c| {
            c.fg.r != c.fg.g || c.fg.g != c.fg.b
        }));
    }

    #[test]
    fn renderer_below_minimum_is_blank() {
        let asset = build_fixture(Fixture::CheckerDrift);
        let mut r = FixtureRenderer::new(&asset, GoldenPalette::MonoGlyphOnly);
        r.reflow(10, 4);
        assert!(r.viewport().is_none());
        let grid = r.render(0);
        assert!(grid.as_slice().iter().all(|c| *c == Cell::BLANK));
    }

    #[test]
    fn fnv_vector() {
        // Known FNV-1a 64 test vector: "a" → 0xaf63dc4c8601ec8c.
        assert_eq!(fnv1a64(*b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
