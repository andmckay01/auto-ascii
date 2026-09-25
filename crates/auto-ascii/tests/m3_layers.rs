use std::io::Cursor;

use auto_ascii::pipeline::Player;
use auto_ascii_core::{ColorDepth, GlyphTier, layer};
use auto_ascii_eval::fixtures::{Fixture, build_fixture};
use auto_ascii_format::header::plane_id;
use auto_ascii_format::{Meta, PlaneRef, AsciiReader, AsciiWriter, WriterOptions};
use auto_ascii_term::{Event, Key, SimBackend};

fn meta() -> Meta {
    Meta { factory_version: "m3-test".into(), source: "synthetic".into(), palette_hints: vec![] }
}

fn player<'a>(bytes: &'a [u8], depth: ColorDepth, tier: GlyphTier) -> Player<'a> {
    Player::new(AsciiReader::open(bytes).unwrap(), 2.0, true, depth, tier).unwrap()
}

#[test]
fn m1_era_y_c_asset_plays_with_layers_auto_disabled() {
    let asset = build_fixture(Fixture::GradientMotion);
    let mut p = player(&asset, ColorDepth::True, GlyphTier::UnicodeBlocks);
    let mut backend = SimBackend::new(120, 40);
    p.enable_layer_mask();
    p.reflow(&mut backend, 120, 40);
    for f in [0u32, 1, 2, 24, 25] {
        p.render_present(&mut backend, f).unwrap();
        backend.take_output();
        let mask = p.layer_mask().unwrap();
        for &l in mask.as_slice() {
            assert!(
                l == layer::BASE || l == layer::STRUCTURE,
                "Y+C asset composed layer {l} — edge/highlight must be auto-disabled"
            );
        }
    }
    assert!(p.grid().as_slice().iter().any(|c| c.glyph() != ' '));
}

fn six_plane_asset() -> Vec<u8> {
    const W: usize = 192;
    const H: usize = 108;
    let opts = WriterOptions {
        base_w: W as u16,
        base_h: H as u16,
        plane_ids: vec![
            plane_id::Y,
            plane_id::E,
            plane_id::EX,
            plane_id::EY,
            plane_id::H,
            plane_id::C,
        ],
        zstd_level: 3,
        ..WriterOptions::default()
    };
    let mut writer = AsciiWriter::new(Cursor::new(Vec::new()), opts, &meta()).unwrap();
    let luma = vec![100u8; W * H];
    let chroma = vec![0u8; (W / 2) * (H / 2) * 2];
    for &e_mag in &[200u8, 24, 10] {
        let mut e = vec![0u8; W * H];
        let mut ex = vec![128u8; W * H];
        let ey = vec![128u8; W * H];
        let mut h = vec![0u8; W * H];
        for y in 0..H {
            for x in 0..W {
                let i = y * W + x;
                if (88..96).contains(&x) {
                    e[i] = e_mag;
                    ex[i] = 128 + (e_mag >> 1);
                }
                if (24..48).contains(&x) && (24..48).contains(&y) {
                    h[i] |= 1;
                }
                if y >= 72 {
                    h[i] |= 2;
                }
            }
        }
        writer
            .write_frame(&[
                PlaneRef { id: plane_id::Y, data: &luma },
                PlaneRef { id: plane_id::E, data: &e },
                PlaneRef { id: plane_id::EX, data: &ex },
                PlaneRef { id: plane_id::EY, data: &ey },
                PlaneRef { id: plane_id::H, data: &h },
                PlaneRef { id: plane_id::C, data: &chroma },
            ])
            .unwrap();
    }
    writer.finish().unwrap().into_inner()
}

#[test]
fn six_plane_asset_drives_edge_highlight_shadow_layers() {
    let asset = six_plane_asset();
    let mut p = player(&asset, ColorDepth::True, GlyphTier::UnicodeBlocks);
    let mut backend = SimBackend::new(80, 24);
    p.enable_layer_mask();
    p.reflow(&mut backend, 80, 24);
    let vp = p.viewport().unwrap();
    assert_eq!((vp.cols, vp.rows), (80, 23));

    p.render_present(&mut backend, 0).unwrap();
    backend.take_output();
    let at = |p: &Player<'_>, c: u16, r: u16| {
        (
            p.layer_mask().unwrap().get(vp.pad_left + c, vp.pad_top + r),
            p.grid().get(vp.pad_left + c, vp.pad_top + r),
        )
    };
    let (l, cell) = at(&p, 38, 2);
    assert_eq!(l, layer::EDGE, "stripe cell must be edge-layer");
    assert_eq!(cell.glyph(), '│', "vertical contour → vertical stroke (unicode edge LUT)");

    let (l, _) = at(&p, 14, 7);
    assert_eq!(l, layer::HIGHLIGHT, "highlight block cell");
    let (l, cell) = at(&p, 10, 20);
    assert_eq!(l, layer::SHADOW, "shadow band cell");
    assert_eq!(cell.glyph(), ' ', "shadow clamps to the darkest ramp step");
    let (l, _) = at(&p, 60, 2);
    assert_eq!(l, layer::BASE);

    p.render_present(&mut backend, 1).unwrap();
    backend.take_output();
    let (l, _) = at(&p, 38, 2);
    assert_eq!(l, layer::EDGE, "T_off < e < T_on must hold while was_edge");

    p.render_present(&mut backend, 2).unwrap();
    backend.take_output();
    let (l, _) = at(&p, 38, 2);
    assert_eq!(l, layer::BASE, "below T_off the edge layer releases");

    let mut cold = player(&asset, ColorDepth::True, GlyphTier::UnicodeBlocks);
    cold.enable_layer_mask();
    cold.reflow(&mut backend, 80, 24);
    cold.render_present(&mut backend, 1).unwrap();
    backend.take_output();
    let (l, _) = at(&cold, 38, 2);
    assert_eq!(l, layer::BASE, "cold start at e=24 must not arm the edge gate");
}

#[test]
fn digit_jump_seek_resets_hysteresis_state() {
    let asset = six_plane_asset();
    let mut backend = SimBackend::new(80, 24);

    let mut p = player(&asset, ColorDepth::True, GlyphTier::UnicodeBlocks);
    p.enable_layer_mask();
    p.reflow(&mut backend, 80, 24);
    let vp = p.viewport().unwrap();
    p.render_present(&mut backend, 0).unwrap();
    backend.take_output();

    backend.push_event(Event::Key(Key::Char('5')));
    let drained = p.drain_events(&mut backend);
    assert_eq!(drained.jump_digit, Some(5), "digit key must report a jump");
    p.render_present(&mut backend, 1).unwrap();
    backend.take_output();

    let l = p.layer_mask().unwrap().get(vp.pad_left + 38, vp.pad_top + 2);
    assert_eq!(
        l,
        layer::BASE,
        "post-seek e=24 < T_on must not ghost the pre-seek edge into the landing frame"
    );

    let mut cold = player(&asset, ColorDepth::True, GlyphTier::UnicodeBlocks);
    cold.reflow(&mut backend, 80, 24);
    cold.render_present(&mut backend, 1).unwrap();
    backend.take_output();
    assert_eq!(
        p.grid().as_slice(),
        cold.grid().as_slice(),
        "a seek landing frame must be byte-identical to a cold start at that frame"
    );
}

#[test]
fn shot_change_resets_hysteresis_state() {
    let asset = build_fixture(Fixture::HardCut);
    let mut backend = SimBackend::new(100, 30);

    let mut warmed = player(&asset, ColorDepth::True, GlyphTier::UnicodeBlocks);
    warmed.reflow(&mut backend, 100, 30);
    for f in 30..=36u32 {
        warmed.render_present(&mut backend, f).unwrap();
        backend.take_output();
    }

    let mut cold = player(&asset, ColorDepth::True, GlyphTier::UnicodeBlocks);
    cold.reflow(&mut backend, 100, 30);
    cold.render_present(&mut backend, 36).unwrap();
    backend.take_output();

    assert_eq!(
        warmed.grid().as_slice(),
        cold.grid().as_slice(),
        "the CUT boundary must reset all hysteresis state (no ghosting across cuts)"
    );
}

#[test]
fn tier_palettes_select_and_subcell_structure_fires() {
    let asset = build_fixture(Fixture::CheckerDrift);
    let mut backend = SimBackend::new(206, 58);

    let mut mono = player(&asset, ColorDepth::Mono, GlyphTier::Ascii);
    mono.reflow(&mut backend, 206, 58);
    mono.render_present(&mut backend, 7).unwrap();
    backend.take_output();
    let allowed: Vec<char> = " .:coO8@\"_-".chars().collect();
    for c in mono.grid().as_slice() {
        assert!(
            allowed.contains(&c.glyph()),
            "mono tier leaked glyph {:?} outside palette 8 + subposition",
            c.glyph()
        );
    }
    let has_subpos = mono
        .grid()
        .as_slice()
        .iter()
        .any(|c| c.glyph() == '"' || c.glyph() == '_');
    assert!(has_subpos, "ascii-tier subposition glyphs must fire on checker content");

    let mut uni = player(&asset, ColorDepth::True, GlyphTier::UnicodeBlocks);
    uni.reflow(&mut backend, 206, 58);
    uni.render_present(&mut backend, 7).unwrap();
    backend.take_output();
    let halfblocks = uni
        .grid()
        .as_slice()
        .iter()
        .filter(|c| matches!(c.glyph(), '▀' | '▄'))
        .count();
    assert!(halfblocks > 0, "unicode tier must emit half-block (fg,bg) pairs");
    assert!(
        uni.grid()
            .as_slice()
            .iter()
            .any(|c| matches!(c.glyph(), '▀' | '▄') && c.fg != c.bg),
        "half-block cells must carry a (fg,bg) pixel pair"
    );
}

#[test]
fn reflow_reallocs_hysteresis_state() {
    let asset = build_fixture(Fixture::GradientMotion);
    let mut p = player(&asset, ColorDepth::True, GlyphTier::UnicodeBlocks);
    let mut backend = SimBackend::new(80, 24);
    for (c, r) in [(80u16, 24u16), (213, 58), (20, 5), (320, 90)] {
        p.reflow(&mut backend, c, r);
        match p.viewport() {
            Some(vp) => {
                assert_eq!(p.hysteresis_dims(), (vp.cols, vp.rows));
                p.render_present(&mut backend, 0).unwrap();
                backend.take_output();
            }
            None => assert_eq!(p.hysteresis_dims(), (0, 0)),
        }
    }
}

#[test]
fn shadow_lift_dial_moves_dark_cells_up_the_ramp() {
    let asset = build_fixture(Fixture::GradientMotion);

    let render = |lift: u8| -> Vec<char> {
        let mut p = player(&asset, ColorDepth::True, GlyphTier::Ascii);
        let mut params = p.compose_params();
        params.shadow_lift = lift;
        p.set_compose_params(params);
        let mut backend = SimBackend::new(120, 40);
        p.reflow(&mut backend, 120, 40);
        p.render_present(&mut backend, 3).unwrap();
        backend.take_output();
        p.grid().as_slice().iter().map(|c| c.glyph()).collect()
    };

    let off = render(0);
    let full = render(255);
    assert_eq!(off.len(), full.len(), "same grid geometry either way");
    assert_ne!(off, full, "a full shadow lift must change the rendered glyphs");

    const RAMP: &str = " .,:;i1tftLCG08@";
    let rank = |c: char| RAMP.find(c).map(|i| i as i32).unwrap_or(-1);
    let (mut lifted, mut darkened) = (0usize, 0usize);
    for (&a, &b) in off.iter().zip(&full) {
        let (ra, rb) = (rank(a), rank(b));
        if ra < 0 || rb < 0 {
            continue;
        }
        if rb > ra {
            lifted += 1;
        } else if rb < ra {
            darkened += 1;
        }
    }
    assert!(lifted > 0, "lift should raise at least some base-ramp cells");
    assert_eq!(darkened, 0, "a shadow lift must never darken a base-ramp cell");
}
