//! Synthetic ASCI fixtures, fixture rendering, snapshots and BGR24 AVI input fixtures.

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

pub const FIXTURE_BASE_W: u16 = 192;
pub const FIXTURE_BASE_H: u16 = 108;
pub const FIXTURE_FRAMES: u32 = 72;
pub const FIXTURE_KEYFRAME_IVL: u8 = 24;
pub const HARD_CUT_FRAME: u32 = 36;
const _: () = assert!(FIXTURE_KEYFRAME_IVL > 0);
const _: () = assert!(FIXTURE_FRAMES >= 3 * FIXTURE_KEYFRAME_IVL as u32, "three keyframe groups");
const _: () = assert!(HARD_CUT_FRAME > 0 && HARD_CUT_FRAME < FIXTURE_FRAMES);
const _: () = assert!(
    !HARD_CUT_FRAME.is_multiple_of(FIXTURE_KEYFRAME_IVL as u32),
    "the cut lands mid-GOP so delta decode crosses a shot change"
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fixture {
    GradientMotion,
    HardCut,
    CheckerDrift,
}

impl Fixture {
    pub const ALL: [Fixture; 3] =
        [Fixture::GradientMotion, Fixture::HardCut, Fixture::CheckerDrift];

    pub fn name(self) -> &'static str {
        match self {
            Fixture::GradientMotion => "gradient-motion",
            Fixture::HardCut => "hard-cut",
            Fixture::CheckerDrift => "checker-drift",
        }
    }
}

fn tri(p: u32) -> u8 {
    let m = p % 512;
    if m < 256 { m as u8 } else { (511 - m) as u8 }
}

pub fn luma_plane(fixture: Fixture, frame: u32) -> Vec<u8> {
    let (w, h) = (u32::from(FIXTURE_BASE_W), u32::from(FIXTURE_BASE_H));
    let mut plane = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let v = match fixture {
                Fixture::GradientMotion => tri(2 * x + 3 * y + 6 * frame),
                Fixture::HardCut => {
                    if frame < HARD_CUT_FRAME {
                        (y * 160 / (h - 1) + x / 24 + frame % 8) as u8
                    } else {
                        (250 - x * 180 / (w - 1) + (frame - HARD_CUT_FRAME) % 6) as u8
                    }
                }
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
                        (40, (60 + cy * 3).min(255), 200)
                    } else {
                        (220, 160 - cy.min(53) * 2, 60 + cx / 2)
                    }
                }
                Fixture::CheckerDrift => {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoldenPalette {
    Ascii,
    Unicode,
    MonoGlyphOnly,
}

impl GoldenPalette {
    pub const ALL: [GoldenPalette; 3] =
        [GoldenPalette::Ascii, GoldenPalette::Unicode, GoldenPalette::MonoGlyphOnly];

    pub fn name(self) -> &'static str {
        match self {
            GoldenPalette::Ascii => "ascii",
            GoldenPalette::Unicode => "unicode",
            GoldenPalette::MonoGlyphOnly => "mono",
        }
    }

    pub fn is_glyph_only(self) -> bool {
        self == GoldenPalette::MonoGlyphOnly
    }

    pub fn config(self) -> (GlyphTier, ColorDepth) {
        match self {
            GoldenPalette::Ascii => (GlyphTier::Ascii, ColorDepth::True),
            GoldenPalette::Unicode => (GlyphTier::UnicodeBlocks, ColorDepth::True),
            GoldenPalette::MonoGlyphOnly => (GlyphTier::Ascii, ColorDepth::Mono),
        }
    }
}

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
        build_levels_lut(&mut this.levels_lut, None);
        this
    }

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

    pub fn viewport(&self) -> Option<Viewport> {
        self.vp
    }

    pub fn resampler_dims(&self) -> Option<((u16, u16), (u16, u16))> {
        self.resampler.as_ref().map(|r| (r.src_dims(), r.dst_dims()))
    }

    pub fn frame_count(&self) -> u32 {
        self.frame_count
    }

    pub fn grid(&self) -> &Grid<Cell> {
        &self.grid
    }

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

    fn update_levels(&mut self, frame: u32) {
        let shot = self.reader.shot_for_frame(frame).map(|s| s.first_frame);
        if shot != self.lut_shot {
            build_levels_lut(&mut self.levels_lut, self.reader.norm_levels(frame, plane_id::Y));
            self.lut_shot = shot;
            self.state.reset();
        }
    }
}

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

fn fnv1a64(bytes: impl IntoIterator<Item = u8>) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

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

fn le32(out: &mut Vec<u8>, n: u32) {
    out.extend_from_slice(&n.to_le_bytes());
}

fn le16(out: &mut Vec<u8>, n: u16) {
    out.extend_from_slice(&n.to_le_bytes());
}

fn riff_chunk(out: &mut Vec<u8>, fourcc: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(fourcc);
    le32(out, payload.len() as u32);
    out.extend_from_slice(payload);
}

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
        le32(&mut idx1, 0x10);
        le32(&mut idx1, offset);
        le32(&mut idx1, frame_bytes);
        frame_count += 1;
    }
    if frame_count == 0 {
        return Err(std::io::Error::other("write_bgr24_avi: no frames"));
    }

    let mut avih = Vec::with_capacity(56);
    le32(&mut avih, 1_000_000 / fps);
    le32(&mut avih, frame_bytes * fps);
    le32(&mut avih, 0);
    le32(&mut avih, 0x10);
    le32(&mut avih, frame_count);
    le32(&mut avih, 0);
    le32(&mut avih, 1);
    le32(&mut avih, frame_bytes);
    le32(&mut avih, width);
    le32(&mut avih, height);
    for _ in 0..4 {
        le32(&mut avih, 0);
    }

    let mut strh = Vec::with_capacity(56);
    strh.extend_from_slice(b"vids");
    strh.extend_from_slice(b"DIB ");
    le32(&mut strh, 0);
    le16(&mut strh, 0);
    le16(&mut strh, 0);
    le32(&mut strh, 0);
    le32(&mut strh, 1);
    le32(&mut strh, fps);
    le32(&mut strh, 0);
    le32(&mut strh, frame_count);
    le32(&mut strh, frame_bytes);
    le32(&mut strh, 0xFFFF_FFFF);
    le32(&mut strh, 0);
    for v in [0, 0, width as u16, height as u16] {
        le16(&mut strh, v);
    }

    let mut strf = Vec::with_capacity(40);
    le32(&mut strf, 40);
    le32(&mut strf, width);
    le32(&mut strf, height);
    le16(&mut strf, 1);
    le16(&mut strf, 24);
    le32(&mut strf, 0);
    le32(&mut strf, frame_bytes);
    for _ in 0..4 {
        le32(&mut strf, 0);
    }

    let mut strl = Vec::new();
    strl.extend_from_slice(b"strl");
    riff_chunk(&mut strl, b"strh", &strh);
    riff_chunk(&mut strl, b"strf", &strf);
    let mut hdrl = Vec::new();
    hdrl.extend_from_slice(b"hdrl");
    riff_chunk(&mut hdrl, b"avih", &avih);
    riff_chunk(&mut hdrl, b"LIST", &strl);

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
        assert_eq!((vp.cols, vp.rows), (80, 23));
        let grid = r.render(10);
        assert_eq!((grid.cols(), grid.rows()), (80, 24));
        assert!(grid.row(23).iter().all(|c| *c == Cell::BLANK));
        assert!(grid.row(5).iter().any(|c| c.glyph() != ' '));
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
        let hdrl = 8 + 4 + (8 + 56) + (8 + 4 + (8 + 56) + (8 + 40));
        let movi = 8 + 4 + n as usize * (8 + frame_bytes);
        let idx1 = 8 + n as usize * 16;
        assert_eq!(bytes.len(), 8 + 4 + hdrl + movi + idx1);
        let first = 8 + 4 + hdrl + 8 + 4 + 8;
        assert_eq!(&bytes[first..first + frame_bytes], &vec![0u8; frame_bytes][..]);

        assert!(write_bgr24_avi(&path, 6, h, fps, std::iter::once(vec![0u8; 72])).is_err());
        assert!(write_bgr24_avi(&path, w, h, fps, std::iter::empty()).is_err());
        assert!(write_bgr24_avi(&path, w, h, fps, std::iter::once(vec![0u8; 95])).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fnv_vector() {
        assert_eq!(fnv1a64(*b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
