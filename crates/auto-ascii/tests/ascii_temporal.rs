use std::path::PathBuf;

use auto_ascii::deck::{ClipDeck, DeckConfig};
use auto_ascii::pipeline::Player;
use auto_ascii::{Codec, Dial, Located};
use auto_ascii_core::codec::ascii::cell_within_cap;
use auto_ascii_core::{Cell, ColorDepth, GlyphTier};
use auto_ascii_format::AsciiReader;
use auto_ascii_term::{Event, Key, SimBackend};

const ASSET: &[u8] = include_bytes!("fixtures/architect-motion.bin");

fn player(codec: Codec, cols: u16, rows: u16) -> Player<'static> {
    let mut p = Player::new(
        AsciiReader::open(ASSET).unwrap(), 2.0, false, ColorDepth::True, GlyphTier::UnicodeBlocks,
    ).unwrap();
    p.set_codec(codec);
    p.reflow_grid(cols, rows);
    p
}

fn round_trip(p: &mut Player<'_>, dial: Dial) {
    let original = p.compose_params();
    let mut changed = original;
    dial.turn(&mut changed, if dial.get(&original) == dial.max() { -1 } else { 1 });
    assert_ne!(changed, original);
    p.set_compose_params(changed);
    p.set_compose_params(original);
}

#[test]
fn ascii_startup_matches_pixels_then_codec_cycle() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/architect-motion.bin");
    let cfg = DeckConfig {
        cell_aspect: 2.0, repaint_full: false, color: ColorDepth::True, glyph_tier: GlyphTier::UnicodeBlocks,
    };
    for (cols, rows) in [(80, 24), (239, 36), (240, 36), (500, 140)] {
        let mut direct = ClipDeck::new(vec![path.clone()], cfg);
        direct.set_codec(Codec::Ascii);
        direct.set_size(cols, rows);
        let mut cycled = ClipDeck::new(vec![path.clone()], cfg);
        cycled.set_size(cols, rows);
        let mut backend = SimBackend::new(cols, rows);
        for f in 0..20 {
            cycled.render_at(Some(Located { clip_idx: 0, local_frame: f })).unwrap();
        }
        for _ in 0..2 {
            backend.push_event(Event::Key(Key::Char('/')));
            let event = cycled.drain_events(&mut backend);
            assert_eq!(event.codec_cycle, 1);
            cycled.set_codec(cycled.codec().next());
        }
        assert_eq!(cycled.codec(), Codec::Ascii);
        for f in 20..24 {
            let at = Some(Located { clip_idx: 0, local_frame: f });
            direct.render_at(at).unwrap();
            cycled.render_at(at).unwrap();
            assert_eq!(direct.showing().as_slice(), cycled.showing().as_slice(), "{cols}x{rows}, f{f}");
        }
    }
}

#[test]
fn ascii_startup_and_resize_match_every_dial_round_trip() {
    for dial in Dial::ALL {
        for resized in [false, true] {
            let mut plain = player(Codec::Ascii, 80, 24);
            let mut reset = player(Codec::Ascii, 80, 24);
            if resized {
                for f in 0..30 {
                    plain.render_grid(f).unwrap();
                    reset.render_grid(f).unwrap();
                }
            }
            for (cols, rows) in [(200, 56), (500, 140), (97, 31)] {
                plain.reflow_grid(cols, rows);
                reset.reflow_grid(cols, rows);
                round_trip(&mut reset, dial);
                for f in 30..34 {
                    plain.render_grid(f).unwrap();
                    reset.render_grid(f).unwrap();
                    assert_eq!(plain.grid().as_slice(), reset.grid().as_slice(), "{dial:?}, {resized}, {cols}x{rows}");
                }
            }
        }
    }
}

#[test]
fn ascii_real_playback_stays_close_to_cold_at_zoom_sizes() {
    for (cols, rows) in [(80, 24), (128, 45), (200, 56), (320, 90), (500, 140)] {
        let mut p = player(Codec::Ascii, cols, rows);
        for f in 0..=120 {
            p.render_grid(f).unwrap();
        }
        let warm = p.grid().as_slice().to_vec();
        p.reset_temporal_state();
        p.render_grid(120).unwrap();
        let different = warm.iter().zip(p.grid().as_slice()).filter(|(a, b)| a != b).count();
        let shade = warm.iter().zip(p.grid().as_slice()).filter(|(a, b)| a.bg != b.bg || a.attrs != b.attrs).count();
        eprintln!("{cols}x{rows}: {different}/{} cells, {shade} shades", warm.len());
        assert!(different * 100 <= warm.len() * 22, "{cols}x{rows}: {different}/{}", warm.len());
        assert!(shade * 100 <= warm.len() * 3, "{cols}x{rows}: {shade} stale shades");
        assert!(warm.iter().all(|c| cell_within_cap(c, ColorDepth::True)));
    }
}

fn toggles(codec: Codec) -> usize {
    let mut p = player(codec, 200, 56);
    let mut prev: Vec<Cell> = Vec::new();
    let mut switches = 0;
    for f in 60..150 {
        p.render_grid(f).unwrap();
        switches += prev.iter().zip(p.grid().as_slice()).filter(|(a, b)| a.glyph() != b.glyph()).count();
        prev.clear();
        prev.extend_from_slice(p.grid().as_slice());
    }
    switches
}

#[test]
fn ascii_real_asset_glyph_toggle_rate_is_bounded_by_pixels() {
    let ascii = toggles(Codec::Ascii);
    let pixels = toggles(Codec::Pixels);
    eprintln!("glyph switches: ascii {ascii}, pixels {pixels}");
    assert!(ascii * 5 <= pixels * 6, "ascii {ascii}, pixels {pixels}");
}
