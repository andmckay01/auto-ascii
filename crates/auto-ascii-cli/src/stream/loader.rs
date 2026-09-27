//! The loading screen: real stages mapped to a monotonic 0-100 percent, and
//! a pure renderer for the bar (brightening left to right, a soft shimmer
//! band travelling across the filled part) with `loading...` centred under
//! it, obeying the active codec's glyph and background rules.

use auto_ascii::{Cell, Codec, ColorTier, Grid, Rgb};
use auto_ascii_core::GlyphTier;
use auto_ascii_core::cell::attrs;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LoadState {
    pub ytdlp_started: bool,
    pub entry_found: bool,
    pub media_ready: bool,
    pub decoders_spawned: bool,
    pub first_audio: bool,
    pub first_video: bool,
    pub audio_fill: f64,
    pub video_fill: f64,
}

impl LoadState {
    pub fn rebuffering(audio_fill: f64, video_fill: f64) -> LoadState {
        LoadState {
            ytdlp_started: true,
            entry_found: true,
            media_ready: true,
            decoders_spawned: true,
            first_audio: true,
            first_video: true,
            audio_fill,
            video_fill,
        }
    }
}

pub fn percent(s: &LoadState) -> u8 {
    let resolve = if s.media_ready {
        30
    } else if s.entry_found {
        15
    } else if s.ytdlp_started {
        5
    } else {
        0
    };
    if !s.media_ready {
        return resolve;
    }
    let first = u8::from(s.decoders_spawned) * 6 + u8::from(s.first_audio) * 7 + u8::from(s.first_video) * 7;
    if first < 20 {
        return resolve + first;
    }
    let clean = |f: f64| if f.is_nan() { 0.0 } else { f.clamp(0.0, 1.0) };
    let fill = clean(s.audio_fill).min(clean(s.video_fill));
    50 + (fill * 50.0).floor() as u8
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    shown: u8,
}

impl Progress {
    pub fn update(&mut self, pct: u8) -> u8 {
        self.shown = self.shown.max(pct.min(100));
        self.shown
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyphs {
    AsciiDensity,
    UnicodeSolid,
    UnicodeDensity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoaderStyle {
    pub glyphs: Glyphs,
    pub default_bg: bool,
    pub pad: Cell,
}

impl LoaderStyle {
    pub fn for_codec(codec: Codec, glyph_tier: GlyphTier, color: ColorTier) -> LoaderStyle {
        let pad = codec.pad();
        let default_bg = pad.attrs & attrs::DEFAULT_BG != 0;
        let glyphs = if default_bg || glyph_tier == GlyphTier::Ascii {
            Glyphs::AsciiDensity
        } else if matches!(color, ColorTier::True | ColorTier::C256) {
            Glyphs::UnicodeSolid
        } else {
            Glyphs::UnicodeDensity
        };
        LoaderStyle { glyphs, default_bg, pad }
    }
}

pub const LOADING: &str = "loading...";

const ASCII_DENSITY: [char; 8] = [':', '-', '=', '+', '*', '#', '%', '@'];
const UNICODE_DENSITY: [char; 4] = ['░', '▒', '▓', '█'];
const DIM: u32 = 70;
const BRIGHT: u32 = 255;
const EMPTY_GRAY: u8 = 58;
const SHIMMER_GAIN: u32 = 72;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BarLayout {
    pub row: u16,
    pub left: u16,
    pub width: u16,
    pub percent_col: Option<u16>,
    pub text_row: Option<u16>,
    pub title_row: Option<u16>,
}

pub fn layout(cols: u16, rows: u16) -> Option<BarLayout> {
    if cols == 0 || rows == 0 {
        return None;
    }
    let (width, with_pct) = if cols >= 12 {
        ((cols * 3 / 5).clamp(6, 72).min(cols - 5), true)
    } else if cols > 5 {
        (cols - 5, true)
    } else {
        (cols, false)
    };
    let total = width + if with_pct { 5 } else { 0 };
    let left = (cols - total) / 2;
    let row = rows.saturating_sub(2) / 2;
    let text_row = (row + 1 < rows).then_some(row + 1);
    let title_row = (row >= 2 && rows >= 7).then(|| row - 2);
    Some(BarLayout {
        row,
        left,
        width,
        percent_col: with_pct.then_some(left + width + 1),
        text_row,
        title_row,
    })
}

pub fn filled_cells(width: u16, pct: u8) -> u16 {
    (u32::from(width) * u32::from(pct.min(100)) / 100) as u16
}

pub fn cell_brightness(i: u16, filled: u16, tick: Option<u32>) -> u8 {
    if i >= filled {
        return EMPTY_GRAY;
    }
    let base = DIM + (BRIGHT - DIM) * (u32::from(i) + 1) / u32::from(filled);
    let boost = match tick {
        None => 0,
        Some(t) => {
            let half = u32::from((filled / 8).max(2));
            let period = u32::from(filled) + 2 * half;
            let center = (t / 2) % period;
            let pos = u32::from(i) + half;
            let dist = pos.abs_diff(center);
            if dist < half { SHIMMER_GAIN * (half - dist) / half } else { 0 }
        }
    };
    (base + boost).min(255) as u8
}

fn tint(b: u8) -> Rgb {
    let b = u32::from(b);
    Rgb::new((b * 200 / 255) as u8, (b * 228 / 255) as u8, b as u8)
}

fn density(ramp: &[char], b: u8) -> char {
    let span = (BRIGHT - DIM) as usize;
    let v = usize::from(b).saturating_sub(DIM as usize).min(span);
    ramp[(v * (ramp.len() - 1) + span / 2) / span]
}

fn put(grid: &mut Grid<Cell>, style: &LoaderStyle, col: u16, row: u16, ch: char, fg: Rgb) {
    if col >= grid.cols() || row >= grid.rows() {
        return;
    }
    let ch = if style.glyphs == Glyphs::AsciiDensity && !(ch == ' ' || ch.is_ascii_graphic()) {
        '?'
    } else {
        ch
    };
    let cell = if style.default_bg {
        Cell { ch: ch as u32, fg, bg: Rgb::BLACK, attrs: attrs::DEFAULT_BG }
    } else {
        Cell { ch: ch as u32, fg, ..style.pad }
    };
    grid.set(col, row, cell);
}

fn text(grid: &mut Grid<Cell>, style: &LoaderStyle, row: u16, line: &str, fg: Rgb) {
    let cols = usize::from(grid.cols());
    let clean: String = line
        .chars()
        .map(|c| if c == ' ' || c.is_ascii_graphic() { c } else { '?' })
        .take(cols)
        .collect();
    let left = (cols - clean.chars().count()) / 2;
    for (i, ch) in clean.chars().enumerate() {
        put(grid, style, (left + i) as u16, row, ch, fg);
    }
}

pub fn draw_loader(grid: &mut Grid<Cell>, pct: u8, tick: Option<u32>, style: &LoaderStyle, title: Option<&str>) {
    grid.fill(style.pad);
    let Some(lay) = layout(grid.cols(), grid.rows()) else {
        return;
    };
    let pct = pct.min(100);
    let filled = filled_cells(lay.width, pct);
    for i in 0..lay.width {
        let b = cell_brightness(i, filled, tick);
        let (ch, fg) = if i < filled {
            let ch = match style.glyphs {
                Glyphs::AsciiDensity => density(&ASCII_DENSITY, b),
                Glyphs::UnicodeSolid => '█',
                Glyphs::UnicodeDensity => density(&UNICODE_DENSITY, b),
            };
            (ch, tint(b))
        } else {
            let ch = match style.glyphs {
                Glyphs::UnicodeSolid => '░',
                Glyphs::AsciiDensity | Glyphs::UnicodeDensity => '.',
            };
            (ch, Rgb::gray(b))
        };
        put(grid, style, lay.left + i, lay.row, ch, fg);
    }
    if let Some(col) = lay.percent_col {
        for (j, ch) in format!("{pct:>3}%").chars().enumerate() {
            put(grid, style, col + j as u16, lay.row, ch, Rgb::gray(210));
        }
    }
    if let Some(row) = lay.text_row {
        text(grid, style, row, LOADING, Rgb::gray(160));
    }
    if let (Some(row), Some(title)) = (lay.title_row, title) {
        text(grid, style, row, title, Rgb::gray(190));
    }
}

pub fn grid_text(grid: &Grid<Cell>) -> String {
    let mut out = String::new();
    for row in 0..grid.rows() {
        let line: String = grid.row(row).iter().map(Cell::glyph).collect();
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use auto_ascii_term::{Backend, SimBackend};

    fn ascii_style(color: ColorTier) -> LoaderStyle {
        LoaderStyle::for_codec(Codec::Ascii, GlyphTier::UnicodeBlocks, color)
    }

    fn render(cols: u16, rows: u16, pct: u8, tick: Option<u32>, style: &LoaderStyle) -> Grid<Cell> {
        let mut grid = Grid::new(cols, rows);
        draw_loader(&mut grid, pct, tick, style, None);
        grid
    }

    fn luma(c: &Cell) -> u32 {
        u32::from(c.fg.r) + u32::from(c.fg.g) + u32::from(c.fg.b)
    }

    #[test]
    fn stages_map_to_fixed_bands() {
        let mut s = LoadState::default();
        assert_eq!(percent(&s), 0);
        s.ytdlp_started = true;
        assert_eq!(percent(&s), 5);
        s.entry_found = true;
        assert_eq!(percent(&s), 15);
        s.media_ready = true;
        assert_eq!(percent(&s), 30);
        s.decoders_spawned = true;
        assert_eq!(percent(&s), 36);
        s.first_audio = true;
        assert_eq!(percent(&s), 43);
        s.audio_fill = 1.0;
        assert_eq!(percent(&s), 43, "fill counts only once both streams delivered");
        s.first_video = true;
        assert_eq!(percent(&s), 50);
        s.video_fill = 0.5;
        assert_eq!(percent(&s), 75, "min(audio 1.0, video 0.5) of the 50-100 band");
        s.audio_fill = 0.2;
        assert_eq!(percent(&s), 60);
        s.audio_fill = 1.0;
        s.video_fill = 1.0;
        assert_eq!(percent(&s), 100);
        s.video_fill = 7.0;
        s.audio_fill = f64::NAN;
        assert_eq!(percent(&s), 50, "garbage fills stay in bounds");
        assert_eq!(percent(&LoadState::rebuffering(0.0, 0.9)), 50);
    }

    #[test]
    fn progress_is_bounded_and_monotonic_over_any_event_order() {
        let steps: [fn(&mut LoadState); 8] = [
            |s| s.ytdlp_started = true,
            |s| s.entry_found = true,
            |s| s.media_ready = true,
            |s| s.decoders_spawned = true,
            |s| s.first_video = true,
            |s| s.first_audio = true,
            |s| s.video_fill = 0.4,
            |s| s.audio_fill = 0.9,
        ];
        for rot in 0..steps.len() {
            let mut s = LoadState::default();
            let mut p = Progress::default();
            let mut last = 0;
            for k in 0..steps.len() {
                steps[(k + rot) % steps.len()](&mut s);
                let shown = p.update(percent(&s));
                assert!(shown >= last && shown <= 100);
                last = shown;
            }
            assert_eq!(p.update(percent(&s)), 70);
        }
        let mut p = Progress::default();
        assert_eq!(p.update(80), 80);
        assert_eq!(p.update(40), 80, "never moves backwards");
        assert_eq!(p.update(250), 100);
    }

    #[test]
    fn buffer_fill_is_proportional() {
        for f in 0..=20 {
            let fill = f64::from(f) / 20.0;
            let s = LoadState::rebuffering(fill, 1.0);
            assert_eq!(percent(&s), 50 + (fill * 50.0).floor() as u8);
        }
    }

    #[test]
    fn snapshot_80x24_at_42_percent() {
        let grid = render(80, 24, 42, None, &ascii_style(ColorTier::True));
        let text = grid_text(&grid);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 24);
        assert_eq!(
            lines[11],
            "             :---===+++**###%%%@@............................  42%"
        );
        assert_eq!(lines[12], "                                   loading...");
        assert!(lines.iter().enumerate().all(|(i, l)| i == 11 || i == 12 || l.is_empty()));
    }

    #[test]
    fn snapshot_unicode_blocks_and_tiny_widths() {
        let solid = LoaderStyle::for_codec(Codec::Pixels, GlyphTier::UnicodeBlocks, ColorTier::True);
        let text = grid_text(&render(40, 6, 50, None, &solid));
        assert_eq!(text, "\n\n     ████████████░░░░░░░░░░░░  50%\n               loading...\n\n\n");
        let dense = LoaderStyle::for_codec(Codec::Letters, GlyphTier::UnicodeBlocks, ColorTier::Mono);
        let text = grid_text(&render(20, 5, 100, None, &dense));
        assert_eq!(text, "\n ░░▒▒▒▒▓▓▓▓██ 100%\n     loading...\n\n\n");
        let text = grid_text(&render(20, 5, 0, None, &ascii_style(ColorTier::C16)));
        assert_eq!(text, "\n ............   0%\n     loading...\n\n\n");
    }

    #[test]
    fn brightness_rises_left_to_right_without_shimmer() {
        for style in [ascii_style(ColorTier::True), LoaderStyle::for_codec(Codec::Pixels, GlyphTier::UnicodeBlocks, ColorTier::True)] {
            for (cols, pct) in [(80, 42), (200, 100), (30, 7), (120, 99)] {
                let grid = render(cols, 10, pct, None, &style);
                let lay = layout(cols, 10).unwrap();
                let filled = filled_cells(lay.width, pct);
                assert!(filled > 0);
                let row = grid.row(lay.row);
                let lit: Vec<u32> = (0..filled).map(|i| luma(&row[usize::from(lay.left + i)])).collect();
                assert!(lit.windows(2).all(|w| w[0] <= w[1]), "{cols} {pct}: {lit:?}");
                assert!(lit[0] < lit[lit.len() - 1] || filled == 1);
                let empty = luma(&row[usize::from(lay.left + lay.width - 1)]);
                if filled < lay.width {
                    assert_eq!(empty, 3 * u32::from(EMPTY_GRAY), "empty cells stay dim and neutral");
                }
            }
        }
        let ascii = render(80, 24, 100, None, &ascii_style(ColorTier::Mono));
        let lay = layout(80, 24).unwrap();
        let glyphs: Vec<usize> = ascii.row(lay.row)[usize::from(lay.left)..][..usize::from(lay.width)]
            .iter()
            .map(|c| ASCII_DENSITY.iter().position(|&g| g as u32 == c.ch).unwrap())
            .collect();
        assert!(glyphs.windows(2).all(|w| w[0] <= w[1]), "glyph density carries it on mono: {glyphs:?}");
        assert_eq!((glyphs[0], glyphs[glyphs.len() - 1]), (0, ASCII_DENSITY.len() - 1));
    }

    #[test]
    fn the_shimmer_band_travels_with_the_tick() {
        let filled = 40;
        let peak = |t: u32| {
            let base: Vec<u8> = (0..filled).map(|i| cell_brightness(i, filled, None)).collect();
            (0..filled)
                .max_by_key(|&i| i32::from(cell_brightness(i, filled, Some(t))) - i32::from(base[usize::from(i)]))
                .unwrap()
        };
        let a = peak(20);
        let b = peak(40);
        let c = peak(60);
        assert!(a < b && b < c, "{a} {b} {c}");
        assert_eq!(cell_brightness(filled, filled, Some(20)), EMPTY_GRAY, "no shimmer on empty cells");
        let lit = (0..filled).filter(|&i| cell_brightness(i, filled, Some(40)) > cell_brightness(i, filled, None)).count();
        assert!(lit > 0 && lit < usize::from(filled), "a band, not a flash: {lit}");
    }

    #[test]
    fn loading_is_centred_under_the_bar() {
        for (cols, rows) in [(80, 24), (81, 25), (200, 60), (20, 5), (12, 3)] {
            let grid = render(cols, rows, 33, Some(5), &ascii_style(ColorTier::True));
            let lay = layout(cols, rows).unwrap();
            let row = lay.text_row.unwrap();
            assert_eq!(row, lay.row + 1);
            let line: String = grid.row(row).iter().map(Cell::glyph).collect();
            let start = line.find(LOADING).expect("text present");
            let right = usize::from(cols) - start - LOADING.len();
            assert!(start.abs_diff(right) <= 1, "{cols}x{rows}: {line:?}");
        }
    }

    #[test]
    fn the_ascii_codec_loader_is_printable_ascii_on_the_default_background() {
        for tier in [ColorTier::True, ColorTier::C256, ColorTier::C16, ColorTier::Mono] {
            let style = ascii_style(tier);
            for (pct, tick) in [(0, None), (42, Some(9)), (100, Some(77))] {
                let mut grid = Grid::new(64, 12);
                draw_loader(&mut grid, pct, tick, &style, Some("Café — über 🎥"));
                for c in grid.as_slice() {
                    let ch = char::from_u32(c.ch).unwrap();
                    assert!(ch == ' ' || ch.is_ascii_graphic(), "{ch:?}");
                    assert_ne!(c.attrs & attrs::DEFAULT_BG, 0);
                }
                let mut sim = SimBackend::new(64, 12);
                let mut caps = sim.caps().clone();
                caps.color = tier;
                sim.set_caps(caps);
                sim.present(&grid);
                let bytes = String::from_utf8(sim.take_output()).unwrap();
                assert!(!bytes.contains("48;"), "{tier:?}: background SGR in {bytes:?}");
                assert!(bytes.is_ascii());
            }
        }
    }

    #[test]
    fn tiny_and_empty_terminals_do_not_panic() {
        for style in [
            ascii_style(ColorTier::True),
            LoaderStyle::for_codec(Codec::Pixels, GlyphTier::UnicodeBlocks, ColorTier::C16),
        ] {
            for cols in 0..24 {
                for rows in 0..6 {
                    for pct in [0, 1, 50, 99, 100] {
                        let mut grid = Grid::new(cols, rows);
                        draw_loader(&mut grid, pct, Some(cols as u32 * 7), &style, Some("a title"));
                    }
                }
            }
        }
        let grid = render(20, 5, 42, Some(3), &ascii_style(ColorTier::True));
        let text = grid_text(&grid);
        assert!(text.contains("loading...") && text.contains("42%"), "{text}");
    }
}
