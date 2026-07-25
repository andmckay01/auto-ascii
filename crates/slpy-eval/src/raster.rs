//! `Grid<Cell>` → grayscale rasterizer (PLAN §6 downscale-SSIM, step 1).
//!
//! Each cell becomes a constant `cell_w_px × cell_h_px` block of the cell's
//! *average luminance* under the ink-coverage model:
//!
//! ```text
//! g = min(coverage(glyph) · gain, 1)          // effective ink fraction
//! y = g · luma(fg) + (1 − g) · luma(bg)       // area-weighted mix
//! ```
//!
//! No sub-cell glyph shape is modeled — deliberately. The renderer's only
//! controllable quantity is per-cell (glyph, fg, bg); measuring at cell
//! granularity scores exactly what the compositor can influence, and the
//! matching source image is produced by downscaling to the same dimensions
//! ([`crate::ssim::downscale_ssim`]).

use slpy_core::{Cell, Grid, Rgb};

use crate::coverage::CoverageTable;

/// A row-major 8-bit grayscale image (u16 dims, like everything upstream:
/// grids are u16 and source planes are 480×270).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrayImage {
    w: u16,
    h: u16,
    data: Vec<u8>,
}

impl GrayImage {
    /// A zeroed `w × h` image.
    pub fn new(w: u16, h: u16) -> GrayImage {
        GrayImage { w, h, data: vec![0; w as usize * h as usize] }
    }

    /// Wrap raw row-major bytes (e.g. a decoded source luma plane).
    ///
    /// # Panics
    /// If `data.len() != w · h`.
    pub fn from_raw(w: u16, h: u16, data: Vec<u8>) -> GrayImage {
        assert_eq!(data.len(), w as usize * h as usize, "gray image size mismatch");
        GrayImage { w, h, data }
    }

    #[inline]
    pub fn w(&self) -> u16 {
        self.w
    }

    #[inline]
    pub fn h(&self) -> u16 {
        self.h
    }

    #[inline]
    pub fn get(&self, x: u16, y: u16) -> u8 {
        debug_assert!(x < self.w && y < self.h);
        self.data[y as usize * self.w as usize + x as usize]
    }

    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    /// Copy out a sub-rectangle — e.g. the viewport region of a full-terminal
    /// raster, so letterbox pads don't enter the SSIM comparison.
    ///
    /// # Panics
    /// If the rectangle exceeds the image or is empty.
    pub fn crop(&self, x: u16, y: u16, w: u16, h: u16) -> GrayImage {
        assert!(w > 0 && h > 0, "empty crop");
        assert!(
            x as u32 + w as u32 <= self.w as u32 && y as u32 + h as u32 <= self.h as u32,
            "crop out of bounds"
        );
        let mut data = Vec::with_capacity(w as usize * h as usize);
        for row in y..y + h {
            let start = row as usize * self.w as usize + x as usize;
            data.extend_from_slice(&self.data[start..start + w as usize]);
        }
        GrayImage { w, h, data }
    }
}

/// Gamma-space Rec. 709 luma of an [`Rgb`] cell color, integer fixed-point
/// (`(13933·r + 46875·g + 4732·b) >> 16`, weights = round(coeff · 65536)).
/// The common video approximation — chroma fg is near-gray at cell
/// granularity, so linear-light exactness buys nothing here.
#[inline]
pub fn luma8(c: Rgb) -> u8 {
    ((13933 * c.r as u32 + 46875 * c.g as u32 + 4732 * c.b as u32 + 32768) >> 16) as u8
}

/// Rasterization knobs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RasterOptions {
    /// Horizontal pixels per cell (default 1).
    pub cell_w_px: u16,
    /// Vertical pixels per cell (default 2 — with `cell_w_px = 1` this
    /// matches the default 1:2 cell aspect, PLAN §3.2, at the minimal
    /// resolution: one sample per half-cell, the render's true information
    /// content. Contact sheets use larger blocks for visibly chunky PNGs.).
    pub cell_h_px: u16,
    /// Scale coverage so the table's densest glyph reaches full ink
    /// (`gain = 1 / max_coverage`, default true). Real fonts top out near
    /// 26% physical cell coverage; ASCII art reads correctly because vision
    /// adapts to the compressed range, so the metric compares in *relative
    /// ink* — otherwise a uniform ~4× darkening would dominate SSIM's
    /// luminance term and drown real regressions. Set false for the raw
    /// physical model.
    pub normalize_ink: bool,
}

impl Default for RasterOptions {
    fn default() -> RasterOptions {
        RasterOptions { cell_w_px: 1, cell_h_px: 2, normalize_ink: true }
    }
}

/// Rasterize a rendered cell grid to grayscale through an ink-coverage table.
///
/// # Panics
/// On zero `cell_*_px`, or if the output dimensions overflow `u16`
/// (the resize-fuzz ceiling is 1000×1000 cells — ample headroom).
pub fn rasterize(grid: &Grid<Cell>, table: &CoverageTable, opts: &RasterOptions) -> GrayImage {
    assert!(opts.cell_w_px > 0 && opts.cell_h_px > 0, "zero px-per-cell");
    let w = (grid.cols() as u32).checked_mul(opts.cell_w_px as u32).unwrap();
    let h = (grid.rows() as u32).checked_mul(opts.cell_h_px as u32).unwrap();
    assert!(w <= u16::MAX as u32 && h <= u16::MAX as u32, "raster dims overflow u16");
    let (w, h) = (w as u16, h as u16);

    let gain = if opts.normalize_ink && table.max_coverage() > 0.0 {
        1.0 / table.max_coverage() as f64
    } else {
        1.0
    };

    let mut img = GrayImage::new(w, h);
    for row in 0..grid.rows() {
        let cells = grid.row(row);
        // Compute one row of cell values, then replicate over the block rows.
        let base_y = row as usize * opts.cell_h_px as usize;
        for (col, cell) in cells.iter().enumerate() {
            let g = (table.coverage_or_fallback(cell.glyph()) as f64 * gain).min(1.0);
            let y = g * luma8(cell.fg) as f64 + (1.0 - g) * luma8(cell.bg) as f64;
            let v = y.round().clamp(0.0, 255.0) as u8;
            for py in 0..opts.cell_h_px as usize {
                let row_start = (base_y + py) * w as usize + col * opts.cell_w_px as usize;
                img.data[row_start..row_start + opts.cell_w_px as usize].fill(v);
            }
        }
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden: a 2×2 grid with known cells at 2×4 px per cell, against
    /// values hand-computed from the committed coverage constants:
    ///   '@' white-on-black:  (0.2627/0.2630)·255 = 254.71 → 255
    ///   ':' gray200-on-black:(0.0510/0.2630)·200 =  38.78 →  39
    ///   ' ' white-on-black:  0
    ///   '+' black-on-white:  (1 − 0.1090/0.2630)·255 = 149.32 → 149
    #[test]
    fn rasterizer_golden_tiny_grid() {
        let t = CoverageTable::conservative();
        let mut grid: Grid<Cell> = Grid::new(2, 2);
        grid.set(0, 0, Cell::new('@', Rgb::WHITE, Rgb::BLACK));
        grid.set(1, 0, Cell::new(':', Rgb::gray(200), Rgb::BLACK));
        grid.set(0, 1, Cell::new(' ', Rgb::WHITE, Rgb::BLACK));
        grid.set(1, 1, Cell::new('+', Rgb::BLACK, Rgb::WHITE));

        let opts = RasterOptions { cell_w_px: 2, cell_h_px: 4, normalize_ink: true };
        let img = rasterize(&grid, t, &opts);
        assert_eq!((img.w(), img.h()), (4, 8));

        let expect_cells = [[255u8, 39], [0, 149]];
        for y in 0..8u16 {
            for x in 0..4u16 {
                let e = expect_cells[(y / 4) as usize][(x / 2) as usize];
                assert_eq!(img.get(x, y), e, "at ({x},{y})");
            }
        }
    }

    #[test]
    fn raw_physical_model_without_normalization() {
        let t = CoverageTable::conservative();
        let mut grid: Grid<Cell> = Grid::new(1, 1);
        grid.set(0, 0, Cell::new('@', Rgb::WHITE, Rgb::BLACK));
        let opts = RasterOptions { normalize_ink: false, ..RasterOptions::default() };
        let img = rasterize(&grid, t, &opts);
        // 0.2627 · 255 = 66.99 → 67
        assert_eq!(img.as_slice(), &[67, 67]);
    }

    #[test]
    fn default_px_per_cell_matches_1_to_2_aspect() {
        let opts = RasterOptions::default();
        assert_eq!((opts.cell_w_px, opts.cell_h_px), (1, 2));
        let grid: Grid<Cell> = Grid::new(3, 2); // all BLANK
        let img = rasterize(&grid, CoverageTable::conservative(), &opts);
        assert_eq!((img.w(), img.h()), (3, 4));
        assert!(img.as_slice().iter().all(|&v| v == 0)); // BLANK = space on black
    }

    #[test]
    fn crop_extracts_viewport_region() {
        let img = GrayImage::from_raw(4, 3, vec![
            0, 1, 2, 3, //
            4, 5, 6, 7, //
            8, 9, 10, 11,
        ]);
        let c = img.crop(1, 1, 2, 2);
        assert_eq!(c.as_slice(), &[5, 6, 9, 10]);
    }

    #[test]
    fn luma8_rec709() {
        assert_eq!(luma8(Rgb::BLACK), 0);
        assert_eq!(luma8(Rgb::WHITE), 255);
        assert_eq!(luma8(Rgb::gray(128)), 128);
        // Green dominates, blue barely counts.
        assert!(luma8(Rgb::new(0, 255, 0)) > luma8(Rgb::new(255, 0, 0)));
        assert!(luma8(Rgb::new(255, 0, 0)) > luma8(Rgb::new(0, 0, 255)));
    }
}
