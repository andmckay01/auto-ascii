//! Live dials (M6 tuning UX): `d` selects, `[`/`]` turn. Two things the
//! player owes anyone turning one — the VALUE walks up and back down through
//! the same detents and returns exactly to where it started, and the PICTURE
//! does the same: a dial position means one look, whichever way it was
//! reached. The second is what the one-way-dial bug broke. A turn changed the
//! compositor's thresholds but left every cell's hysteresis memory (held ramp
//! index, `was_edge`) decided under the old ones, so a dial moved the picture
//! only when the change cleared the band, and the way back never did.

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
/// Probe row: clear of the letterbox and of nothing else — every stripe in
/// [`dial_asset`] runs the full height.
const PROBE_ROW: u16 = 2;
/// Cell columns fully inside each stripe at 80 columns over 192 px (2.4 px
/// per cell): faint E stripe x 32..56, luma stripe P x 64..88, firm E stripe
/// x 128..152, luma stripe Q x 160..184.
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

/// Two frames, 192×108, Y + E/Ex/Ey, no NORM table (identity LUT, single
/// shot — so the shot-change reset in `update_levels` never fires and only
/// a dial can reset state). Flat luma 100 with four vertical stripes:
///
/// - a FAINT edge stripe at E = 24, between `edge_t_off` (16) and
///   `edge_t_on` (32): off cold, armed once the dial lowers T_on below 24,
///   and — this is the trap — held by T_off for as long as `was_edge` lives;
/// - a FIRM edge stripe at E = 60: drawn at the default, released once the
///   dial raises T_on to 60 (the gate is strict);
/// - luma stripe P: 100 on frame 0, 136 on frame 1 — a quarter of a ramp
///   step past the boundary at 128 (the truecolor ASCII ramp is capped to 8
///   steps of 32), so the default hysteresis width (160 = 0.625 step) holds
///   it and a width of 0 lets it move;
/// - luma stripe Q: 100 then 152 — 0.75 step past, so the default width
///   lets it move and the full width (255) holds it.
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
            // Gradient along +x → Ex = 128 + E/2 (INTERFACES note 18): a
            // fully coherent vertical contour.
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

/// Render `frame` on the player as it stands (warm state and all) and hand
/// back the grid.
fn render(p: &mut Player<'_>, backend: &mut SimBackend, frame: u32) -> Vec<Cell> {
    p.render_present(backend, frame).unwrap();
    backend.take_output();
    p.grid().as_slice().to_vec()
}

/// A fresh player at `params` rendering `frame` cold: the one picture a dial
/// position is supposed to mean.
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

/// The glyph a probe column shows in `grid`. The glyph, not the whole cell:
/// the gray foreground is the cell's instantaneous luma and follows the
/// planes on every frame; only the ramp index has memory.
fn glyph_at(grid: &[Cell], p: &Player<'_>, col: u16) -> char {
    let vp = p.viewport().unwrap();
    grid[(vp.pad_top + PROBE_ROW) as usize * COLS as usize + (vp.pad_left + col) as usize].glyph()
}

/// Every dial climbs to its top one press at a time, comes back down through
/// the SAME values to exactly where it started, carries on to its floor and
/// climbs back the same way — and a coalesced turn (a held key nets one
/// `dial_delta` of N per drain) lands where N single presses do. The top of
/// the 255 scales is not on the 16 grid, which is where a naive clamp lost a
/// step: up 6 from 160 saturates at 255, and down 6 used to land on 159.
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

        // Every detent the walk visited is on the step grid, bar the clamped top.
        for &v in up.iter().chain(&down) {
            assert!(
                v == dial.max() || i32::from(v) % dial.step() == 0,
                "{label}: {v} is off the {} grid",
                dial.step()
            );
        }

        // One coalesced turn of N (a held key nets one `dial_delta` of N per
        // drain) lands where N single presses do, in both directions and
        // through the stops — a press absorbed at a stop is not banked, so
        // an overshooting walk back ends past the origin either way.
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

/// `[` and `]` through the REAL event queue: net steps per drain, so a held
/// key is one bigger move, and `d` counts its presses.
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

/// Edge strength up N then down N restores the picture byte for byte, and
/// turning on past the origin releases edges the default draws: the dial
/// works in both directions from wherever it is.
///
/// This is the bug as reported. Turning the dial up lowered T_on and armed
/// the faint stripe; turning it back raised T_on again but `was_edge` held
/// the stripe above T_off, so the edges never went away — one direction
/// worked and the other did nothing.
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

    // Up 3: T_on 32 → 20 — the faint stripe arms.
    dial.turn(&mut compose, 3);
    assert_eq!(compose.edge_t_on, 20);
    p.set_compose_params(compose);
    let more = render(&mut p, &mut backend, 0);
    assert_eq!(probe(&p, FAINT_COL).0, layer::EDGE, "T_on=20 < 24 arms the faint stripe");
    assert_ne!(more, origin);

    // Down 3: T_on back to 32 — the faint stripe must let go, and the whole
    // picture must be the one the default draws.
    dial.turn(&mut compose, -3);
    assert_eq!(compose, ComposeParams::default());
    p.set_compose_params(compose);
    let back = render(&mut p, &mut backend, 0);
    assert_eq!(probe(&p, FAINT_COL).0, layer::BASE, "T_on back at 32: the faint edge releases");
    assert_eq!(back, origin, "up 3 / down 3 must restore the picture exactly");

    // Down 7 more: T_on 60 — the firm stripe (E=60, strict gate) releases
    // too. The opposite direction from the origin, not merely the way back.
    dial.turn(&mut compose, -7);
    assert_eq!(compose.edge_t_on, 60);
    p.set_compose_params(compose);
    let fewer = render(&mut p, &mut backend, 0);
    assert_eq!(probe(&p, FIRM_COL).0, layer::BASE, "T_on=60 releases the firm stripe");
    assert_eq!(probe(&p, FAINT_COL).0, layer::BASE);
    assert_ne!(fewer, origin);
    assert_eq!(fewer, cold(&asset, compose, 0), "a turned dial is a cold start at its position");

    // And up 7: the origin again.
    dial.turn(&mut compose, 7);
    p.set_compose_params(compose);
    assert_eq!(render(&mut p, &mut backend, 0), origin, "down 7 / up 7 must restore the picture");
}

/// Shadow lift up N then down N restores the picture, and 0 is its floor:
/// the origin IS the limit in that direction, so pressing on holds both the
/// value and the picture there. On a NORM-carrying asset (GradientMotion)
/// and on one without a shot table, where the levels LUT is the identity —
/// the lift is folded into that LUT, and a cache keyed on the shot alone
/// never rebuilt it there, so the dial did nothing at all.
#[test]
fn shadow_lift_turned_up_and_back_restores_the_picture() {
    // The fine ASCII ramp backs every base glyph at 80 viewport columns; its
    // order is the ink order whatever length the color depth caps it to.
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

        // Up 8: a half lift. Base-ramp cells may only move to denser glyphs.
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

        // Down 8: exactly the origin again …
        dial.turn(&mut compose, -8);
        assert_eq!(compose, ComposeParams::default());
        p.set_compose_params(compose);
        assert_eq!(render(&mut p, &mut backend, frame), origin, "{name}: up 8 / down 8");

        // … and 0 is the floor: pressing on changes neither value nor picture.
        dial.turn(&mut compose, -1);
        assert_eq!(compose.shadow_lift, 0);
        p.set_compose_params(compose);
        assert_eq!(render(&mut p, &mut backend, frame), origin, "{name}: the floor holds");
    }
}

/// The hysteresis dial's effect is temporal — how far a cell's luma must
/// cross a ramp boundary before its glyph follows — so it is observed over
/// a frame pair played in order. Narrower = more responsive, wider =
/// stickier, and the default in the middle is restored exactly from either
/// side. Then the mechanism itself: after a turn, the frame on screen is a
/// cold start at the new width, not the old width's memory.
#[test]
fn hysteresis_turned_down_and_up_tracks_in_both_directions() {
    let asset = dial_asset();
    let mut backend = SimBackend::new(COLS, ROWS);
    let mut p = player(&asset);
    p.enable_layer_mask();
    p.reflow(&mut backend, COLS, ROWS);
    let dial = Dial::Hysteresis;
    let mut compose = ComposeParams::default();

    // The plain-quantized pictures of each frame: what a cell shows with no
    // memory at all. The stripes must genuinely cross a ramp boundary.
    let (plain0, plain1) = (cold(&asset, compose, 0), cold(&asset, compose, 1));
    let held_p = glyph_at(&plain0, &p, P_COL);
    let moved_p = glyph_at(&plain1, &p, P_COL);
    let held_q = glyph_at(&plain0, &p, Q_COL);
    let moved_q = glyph_at(&plain1, &p, Q_COL);
    assert_ne!(held_p, moved_p, "stripe P must cross a ramp step between the frames");
    assert_ne!(held_q, moved_q, "stripe Q must cross a ramp step between the frames");

    // Play the pair on the player as it stands and hand back frame 1.
    let pair = |p: &mut Player<'_>, backend: &mut SimBackend| {
        render(p, backend, 0);
        render(p, backend, 1)
    };

    // Default width 160 (0.625 step): P (0.25 step past) holds, Q (0.75
    // step past) moves.
    let origin = pair(&mut p, &mut backend);
    assert_eq!(glyph_at(&origin, &p, P_COL), held_p, "width 160 holds a 0.25-step crossing");
    assert_eq!(glyph_at(&origin, &p, Q_COL), moved_q, "width 160 lets a 0.75-step crossing move");

    // Down 10: width 0 — no memory, every cell follows its luma.
    dial.turn(&mut compose, -10);
    assert_eq!(compose.idx_hyst_q8, 0);
    p.set_compose_params(compose);
    let responsive = pair(&mut p, &mut backend);
    assert_eq!(responsive, plain1, "width 0 is plain quantization");
    assert_ne!(responsive, origin);

    // Up 10: the default again, exactly.
    dial.turn(&mut compose, 10);
    assert_eq!(compose, ComposeParams::default());
    p.set_compose_params(compose);
    assert_eq!(pair(&mut p, &mut backend), origin, "down 10 / up 10 must restore the picture");

    // Up 6: width 255 (a whole step less one) — the opposite direction:
    // even Q holds now.
    dial.turn(&mut compose, 6);
    assert_eq!(compose.idx_hyst_q8, 255);
    p.set_compose_params(compose);
    let sticky = pair(&mut p, &mut backend);
    assert_eq!(glyph_at(&sticky, &p, P_COL), held_p);
    assert_eq!(glyph_at(&sticky, &p, Q_COL), held_q, "width 255 holds a 0.75-step crossing");
    assert_ne!(sticky, origin);

    // Down 6: the default again, from the other side.
    dial.turn(&mut compose, -6);
    assert_eq!(compose, ComposeParams::default());
    p.set_compose_params(compose);
    assert_eq!(pair(&mut p, &mut backend), origin, "up 6 / down 6 must restore the picture");

    // The mechanism. Frame 1 is on screen with P held at its frame-0 glyph.
    // Widening the band and re-rendering that same frame must show the cold
    // picture at the new width — plain frame 1 — not keep P where the OLD
    // width's memory left it. Without the reset a wider band just kept
    // holding, and the dial read as doing nothing.
    dial.turn(&mut compose, 6);
    p.set_compose_params(compose);
    let after_turn = render(&mut p, &mut backend, 1);
    assert_eq!(glyph_at(&after_turn, &p, P_COL), moved_p, "a turn re-quantizes the frame on screen");
    assert_eq!(after_turn, cold(&asset, compose, 1), "a turned dial is a cold start at its position");
}

/// The rule behind all of the above, for every dial: after a turn the frame
/// on screen equals a cold start at the new position — but a press that
/// moves nothing (the dial is at its stop) leaves the warm picture alone,
/// so leaning on a saturated dial never flashes the screen.
#[test]
fn a_turn_is_a_cold_start_and_a_stopped_dial_is_not() {
    let asset = dial_asset();
    for dial in Dial::ALL {
        let label = dial.label();
        let mut backend = SimBackend::new(COLS, ROWS);
        let mut p = player(&asset);
        p.reflow(&mut backend, COLS, ROWS);
        let mut compose = ComposeParams::default();
        // Warm up on both frames so held state exists to go stale.
        render(&mut p, &mut backend, 0);
        render(&mut p, &mut backend, 1);

        dial.turn(&mut compose, 1);
        p.set_compose_params(compose);
        assert_eq!(render(&mut p, &mut backend, 1), cold(&asset, compose, 1), "{label}: +1");
        dial.turn(&mut compose, -2);
        p.set_compose_params(compose);
        assert_eq!(render(&mut p, &mut backend, 1), cold(&asset, compose, 1), "{label}: -2");

        // Drive it onto a stop, warm the picture there, then press again:
        // nothing changed, so nothing resets.
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
