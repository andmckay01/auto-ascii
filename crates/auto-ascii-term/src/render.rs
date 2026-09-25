use auto_ascii_core::cell::attrs;
use auto_ascii_core::{Cell, Grid, Rgb};

use crate::caps::ColorTier;
use crate::quant;

const SKIP_MAX: usize = 6;

pub(crate) const MIN_BUF: usize = 64 * 1024;

const WORST_CELL_BYTES: usize = 48;

pub(crate) const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
pub(crate) const SYNC_END: &[u8] = b"\x1b[?2026l";

pub(crate) struct FramePainter {
    prev: Grid<Cell>,
    quantized: Grid<Cell>,
    pub(crate) buf: Vec<u8>,
    invalidate_next: bool,
}

impl FramePainter {
    pub(crate) fn new(cols: u16, rows: u16) -> FramePainter {
        let mut painter = FramePainter {
            prev: Grid::new(cols, rows),
            quantized: Grid::new(cols, rows),
            buf: Vec::with_capacity(MIN_BUF),
            invalidate_next: true,
        };
        painter.reserve_for(cols, rows);
        painter
    }

    pub(crate) fn resize(&mut self, cols: u16, rows: u16) {
        self.prev.resize(cols, rows);
        self.quantized.resize(cols, rows);
        self.reserve_for(cols, rows);
        self.invalidate_next = true;
    }

    pub(crate) fn invalidate(&mut self) {
        self.invalidate_next = true;
    }

    fn reserve_for(&mut self, cols: u16, rows: u16) {
        let want = (cols as usize * rows as usize * WORST_CELL_BYTES).max(MIN_BUF);
        if self.buf.capacity() < want {
            self.buf.reserve(want.saturating_sub(self.buf.len()));
        }
    }

    pub(crate) fn paint(&mut self, grid: &Grid<Cell>, tier: ColorTier, sync_2026: bool) -> u32 {
        assert!(
            grid.cols() == self.prev.cols() && grid.rows() == self.prev.rows(),
            "present(): grid is {}x{} but backend is {}x{} — call resize() first (PLAN §3.1)",
            grid.cols(),
            grid.rows(),
            self.prev.cols(),
            self.prev.rows(),
        );

        let src: &Grid<Cell> = if tier == ColorTier::True {
            grid
        } else {
            quantize_into(grid, &mut self.quantized, tier);
            &self.quantized
        };

        self.buf.clear();
        if sync_2026 {
            self.buf.extend_from_slice(SYNC_BEGIN);
        }
        let prologue = self.buf.len();

        let full = self.invalidate_next;
        let w = src.cols() as usize;
        let mut fg: Option<Rgb> = None;
        let mut bg: Option<Option<Rgb>> = None;
        let mut damage: u32 = 0;

        for r in 0..src.rows() {
            let pr = self.prev.row(r);
            let gr = src.row(r);
            if !full && pr == gr {
                continue;
            }
            let mut c = 0usize;
            while c < w {
                if !full && pr[c] == gr[c] {
                    c += 1;
                    continue;
                }
                let start = c;
                let mut end = c + 1;
                c += 1;
                if full {
                    end = w;
                    c = w;
                } else {
                    let mut gap = 0usize;
                    while c < w {
                        if pr[c] != gr[c] {
                            end = c + 1;
                            gap = 0;
                        } else {
                            gap += 1;
                            if gap > SKIP_MAX {
                                c += 1;
                                break;
                            }
                        }
                        c += 1;
                    }
                }
                emit_cup(&mut self.buf, r, start as u16);
                for cell in &gr[start..end] {
                    emit_cell(&mut self.buf, cell, &mut fg, &mut bg, tier);
                }
                damage += (end - start) as u32;
            }
        }

        if self.buf.len() == prologue {
            self.buf.clear();
        } else if sync_2026 {
            self.buf.extend_from_slice(SYNC_END);
        }

        self.prev.as_mut_slice().copy_from_slice(src.as_slice());
        self.invalidate_next = false;
        damage
    }
}

fn quantize_into(src: &Grid<Cell>, dst: &mut Grid<Cell>, tier: ColorTier) {
    debug_assert!(src.cols() == dst.cols() && src.rows() == dst.rows());
    let d = dst.as_mut_slice();
    for (i, cell) in src.as_slice().iter().enumerate() {
        let mut q = *cell;
        match tier {
            ColorTier::True => {}
            ColorTier::C256 => {
                q.fg = quant::ansi256_to_rgb(quant::rgb_to_256(cell.fg));
                q.bg = quant::ansi256_to_rgb(quant::rgb_to_256(cell.bg));
            }
            ColorTier::C16 => {
                q.fg = quant::ansi16_to_rgb(quant::rgb_to_16(cell.fg));
                q.bg = quant::ansi16_to_rgb(quant::rgb_to_16(cell.bg));
            }
            ColorTier::Mono => {
                q.fg = Rgb::WHITE;
                q.bg = Rgb::BLACK;
            }
        }
        d[i] = q;
    }
}

fn push_dec(buf: &mut Vec<u8>, mut v: u32) {
    let mut tmp = [0u8; 10];
    let mut i = tmp.len();
    loop {
        i -= 1;
        tmp[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    buf.extend_from_slice(&tmp[i..]);
}

fn emit_cup(buf: &mut Vec<u8>, row: u16, col: u16) {
    buf.extend_from_slice(b"\x1b[");
    push_dec(buf, u32::from(row) + 1);
    buf.push(b';');
    push_dec(buf, u32::from(col) + 1);
    buf.push(b'H');
}

fn emit_cell(
    buf: &mut Vec<u8>,
    cell: &Cell,
    fg: &mut Option<Rgb>,
    bg: &mut Option<Option<Rgb>>,
    tier: ColorTier,
) {
    if tier != ColorTier::Mono {
        let want_bg = (cell.attrs & attrs::DEFAULT_BG == 0).then_some(cell.bg);
        let fg_new = *fg != Some(cell.fg);
        let bg_new = *bg != Some(want_bg);
        if fg_new || bg_new {
            buf.extend_from_slice(b"\x1b[");
            if fg_new {
                emit_color(buf, cell.fg, tier, false);
                *fg = Some(cell.fg);
            }
            if bg_new {
                if fg_new {
                    buf.push(b';');
                }
                match want_bg {
                    Some(c) => emit_color(buf, c, tier, true),
                    None => buf.extend_from_slice(b"49"),
                }
                *bg = Some(want_bg);
            }
            buf.push(b'm');
        }
    }
    let mut utf8 = [0u8; 4];
    buf.extend_from_slice(cell.glyph().encode_utf8(&mut utf8).as_bytes());
}

fn emit_color(buf: &mut Vec<u8>, color: Rgb, tier: ColorTier, is_bg: bool) {
    match tier {
        ColorTier::True => {
            buf.extend_from_slice(if is_bg { b"48;2;" } else { b"38;2;" });
            push_dec(buf, u32::from(color.r));
            buf.push(b';');
            push_dec(buf, u32::from(color.g));
            buf.push(b';');
            push_dec(buf, u32::from(color.b));
        }
        ColorTier::C256 => {
            buf.extend_from_slice(if is_bg { b"48;5;" } else { b"38;5;" });
            push_dec(buf, u32::from(quant::rgb_to_256(color)));
        }
        ColorTier::C16 => {
            let n = u32::from(quant::rgb_to_16(color));
            let code = match (n < 8, is_bg) {
                (true, false) => 30 + n,
                (false, false) => 90 + (n - 8),
                (true, true) => 40 + n,
                (false, true) => 100 + (n - 8),
            };
            push_dec(buf, code);
        }
        ColorTier::Mono => unreachable!("mono emits no color SGR"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_dec_formats() {
        for (v, s) in [(0u32, "0"), (7, "7"), (10, "10"), (255, "255"), (65535, "65535")] {
            let mut buf = Vec::new();
            push_dec(&mut buf, v);
            assert_eq!(buf, s.as_bytes());
        }
    }

    #[test]
    fn buffer_starts_at_64k() {
        let painter = FramePainter::new(1, 1);
        assert!(painter.buf.capacity() >= MIN_BUF);
    }

    #[test]
    #[should_panic(expected = "call resize() first")]
    fn dims_mismatch_panics() {
        let mut painter = FramePainter::new(4, 2);
        let grid: Grid<Cell> = Grid::new(5, 2);
        painter.paint(&grid, ColorTier::True, false);
    }

    #[test]
    fn default_bg_cells_emit_sgr_49_and_never_a_color() {
        let mut keep = Cell::new('x', Rgb::new(200, 40, 40), Rgb::BLACK);
        keep.attrs = attrs::DEFAULT_BG;
        for (tier, bg) in [
            (ColorTier::True, &b"48;2;0;0;0"[..]),
            (ColorTier::C256, &b"48;5;"[..]),
            (ColorTier::C16, &b";40m"[..]),
        ] {
            let mut painter = FramePainter::new(3, 1);
            let mut grid: Grid<Cell> = Grid::new(3, 1);
            grid.fill(keep);
            painter.paint(&grid, tier, false);
            let out = painter.buf.clone();
            assert!(out.windows(3).any(|w| w == b";49"), "{tier:?}");
            assert!(!out.windows(bg.len()).any(|w| w == bg), "{tier:?}");
            grid.set(1, 0, Cell::new('y', Rgb::new(200, 40, 40), Rgb::BLACK));
            painter.invalidate();
            painter.paint(&grid, tier, false);
            let n49 = painter.buf.windows(2).filter(|w| *w == b"49").count();
            assert_eq!(n49, 2, "{tier:?}: back to the default after a colored bg");
        }
    }

    #[test]
    fn all_tiers_paint() {
        for tier in [ColorTier::True, ColorTier::C256, ColorTier::C16, ColorTier::Mono] {
            let mut painter = FramePainter::new(2, 2);
            let mut grid: Grid<Cell> = Grid::new(2, 2);
            grid.fill(Cell::new('x', Rgb::new(200, 40, 40), Rgb::BLACK));
            let damage = painter.paint(&grid, tier, false);
            assert_eq!(damage, 4, "{tier:?}");
            assert!(!painter.buf.is_empty(), "{tier:?}");
        }
    }
}
