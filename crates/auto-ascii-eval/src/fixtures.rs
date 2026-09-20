//! Deterministic synthetic fixtures + golden render support (PLAN §6, M2
//! items C/D).
//!
//! Everything committed to the repo (insta cell-grid snapshots, per-tier
//! escape-stream goldens, fuzz drivers) must be reproducible WITHOUT the
//! corpus mp4s. This module is that guarantee: pure integer-math plane
//! generators feed [`auto_ascii_format::AsciiWriter`] in memory — no ffmpeg, no
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
//! resample at Vc×2Vr → per-shot NORM LUT → the §3.5 three-layer
//! `compose_frame`) against these assets using only public
//! auto-ascii-core/auto-ascii-format APIs. It is pinned cell-for-cell
//! to the REAL `auto_ascii::pipeline::Player` by
//! `auto-ascii/tests/pipeline_parity.rs` (M2 review fix — the committed
//! goldens transitively cover the shipping renderer through that pin; a
//! divergence fails the parity test, not silently the replica alone).
//! [`snapshot`] is the compact text serialization the
//! insta goldens store: the glyph grid verbatim plus one FNV-1a 64 hash of
//! the fg bytes per row (compact, and a mismatch pinpoints the row).
//!
//! [`write_bgr24_avi`] (M7) is the mirror image of all of the above: an
//! INPUT fixture — uncompressed video the factory's ffmpeg ingest can read
//! — rather than an asset. See its docs for why it is written from Rust.

use std::io::Cursor;

use auto_ascii_core::{
    Cell, ColorDepth, ComposeParams, DEFAULT_CELL_ASPECT, FramePlanes, GlyphTier, Grid,
    HysteresisState, PaletteSet, Resampler, Viewport, compose_frame, compute_viewport,
    select_palettes,
};
use auto_ascii_format::header::plane_id;
use auto_ascii_format::{
    Meta, PlaneLevels, PlaneRef, ShotRecord, AsciiReader, AsciiWriter, WriterOptions, norm_flags,
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

/// Build the fixture as a complete in-memory ASCI v1 asset (Y + C planes,
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
        factory_version: "auto-ascii-eval-fixtures".to_owned(),
        source: fixture.name().to_owned(),
        palette_hints: Vec::new(),
    };
    let mut writer = AsciiWriter::new(Cursor::new(Vec::new()), opts, &meta)
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

/// Golden palette configurations. M3: keyed exactly like the player —
/// (charset tier × color depth); the density band and the per-tier ramp
/// caps fall out of `select_palettes` at reflow, so `ascii` covers both the
/// coarse and fine ramps across the golden grid sweep. `mono` mirrors the
/// player's Mono-tier path: chroma decode skipped, palette 8 base, and the
/// snapshot serializes glyphs only (the Mono painter emits no color SGR).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoldenPalette {
    /// `GlyphTier::Ascii` × truecolor (palettes 1/2 base + subposition).
    Ascii,
    /// `GlyphTier::UnicodeBlocks` × truecolor (palette 5 + half-blocks).
    Unicode,
    /// `GlyphTier::Ascii` × mono (palette 8, glyph-only serialization).
    MonoGlyphOnly,
}

impl GoldenPalette {
    pub const ALL: [GoldenPalette; 3] =
        [GoldenPalette::Ascii, GoldenPalette::Unicode, GoldenPalette::MonoGlyphOnly];

    /// Stable kebab-case name (snapshot titles, file names).
    pub fn name(self) -> &'static str {
        match self {
            GoldenPalette::Ascii => "ascii",
            GoldenPalette::Unicode => "unicode",
            GoldenPalette::MonoGlyphOnly => "mono",
        }
    }

    /// Glyph-only rendering: no chroma decode, no fg in snapshots.
    pub fn is_glyph_only(self) -> bool {
        self == GoldenPalette::MonoGlyphOnly
    }

    /// The player-shaped palette-selection inputs (PLAN §3.4 key).
    pub fn config(self) -> (GlyphTier, ColorDepth) {
        match self {
            GoldenPalette::Ascii => (GlyphTier::Ascii, ColorDepth::True),
            GoldenPalette::Unicode => (GlyphTier::UnicodeBlocks, ColorDepth::True),
            GoldenPalette::MonoGlyphOnly => (GlyphTier::Ascii, ColorDepth::Mono),
        }
    }
}

/// Replays the player's frame pipeline against a fixture asset using public
/// APIs only: decode (sequential delta roll / FIDX seek) → shared separable
/// resample (luma at Vc×2Vr, §3.3) → per-shot NORM LUT (with the M3
/// shot-change hysteresis reset) → the §3.5 three-layer `compose_frame`.
/// Buffers reallocate only in [`reflow`](FixtureRenderer::reflow) — render
/// loops are allocation-free, like the player's. Fixture assets carry Y+C
/// only, so the edge/highlight layers compose auto-disabled — exactly the
/// PLAN §4 back-compat path the goldens must pin.
///
/// Test support for goldens and fuzzing: invalid fixture assets panic.
pub struct FixtureRenderer<'a> {
    reader: AsciiReader<'a>,
    palette: GoldenPalette,
    frame_count: u32,
    src_w: u16,
    src_h: u16,
    chroma_dims: Option<(u16, u16)>,
    use_chroma: bool,
    vp: Option<Viewport>,
    palette_set: Option<PaletteSet>,
    state: HysteresisState,
    resampler: Option<Resampler>,
    chroma_resampler: Option<Resampler>,
    luma_src: Vec<u8>,
    /// Resampled luma at Vc × 2Vr (player parity, §3.3).
    luma2_dst: Vec<u8>,
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
    /// Open `asset` (ASCI bytes) for rendering with `palette`. Call
    /// [`reflow`](FixtureRenderer::reflow) before the first render.
    pub fn new(asset: &'a [u8], palette: GoldenPalette) -> FixtureRenderer<'a> {
        let reader = AsciiReader::open(asset).expect("fixture asset must be a valid ASCI");
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
            palette_set: None,
            state: HysteresisState::new(0, 0),
            resampler: None,
            chroma_resampler: None,
            luma_src: vec![0; src_w as usize * src_h as usize],
            luma2_dst: Vec::new(),
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
    /// viewport recompute at cell aspect 2.0, palette reselection, resampler
    /// tap rebuilds (luma at 2× vertical), hysteresis realloc+reset. Does
    /// NOT touch any backend — callers pair this with `Backend::resize` +
    /// `invalidate` themselves (the fuzz driver asserts that pairing).
    pub fn reflow(&mut self, cols: u16, rows: u16) {
        self.grid.resize(cols, rows);
        self.vp = compute_viewport(cols, rows, DEFAULT_CELL_ASPECT);
        if let Some(vp) = self.vp {
            let (tier, depth) = self.palette.config();
            self.palette_set = Some(select_palettes(tier, depth, vp.cols));
            self.resampler = Some(Resampler::build(self.src_w, self.src_h, vp.cols, 2 * vp.rows));
            self.state.resize(vp.cols, vp.rows);
            let cells = vp.cols as usize * vp.rows as usize;
            self.luma2_dst.resize(cells * 2, 0);
            if self.use_chroma {
                let (cw, ch) = self.chroma_dims.expect("use_chroma implies C dims");
                self.chroma_resampler = Some(Resampler::build(cw, ch, vp.cols, vp.rows));
                self.cr_dst.resize(cells, 0);
                self.cg_dst.resize(cells, 0);
                self.cb_dst.resize(cells, 0);
            }
        } else {
            self.palette_set = None;
            self.resampler = None;
            self.chroma_resampler = None;
            self.state.resize(0, 0);
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
        resampler.apply(&self.luma_src, &mut self.luma2_dst);
        if self.use_chroma {
            unpack_rgb565(&self.chroma_src, &mut self.cr_src, &mut self.cg_src, &mut self.cb_src);
            let cres = self.chroma_resampler.as_mut().expect("use_chroma implies resampler");
            cres.apply(&self.cr_src, &mut self.cr_dst);
            cres.apply(&self.cg_src, &mut self.cg_dst);
            cres.apply(&self.cb_src, &mut self.cb_dst);
        }

        // Compose (player parity): the §3.5 three-layer path with the NORM
        // LUT applied per tap inside compose_cell; Y+C fixtures compose
        // with edge/highlight auto-disabled (E/Ex/Ey/H = None, PLAN §4).
        let set = self.palette_set.as_ref().expect("viewport implies palette");
        let planes = FramePlanes {
            luma2: &self.luma2_dst,
            e: None,
            ex: None,
            ey: None,
            h: None,
            chroma: self
                .use_chroma
                .then(|| (&self.cr_dst[..], &self.cg_dst[..], &self.cb_dst[..])),
        };
        compose_frame(
            &planes,
            &vp,
            &self.levels_lut,
            set,
            &ComposeParams::default(),
            &mut self.state,
            &mut self.grid,
        );
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

    /// Player `update_levels` parity: rebuild the LUT only on shot change —
    /// and (M3) reset ALL hysteresis state with it: a changed LUT makes
    /// every remembered ramp index stale, and every §3.5 CUT boundary is a
    /// shot change, so the scene-cut reset is covered exactly.
    fn update_levels(&mut self, frame: u32) {
        let shot = self.reader.shot_for_frame(frame).map(|s| s.first_frame);
        if shot != self.lut_shot {
            build_levels_lut(&mut self.levels_lut, self.reader.norm_levels(frame, plane_id::Y));
            self.lut_shot = shot;
            self.state.reset();
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

// ---------------------------------------------------------------------------
// Raw BGR24 AVI input fixtures (M7, PLAN-M6-M8 §2)
// ---------------------------------------------------------------------------
//
// The rest of this module writes ASCI assets — the factory's OUTPUT. This
// section writes the factory's INPUT: a minimal RIFF AVI of uncompressed
// 24-bit BGR frames, so a test can exercise the whole ffmpeg ingest without
// the corpus and without depending on how any particular ffmpeg build
// renders and encodes a lavfi source. It is the one file-writing helper in
// an otherwise I/O-free crate, and it exists so the factory's byte-pinned
// determinism guard and `auto-ascii import`'s integration tests share ONE
// container writer (a second copy would drift and silently move the pin).

/// Little-endian `u32` into a byte buffer.
fn le32(out: &mut Vec<u8>, n: u32) {
    out.extend_from_slice(&n.to_le_bytes());
}

/// Little-endian `u16` into a byte buffer.
fn le16(out: &mut Vec<u8>, n: u16) {
    out.extend_from_slice(&n.to_le_bytes());
}

/// `fourcc` + little-endian payload size + payload.
fn riff_chunk(out: &mut Vec<u8>, fourcc: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(fourcc);
    le32(out, payload.len() as u32);
    out.extend_from_slice(payload);
}

/// Write `frames` as a minimal RIFF AVI of uncompressed 24-bit BGR
/// (`BI_RGB`) video at `width`x`height`, `fps`/1.
///
/// Each item of `frames` is ONE frame already in DIB order: rows
/// **bottom-up** (last image row first), pixels **B,G,R** — exactly the
/// bytes that land in its `00db` chunk. `width` must be a multiple of 4 so
/// the row stride is 4-byte aligned (no DIB row padding) and every chunk is
/// even-sized (no RIFF pad byte), which is what keeps this writer a
/// straight-line byte layout.
///
/// Every ffmpeg build decodes these frames identically — there is no codec,
/// no colour conversion beyond a byte permutation and no scaler in the way
/// — which is the whole point: `auto-ascii-factory`'s determinism guard
/// pins the sha256 of this file (`FIXTURE_AVI_SHA`), and an ffmpeg upgrade
/// must not move it.
pub fn write_bgr24_avi(
    path: &std::path::Path,
    width: u32,
    height: u32,
    fps: u32,
    frames: impl Iterator<Item = Vec<u8>>,
) -> std::io::Result<()> {
    if width == 0 || height == 0 || !width.is_multiple_of(4) || fps == 0 {
        return Err(std::io::Error::other(format!(
            "write_bgr24_avi: bad geometry {width}x{height} @ {fps} fps \
             (width must be a nonzero multiple of 4)"
        )));
    }
    let frame_bytes = width * height * 3;

    // movi: one `00db` chunk per frame, each with an idx1 entry. idx1
    // offsets are relative to the `movi` fourcc itself (4 for the first).
    let mut movi = Vec::new();
    movi.extend_from_slice(b"movi");
    let mut idx1 = Vec::new();
    let mut frame_count = 0u32;
    for frame in frames {
        if frame.len() != frame_bytes as usize {
            return Err(std::io::Error::other(format!(
                "write_bgr24_avi: frame {frame_count} is {} bytes, expected {frame_bytes}",
                frame.len()
            )));
        }
        let offset = movi.len() as u32;
        riff_chunk(&mut movi, b"00db", &frame);
        idx1.extend_from_slice(b"00db");
        le32(&mut idx1, 0x10); // dwFlags = AVIIF_KEYFRAME
        le32(&mut idx1, offset);
        le32(&mut idx1, frame_bytes);
        frame_count += 1;
    }
    if frame_count == 0 {
        return Err(std::io::Error::other("write_bgr24_avi: no frames"));
    }

    // hdrl { avih, LIST strl { strh, strf } }.
    let mut avih = Vec::with_capacity(56); // MainAVIHeader
    le32(&mut avih, 1_000_000 / fps); // dwMicroSecPerFrame
    le32(&mut avih, frame_bytes * fps); // dwMaxBytesPerSec
    le32(&mut avih, 0); // dwPaddingGranularity
    le32(&mut avih, 0x10); // dwFlags = AVIF_HASINDEX
    le32(&mut avih, frame_count); // dwTotalFrames
    le32(&mut avih, 0); // dwInitialFrames
    le32(&mut avih, 1); // dwStreams
    le32(&mut avih, frame_bytes); // dwSuggestedBufferSize
    le32(&mut avih, width); // dwWidth
    le32(&mut avih, height); // dwHeight
    for _ in 0..4 {
        le32(&mut avih, 0); // dwReserved[4]
    }

    let mut strh = Vec::with_capacity(56); // AVIStreamHeader
    strh.extend_from_slice(b"vids"); // fccType
    strh.extend_from_slice(b"DIB "); // fccHandler = uncompressed DIB
    le32(&mut strh, 0); // dwFlags
    le16(&mut strh, 0); // wPriority
    le16(&mut strh, 0); // wLanguage
    le32(&mut strh, 0); // dwInitialFrames
    le32(&mut strh, 1); // dwScale
    le32(&mut strh, fps); // dwRate => fps/1
    le32(&mut strh, 0); // dwStart
    le32(&mut strh, frame_count); // dwLength
    le32(&mut strh, frame_bytes); // dwSuggestedBufferSize
    le32(&mut strh, 0xFFFF_FFFF); // dwQuality = default
    le32(&mut strh, 0); // dwSampleSize
    for v in [0, 0, width as u16, height as u16] {
        le16(&mut strh, v); // rcFrame, four i16
    }

    let mut strf = Vec::with_capacity(40); // BITMAPINFOHEADER
    le32(&mut strf, 40); // biSize
    le32(&mut strf, width); // biWidth
    le32(&mut strf, height); // biHeight > 0 => bottom-up rows
    le16(&mut strf, 1); // biPlanes
    le16(&mut strf, 24); // biBitCount
    le32(&mut strf, 0); // biCompression = BI_RGB
    le32(&mut strf, frame_bytes); // biSizeImage
    for _ in 0..4 {
        le32(&mut strf, 0); // bi{X,Y}PelsPerMeter, biClrUsed, biClrImportant
    }

    let mut strl = Vec::new();
    strl.extend_from_slice(b"strl");
    riff_chunk(&mut strl, b"strh", &strh);
    riff_chunk(&mut strl, b"strf", &strf);
    let mut hdrl = Vec::new();
    hdrl.extend_from_slice(b"hdrl");
    riff_chunk(&mut hdrl, b"avih", &avih);
    riff_chunk(&mut hdrl, b"LIST", &strl);

    // RIFF `AVI ` { LIST hdrl, LIST movi, idx1 }.
    let mut body = Vec::with_capacity(4 + 8 + hdrl.len() + 8 + movi.len() + 8 + idx1.len());
    body.extend_from_slice(b"AVI ");
    riff_chunk(&mut body, b"LIST", &hdrl);
    riff_chunk(&mut body, b"LIST", &movi);
    riff_chunk(&mut body, b"idx1", &idx1);
    let mut avi = Vec::with_capacity(8 + body.len());
    riff_chunk(&mut avi, b"RIFF", &body);

    std::fs::write(path, &avi)
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
    /// byte-identical ASCI assets (writer determinism is already a §4 gate;
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
        let mut r = FixtureRenderer::new(&asset, GoldenPalette::Ascii);
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

    /// The AVI writer's layout is arithmetic, so pin the arithmetic: the
    /// RIFF size field, the fourccs and the exact file length. The BYTES
    /// are pinned end to end by auto-ascii-factory's `FIXTURE_AVI_SHA`.
    #[test]
    fn bgr24_avi_layout_is_exact() {
        let (w, h, fps, n) = (8u32, 4u32, 25u32, 3u32);
        let frame_bytes = (w * h * 3) as usize;
        let dir = std::env::temp_dir().join(format!("auto-ascii-eval-avi-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tiny.avi");
        write_bgr24_avi(&path, w, h, fps, (0..n).map(|f| vec![f as u8; frame_bytes])).unwrap();
        let bytes = std::fs::read(&path).unwrap();

        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize, bytes.len() - 8);
        assert_eq!(&bytes[8..12], b"AVI ");
        // LIST hdrl { avih(56), LIST strl { strh(56), strf(40) } }
        let hdrl = 8 + 4 + (8 + 56) + (8 + 4 + (8 + 56) + (8 + 40));
        let movi = 8 + 4 + n as usize * (8 + frame_bytes);
        let idx1 = 8 + n as usize * 16;
        assert_eq!(bytes.len(), 8 + 4 + hdrl + movi + idx1);
        // Frames land verbatim, in order, right after their chunk header.
        let first = 8 + 4 + hdrl + 8 + 4 + 8;
        assert_eq!(&bytes[first..first + frame_bytes], &vec![0u8; frame_bytes][..]);

        // Geometry and frame size are checked, not trusted.
        assert!(write_bgr24_avi(&path, 6, h, fps, std::iter::once(vec![0u8; 72])).is_err());
        assert!(write_bgr24_avi(&path, w, h, fps, std::iter::empty()).is_err());
        assert!(write_bgr24_avi(&path, w, h, fps, std::iter::once(vec![0u8; 95])).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fnv_vector() {
        // Known FNV-1a 64 test vector: "a" → 0xaf63dc4c8601ec8c.
        assert_eq!(fnv1a64(*b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
