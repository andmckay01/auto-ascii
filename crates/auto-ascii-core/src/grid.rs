//! `Grid<T>` — a dense row-major 2-D buffer sized in terminal cells.
//!
//! `Grid<Cell>` is what `Backend::present` consumes. Allocation happens only in
//! `new`/`resize`; `resize` is the only allocation point in the hot path.

/// Dense row-major grid, indexed `(col, row)`, `cols × rows` in `u16` like the
/// terminal itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grid<T> {
    cols: u16,
    rows: u16,
    data: Vec<T>,
}

impl<T: Copy + Default> Grid<T> {
    /// Allocate a `cols × rows` grid filled with `T::default()`.
    pub fn new(cols: u16, rows: u16) -> Grid<T> {
        Grid {
            cols,
            rows,
            data: vec![T::default(); cols as usize * rows as usize],
        }
    }

    #[inline]
    pub fn cols(&self) -> u16 {
        self.cols
    }

    #[inline]
    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// Total cell count (`cols * rows`).
    #[inline]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Reallocate to the new dimensions and reset every cell to `T::default()`.
    /// Contents are NOT preserved — a resize invalidates the frame anyway.
    /// This is the hot path's only allocation point.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        self.data.clear();
        self.data.resize(cols as usize * rows as usize, T::default());
    }

    /// Overwrite every cell with `v`.
    pub fn fill(&mut self, v: T) {
        self.data.fill(v);
    }

    #[inline]
    pub fn get(&self, col: u16, row: u16) -> T {
        debug_assert!(col < self.cols && row < self.rows);
        self.data[row as usize * self.cols as usize + col as usize]
    }

    #[inline]
    pub fn set(&mut self, col: u16, row: u16, v: T) {
        debug_assert!(col < self.cols && row < self.rows);
        self.data[row as usize * self.cols as usize + col as usize] = v;
    }

    /// One row as a slice — the diff renderer memcmps row pairs.
    #[inline]
    pub fn row(&self, row: u16) -> &[T] {
        let w = self.cols as usize;
        let start = row as usize * w;
        &self.data[start..start + w]
    }

    #[inline]
    pub fn row_mut(&mut self, row: u16) -> &mut [T] {
        let w = self.cols as usize;
        let start = row as usize * w;
        &mut self.data[start..start + w]
    }

    #[inline]
    pub fn as_slice(&self) -> &[T] {
        &self.data
    }

    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        &mut self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_resize() {
        let mut g: Grid<u8> = Grid::new(4, 3);
        assert_eq!(g.len(), 12);
        g.set(3, 2, 7);
        assert_eq!(g.get(3, 2), 7);
        assert_eq!(g.row(2), &[0, 0, 0, 7]);
        g.resize(2, 2);
        assert_eq!(g.len(), 4);
        assert_eq!(g.get(1, 1), 0);
    }
}
