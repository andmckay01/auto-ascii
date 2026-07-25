//! Glyph ink-coverage table — the bridge from `Grid<Cell>` back to grayscale
//! (PLAN §6 "rasterize through the stored glyph-coverage tables").
//!
//! A coverage value is the fraction of the terminal cell's area covered by
//! the glyph's ink, in `0.0..=1.0`. The eval rasterizer models a rendered
//! cell as `coverage · luma(fg) + (1 − coverage) · luma(bg)` — the average
//! luminance a camera pointed at the screen would measure before blur.
//!
//! # Derivation of the built-in conservative table
//!
//! `CONSERVATIVE_COVERAGE` was derived on 2026-07-25 by
//! `tools/derive_coverage.py` (committed next to this crate):
//!
//! - Every printable ASCII glyph (U+0020..=U+007E — a superset of all
//!   currently shipped palettes: `ascii/base/coarse` `" .:-=+*#%@"`,
//!   `ascii/base/fine` `" .,:;i1tfLCG08@"`, and the PLAN §3.4 palette-8
//!   mono ramp `" .:coO8@"`) is rendered white-on-black into a 64×128 px
//!   cell (PLAN §3.4's rasterization size) with ffmpeg's libfreetype
//!   `drawtext`.
//! - Font: **DejaVu Sans Mono Book** — the de-facto default Linux monospace
//!   and a mid-pack "conservative" choice for ink coverage (fonts vary ±15%,
//!   PLAN §9.5; per-font tables land at M5).
//! - Font size 106 px: DejaVu Sans Mono's advance is 1233/2048 em ≈ 0.602 em
//!   → ~64 px advance (= cell width), and its line box is
//!   (1901+483)/2048 em ≈ 1.164 em → ~123 px ≈ cell height, i.e. the 1:2
//!   cell aspect the engine assumes by default (PLAN §3.2).
//! - `coverage = Σ gray / (255 · 64 · 128)` — the mean pixel value
//!   integrates fractional (antialiased) ink exactly instead of
//!   thresholding. Glyph position inside the cell does not affect the
//!   integral (no glyph clips at this size).
//!
//! The constants are the committed artifact; the script is the reproducible
//! reference (needs only ffmpeg + the system DejaVu font — no corpus).

/// One glyph's fraction-of-cell ink coverage plus lookup machinery.
///
/// Entries are raw physical coverage (DejaVu tops out at ~0.263 for `M`/`@` —
/// real fonts never blacken a whole cell with text glyphs). The rasterizer
/// optionally normalizes by [`CoverageTable::max_coverage`] so the densest
/// glyph reaches full scale (see `RasterOptions::normalize_ink`).
#[derive(Clone, Debug)]
pub struct CoverageTable {
    /// Sorted by `char` for binary search.
    entries: Vec<(char, f32)>,
    max: f32,
}

impl CoverageTable {
    /// The built-in conservative table (see module docs for derivation).
    pub fn conservative() -> &'static CoverageTable {
        static TABLE: std::sync::LazyLock<CoverageTable> =
            std::sync::LazyLock::new(|| CoverageTable::from_entries(CONSERVATIVE_COVERAGE.to_vec()));
        &TABLE
    }

    /// Build a table from `(glyph, coverage)` pairs (per-font tables at M5,
    /// `--font-table` override). Sorts by char.
    ///
    /// # Panics
    /// On an empty list, duplicate glyphs, or coverage outside `0.0..=1.0`
    /// (programmer/table-data error, same posture as `compose_luma`).
    pub fn from_entries(mut entries: Vec<(char, f32)>) -> CoverageTable {
        assert!(!entries.is_empty(), "coverage table must not be empty");
        entries.sort_unstable_by_key(|&(ch, _)| ch);
        let mut max = 0.0f32;
        for w in entries.windows(2) {
            assert!(w[0].0 != w[1].0, "duplicate glyph {:?} in coverage table", w[0].0);
        }
        for &(ch, c) in &entries {
            assert!(
                (0.0..=1.0).contains(&c),
                "coverage for {ch:?} out of range: {c}"
            );
            max = max.max(c);
        }
        CoverageTable { entries, max }
    }

    /// Ink coverage for `ch`, or `None` if the glyph is not in the table.
    #[inline]
    pub fn coverage(&self, ch: char) -> Option<f32> {
        self.entries
            .binary_search_by_key(&ch, |&(c, _)| c)
            .ok()
            .map(|i| self.entries[i].1)
    }

    /// Coverage with a conservative fallback for unknown glyphs: half the
    /// table maximum (a mid-gray guess — better than 0, which would score
    /// unknown ink as blank). All currently shipped palette glyphs are in the
    /// built-in table, so the fallback only fires on future palettes whose
    /// table entry is missing.
    #[inline]
    pub fn coverage_or_fallback(&self, ch: char) -> f32 {
        self.coverage(ch).unwrap_or(self.max * 0.5)
    }

    /// Largest coverage in the table (the normalization anchor).
    #[inline]
    pub fn max_coverage(&self) -> f32 {
        self.max
    }

    /// Number of glyphs in the table.
    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Derived constants — see module docs. Printable ASCII, sorted by codepoint.
pub const CONSERVATIVE_COVERAGE: &[(char, f32)] = &[
    (' ', 0.0000),
    ('!', 0.0810),
    ('"', 0.0638),
    ('#', 0.2316),
    ('$', 0.2018),
    ('%', 0.1788),
    ('&', 0.2224),
    ('\'', 0.0319),
    ('(', 0.1092),
    (')', 0.1095),
    ('*', 0.0910),
    ('+', 0.1090),
    (',', 0.0431),
    ('-', 0.0263),
    ('.', 0.0255),
    ('/', 0.1047),
    ('0', 0.2366),
    ('1', 0.1460),
    ('2', 0.1717),
    ('3', 0.1779),
    ('4', 0.1882),
    ('5', 0.1837),
    ('6', 0.2212),
    ('7', 0.1366),
    ('8', 0.2414),
    ('9', 0.2214),
    (':', 0.0510),
    (';', 0.0686),
    ('<', 0.1136),
    ('=', 0.1202),
    ('>', 0.1137),
    ('?', 0.1189),
    ('@', 0.2627),
    ('A', 0.2053),
    ('B', 0.2581),
    ('C', 0.1560),
    ('D', 0.2304),
    ('E', 0.2015),
    ('F', 0.1640),
    ('G', 0.2048),
    ('H', 0.2257),
    ('I', 0.1621),
    ('J', 0.1484),
    ('K', 0.2212),
    ('L', 0.1354),
    ('M', 0.2630),
    ('N', 0.2629),
    ('O', 0.2255),
    ('P', 0.1984),
    ('Q', 0.2419),
    ('R', 0.2375),
    ('S', 0.1903),
    ('T', 0.1462),
    ('U', 0.2109),
    ('V', 0.1808),
    ('W', 0.2580),
    ('X', 0.1919),
    ('Y', 0.1489),
    ('Z', 0.1857),
    ('[', 0.1315),
    ('\\', 0.1047),
    (']', 0.1318),
    ('^', 0.0684),
    ('_', 0.0546),
    ('`', 0.0222),
    ('a', 0.1855),
    ('b', 0.2111),
    ('c', 0.1247),
    ('d', 0.2112),
    ('e', 0.1787),
    ('f', 0.1388),
    ('g', 0.2300),
    ('h', 0.1839),
    ('i', 0.1264),
    ('j', 0.1363),
    ('k', 0.1844),
    ('l', 0.1195),
    ('m', 0.2090),
    ('n', 0.1571),
    ('o', 0.1730),
    ('p', 0.2100),
    ('q', 0.2103),
    ('r', 0.1006),
    ('s', 0.1466),
    ('t', 0.1322),
    ('u', 0.1571),
    ('v', 0.1326),
    ('w', 0.1804),
    ('x', 0.1408),
    ('y', 0.1650),
    ('z', 0.1345),
    ('{', 0.1447),
    ('|', 0.1151),
    ('}', 0.1433),
    ('~', 0.0611),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Every glyph of every currently shipped/planned-at-M2 palette must be
    /// in the built-in table: coarse + fine (slpy-core ramp.rs) and the
    /// PLAN §3.4 palette-8 mono ramp `" .:coO8@"`.
    #[test]
    fn covers_all_current_palette_glyphs() {
        let t = CoverageTable::conservative();
        let mono: &[char] = &[' ', '.', ':', 'c', 'o', 'O', '8', '@'];
        for &ch in slpy_core::ramp::ASCII_BASE_COARSE
            .iter()
            .chain(slpy_core::ramp::ASCII_BASE_FINE)
            .chain(mono)
        {
            assert!(t.coverage(ch).is_some(), "glyph {ch:?} missing from table");
        }
        // Full printable ASCII, actually.
        for cp in 0x20u32..0x7F {
            let ch = char::from_u32(cp).unwrap();
            assert!(t.coverage(ch).is_some(), "printable ASCII {ch:?} missing");
        }
        assert_eq!(t.len(), 95);
    }

    #[test]
    fn coverage_is_ordered_sanely() {
        let t = CoverageTable::conservative();
        let c = |ch| t.coverage(ch).unwrap();
        assert_eq!(c(' '), 0.0);
        assert!(c(' ') < c('.'));
        assert!(c('.') < c(':'));
        assert!(c(':') < c('+'));
        assert!(c('+') < c('#'));
        assert!(c('#') < c('@'));
        // Physical, not normalized: text glyphs never fill a cell.
        assert!(t.max_coverage() > 0.2 && t.max_coverage() < 0.5);
    }

    #[test]
    fn unknown_glyph_fallback_is_mid_gray() {
        let t = CoverageTable::conservative();
        assert_eq!(t.coverage('█'), None);
        let fb = t.coverage_or_fallback('█');
        assert!((fb - t.max_coverage() * 0.5).abs() < 1e-6);
    }

    #[test]
    #[should_panic(expected = "duplicate glyph")]
    fn rejects_duplicates() {
        let _ = CoverageTable::from_entries(vec![('a', 0.1), ('a', 0.2)]);
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn rejects_out_of_range() {
        let _ = CoverageTable::from_entries(vec![('a', 1.5)]);
    }
}
