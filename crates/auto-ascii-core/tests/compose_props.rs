//! M3 compositor property tests (task spec §3.4/§3.5): uniform input composes
//! to the pure base ramp with zero edge/highlight leakage on every palette
//! configuration, and repeated composition is a hysteresis fixed point.

use proptest::prelude::*;
use auto_ascii_core::compose::{ComposeParams, FramePlanes, compose_frame};
use auto_ascii_core::hysteresis::HysteresisState;
use auto_ascii_core::palette::{ColorDepth, GlyphTier, select_palettes};
use auto_ascii_core::{Cell, Grid, Rgb, compute_viewport, hysteresis_idx};

fn ident_lut() -> [u8; 256] {
    core::array::from_fn(|i| i as u8)
}

fn tier(i: u8) -> GlyphTier {
    match i % 3 {
        0 => GlyphTier::Ascii,
        1 => GlyphTier::UnicodeBlocks,
        _ => GlyphTier::BrailleVerified,
    }
}

fn color(i: u8) -> ColorDepth {
    match i % 4 {
        0 => ColorDepth::True,
        1 => ColorDepth::C256,
        2 => ColorDepth::C16,
        _ => ColorDepth::Mono,
    }
}

proptest! {
    /// Uniform luma + zero-feature planes → every viewport cell is exactly the
    /// base ramp cell (no edge, highlight, half-block, subpos or braille
    /// leaks), pads stay BLANK, and a second pass is byte-identical.
    #[test]
    fn uniform_input_is_pure_base_ramp(
        n in 0u8..=255,
        cols in 32u16..=200,
        rows in 9u16..=60,
        t in 0u8..3,
        c in 0u8..4,
    ) {
        let Some(vp) = compute_viewport(cols, rows, 2.0) else { return Ok(()); };
        let (vc, vr) = (vp.cols as usize, vp.rows as usize);
        let set = select_palettes(tier(t), color(c), vp.cols);
        let lut = ident_lut();
        let params = ComposeParams::default();

        let luma2 = vec![n; vc * 2 * vr];
        let e = vec![0u8; vc * vr];
        let exy = vec![128u8; vc * vr];
        let h = vec![0u8; vc * vr];
        let planes = FramePlanes {
            luma2: &luma2, e: Some(&e), ex: Some(&exy), ey: Some(&exy),
            h: Some(&h), chroma: None,
        };

        let mut state = HysteresisState::new(vp.cols, vp.rows);
        let mut grid: Grid<Cell> = Grid::new(cols, rows);
        compose_frame(&planes, &vp, &lut, &set, &params, &mut state, &mut grid);

        let idx = hysteresis_idx(
            n,
            set.base.len(),
            auto_ascii_core::hysteresis::IDX_UNSET,
            auto_ascii_core::hysteresis::IDX_HYST_Q8,
        );
        let expected = Cell::new(set.base.glyph(idx), Rgb::gray(n), Rgb::BLACK);
        for r in 0..rows {
            for col in 0..cols {
                let in_vp = col >= vp.pad_left && col < vp.pad_left + vp.cols
                    && r >= vp.pad_top && r < vp.pad_top + vp.rows;
                let cell = grid.get(col, r);
                if in_vp {
                    prop_assert_eq!(cell, expected);
                } else {
                    prop_assert_eq!(cell, Cell::BLANK);
                }
            }
        }

        // Hysteresis fixed point: same input again → identical frame.
        let snapshot = grid.clone();
        compose_frame(&planes, &vp, &lut, &set, &params, &mut state, &mut grid);
        prop_assert_eq!(&grid, &snapshot);
    }

    /// Hysteresis damping: a ±1 luma wobble around a step boundary never
    /// changes the glyph after the first frame (the §3.5 flicker killer).
    #[test]
    fn index_hysteresis_absorbs_lsb_wobble(
        base in 1u8..=254,
        t in 0u8..3,
        c in 0u8..4,
    ) {
        let vp = compute_viewport(40, 12, 2.0).unwrap();
        let (vc, vr) = (vp.cols as usize, vp.rows as usize);
        let set = select_palettes(tier(t), color(c), vp.cols);
        let lut = ident_lut();
        let params = ComposeParams::default();
        let mut state = HysteresisState::new(vp.cols, vp.rows);
        let mut grid: Grid<Cell> = Grid::new(40, 12);

        let mut glyphs = Vec::new();
        for wobble in [0i16, 1, -1, 1, 0, -1, 1, -1] {
            let v = (base as i16 + wobble).clamp(0, 255) as u8;
            let luma2 = vec![v; vc * 2 * vr];
            let planes = FramePlanes {
                luma2: &luma2, e: None, ex: None, ey: None, h: None, chroma: None,
            };
            compose_frame(&planes, &vp, &lut, &set, &params, &mut state, &mut grid);
            glyphs.push(grid.get(vp.pad_left, vp.pad_top).glyph());
        }
        // After the seed frame, the glyph must never switch again.
        for g in &glyphs[1..] {
            prop_assert_eq!(*g, glyphs[1]);
        }
    }
}
