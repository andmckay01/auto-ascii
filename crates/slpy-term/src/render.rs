//! Shared M0 frame painter (crate-private): per-row diff → changed spans with
//! the skip-vs-move heuristic → SGR run-length elision → ONE reused ≥64 KB
//! buffer (PLAN §3.6 step 6). Both `AnsiBackend` and `SimBackend` present
//! through this exact code — one render path, tested everywhere (PLAN §3.1).

use slpy_core::{Cell, Grid, Rgb};

use crate::caps::ColorTier;

/// Rewrite up to this many *unchanged* cells between two changed spans rather
/// than emit a cursor move — a CUP costs ~8–14 B while ≤6 rewritten cells
/// usually cost less (PLAN §3.6 step 6 "skip-vs-move heuristic").
const SKIP_MAX: usize = 6;

/// The reused output buffer never has less capacity than this
/// (PLAN §3.6: "single `write(2)` from a reused ≥64 KB buffer").
pub(crate) const MIN_BUF: usize = 64 * 1024;

/// Worst-case bytes per emitted cell: fg SGR (≤19) + joined bg SGR (≤18) +
/// UTF-8 glyph (≤4), rounded up to amortize span CUPs (≤14 B each, at most
/// one per ≥7-cell gap). Reserving `cells × this` at `resize()` keeps
/// `paint()` allocation-free even on adversarial frames.
const WORST_CELL_BYTES: usize = 48;

/// Diff state + frame assembly buffer shared by both backends.
pub(crate) struct FramePainter {
    /// Previous presented grid — the diff baseline (PLAN §3.1).
    prev: Grid<Cell>,
    /// The single reused frame buffer; one write per `present` (PLAN §3.6).
    pub(crate) buf: Vec<u8>,
    invalidate_next: bool,
}

impl FramePainter {
    pub(crate) fn new(cols: u16, rows: u16) -> FramePainter {
        let mut painter = FramePainter {
            prev: Grid::new(cols, rows),
            buf: Vec::with_capacity(MIN_BUF),
            invalidate_next: true,
        };
        painter.reserve_for(cols, rows);
        painter
    }

    /// The ONLY allocation point in the hot path (PLAN §3.1): reallocates the
    /// diff baseline and grows the frame buffer; implies `invalidate`.
    pub(crate) fn resize(&mut self, cols: u16, rows: u16) {
        self.prev.resize(cols, rows);
        self.reserve_for(cols, rows);
        self.invalidate_next = true;
    }

    /// Force a full repaint on the next `paint` (M0 default mode does this
    /// every frame, PLAN §7).
    pub(crate) fn invalidate(&mut self) {
        self.invalidate_next = true;
    }

    fn reserve_for(&mut self, cols: u16, rows: u16) {
        let want = (cols as usize * rows as usize * WORST_CELL_BYTES).max(MIN_BUF);
        if self.buf.capacity() < want {
            self.buf.reserve(want.saturating_sub(self.buf.len()));
        }
    }

    /// Diff `grid` against the previously presented grid and assemble the
    /// ANSI frame into `self.buf` (cleared first). Returns the number of
    /// cells emitted (changed cells plus rewritten ≤`SKIP_MAX` gap cells).
    ///
    /// M0 is truecolor-only: the 256/16/mono tiers land with
    /// quantize-before-diff at M1 and are cleanly `unimplemented!` here.
    ///
    /// Panics if `grid` dimensions differ from the backend's — callers must
    /// go through `resize()` (the only hot-path allocation point).
    pub(crate) fn paint(&mut self, grid: &Grid<Cell>, tier: ColorTier) -> u32 {
        match tier {
            ColorTier::True => {}
            other => unimplemented!(
                "color tier {other:?} lands at M1 — M0 renders truecolor only (PLAN §7)"
            ),
        }
        assert!(
            grid.cols() == self.prev.cols() && grid.rows() == self.prev.rows(),
            "present(): grid is {}x{} but backend is {}x{} — call resize() first (PLAN §3.1)",
            grid.cols(),
            grid.rows(),
            self.prev.cols(),
            self.prev.rows(),
        );

        self.buf.clear();
        let full = self.invalidate_next;
        let w = grid.cols() as usize;
        // SGR state trackers reset per frame: the first colored cell always
        // re-emits, so frames never depend on terminal state left by earlier
        // frames (and full repaints are byte-reproducible).
        let mut fg: Option<Rgb> = None;
        let mut bg: Option<Rgb> = None;
        let mut damage: u32 = 0;

        for r in 0..grid.rows() {
            let pr = self.prev.row(r);
            let gr = grid.row(r);
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
                    emit_cell(&mut self.buf, cell, &mut fg, &mut bg);
                }
                damage += (end - start) as u32;
            }
        }

        self.prev.as_mut_slice().copy_from_slice(grid.as_slice());
        self.invalidate_next = false;
        damage
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
fn emit_cell(buf: &mut Vec<u8>, cell: &Cell, fg: &mut Option<Rgb>, bg: &mut Option<Rgb>) {
    let fg_new = *fg != Some(cell.fg);
    let bg_new = *bg != Some(cell.bg);
    if fg_new || bg_new {
        buf.extend_from_slice(b"\x1b[");
        if fg_new {
            buf.extend_from_slice(b"38;2;");
            push_dec(buf, u32::from(cell.fg.r));
            buf.push(b';');
            push_dec(buf, u32::from(cell.fg.g));
            buf.push(b';');
            push_dec(buf, u32::from(cell.fg.b));
            *fg = Some(cell.fg);
        }
        if bg_new {
            if fg_new {
                buf.push(b';');
            }
            buf.extend_from_slice(b"48;2;");
            push_dec(buf, u32::from(cell.bg.r));
            buf.push(b';');
            push_dec(buf, u32::from(cell.bg.g));
            buf.push(b';');
            push_dec(buf, u32::from(cell.bg.b));
            *bg = Some(cell.bg);
        }
        buf.push(b'm');
    }
    let mut utf8 = [0u8; 4];
    buf.extend_from_slice(cell.glyph().encode_utf8(&mut utf8).as_bytes());
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
        painter.paint(&grid, ColorTier::True);
    }

    #[test]
    #[should_panic(expected = "lands at M1")]
    fn non_true_tier_is_unimplemented() {
        let mut painter = FramePainter::new(2, 2);
        let grid: Grid<Cell> = Grid::new(2, 2);
        painter.paint(&grid, ColorTier::C256);
    }
}
