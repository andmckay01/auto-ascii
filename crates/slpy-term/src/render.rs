//! Shared frame painter (crate-private): tier quantize → per-row diff →
//! changed spans with the skip-vs-move heuristic → SGR run-length elision →
//! ONE reused ≥64 KB buffer (PLAN §3.6 step 6). Both `AnsiBackend` and
//! `SimBackend` present through this exact code — one render path, tested
//! everywhere (PLAN §3.1).
//!
//! **Quantize before diff (PLAN §3.1):** the incoming grid is quantized to
//! the caps color tier *first* (canonical tier RGB written back into the
//! cells), then diffed against the previous *quantized* grid. On 256/16/mono
//! tiers, cells that quantize equal are byte-equal PODs and produce zero
//! damage — a free byte reducer purely from ordering.

use slpy_core::{Cell, Grid, Rgb};

use crate::caps::ColorTier;
use crate::quant;

/// Rewrite up to this many *unchanged* cells between two changed spans rather
/// than emit a cursor move — a CUP costs ~8–14 B while ≤6 rewritten cells
/// usually cost less (PLAN §3.6 step 6 "skip-vs-move heuristic").
const SKIP_MAX: usize = 6;

/// The reused output buffer never has less capacity than this
/// (PLAN §3.6: "single `write(2)` from a reused ≥64 KB buffer").
pub(crate) const MIN_BUF: usize = 64 * 1024;

/// Worst-case bytes per emitted cell: fg SGR (≤19) + joined bg SGR (≤18) +
/// UTF-8 glyph (≤4), rounded up to amortize span CUPs (≤14 B each, at most
/// one per ≥7-cell gap) and the ?2026 wrap (16 B/frame). Reserving
/// `cells × this` at `resize()` keeps `paint()` allocation-free even on
/// adversarial frames.
const WORST_CELL_BYTES: usize = 48;

/// DEC 2026 synchronized-output wrap (PLAN §3.6 step 6): emitted around every
/// non-empty frame when DECRQM confirmed support (`Caps::sync_2026`).
pub(crate) const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
pub(crate) const SYNC_END: &[u8] = b"\x1b[?2026l";

/// Diff state + frame assembly buffer shared by both backends.
pub(crate) struct FramePainter {
    /// Previous presented grid, post-quantization — the diff baseline
    /// (PLAN §3.1 "diffs against the previous quantized grid").
    prev: Grid<Cell>,
    /// Scratch for the current frame's quantized cells (unused on `True`,
    /// where quantization is the identity and the caller's grid is diffed
    /// directly — byte-identical to the M0 truecolor path).
    quantized: Grid<Cell>,
    /// The single reused frame buffer; one write per `present` (PLAN §3.6).
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

    /// The ONLY allocation point in the hot path (PLAN §3.1): reallocates the
    /// diff baseline + quantize scratch and grows the frame buffer; implies
    /// `invalidate`.
    pub(crate) fn resize(&mut self, cols: u16, rows: u16) {
        self.prev.resize(cols, rows);
        self.quantized.resize(cols, rows);
        self.reserve_for(cols, rows);
        self.invalidate_next = true;
    }

    /// Force a full repaint on the next `paint` (the "full repaint mode" is
    /// just this every frame, PLAN §3.1/§7).
    pub(crate) fn invalidate(&mut self) {
        self.invalidate_next = true;
    }

    fn reserve_for(&mut self, cols: u16, rows: u16) {
        let want = (cols as usize * rows as usize * WORST_CELL_BYTES).max(MIN_BUF);
        if self.buf.capacity() < want {
            self.buf.reserve(want.saturating_sub(self.buf.len()));
        }
    }

    /// Quantize `grid` to `tier`, diff against the previously presented
    /// quantized grid and assemble the ANSI frame into `self.buf` (cleared
    /// first). When `sync_2026`, a non-empty frame is wrapped in
    /// `CSI ? 2026 h … l`; an empty frame stays empty (no bare wrap).
    /// Returns the number of cells emitted (changed cells plus rewritten
    /// ≤`SKIP_MAX` gap cells).
    ///
    /// Panics if `grid` dimensions differ from the backend's — callers must
    /// go through `resize()` (the only hot-path allocation point).
    pub(crate) fn paint(&mut self, grid: &Grid<Cell>, tier: ColorTier, sync_2026: bool) -> u32 {
        assert!(
            grid.cols() == self.prev.cols() && grid.rows() == self.prev.rows(),
            "present(): grid is {}x{} but backend is {}x{} — call resize() first (PLAN §3.1)",
            grid.cols(),
            grid.rows(),
            self.prev.cols(),
            self.prev.rows(),
        );

        // Quantize BEFORE diff (PLAN §3.1). True tier is the identity — diff
        // the caller's grid directly (no copy, M0 byte parity).
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
        // SGR state trackers reset per frame: the first colored cell always
        // re-emits, so frames never depend on terminal state left by earlier
        // frames (and full repaints are byte-reproducible).
        let mut fg: Option<Rgb> = None;
        let mut bg: Option<Rgb> = None;
        let mut damage: u32 = 0;

        for r in 0..src.rows() {
            let pr = self.prev.row(r);
            let gr = src.row(r);
            // Cell is 12-B POD (PLAN §3.1) — whole-row slice compare is the
            // fast path that skips clean rows outright.
            if !full && pr == gr {
                continue;
            }
            let mut c = 0usize;
            while c < w {
                if !full && pr[c] == gr[c] {
                    c += 1;
                    continue;
                }
                // Span start; absorb gaps of ≤ SKIP_MAX unchanged cells.
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
            // Nothing damaged: emit nothing at all (no bare ?2026 wrap).
            self.buf.clear();
        } else if sync_2026 {
            self.buf.extend_from_slice(SYNC_END);
        }

        self.prev.as_mut_slice().copy_from_slice(src.as_slice());
        self.invalidate_next = false;
        damage
    }
}

/// Quantize every cell of `src` into `dst` as *canonical* tier RGB
/// (PLAN §3.1): C256 → xterm cube/gray canonical color, C16 → standard-16
/// canonical color, Mono → fg WHITE / bg BLACK (color carries nothing; the
/// glyph is the signal). Glyph and attrs pass through untouched.
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

/// Append `v` in decimal (no allocation, no `fmt` machinery).
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

/// CUP — cursor position, 1-based row;col.
fn emit_cup(buf: &mut Vec<u8>, row: u16, col: u16) {
    buf.extend_from_slice(b"\x1b[");
    push_dec(buf, u32::from(row) + 1);
    buf.push(b';');
    push_dec(buf, u32::from(col) + 1);
    buf.push(b'H');
}

/// One cell: SGR only when fg/bg actually change (run-length elision,
/// PLAN §3.6 step 6), then the UTF-8 glyph. fg and bg changes share one CSI.
/// SGR form per tier (PLAN §3.1): `38;2;R;G;B` / `38;5;N` / `30–37;90–97`
/// (bg `40–47;100–107`) / none at all on Mono. Cells arrive already
/// quantized to canonical tier RGB, so index re-derivation is exact.
fn emit_cell(
    buf: &mut Vec<u8>,
    cell: &Cell,
    fg: &mut Option<Rgb>,
    bg: &mut Option<Rgb>,
    tier: ColorTier,
) {
    if tier != ColorTier::Mono {
        let fg_new = *fg != Some(cell.fg);
        let bg_new = *bg != Some(cell.bg);
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
                emit_color(buf, cell.bg, tier, true);
                *bg = Some(cell.bg);
            }
            buf.push(b'm');
        }
    }
    let mut utf8 = [0u8; 4];
    buf.extend_from_slice(cell.glyph().encode_utf8(&mut utf8).as_bytes());
}

/// SGR parameter bytes for one color at `tier` (no CSI/`m` framing).
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

    /// M1: all four tiers paint (the M0 `unimplemented!` is gone).
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
