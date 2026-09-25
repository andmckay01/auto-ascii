use auto_ascii_core::codec::letters::letters_glyphs;
use auto_ascii_core::compose::{ComposeParams, FramePlanes, compose_frame, compose_frame_masked};
use auto_ascii_core::hysteresis::HysteresisState;
use auto_ascii_core::palette::{ColorDepth, GlyphTier, select_palettes};
use auto_ascii_core::{Cell, Codec, Grid, compose_frame_codec, compute_viewport};
use proptest::prelude::*;

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

fn plane(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn letters_only_emits_allowed_glyphs(
        seed in any::<u64>(),
        t in 0u8..3,
        c in 0u8..4,
        cols in 32u16..=160,
        rows in 9u16..=50,
    ) {
        let vp = compute_viewport(cols, rows, 2.0).unwrap();
        let (vc, vr) = (vp.cols as usize, vp.rows as usize);
        let set = select_palettes(tier(t), color(c), vp.cols);
        let allowed = letters_glyphs(set.halfblock);
        let mut st = HysteresisState::new(vp.cols, vp.rows);
        let mut grid: Grid<Cell> = Grid::new(cols, rows);
        let lut: [u8; 256] = core::array::from_fn(|i| i as u8);
        for f in 0..3u64 {
            let s = seed.wrapping_add(f * 0x9E37_79B9);
            let luma2 = plane(s, vc * 2 * vr);
            let (e, ex, ey) = (plane(s ^ 1, vc * vr), plane(s ^ 2, vc * vr), plane(s ^ 3, vc * vr));
            let h: Vec<u8> = plane(s ^ 4, vc * vr).iter().map(|v| v & 3).collect();
            let (r, g, b) = (plane(s ^ 5, vc * vr), plane(s ^ 6, vc * vr), plane(s ^ 7, vc * vr));
            let planes = FramePlanes {
                luma2: &luma2,
                e: Some(&e),
                ex: Some(&ex),
                ey: Some(&ey),
                h: Some(&h),
                chroma: Some((&r, &g, &b)),
            };
            compose_frame_codec(
                Codec::Letters, &planes, &vp, &lut, &set, &ComposeParams::default(),
                &mut st, &mut grid, None,
            );
            for cell in grid.as_slice() {
                let g = cell.glyph();
                prop_assert!(allowed.contains(&g), "{g:?} (U+{:04X}) not allowed on {:?}", g as u32, tier(t));
                if !set.halfblock {
                    prop_assert!(g == ' ' || g.is_ascii_graphic(), "non-ASCII {g:?} on the ascii tier");
                }
            }
        }
    }

    #[test]
    fn pixels_through_the_registry_is_the_plain_path(
        seed in any::<u64>(),
        t in 0u8..3,
        c in 0u8..4,
    ) {
        let vp = compute_viewport(100, 30, 2.0).unwrap();
        let (vc, vr) = (vp.cols as usize, vp.rows as usize);
        let set = select_palettes(tier(t), color(c), vp.cols);
        let lut: [u8; 256] = core::array::from_fn(|i| i as u8);
        let p = ComposeParams::default();
        let (mut st_a, mut st_b) = (HysteresisState::new(vp.cols, vp.rows), HysteresisState::new(vp.cols, vp.rows));
        let (mut a, mut b): (Grid<Cell>, Grid<Cell>) = (Grid::new(100, 30), Grid::new(100, 30));
        let (mut ma, mut mb): (Grid<u8>, Grid<u8>) = (Grid::new(100, 30), Grid::new(100, 30));
        for f in 0..3u64 {
            let s = seed.wrapping_add(f);
            let luma2 = plane(s, vc * 2 * vr);
            let (e, ex, ey) = (plane(s ^ 1, vc * vr), plane(s ^ 2, vc * vr), plane(s ^ 3, vc * vr));
            let h: Vec<u8> = plane(s ^ 4, vc * vr).iter().map(|v| v & 3).collect();
            let planes = FramePlanes { luma2: &luma2, e: Some(&e), ex: Some(&ex), ey: Some(&ey), h: Some(&h), chroma: None };
            compose_frame_masked(&planes, &vp, &lut, &set, &p, &mut st_a, &mut a, &mut ma);
            compose_frame_codec(Codec::Pixels, &planes, &vp, &lut, &set, &p, &mut st_b, &mut b, Some(&mut mb));
            prop_assert_eq!(a.as_slice(), b.as_slice());
            prop_assert_eq!(ma.as_slice(), mb.as_slice());
            let mut st_c = st_a.clone();
            let mut c_grid: Grid<Cell> = Grid::new(100, 30);
            let mut st_d = st_a.clone();
            let mut d_grid: Grid<Cell> = Grid::new(100, 30);
            compose_frame(&planes, &vp, &lut, &set, &p, &mut st_c, &mut c_grid);
            compose_frame_codec(Codec::Pixels, &planes, &vp, &lut, &set, &p, &mut st_d, &mut d_grid, None);
            prop_assert_eq!(c_grid.as_slice(), d_grid.as_slice());
        }
    }
}
