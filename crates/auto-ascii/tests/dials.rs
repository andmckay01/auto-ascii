use std::io::Cursor;

use auto_ascii::Dial;
use auto_ascii::pipeline::Player;
use auto_ascii_core::ramp::ASCII_BASE_FINE;
use auto_ascii_core::{Cell, ColorDepth, ComposeParams, GlyphTier, layer};
use auto_ascii_eval::fixtures::{Fixture, build_fixture};
use auto_ascii_format::header::plane_id;
use auto_ascii_format::{AsciiReader, AsciiWriter, Meta, PlaneRef, WriterOptions};
use auto_ascii_term::{Event, Key, SimBackend};

const COLS: u16 = 80;
const ROWS: u16 = 24;
const PROBE_ROW: u16 = 2;
const FAINT_COL: u16 = 18;
const P_COL: u16 = 30;
const FIRM_COL: u16 = 57;
const Q_COL: u16 = 70;

fn meta() -> Meta {
    Meta { factory_version: "dials-test".into(), source: "synthetic".into(), palette_hints: vec![] }
}

fn player(bytes: &[u8]) -> Player<'_> {
    Player::new(AsciiReader::open(bytes).unwrap(), 2.0, true, ColorDepth::True, GlyphTier::Ascii)
        .unwrap()
}

fn dial_asset() -> Vec<u8> {
    const W: usize = 192;
    const H: usize = 108;
    let opts = WriterOptions {
        base_w: W as u16,
        base_h: H as u16,
        plane_ids: vec![plane_id::Y, plane_id::E, plane_id::EX, plane_id::EY],
        zstd_level: 3,
        ..WriterOptions::default()
    };
    let mut writer = AsciiWriter::new(Cursor::new(Vec::new()), opts, &meta()).unwrap();
    let mut e = vec![0u8; W * H];
    let mut ex = vec![128u8; W * H];
    let ey = vec![128u8; W * H];
    for y in 0..H {
        for x in 0..W {
            let mag = if (32..56).contains(&x) {
                24
            } else if (128..152).contains(&x) {
                60
            } else {
                continue;
            };
            e[y * W + x] = mag;
            ex[y * W + x] = 128 + (mag >> 1);
        }
    }
    for frame in 0..2u8 {
        let mut luma = vec![100u8; W * H];
        if frame == 1 {
            for y in 0..H {
                for x in 0..W {
                    if (64..88).contains(&x) {
                        luma[y * W + x] = 136;
                    } else if (160..184).contains(&x) {
                        luma[y * W + x] = 152;
                    }
                }
            }
        }
        writer
            .write_frame(&[
                PlaneRef { id: plane_id::Y, data: &luma },
                PlaneRef { id: plane_id::E, data: &e },
                PlaneRef { id: plane_id::EX, data: &ex },
                PlaneRef { id: plane_id::EY, data: &ey },
            ])
            .unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn render(p: &mut Player<'_>, backend: &mut SimBackend, frame: u32) -> Vec<Cell> {
    p.render_present(backend, frame).unwrap();
    backend.take_output();
    p.grid().as_slice().to_vec()
}

fn cold(asset: &[u8], params: ComposeParams, frame: u32) -> Vec<Cell> {
    let mut p = player(asset);
    p.set_compose_params(params);
    let mut backend = SimBackend::new(COLS, ROWS);
    p.reflow(&mut backend, COLS, ROWS);
    render(&mut p, &mut backend, frame)
}

fn probe(p: &Player<'_>, col: u16) -> (u8, char) {
    let vp = p.viewport().unwrap();
    let (c, r) = (vp.pad_left + col, vp.pad_top + PROBE_ROW);
    (p.layer_mask().unwrap().get(c, r), p.grid().get(c, r).glyph())
}

fn glyph_at(grid: &[Cell], p: &Player<'_>, col: u16) -> char {
    let vp = p.viewport().unwrap();
    grid[(vp.pad_top + PROBE_ROW) as usize * COLS as usize + (vp.pad_left + col) as usize].glyph()
}

#[test]
fn every_dial_retraces_its_own_steps() {
    for dial in Dial::ALL {
        let label = dial.label();
        let mut p = ComposeParams::default();
        let origin = dial.get(&p);

        let mut up = vec![origin];
        loop {
            dial.turn(&mut p, 1);
            let v = dial.get(&p);
            let last = *up.last().unwrap();
            if v == last {
                break;
            }
            assert!(v > last, "{label}: every press up must climb ({last} -> {v})");
            up.push(v);
        }
        assert_eq!(*up.last().unwrap(), dial.max(), "{label}: the climb ends at max()");
        let presses = up.len() - 1;
        assert!(presses >= 4, "{label}: a dial has a range to walk ({presses} presses)");

        for &expect in up.iter().rev().skip(1) {
            dial.turn(&mut p, -1);
            assert_eq!(dial.get(&p), expect, "{label}: the way down retraces the way up");
        }
        assert_eq!(dial.get(&p), origin, "{label}: up N then down N is where it started");

        let mut down = vec![origin];
        loop {
            dial.turn(&mut p, -1);
            let v = dial.get(&p);
            let last = *down.last().unwrap();
            if v == last {
                break;
            }
            assert!(v < last, "{label}: every press down must descend ({last} -> {v})");
            down.push(v);
        }
        assert_eq!(*down.last().unwrap(), 0, "{label}: the descent ends at the floor");
        for &expect in down.iter().rev().skip(1) {
            dial.turn(&mut p, 1);
            assert_eq!(dial.get(&p), expect, "{label}: the climb back retraces the descent");
        }
        assert_eq!(dial.get(&p), origin, "{label}: down M then up M is where it started");

        for &v in up.iter().chain(&down) {
            assert!(
                v == dial.max() || i32::from(v) % dial.step() == 0,
                "{label}: {v} is off the {} grid",
                dial.step()
            );
        }

        for n in 1..=(presses as i32 + 2) {
            let (mut single, mut coalesced) = (ComposeParams::default(), ComposeParams::default());
            for _ in 0..n {
                dial.turn(&mut single, 1);
            }
            dial.turn(&mut coalesced, n);
            assert_eq!(dial.get(&coalesced), dial.get(&single), "{label}: turn({n}) vs {n} presses");
            for _ in 0..n {
                dial.turn(&mut single, -1);
            }
            dial.turn(&mut coalesced, -n);
            assert_eq!(dial.get(&coalesced), dial.get(&single), "{label}: turn(-{n}) vs {n} back");
            if n as usize <= presses {
                assert_eq!(dial.get(&coalesced), origin, "{label}: turn({n}) then turn(-{n})");
            }
        }
    }
}

#[test]
fn brackets_report_net_dial_steps() {
    let asset = build_fixture(Fixture::GradientMotion);
    let mut backend = SimBackend::new(COLS, ROWS);
    let mut p = player(&asset);
    p.reflow(&mut backend, COLS, ROWS);

    for key in [']', ']', '[', ']', 'd', 'd'] {
        backend.push_event(Event::Key(Key::Char(key)));
    }
    let d = p.drain_events(&mut backend);
    assert_eq!((d.dial_delta, d.dial_cycle), (2, 2));
    assert_eq!((d.jump_digit, d.seek_steps, d.toggle_hints, d.toggle_pause), (None, 0, false, false));

    backend.push_event(Event::Key(Key::Char('[')));
    assert_eq!(p.drain_events(&mut backend).dial_delta, -1);
}

#[test]
fn edge_strength_turned_up_and_back_restores_the_picture() {
    let asset = dial_asset();
    let mut backend = SimBackend::new(COLS, ROWS);
    let mut p = player(&asset);
    p.enable_layer_mask();
    p.reflow(&mut backend, COLS, ROWS);
    let dial = Dial::EdgeStrength;
    let mut compose = ComposeParams::default();

    let origin = render(&mut p, &mut backend, 0);
    assert_eq!(probe(&p, FAINT_COL).0, layer::BASE, "E=24 < T_on=32 draws no edge cold");
    assert_eq!(probe(&p, FIRM_COL).0, layer::EDGE, "E=60 > T_on=32 draws an edge");

    dial.turn(&mut compose, 3);
    assert_eq!(compose.edge_t_on, 20);
    p.set_compose_params(compose);
    let more = render(&mut p, &mut backend, 0);
    assert_eq!(probe(&p, FAINT_COL).0, layer::EDGE, "T_on=20 < 24 arms the faint stripe");
    assert_ne!(more, origin);

    dial.turn(&mut compose, -3);
    assert_eq!(compose, ComposeParams::default());
    p.set_compose_params(compose);
    let back = render(&mut p, &mut backend, 0);
    assert_eq!(probe(&p, FAINT_COL).0, layer::BASE, "T_on back at 32: the faint edge releases");
    assert_eq!(back, origin, "up 3 / down 3 must restore the picture exactly");

    dial.turn(&mut compose, -7);
    assert_eq!(compose.edge_t_on, 60);
    p.set_compose_params(compose);
    let fewer = render(&mut p, &mut backend, 0);
    assert_eq!(probe(&p, FIRM_COL).0, layer::BASE, "T_on=60 releases the firm stripe");
    assert_eq!(probe(&p, FAINT_COL).0, layer::BASE);
    assert_ne!(fewer, origin);
    assert_eq!(fewer, cold(&asset, compose, 0), "a turned dial is a cold start at its position");

    dial.turn(&mut compose, 7);
    p.set_compose_params(compose);
    assert_eq!(render(&mut p, &mut backend, 0), origin, "down 7 / up 7 must restore the picture");
}

#[test]
fn shadow_lift_turned_up_and_back_restores_the_picture() {
    let rank = |c: char| ASCII_BASE_FINE.iter().position(|&g| g == c).map_or(-1, |i| i as i32);
    let dial = Dial::ShadowLift;
    for (name, asset, frame) in [
        ("GradientMotion", build_fixture(Fixture::GradientMotion), 3u32),
        ("no-NORM", dial_asset(), 0),
    ] {
        let mut backend = SimBackend::new(COLS, ROWS);
        let mut p = player(&asset);
        p.reflow(&mut backend, COLS, ROWS);
        let mut compose = ComposeParams::default();
        let origin = render(&mut p, &mut backend, frame);

        dial.turn(&mut compose, 8);
        assert_eq!(compose.shadow_lift, 128);
        p.set_compose_params(compose);
        let lifted = render(&mut p, &mut backend, frame);
        assert_ne!(lifted, origin, "{name}: a half lift must change the picture");
        let (mut up, mut down) = (0usize, 0usize);
        for (a, b) in origin.iter().zip(&lifted) {
            let (ra, rb) = (rank(a.glyph()), rank(b.glyph()));
            if ra >= 0 && rb >= 0 {
                if rb > ra {
                    up += 1;
                } else if rb < ra {
                    down += 1;
                }
            }
        }
        assert!(up > 0, "{name}: the lift must raise base-ramp cells");
        assert_eq!(down, 0, "{name}: the lift must never darken a base-ramp cell");

        dial.turn(&mut compose, -8);
        assert_eq!(compose, ComposeParams::default());
        p.set_compose_params(compose);
        assert_eq!(render(&mut p, &mut backend, frame), origin, "{name}: up 8 / down 8");

        dial.turn(&mut compose, -1);
        assert_eq!(compose.shadow_lift, 0);
        p.set_compose_params(compose);
        assert_eq!(render(&mut p, &mut backend, frame), origin, "{name}: the floor holds");
    }
}

#[test]
fn hysteresis_turned_down_and_up_tracks_in_both_directions() {
    let asset = dial_asset();
    let mut backend = SimBackend::new(COLS, ROWS);
    let mut p = player(&asset);
    p.enable_layer_mask();
    p.reflow(&mut backend, COLS, ROWS);
    let dial = Dial::Hysteresis;
    let mut compose = ComposeParams::default();

    let (plain0, plain1) = (cold(&asset, compose, 0), cold(&asset, compose, 1));
    let held_p = glyph_at(&plain0, &p, P_COL);
    let moved_p = glyph_at(&plain1, &p, P_COL);
    let held_q = glyph_at(&plain0, &p, Q_COL);
    let moved_q = glyph_at(&plain1, &p, Q_COL);
    assert_ne!(held_p, moved_p, "stripe P must cross a ramp step between the frames");
    assert_ne!(held_q, moved_q, "stripe Q must cross a ramp step between the frames");

    let pair = |p: &mut Player<'_>, backend: &mut SimBackend| {
        render(p, backend, 0);
        render(p, backend, 1)
    };

    let origin = pair(&mut p, &mut backend);
    assert_eq!(glyph_at(&origin, &p, P_COL), held_p, "width 160 holds a 0.25-step crossing");
    assert_eq!(glyph_at(&origin, &p, Q_COL), moved_q, "width 160 lets a 0.75-step crossing move");

    dial.turn(&mut compose, -10);
    assert_eq!(compose.idx_hyst_q8, 0);
    p.set_compose_params(compose);
    let responsive = pair(&mut p, &mut backend);
    assert_eq!(responsive, plain1, "width 0 is plain quantization");
    assert_ne!(responsive, origin);

    dial.turn(&mut compose, 10);
    assert_eq!(compose, ComposeParams::default());
    p.set_compose_params(compose);
    assert_eq!(pair(&mut p, &mut backend), origin, "down 10 / up 10 must restore the picture");

    dial.turn(&mut compose, 6);
    assert_eq!(compose.idx_hyst_q8, 255);
    p.set_compose_params(compose);
    let sticky = pair(&mut p, &mut backend);
    assert_eq!(glyph_at(&sticky, &p, P_COL), held_p);
    assert_eq!(glyph_at(&sticky, &p, Q_COL), held_q, "width 255 holds a 0.75-step crossing");
    assert_ne!(sticky, origin);

    dial.turn(&mut compose, -6);
    assert_eq!(compose, ComposeParams::default());
    p.set_compose_params(compose);
    assert_eq!(pair(&mut p, &mut backend), origin, "up 6 / down 6 must restore the picture");

    dial.turn(&mut compose, 6);
    p.set_compose_params(compose);
    let after_turn = render(&mut p, &mut backend, 1);
    assert_eq!(glyph_at(&after_turn, &p, P_COL), moved_p, "a turn re-quantizes the frame on screen");
    assert_eq!(after_turn, cold(&asset, compose, 1), "a turned dial is a cold start at its position");
}

#[test]
fn a_turn_is_a_cold_start_and_a_stopped_dial_is_not() {
    let asset = dial_asset();
    for dial in Dial::ALL {
        let label = dial.label();
        let mut backend = SimBackend::new(COLS, ROWS);
        let mut p = player(&asset);
        p.reflow(&mut backend, COLS, ROWS);
        let mut compose = ComposeParams::default();
        render(&mut p, &mut backend, 0);
        render(&mut p, &mut backend, 1);

        dial.turn(&mut compose, 1);
        p.set_compose_params(compose);
        assert_eq!(render(&mut p, &mut backend, 1), cold(&asset, compose, 1), "{label}: +1");
        dial.turn(&mut compose, -2);
        p.set_compose_params(compose);
        assert_eq!(render(&mut p, &mut backend, 1), cold(&asset, compose, 1), "{label}: -2");

        dial.turn(&mut compose, 64);
        assert_eq!(dial.get(&compose), dial.max());
        p.set_compose_params(compose);
        render(&mut p, &mut backend, 0);
        let warm = render(&mut p, &mut backend, 1);
        dial.turn(&mut compose, 1);
        assert_eq!(dial.get(&compose), dial.max(), "{label}: the top is a stop");
        p.set_compose_params(compose);
        assert_eq!(render(&mut p, &mut backend, 1), warm, "{label}: a stopped press leaves state alone");
    }
}
