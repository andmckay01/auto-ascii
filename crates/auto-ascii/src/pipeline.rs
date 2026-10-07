//! Frame decode, resampling, levels, composition and presentation.

use std::time::Instant;

use auto_ascii_core::{
    Cell, ColorDepth, ComposeParams, FramePlanes, GlyphTier, Grid, HysteresisState,
    PaletteSet, Resampler, Rgb, Style, Viewport, compose_frame_style, compute_viewport_for,
    select_palettes,
};
use auto_ascii_format::header::plane_id;
use auto_ascii_format::{PlaneLevels, AsciiReader};
use auto_ascii_term::{Backend, Caps, ColorTier, Event, FrameStats, GlyphFlags, GlyphSupportTier, Key};

use crate::error::Error;

type Result<T> = std::result::Result<T, Error>;

const H_HIGHLIGHT_MIN: u8 = 64;
const H_SHADOW_MIN: u8 = 128;

pub const SCRUB_STEP_SECS: f64 = 5.0;

const PROGRESS_HINT_MIN_COLS: u16 = 64;

const HINT_SEP: &str = "   ";

const HINT_DROP_ORDER: [usize; 9] = [8, 6, 7, 5, 4, 1, 3, 2, 0];

pub const ZOOM_HINT_MAX_COLS: u16 = 160;

const ZOOM_KEY: &str = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };

pub const BIG_OVERLAY_MIN_COLS: u16 = 240;
pub const BIG_OVERLAY_MIN_ROWS: u16 = 36;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayScale {
    Normal,
    Big,
}

impl OverlayScale {
    pub fn for_grid(cols: u16, rows: u16, glyph_tier: GlyphTier) -> OverlayScale {
        if glyph_tier != GlyphTier::Ascii
            && cols >= BIG_OVERLAY_MIN_COLS
            && rows >= BIG_OVERLAY_MIN_ROWS
        {
            OverlayScale::Big
        } else {
            OverlayScale::Normal
        }
    }

    pub fn line_chars(self, cols: u16) -> u16 {
        match self {
            OverlayScale::Normal => cols,
            OverlayScale::Big => cols / BIG_CHAR_COLS,
        }
    }

    pub fn line_rows(self) -> u16 {
        match self {
            OverlayScale::Normal => 1,
            OverlayScale::Big => BIG_LINE_ROWS,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UiRows {
    rows: u16,
    line_rows: u16,
    lines: u8,
}

impl UiRows {
    pub const NONE: UiRows = UiRows { rows: 0, line_rows: 0, lines: 0 };

    pub fn contains(self, row: u16) -> bool {
        if row >= self.rows || self.line_rows == 0 {
            return false;
        }
        let line = (self.rows - 1 - row) / self.line_rows;
        line < 8 && self.lines >> line & 1 == 1
    }

    pub fn is_empty(self) -> bool {
        self.lines == 0
    }
}

impl std::ops::BitOr for UiRows {
    type Output = UiRows;

    fn bitor(self, other: UiRows) -> UiRows {
        if self.is_empty() {
            return other;
        }
        debug_assert!(other.is_empty() || (self.rows, self.line_rows) == (other.rows, other.line_rows));
        UiRows { lines: self.lines | other.lines, ..self }
    }
}

impl std::ops::BitOrAssign for UiRows {
    fn bitor_assign(&mut self, other: UiRows) {
        *self = *self | other;
    }
}

const BIG_CHAR_COLS: u16 = 4;
const BIG_LINE_ROWS: u16 = 3;

#[rustfmt::skip]
const BIG_FONT: [u16; 64] = [
    0b000_000_000_000_000, 0b010_010_010_000_010, 0b101_101_000_000_000, 0b101_111_101_111_101,
    0b011_110_010_011_110, 0b101_001_010_100_101, 0b010_101_010_101_011, 0b010_010_000_000_000,
    0b001_010_010_010_001, 0b100_010_010_010_100, 0b000_101_010_101_000, 0b000_010_111_010_000,
    0b000_000_000_010_100, 0b000_000_111_000_000, 0b000_000_000_000_010, 0b001_001_010_100_100,
    0b111_101_101_101_111, 0b010_110_010_010_111, 0b111_001_111_100_111, 0b111_001_111_001_111,
    0b101_101_111_001_001, 0b111_100_111_001_111, 0b111_100_111_101_111, 0b111_001_001_010_010,
    0b111_101_111_101_111, 0b111_101_111_001_111, 0b000_010_000_010_000, 0b000_010_000_010_100,
    0b001_010_100_010_001, 0b000_111_000_111_000, 0b100_010_001_010_100, 0b111_001_011_000_010,
    0b111_101_111_100_011, 0b010_101_111_101_101, 0b110_101_110_101_110, 0b011_100_100_100_011,
    0b110_101_101_101_110, 0b111_100_110_100_111, 0b111_100_110_100_100, 0b011_100_101_101_011,
    0b101_101_111_101_101, 0b111_010_010_010_111, 0b001_001_001_101_010, 0b101_101_110_101_101,
    0b100_100_100_100_111, 0b101_111_111_101_101, 0b110_101_101_101_101, 0b010_101_101_101_010,
    0b110_101_110_100_100, 0b010_101_101_111_011, 0b110_101_110_101_101, 0b011_100_010_001_110,
    0b111_010_010_010_010, 0b101_101_101_101_111, 0b101_101_101_101_010, 0b101_101_111_111_101,
    0b101_101_010_101_101, 0b101_101_010_010_010, 0b111_001_010_100_111, 0b110_100_100_100_110,
    0b100_100_010_001_001, 0b011_001_001_001_011, 0b010_101_000_000_000, 0b000_000_000_000_111,
];

const BIG_BAR: u16 = 0b010_010_010_010_010;

fn big_glyph(c: char) -> u16 {
    let c = match c {
        'a'..='z' => c.to_ascii_uppercase(),
        '|' => return BIG_BAR,
        '{' => '(',
        '}' => ')',
        '`' => '\'',
        '~' => '-',
        c => c,
    };
    match c {
        ' '..='_' => BIG_FONT[c as usize - 0x20],
        _ => BIG_FONT['?' as usize - 0x20],
    }
}

fn paint_line(grid: &mut Grid<Cell>, slot: u16, line: &str, fg: Rgb, bg: Rgb, scale: OverlayScale) -> UiRows {
    let (cols, rows) = (grid.cols(), grid.rows());
    let h = scale.line_rows();
    let Some(top) = rows.checked_sub(h * (slot + 1)) else {
        return UiRows::NONE;
    };
    if cols == 0 {
        return UiRows::NONE;
    }
    match scale {
        OverlayScale::Normal => {
            let mut chars = line.chars();
            for col in 0..cols {
                let ch = chars.next().unwrap_or(' ');
                grid.set(col, top, Cell::new(ch, fg, bg));
            }
        }
        OverlayScale::Big => {
            let mut chars = line.chars();
            let mut glyph = 0u16;
            for col in 0..cols {
                let px = col % BIG_CHAR_COLS;
                if px == 0 {
                    glyph = match chars.next() {
                        Some(c) if col + BIG_CHAR_COLS <= cols => big_glyph(c),
                        _ => 0,
                    };
                }
                let lit = |y: u16| -> bool {
                    if px >= 3 || y == 0 {
                        return false;
                    }
                    let bit = 14 - ((y - 1) * 3 + px);
                    glyph >> bit & 1 == 1
                };
                for r in 0..h {
                    let ch = match (lit(2 * r), lit(2 * r + 1)) {
                        (true, true) => '█',
                        (true, false) => '▀',
                        (false, true) => '▄',
                        (false, false) => ' ',
                    };
                    grid.set(col, top + r, Cell::new(ch, fg, bg));
                }
            }
        }
    }
    UiRows { rows, line_rows: h, lines: 1 << slot }
}

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

pub fn color_depth(tier: ColorTier) -> ColorDepth {
    match tier {
        ColorTier::True => ColorDepth::True,
        ColorTier::C256 => ColorDepth::C256,
        ColorTier::C16 => ColorDepth::C16,
        ColorTier::Mono => ColorDepth::Mono,
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct StageNs {
    pub decode: u64,
    pub resample: u64,
    pub compose: u64,
    pub present: u64,
}

pub struct Drained {
    pub quit: bool,
    pub jump_digit: Option<u8>,
    pub seek_steps: i32,
    pub dial_cycle: u32,
    pub dial_delta: i32,
    pub toggle_hints: bool,
    pub toggle_pause: bool,
    pub style_cycle: u32,
    pub save: bool,
    pub toggle_sound: bool,
}

impl Drained {
    pub fn requests_temporal_reset(&self) -> bool {
        self.jump_digit.is_some() || self.seek_steps != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProgressContext {
    pub frame: u32,
    pub frame_count: u32,
    pub fps_num: u16,
    pub fps_den: u16,
    pub clip: Option<(usize, usize)>,
}

impl ProgressContext {
    pub fn fps(&self) -> f64 {
        (f64::from(self.fps_num) / f64::from(self.fps_den.max(1))).max(1e-9)
    }
}

pub fn drain_backend_events<B: Backend>(backend: &mut B) -> (Drained, Option<(u16, u16)>) {
    let mut resize: Option<(u16, u16)> = None;
    let mut jump_digit = None;
    let mut seek_steps: i32 = 0;
    let mut dial_cycle: u32 = 0;
    let mut dial_delta: i32 = 0;
    let mut toggle_hints = false;
    let mut toggle_pause = false;
    let mut style_cycle: u32 = 0;
    let mut save = false;
    let mut toggle_sound = false;
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
                    style_cycle: 0,
                    save: false,
                    toggle_sound: false,
                };
                return (quit, None);
            }
            Event::Resize(c, r) => resize = Some((c, r)),
            Event::Key(Key::Char(c @ '0'..='9')) => jump_digit = Some(c as u8 - b'0'),
            Event::Key(Key::Left) => seek_steps = seek_steps.saturating_sub(1),
            Event::Key(Key::Right) => seek_steps = seek_steps.saturating_add(1),
            Event::Key(Key::Char('d')) => dial_cycle = dial_cycle.saturating_add(1),
            Event::Key(Key::Char('[')) => dial_delta = dial_delta.saturating_sub(1),
            Event::Key(Key::Char(']')) => dial_delta = dial_delta.saturating_add(1),
            Event::Key(Key::Char('v')) => toggle_hints = true,
            Event::Key(Key::Char(' ')) => toggle_pause = true,
            Event::Key(Key::Char('/')) => style_cycle = style_cycle.saturating_add(1),
            Event::Key(Key::Char('s')) => save = true,
            Event::Key(Key::Char('m')) => toggle_sound = true,
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
        style_cycle,
        save,
        toggle_sound,
    };
    (drained, resize)
}

pub struct Player<'a> {
    reader: Option<AsciiReader<'a>>,
    live_shot: u32,
    live_levels: Option<PlaneLevels>,
    live_lut_levels: Option<PlaneLevels>,
    frame_count: u32,
    src_w: u16,
    src_h: u16,
    chroma_dims: Option<(u16, u16)>,
    use_chroma: bool,
    has_edges: bool,
    has_h: bool,
    aspect_num: u16,
    aspect_den: u16,
    cell_aspect: f64,
    repaint_full: bool,
    glyph_tier: GlyphTier,
    color: ColorDepth,
    palette: Option<PaletteSet>,
    compose_params: ComposeParams,
    style: Style,
    state: HysteresisState,
    vp: Option<Viewport>,
    resampler: Option<Resampler>,
    feat_resampler: Option<Resampler>,
    chroma_resampler: Option<Resampler>,
    luma_src: Vec<u8>,
    e_src: Vec<u8>,
    ex_src: Vec<u8>,
    ey_src: Vec<u8>,
    h_src: Vec<u8>,
    hl_mask: Vec<u8>,
    sh_mask: Vec<u8>,
    luma2_dst: Vec<u8>,
    e_dst: Vec<u8>,
    ex_dst: Vec<u8>,
    ey_dst: Vec<u8>,
    hl_dst: Vec<u8>,
    sh_dst: Vec<u8>,
    h_dst: Vec<u8>,
    chroma_src: Vec<u8>,
    cr_src: Vec<u8>,
    cg_src: Vec<u8>,
    cb_src: Vec<u8>,
    cr_dst: Vec<u8>,
    cg_dst: Vec<u8>,
    cb_dst: Vec<u8>,
    dial_overlay: Option<(&'static str, u8, u8)>,
    levels_lut: [u8; 256],
    chroma_gain_q8: [u16; 256],
    chroma_gain_on: bool,
    lut_key: Option<(Option<u32>, u8, u8)>,
    loaded: Option<u32>,
    grid: Grid<Cell>,
    layer_mask: Option<Grid<u8>>,
    ui_rows: UiRows,
    stage: StageNs,
    overlay_visible: bool,
    progress_ctx: Option<ProgressContext>,
    hint_visible: bool,
    info: Option<String>,
    paused: bool,
    overlay_hide_pending: bool,
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
        let has_edges = reader.plane_dims(plane_id::E).is_some()
            && reader.plane_dims(plane_id::EX).is_some()
            && reader.plane_dims(plane_id::EY).is_some();
        let has_h = reader.plane_dims(plane_id::H).is_some();
        let header = reader.header();
        let aspect = (header.aspect_num, header.aspect_den);
        let fps = (f64::from(header.fps_num) / f64::from(header.fps_den.max(1))).max(1e-9);
        let geometry = Geometry {
            src_w,
            src_h,
            frame_count,
            chroma_dims,
            has_edges,
            has_h,
            aspect,
            fps,
        };
        Ok(Player::with_geometry(Some(reader), &geometry, cell_aspect, repaint_full, color, glyph_tier))
    }

    pub fn live(
        spec: &LiveSpec,
        cell_aspect: f64,
        repaint_full: bool,
        color: ColorDepth,
        glyph_tier: GlyphTier,
    ) -> Result<Player<'a>> {
        if spec.w < 2 || spec.h < 2 || !spec.w.is_multiple_of(2) || !spec.h.is_multiple_of(2) {
            return Err(Error::Config(format!(
                "live planes must be even and at least 2x2 (got {}x{})",
                spec.w, spec.h
            )));
        }
        let geometry = Geometry {
            src_w: spec.w,
            src_h: spec.h,
            frame_count: spec.frame_count.max(1),
            chroma_dims: Some((spec.w / 2, spec.h / 2)),
            has_edges: true,
            has_h: true,
            aspect: (spec.aspect_num, spec.aspect_den),
            fps: spec.fps.max(1e-9),
        };
        Ok(Player::with_geometry(None, &geometry, cell_aspect, repaint_full, color, glyph_tier))
    }

    fn with_geometry(
        reader: Option<AsciiReader<'a>>,
        g: &Geometry,
        cell_aspect: f64,
        repaint_full: bool,
        color: ColorDepth,
        glyph_tier: GlyphTier,
    ) -> Player<'a> {
        let Geometry { src_w, src_h, frame_count, chroma_dims, has_edges, has_h, aspect, fps } = *g;
        let use_chroma = color != ColorDepth::Mono && chroma_dims.is_some();
        let chroma_len = chroma_dims.map_or(0, |(w, h)| w as usize * h as usize);
        let (aspect_num, aspect_den) =
            if aspect.0 == 0 || aspect.1 == 0 { (16, 9) } else { aspect };
        let src_len = src_w as usize * src_h as usize;
        let mut levels_lut = [0u8; 256];
        build_levels_lut(&mut levels_lut, None);
        Player {
            reader,
            live_shot: 0,
            live_levels: None,
            live_lut_levels: None,
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
            style: Style::default(),
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
            chroma_gain_q8: [256; 256],
            chroma_gain_on: false,
            lut_key: None,
            loaded: None,
            grid: Grid::new(0, 0),
            layer_mask: None,
            ui_rows: UiRows::NONE,
            stage: StageNs::default(),
            overlay_visible: false,
            progress_ctx: None,
            hint_visible: false,
            info: None,
            paused: false,
            overlay_hide_pending: false,
            fps,
        }
    }

    pub fn compose_params(&self) -> ComposeParams {
        self.compose_params
    }

    pub fn set_compose_params(&mut self, params: ComposeParams) {
        if params == self.compose_params {
            return;
        }
        self.compose_params = params;
        self.state.reset();
    }

    pub fn style(&self) -> Style {
        self.style
    }

    pub fn set_style(&mut self, style: Style) {
        if style != self.style {
            self.style = style;
            self.state.reset();
        }
    }

    pub fn enable_layer_mask(&mut self) {
        let mut mask = Grid::new(self.grid.cols(), self.grid.rows());
        mask.fill(auto_ascii_core::layer::BASE);
        self.layer_mask = Some(mask);
    }

    pub fn layer_mask(&self) -> Option<&Grid<u8>> {
        self.layer_mask.as_ref()
    }

    pub fn frame_count(&self) -> u32 {
        self.frame_count
    }

    pub fn grid(&self) -> &Grid<Cell> {
        &self.grid
    }

    pub fn ui_rows(&self) -> UiRows {
        self.ui_rows
    }

    pub fn viewport(&self) -> Option<Viewport> {
        self.vp
    }

    pub fn resampler_dims(&self) -> Option<((u16, u16), (u16, u16))> {
        self.resampler.as_ref().map(|r| (r.src_dims(), r.dst_dims()))
    }

    pub fn hysteresis_dims(&self) -> (u16, u16) {
        (self.state.cols(), self.state.rows())
    }

    pub fn luma_src(&self) -> &[u8] {
        &self.luma_src
    }

    pub fn levels_lut(&self) -> &[u8; 256] {
        &self.levels_lut
    }

    pub fn stage(&self) -> StageNs {
        self.stage
    }

    pub fn reflow<B: Backend>(&mut self, backend: &mut B, cols: u16, rows: u16) {
        backend.resize(cols, rows);
        self.reflow_grid(cols, rows);
        backend.invalidate();
    }

    pub fn reflow_grid(&mut self, cols: u16, rows: u16) {
        self.grid.resize(cols, rows);
        self.ui_rows = UiRows::NONE;
        if let Some(mask) = &mut self.layer_mask {
            mask.resize(cols, rows);
        }
        self.vp =
            compute_viewport_for(cols, rows, self.cell_aspect, self.aspect_num, self.aspect_den);
        if let Some(vp) = self.vp {
            self.palette = Some(select_palettes(self.glyph_tier, self.color, vp.cols));
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
            self.palette = None;
            self.resampler = None;
            self.feat_resampler = None;
            self.chroma_resampler = None;
            self.state.resize(0, 0);
        }
    }

    pub fn drain_events<B: Backend>(&mut self, backend: &mut B) -> Drained {
        let (drained, resize) = drain_backend_events(backend);
        if let Some((c, r)) = resize {
            self.reflow(backend, c, r);
        }
        if drained.requests_temporal_reset() {
            self.state.reset();
        }
        drained
    }

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

    fn load_frame(&mut self, frame_idx: u32) -> Result<()> {
        if self.loaded == Some(frame_idx) {
            return Ok(());
        }
        let Some(reader) = self.reader.as_mut() else {
            return Ok(());
        };
        let sequential = frame_idx > 0 && self.loaded == Some(frame_idx - 1);
        Self::load_plane(reader, sequential, frame_idx, plane_id::Y, &mut self.luma_src)?;
        if self.has_edges {
            Self::load_plane(reader, sequential, frame_idx, plane_id::E, &mut self.e_src)?;
            Self::load_plane(reader, sequential, frame_idx, plane_id::EX, &mut self.ex_src)?;
            Self::load_plane(reader, sequential, frame_idx, plane_id::EY, &mut self.ey_src)?;
        }
        if self.has_h {
            Self::load_plane(reader, sequential, frame_idx, plane_id::H, &mut self.h_src)?;
        }
        if self.use_chroma {
            Self::load_plane(reader, sequential, frame_idx, plane_id::C, &mut self.chroma_src)?;
        }
        self.loaded = Some(frame_idx);
        Ok(())
    }

    fn update_levels(&mut self, frame_idx: u32) {
        let Some(reader) = self.reader.as_ref() else {
            self.update_live_levels();
            return;
        };
        let shot = reader.shot_for_frame(frame_idx).map(|s| s.first_frame);
        let key = (shot, self.compose_params.shadow_lift, self.compose_params.lift_color);
        if self.lut_key != Some(key) {
            let levels = reader.norm_levels(frame_idx, plane_id::Y);
            build_levels_lut_lifted(&mut self.levels_lut, levels, key.1);
            self.chroma_gain_on = build_chroma_gain_q8(&mut self.chroma_gain_q8, levels, key.1, key.2);
            self.lut_key = Some(key);
            self.state.reset();
        }
    }

    fn update_live_levels(&mut self) {
        let key = (Some(self.live_shot), self.compose_params.shadow_lift, self.compose_params.lift_color);
        let fresh = self.lut_key != Some(key);
        if fresh || self.live_lut_levels != self.live_levels {
            build_levels_lut_lifted(&mut self.levels_lut, self.live_levels, key.1);
            self.chroma_gain_on =
                build_chroma_gain_q8(&mut self.chroma_gain_q8, self.live_levels, key.1, key.2);
            self.live_lut_levels = self.live_levels;
            if fresh {
                self.lut_key = Some(key);
                self.state.reset();
            }
        }
    }

    pub fn load_live(&mut self, frame_idx: u32, frame: &LiveFrame<'_>) -> Result<()> {
        if self.reader.is_some() {
            return Err(Error::Config("load_live needs a live player (Player::live)".into()));
        }
        let n = self.luma_src.len();
        let cn = self.chroma_dims.map_or(0, |(w, h)| w as usize * h as usize);
        let sized = [frame.y, frame.e, frame.ex, frame.ey, frame.h].iter().all(|p| p.len() == n)
            && frame.c.len() == cn * 2;
        if !sized {
            return Err(Error::Config(format!(
                "live frame planes do not match the {}x{} geometry",
                self.src_w, self.src_h
            )));
        }
        self.luma_src.copy_from_slice(frame.y);
        self.e_src.copy_from_slice(frame.e);
        self.ex_src.copy_from_slice(frame.ex);
        self.ey_src.copy_from_slice(frame.ey);
        self.h_src.copy_from_slice(frame.h);
        if self.use_chroma {
            self.chroma_src.copy_from_slice(frame.c);
        }
        self.live_shot = frame.shot_start;
        self.live_levels = frame.levels;
        self.loaded = Some(frame_idx);
        Ok(())
    }

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

    pub fn set_progress_context(&mut self, ctx: Option<ProgressContext>) {
        self.progress_ctx = ctx;
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    pub fn set_hint_overlay(&mut self, visible: bool) {
        if self.hint_visible && !visible {
            self.overlay_hide_pending = true;
        }
        self.hint_visible = visible;
    }

    pub fn set_info_overlay(&mut self, info: Option<&str>) {
        match info {
            None => {
                if self.info.take().is_some() && self.hint_visible {
                    self.overlay_hide_pending = true;
                }
            }
            Some(new) => match &mut self.info {
                Some(cur) if cur == new => {}
                Some(cur) => new.clone_into(cur),
                slot => *slot = Some(new.to_owned()),
            },
        }
    }

    pub fn render_present<B: Backend>(
        &mut self,
        backend: &mut B,
        frame_idx: u32,
    ) -> Result<FrameStats> {
        self.render_grid(frame_idx)?;
        if self.repaint_full || std::mem::take(&mut self.overlay_hide_pending) {
            backend.invalidate();
        }
        let t = Instant::now();
        let stats = backend.present(&self.grid);
        self.stage.present += t.elapsed().as_nanos() as u64;
        Ok(stats)
    }

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
                if self.chroma_gain_on {
                    let vc = self.vp.expect("checked above").cols as usize;
                    for (i, ((r, g), b)) in
                        self.cr_dst.iter_mut().zip(&mut self.cg_dst).zip(&mut self.cb_dst).enumerate()
                    {
                        let (row, col) = (i / vc, i % vc);
                        let top = u16::from(self.luma2_dst[2 * row * vc + col]);
                        let bot = u16::from(self.luma2_dst[(2 * row + 1) * vc + col]);
                        let gain = u32::from(self.chroma_gain_q8[((top + bot + 1) >> 1) as usize]);
                        for v in [r, g, b] {
                            *v = ((u32::from(*v) * gain) >> 8).min(255) as u8;
                        }
                    }
                }
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
            compose_frame_style(
                self.style,
                &planes,
                &vp,
                &self.levels_lut,
                set,
                &self.compose_params,
                &mut self.state,
                &mut self.grid,
                self.layer_mask.as_mut(),
            );
            self.stage.compose += t.elapsed().as_nanos() as u64;
        } else {
            draw_enlarge_card(&mut self.grid, self.style.pad());
            if let Some(mask) = &mut self.layer_mask {
                mask.fill(auto_ascii_core::layer::BASE);
            }
        }
        let scale = OverlayScale::for_grid(self.grid.cols(), self.grid.rows(), self.glyph_tier);
        let mut ui = UiRows::NONE;
        if self.overlay_visible {
            ui |= match self.progress_ctx {
                Some(ctx) => draw_progress_overlay_clips(
                    &mut self.grid,
                    ctx.frame,
                    ctx.frame_count,
                    ctx.fps(),
                    ctx.clip,
                    self.paused,
                    scale,
                ),
                None => draw_progress_overlay_clips(
                    &mut self.grid,
                    frame_idx,
                    self.frame_count,
                    self.fps,
                    None,
                    self.paused,
                    scale,
                ),
            };
        }
        if let Some((label, value, max)) = self.dial_overlay {
            ui |= draw_dial_overlay(&mut self.grid, label, value, max, scale);
        }
        if self.hint_visible && self.vp.is_some() {
            ui |= draw_hint_overlay(&mut self.grid, scale);
            if let Some(info) = &self.info {
                ui |= draw_info_overlay(&mut self.grid, info, scale);
            }
        }
        self.ui_rows = ui;
        Ok(())
    }

    pub fn reset_temporal_state(&mut self) {
        self.state.reset();
    }

    pub fn set_glyph_tier_for_next_reflow(&mut self, glyph_tier: GlyphTier) {
        self.glyph_tier = glyph_tier;
    }

    pub fn set_cell_aspect_for_next_reflow(&mut self, cell_aspect: f64) {
        self.cell_aspect = cell_aspect;
    }
}

#[derive(Clone, Copy, Debug)]
struct Geometry {
    src_w: u16,
    src_h: u16,
    frame_count: u32,
    chroma_dims: Option<(u16, u16)>,
    has_edges: bool,
    has_h: bool,
    aspect: (u16, u16),
    fps: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiveSpec {
    pub w: u16,
    pub h: u16,
    pub aspect_num: u16,
    pub aspect_den: u16,
    pub fps: f64,
    pub frame_count: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct LiveFrame<'f> {
    pub y: &'f [u8],
    pub e: &'f [u8],
    pub ex: &'f [u8],
    pub ey: &'f [u8],
    pub h: &'f [u8],
    pub c: &'f [u8],
    pub levels: Option<PlaneLevels>,
    pub shot_start: u32,
}

pub fn build_levels_lut(lut: &mut [u8; 256], levels: Option<PlaneLevels>) {
    build_levels_lut_lifted(lut, levels, 0);
}

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

fn apply_shadow_lift(lut: &mut [u8; 256], lift: u8) {
    if lift == 0 {
        return;
    }
    let lift = u32::from(lift);
    for out in lut.iter_mut() {
        let n = u32::from(*out);
        let curved = (n * 255).isqrt();
        *out = (n + (curved - n) * lift / 255) as u8;
    }
}

fn build_chroma_gain_q8(
    gain: &mut [u16; 256],
    levels: Option<PlaneLevels>,
    shadow_lift: u8,
    lift_color: u8,
) -> bool {
    let (mut unlifted, mut lifted) = ([0u8; 256], [0u8; 256]);
    build_levels_lut_lifted(&mut unlifted, levels, 0);
    build_levels_lut_lifted(&mut lifted, levels, shadow_lift);
    for (v, out) in gain.iter_mut().enumerate() {
        *out = if shadow_lift == 0 || lift_color == 0 || unlifted[v] == 0 {
            256
        } else {
            let full = (u32::from(lifted[v]) * 256 / u32::from(unlifted[v])).min(1024);
            (256 + (full - 256) * u32::from(lift_color) / 255) as u16
        };
    }
    gain.iter().any(|&g| g != 256)
}

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

pub fn draw_enlarge_card(grid: &mut Grid<Cell>, pad: Cell) {
    grid.fill(pad);
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
        let n = (line.len() as u16).min(cols);
        let left = (cols - n) / 2;
        for (j, ch) in line.chars().take(n as usize).enumerate() {
            grid.set(left + j as u16, row, Cell { ch: ch as u32, fg: Rgb::gray(220), ..pad });
        }
    }
}

pub fn draw_progress_overlay(grid: &mut Grid<Cell>, frame: u32, frame_count: u32, fps: f64) -> UiRows {
    draw_progress_overlay_clips(grid, frame, frame_count, fps, None, false, OverlayScale::Normal)
}

pub fn draw_progress_overlay_paused(
    grid: &mut Grid<Cell>,
    frame: u32,
    frame_count: u32,
    fps: f64,
    paused: bool,
) -> UiRows {
    draw_progress_overlay_clips(grid, frame, frame_count, fps, None, paused, OverlayScale::Normal)
}

pub fn draw_progress_overlay_clips(
    grid: &mut Grid<Cell>,
    frame: u32,
    frame_count: u32,
    fps: f64,
    clip: Option<(usize, usize)>,
    paused: bool,
    scale: OverlayScale,
) -> UiRows {
    if grid.cols() == 0 || grid.rows() == 0 {
        return UiRows::NONE;
    }
    let cols = scale.line_chars(grid.cols());
    let fg = Rgb::gray(235);
    let bg = Rgb::new(24, 24, 40);

    let mmss = crate::timecode::format_mmss;
    let pos = f64::from(frame) / fps;
    let total = f64::from(frame_count) / fps;
    let pct = if frame_count > 1 {
        (u64::from(frame) * 100) / u64::from(frame_count - 1)
    } else {
        100
    };
    let hint = if cols >= PROGRESS_HINT_MIN_COLS {
        format!(" <- {} -> ", scrub_step_label())
    } else {
        String::new()
    };
    let mut left = format!(" {} / {} ", mmss(pos), mmss(total));
    if let Some((c, n)) = clip
        && n > 1
        && cols >= PROGRESS_HINT_MIN_COLS
    {
        left.push_str(&format!("{c}/{n} "));
    }
    let right = if paused { " PAUSED ".to_string() } else { format!(" {pct:>3}% ") };

    let mut line = String::with_capacity(cols as usize);
    line.push_str(&hint);
    line.push_str(&left);
    let fixed = hint.chars().count() + left.chars().count() + right.chars().count() + 2;
    if (cols as usize) > fixed {
        let span = cols as usize - fixed;
        let filled = if frame_count > 1 {
            (u64::from(frame) * span as u64) / u64::from(frame_count - 1)
        } else {
            span as u64
        } as usize;
        line.push('[');
        for i in 0..span {
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
    paint_line(grid, 0, &line, fg, bg, scale)
}

pub fn draw_dial_overlay(
    grid: &mut Grid<Cell>,
    label: &str,
    value: u8,
    max: u8,
    scale: OverlayScale,
) -> UiRows {
    if grid.cols() == 0 || grid.rows() == 0 {
        return UiRows::NONE;
    }
    let cols = scale.line_chars(grid.cols());
    let fg = Rgb::gray(245);
    let bg = Rgb::new(46, 32, 16);

    let left = format!(" {label} ");
    let right = format!(" {value:>3}/{max:<3} ");
    let mut line = String::with_capacity(cols as usize);
    line.push_str(&left);
    let fixed = left.chars().count() + right.chars().count() + 2;
    if (cols as usize) > fixed {
        let span = cols as usize - fixed;
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
    paint_line(grid, 0, &line, fg, bg, scale)
}

fn scrub_step_label() -> String {
    format!("{SCRUB_STEP_SECS}s")
}

fn hint_line(cols: u16) -> String {
    let arrows = format!("<- -> {}", scrub_step_label());
    let items: [&str; 10] = [
        "q quit",
        "space pause",
        "0-9 jump",
        &arrows,
        "d dial",
        "[ ] adjust",
        "/ style",
        "m sound",
        "s save",
        "v controls",
    ];
    let width = |keep: &[bool; 10]| -> usize {
        let kept = items.iter().zip(keep).filter(|(_, k)| **k);
        let (n, len) = kept.fold((0, 0), |(n, len), (it, _)| (n + 1, len + it.len()));
        if n == 0 { 0 } else { len + (n - 1) * HINT_SEP.len() + 2 }
    };

    let mut keep = [true; 10];
    for i in HINT_DROP_ORDER {
        if width(&keep) <= cols as usize {
            break;
        }
        keep[i] = false;
    }
    if width(&keep) > cols as usize {
        return String::new();
    }
    let kept: Vec<&str> =
        items.iter().zip(keep).filter(|(_, k)| *k).map(|(it, _)| *it).collect();
    format!(" {} ", kept.join(HINT_SEP))
}

pub fn draw_hint_overlay(grid: &mut Grid<Cell>, scale: OverlayScale) -> UiRows {
    let fg = Rgb::gray(235);
    let bg = Rgb::new(24, 24, 40);
    let line = hint_line(scale.line_chars(grid.cols()));
    paint_line(grid, 1, &line, fg, bg, scale)
}

fn zoom_line(cols: u16) -> String {
    let long = format!(" {ZOOM_KEY} - to zoom out: more cells, a sharper picture ");
    let short = format!(" {ZOOM_KEY} - for a sharper picture ");
    [long, short].into_iter().find(|l| l.len() <= cols as usize).unwrap_or_default()
}

pub fn draw_info_overlay(grid: &mut Grid<Cell>, text: &str, scale: OverlayScale) -> UiRows {
    let (cols, rows) = (grid.cols(), grid.rows());
    let fg = Rgb::gray(235);
    let bg = Rgb::new(24, 24, 40);
    let width = scale.line_chars(cols) as usize;
    let mut line: String =
        text.chars().map(|c| if c == ' ' || c.is_ascii_graphic() { c } else { '?' }).collect();
    let size = format!(" {cols}x{rows} cells ");
    let used = line.chars().count();
    if used + size.len() <= width {
        line.extend(std::iter::repeat_n(' ', width - used - size.len()));
        line.push_str(&size);
    }
    let mut ui = paint_line(grid, 2, &line, fg, bg, scale);
    if cols < ZOOM_HINT_MAX_COLS {
        let zoom = zoom_line(width as u16);
        if !zoom.is_empty() {
            ui |= paint_line(grid, 3, &zoom, fg, bg, scale);
        }
    }
    ui
}

#[cfg(test)]
mod tests {
    use super::*;
    use auto_ascii_core::cell::attrs;

    #[test]
    fn shadow_lift_opens_shadows_monotonically() {
        let (mut plain, mut lifted) = ([0u8; 256], [0u8; 256]);
        build_levels_lut(&mut plain, None);
        build_levels_lut_lifted(&mut lifted, None, 0);
        assert_eq!(plain, lifted, "shadow_lift 0 must not perturb the LUT");

        for lift in [1u8, 64, 128, 255] {
            let mut lut = [0u8; 256];
            build_levels_lut_lifted(&mut lut, None, lift);
            assert_eq!(lut[0], 0, "lift {lift} raised black");
            assert_eq!(lut[255], 255, "lift {lift} clipped white");
            assert!(lut.windows(2).all(|w| w[0] <= w[1]), "lift {lift} is not monotonic");
            assert!(
                lut.iter().enumerate().all(|(i, &v)| v as usize >= i),
                "lift {lift} darkened a value"
            );
        }

        let mut full = [0u8; 256];
        build_levels_lut_lifted(&mut full, None, 255);
        assert_eq!(full[64], 127, "full lift should take 64 to the sqrt curve");
        assert!(full[32] > 2 * 32, "full lift should more than double deep shadow");
    }

    #[test]
    fn chroma_gain_follows_shadow_lift_only_when_asked() {
        let mut gain = [0u16; 256];
        for (lift, color) in [(0, 0), (0, 255), (160, 0)] {
            assert!(!build_chroma_gain_q8(&mut gain, None, lift, color), "lift {lift} color {color}");
            assert!(gain.iter().all(|&g| g == 256), "lift {lift} color {color}");
        }
        assert!(build_chroma_gain_q8(&mut gain, None, 160, 255));
        assert!(gain[1..64].iter().any(|&g| g > 256), "dark tones gain colour");
        assert!(gain.iter().all(|&g| g <= 1024), "capped at 4x");
    }

    #[test]
    fn live_color_gain_refreshes_when_levels_change_within_a_shot() {
        let spec = LiveSpec { w: 4, h: 4, aspect_num: 1, aspect_den: 1, fps: 30.0, frame_count: 3 };
        let mut p = Player::live(&spec, 2.0, false, ColorDepth::True, GlyphTier::Ascii).unwrap();
        p.set_compose_params(ComposeParams { shadow_lift: 160, lift_color: 255, ..ComposeParams::default() });
        let frame = LiveFrame {
            y: &[50; 16], e: &[0; 16], ex: &[128; 16], ey: &[128; 16], h: &[0; 16],
            c: &[0; 8], levels: Some(PlaneLevels { p2: 0, p98: 255 }), shot_start: 0,
        };
        p.load_live(0, &frame).unwrap();
        p.update_live_levels();
        let first = p.chroma_gain_q8;
        p.load_live(1, &LiveFrame { levels: Some(PlaneLevels { p2: 20, p98: 200 }), ..frame }).unwrap();
        p.update_live_levels();
        assert_ne!(p.chroma_gain_q8, first, "new normalization must refresh gain without a cut");
        assert_eq!(p.chroma_gain_q8[20], 256, "normalized black must not be amplified");
        p.set_compose_params(ComposeParams { shadow_lift: 160, ..ComposeParams::default() });
        p.update_live_levels();
        assert!(!p.chroma_gain_on);
        assert!(p.chroma_gain_q8.iter().all(|&g| g == 256));
    }

    #[test]
    fn levels_lut_identity_without_norm() {
        let mut lut = [0u8; 256];
        build_levels_lut(&mut lut, None);
        assert!(lut.iter().enumerate().all(|(i, &v)| v as usize == i));
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
        assert_eq!(lut[125], 128);
        assert!(lut.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn rgb565_unpack_is_canonical() {
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
    fn progress_overlay_is_deterministic_ascii_and_full_width() {
        let mut g: Grid<Cell> = Grid::new(80, 24);
        draw_progress_overlay(&mut g, 900, 5400, 30.0);
        let bottom: String = (0..80).map(|c| g.get(c, 23).glyph()).collect();
        assert!(bottom.contains("0:30 / 3:00"), "time text: {bottom:?}");
        assert!(bottom.contains('[') && bottom.contains(']') && bottom.contains('>'));
        assert!(bottom.is_ascii(), "overlay must render on every tier: {bottom:?}");
        assert!((0..80).all(|c| g.get(c, 22) == Cell::BLANK));

        draw_progress_overlay(&mut g, 0, 5400, 30.0);
        let s: String = (0..80).map(|c| g.get(c, 23).glyph()).collect();
        assert!(s.contains("  0%"), "{s:?}");
        assert!(!s.contains('='), "{s:?}");
        draw_progress_overlay(&mut g, 5399, 5400, 30.0);
        let s: String = (0..80).map(|c| g.get(c, 23).glyph()).collect();
        assert!(s.contains("100%"), "{s:?}");
        assert!(!s.contains('.') || !s.contains('>'), "bar full at the end: {s:?}");

        let mut g2: Grid<Cell> = Grid::new(80, 24);
        draw_progress_overlay(&mut g2, 5399, 5400, 30.0);
        assert_eq!(g.row(23), g2.row(23));

        for (c, r) in [(1u16, 1u16), (7, 2), (12, 1), (31, 8)] {
            let mut t: Grid<Cell> = Grid::new(c, r);
            draw_progress_overlay(&mut t, 10, 20, 30.0);
        }
    }

    #[test]
    fn progress_overlay_clip_block_is_earned() {
        let row_at = |cols: u16, clip: Option<(usize, usize)>| -> String {
            let mut g: Grid<Cell> = Grid::new(cols, 4);
            draw_progress_overlay_clips(&mut g, 900, 5400, 30.0, clip, false, OverlayScale::Normal);
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

        assert_eq!(row_at(80, Some((1, 1))), m6(80), "single-clip compositions say nothing");
        assert_eq!(row_at(63, Some((2, 3))), m6(63), "below 64 columns the block is dropped");
        assert_eq!(row_at(80, None), m6(80), "no context = the M6 row");
        assert_eq!(row_at(64, Some((2, 3))).len(), 64, "the threshold row still fits");
        assert!(wide.is_ascii(), "overlays stay printable ASCII (FEATURE-MAP §10)");
    }

    #[test]
    fn enlarge_card_fits_tiny_grids() {
        for (c, r) in [(1u16, 1u16), (10, 2), (31, 8), (80, 24)] {
            let mut g = Grid::new(c, r);
            draw_enlarge_card(&mut g, Cell::BLANK);
            assert_eq!(g.cols(), c);
        }
        let mut g = Grid::new(40, 9);
        draw_enlarge_card(&mut g, Cell::BLANK);
        let mid: String = (0..40).map(|col| g.get(col, 3).glyph()).collect();
        assert!(mid.contains("AUTO-ASCII"), "card text missing: {mid:?}");
    }

    fn row_text(g: &Grid<Cell>, row: u16) -> String {
        (0..g.cols()).map(|c| g.get(c, row).glyph()).collect()
    }

    fn read_big_line(g: &Grid<Cell>, slot: u16) -> Option<String> {
        let top = g.rows() - BIG_LINE_ROWS * (slot + 1);
        let lit = |col: u16, y: u16| -> bool {
            let ch = g.get(col, top + y / 2).glyph();
            match ch {
                '█' => true,
                '▀' => y.is_multiple_of(2),
                '▄' => !y.is_multiple_of(2),
                ' ' => false,
                other => panic!("big text drew {other:?}"),
            }
        };
        (0..g.cols() / BIG_CHAR_COLS)
            .map(|i| {
                let mut bits = 0u16;
                for y in 1..6 {
                    for px in 0..3 {
                        bits = bits << 1 | u16::from(lit(i * BIG_CHAR_COLS + px, y));
                    }
                }
                assert!(!lit(i * BIG_CHAR_COLS + 3, 1), "spacing column lit");
                assert!((0..3).all(|px| !lit(i * BIG_CHAR_COLS + px, 0)), "margin row lit");
                if bits == BIG_BAR {
                    return Some('|');
                }
                (' '..='_').find(|&c| BIG_FONT[c as usize - 0x20] == bits)
            })
            .collect()
    }

    #[test]
    fn overlay_scale_tiers() {
        use GlyphTier::*;
        let s = OverlayScale::for_grid;
        for tier in [UnicodeBlocks, BrailleVerified] {
            assert_eq!(s(80, 24, tier), OverlayScale::Normal);
            assert_eq!(s(213, 58, tier), OverlayScale::Normal);
            assert_eq!(s(239, 90, tier), OverlayScale::Normal, "one column short");
            assert_eq!(s(320, 35, tier), OverlayScale::Normal, "one row short");
            assert_eq!(s(240, 36, tier), OverlayScale::Big, "the threshold itself");
            assert_eq!(s(320, 90, tier), OverlayScale::Big);
            assert_eq!(s(1000, 1000, tier), OverlayScale::Big);
            assert_eq!(s(1, 1, tier), OverlayScale::Normal);
        }
        assert_eq!(s(320, 90, Ascii), OverlayScale::Normal, "ASCII never draws blocks");
        assert_eq!(s(1000, 1000, Ascii), OverlayScale::Normal);
        assert_eq!(OverlayScale::Big.line_chars(320), 80, "320 columns set an 80-character line");
        assert_eq!(OverlayScale::Big.line_chars(243), 60);
        assert_eq!(OverlayScale::Normal.line_chars(213), 213);
    }

    #[test]
    fn ui_rows_are_exactly_the_rows_the_overlays_draw() {
        type Draw = fn(&mut Grid<Cell>, OverlayScale) -> UiRows;
        let pad = Style::Ascii.pad();
        for (cols, rows) in [(80u16, 24u16), (159, 45), (239, 36), (240, 36), (400, 120), (1000, 300)] {
            for tier in [GlyphTier::Ascii, GlyphTier::UnicodeBlocks] {
                let scale = OverlayScale::for_grid(cols, rows, tier);
                let h = scale.line_rows();
                let band = |line: u16| rows - h * (line + 1)..rows - h * line;
                let draws: [(&str, Draw, &[u16]); 4] = [
                    ("progress", |g, s| draw_progress_overlay_clips(g, 900, 5400, 30.0, None, true, s), &[0]),
                    ("dial", |g, s| draw_dial_overlay(g, "edge", 7, 255, s), &[0]),
                    ("hints", draw_hint_overlay, &[1]),
                    ("info", |g, s| draw_info_overlay(g, " clip ", s), &[2, 3]),
                ];
                for (what, draw, lines) in draws {
                    let mut g: Grid<Cell> = Grid::new(cols, rows);
                    g.fill(pad);
                    let ui = draw(&mut g, scale);
                    let zoom = what == "info" && cols < ZOOM_HINT_MAX_COLS;
                    let lines = if what == "info" && !zoom { &lines[..1] } else { lines };
                    for r in 0..rows {
                        let want = lines.iter().any(|&l| band(l).contains(&r));
                        assert_eq!(ui.contains(r), want, "{what} {cols}x{rows} {tier:?} row {r}");
                        let painted = (0..cols).any(|c| g.get(c, r) != pad);
                        assert_eq!(painted, want, "{what} {cols}x{rows} {tier:?} row {r} painted");
                    }
                }
                let mut g: Grid<Cell> = Grid::new(cols, rows);
                let ui = draw_hint_overlay(&mut g, scale) | draw_info_overlay(&mut g, " clip ", scale);
                assert!(!ui.contains(rows - 1), "hints alone leave the bottom line to the picture");
                assert!(ui.contains(rows - h - 1) && ui.contains(rows - 2 * h - 1));
            }
        }
        assert!(UiRows::NONE.is_empty() && (0..100).all(|r| !UiRows::NONE.contains(r)));
        let mut g: Grid<Cell> = Grid::new(0, 5);
        assert!(draw_hint_overlay(&mut g, OverlayScale::Normal).is_empty(), "nothing drawn, no UI");
        let mut g: Grid<Cell> = Grid::new(400, 2);
        assert!(draw_hint_overlay(&mut g, OverlayScale::Big).is_empty(), "a band that does not fit");
        let mut card: Grid<Cell> = Grid::new(8, 3);
        draw_enlarge_card(&mut card, pad);
        assert!(card.as_slice().iter().all(|c| c.attrs == attrs::DEFAULT_BG));
        assert!(card.as_slice().iter().any(|c| c.glyph() == 'A'), "the card still reads");
    }

    #[test]
    fn info_row_reads_the_grid_size_and_the_zoom_hint_on_narrow_grids() {
        let text = " clip   style: pixels   settings: default ";
        let draw = |cols: u16, rows: u16| -> Grid<Cell> {
            let mut g = Grid::new(cols, rows);
            draw_info_overlay(&mut g, text, OverlayScale::Normal);
            g
        };
        let zoom = format!(" {ZOOM_KEY} - to zoom out: more cells, a sharper picture ");

        let g = draw(80, 24);
        let info = row_text(&g, 21);
        assert!(info.starts_with(text), "{info:?}");
        assert!(info.ends_with(" 80x24 cells "), "size right-aligned: {info:?}");
        assert_eq!(info.len(), 80);
        assert_eq!(row_text(&g, 20).trim_end(), zoom.trim_end(), "zoom hint above the info row");
        assert!((0..80).all(|c| g.get(c, 19) == Cell::BLANK), "nothing above the zoom hint");
        assert!((0..80).all(|c| g.get(c, 22) == Cell::BLANK), "the hints row is not drawn here");

        assert!(row_text(&draw(159, 40), 36).contains("to zoom out"));
        let g = draw(160, 40);
        assert!(row_text(&g, 37).ends_with(" 160x40 cells "));
        assert!((0..160).all(|c| g.get(c, 36) == Cell::BLANK), "no hint at 160 columns");
        let g = draw(213, 58);
        assert!(row_text(&g, 55).ends_with(" 213x58 cells "));
        assert!((0..213).all(|c| g.get(c, 54) == Cell::BLANK));

        let short = format!(" {ZOOM_KEY} - for a sharper picture ");
        assert_eq!(zoom_line(zoom.len() as u16), zoom);
        assert_eq!(zoom_line(zoom.len() as u16 - 1), short);
        assert_eq!(zoom_line(short.len() as u16), short);
        assert_eq!(zoom_line(short.len() as u16 - 1), "");
        let g = draw(32, 9);
        assert_eq!(row_text(&g, 5).trim_end(), short.trim_end(), "32x9 keeps the short hint");
        assert_eq!(ZOOM_KEY, if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" });
    }

    #[test]
    fn info_text_outranks_the_size_block() {
        let text = " a-very-long-clip-name-from-a-stitch   style: letters   settings: s to save ";
        let mut g = Grid::new(80, 24);
        draw_info_overlay(&mut g, text, OverlayScale::Normal);
        let info = row_text(&g, 21);
        assert!(info.starts_with(text) && !info.contains("cells"), "{info:?}");
        let cols = (text.len() + " 80x24 cells ".len()) as u16;
        let mut g = Grid::new(cols, 24);
        draw_info_overlay(&mut g, text, OverlayScale::Normal);
        assert!(row_text(&g, 21).ends_with(&format!(" {cols}x24 cells ")));
    }

    #[test]
    fn big_overlay_text_reads_back_in_its_bands() {
        let (cols, rows) = (320u16, 90u16);
        let scale = OverlayScale::for_grid(cols, rows, GlyphTier::UnicodeBlocks);
        assert_eq!(scale, OverlayScale::Big);
        let mut g = Grid::new(cols, rows);
        draw_progress_overlay_clips(&mut g, 900, 5400, 30.0, None, true, scale);
        draw_hint_overlay(&mut g, scale);
        draw_info_overlay(&mut g, " The Architect   style: letters ", scale);

        let chars = scale.line_chars(cols) as usize;
        let progress = read_big_line(&g, 0).expect("progress row in the font");
        assert_eq!(progress.len(), chars);
        assert!(progress.starts_with(" <- 5S ->  0:30 / 3:00 [==="), "{progress:?}");
        assert!(progress.contains('|') && progress.ends_with(" PAUSED "), "{progress:?}");
        let hints = read_big_line(&g, 1).unwrap();
        assert_eq!(hints.trim_end(), hint_line(80).to_uppercase().trim_end());
        let info = read_big_line(&g, 2).unwrap();
        assert!(info.starts_with(" THE ARCHITECT   STYLE: LETTERS "), "{info:?}");
        assert!(info.ends_with(" 320X90 CELLS "), "{info:?}");
        let top = rows - 3 * BIG_LINE_ROWS;
        for row in 0..top {
            assert!((0..cols).all(|c| g.get(c, row) == Cell::BLANK), "row {row} touched");
        }
        draw_dial_overlay(&mut g, "shadow lift", 64, 255, scale);
        let dial = read_big_line(&g, 0).unwrap();
        assert!(dial.starts_with(" SHADOW LIFT [####") && dial.ends_with("  64/255 "), "{dial:?}");

        let mut g = Grid::new(243, 40);
        draw_hint_overlay(&mut g, OverlayScale::Big);
        assert!(read_big_line(&g, 1).is_some());
        for r in 34..37 {
            assert_eq!(g.get(242, r).glyph(), ' ');
            assert_eq!(g.get(242, r).bg, Rgb::new(24, 24, 40), "tail painted as panel");
        }
    }

    #[test]
    fn big_font_shapes_are_distinct() {
        for (i, a) in BIG_FONT.iter().enumerate() {
            assert!(*a < 1 << 15, "glyph {i} has a sixth row");
            for b in &BIG_FONT[i + 1..] {
                assert_ne!(a, b, "glyph {:?} duplicated", char::from(i as u8 + 0x20));
            }
            assert_ne!(*a, BIG_BAR);
        }
        assert_eq!(big_glyph('a'), big_glyph('A'), "lower case folds to upper");
        assert_eq!(big_glyph('é'), big_glyph('?'), "non-ASCII draws as ?");
        assert_eq!(big_glyph('|'), BIG_BAR);
    }

    #[test]
    fn overlays_survive_tiny_grids() {
        let sizes = [
            (0u16, 0u16), (1, 1), (1, 3), (3, 1), (4, 3), (10, 3), (10, 4), (31, 8), (32, 9),
            (7, 12), (239, 36), (240, 35), (240, 8), (241, 2),
        ];
        for (cols, rows) in sizes {
            for scale in [OverlayScale::Normal, OverlayScale::Big] {
                let mut g = Grid::new(cols, rows);
                draw_progress_overlay_clips(&mut g, 3, 10, 30.0, Some((1, 2)), false, scale);
                draw_dial_overlay(&mut g, "edge", 7, 255, scale);
                draw_hint_overlay(&mut g, scale);
                draw_info_overlay(&mut g, " clip   style: pixels ", scale);
                assert_eq!((g.cols(), g.rows()), (cols, rows));
            }
        }
        let mut g = Grid::new(10, 3);
        draw_info_overlay(&mut g, " clip ", OverlayScale::Normal);
        assert_eq!(row_text(&g, 0), " clip     ");
        let mut g = Grid::new(240, 8);
        draw_info_overlay(&mut g, " clip ", OverlayScale::Big);
        assert!(g.as_slice().iter().all(|c| *c == Cell::BLANK), "slot 2 needs 9 rows");
    }

    #[test]
    fn m_toggles_sound_once_per_drain_and_quit_still_wins() {
        let mut backend = auto_ascii_term::SimBackend::new(80, 24);
        let (drained, _) = drain_backend_events(&mut backend);
        assert!(!drained.toggle_sound, "no key, no toggle");
        for _ in 0..3 {
            backend.push_event(Event::Key(Key::Char('m')));
        }
        let (drained, _) = drain_backend_events(&mut backend);
        assert!(drained.toggle_sound, "a held m is one toggle, like space");
        assert!(!drained.save && !drained.toggle_pause && drained.style_cycle == 0, "m is its own key");
        backend.push_event(Event::Key(Key::Char('m')));
        backend.push_event(Event::Quit);
        let (drained, _) = drain_backend_events(&mut backend);
        assert!(drained.quit && !drained.toggle_sound);
    }

    #[test]
    fn hint_line_fits_m_sound_between_style_and_save() {
        assert!(hint_line(108).contains("/ style   m sound   s save   v controls"), "{:?}", hint_line(108));
        assert_eq!(hint_line(108).len(), 108, "the full row is exactly 108 columns");
        assert!(hint_line(89).ends_with("[ ] adjust   m sound   v controls "), "{:?}", hint_line(89));
        assert!(!hint_line(88).contains("m sound"));
        assert_eq!(hint_line(80), hint_line(88), "80 columns keep the M6 row");
    }
}
