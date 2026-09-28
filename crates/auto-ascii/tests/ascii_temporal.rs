use std::path::PathBuf;

use auto_ascii::deck::{ClipDeck, DeckConfig};
use auto_ascii::pipeline::Player;
use auto_ascii::{Codec, Dial, Located};
use auto_ascii_core::codec::ascii::{ASCII_RAMP, cell_within_cap};
use auto_ascii_core::{Cell, ColorDepth, GlyphTier};
use auto_ascii_format::AsciiReader;
use auto_ascii_term::{Event, Key, SimBackend};

const ASSET: &[u8] = include_bytes!("fixtures/architect-motion.bin");

fn player(codec: Codec, cols: u16, rows: u16) -> Player<'static> {
    player_of(ASSET, codec, cols, rows)
}

fn player_of(asset: &'static [u8], codec: Codec, cols: u16, rows: u16) -> Player<'static> {
    let mut p = Player::new(
        AsciiReader::open(asset).unwrap(), 2.0, false, ColorDepth::True, GlyphTier::UnicodeBlocks,
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

fn ramp_gap(a: char, b: char) -> usize {
    let step = |c| ASCII_RAMP.iter().position(|&r| r == c);
    step(a).zip(step(b)).map_or(0, |(a, b)| a.abs_diff(b))
}

#[test]
fn ascii_glyphs_settle_on_real_playback() {
    for (cols, rows) in [(80, 24), (200, 56)] {
        let mut warm = player(Codec::Ascii, cols, rows);
        let mut cold = player(Codec::Ascii, cols, rows);
        let mut stuck = vec![true; cols as usize * rows as usize];
        for f in 0..150 {
            warm.render_grid(f).unwrap();
            if f < 105 {
                continue;
            }
            cold.reset_temporal_state();
            cold.render_grid(f).unwrap();
            for ((s, a), b) in stuck.iter_mut().zip(warm.grid().as_slice()).zip(cold.grid().as_slice()) {
                *s &= ramp_gap(a.glyph(), b.glyph()) >= 2;
            }
        }
        let stuck = stuck.iter().filter(|&&s| s).count();
        eprintln!("{cols}x{rows}: {stuck} cells two or more ramp steps from cold for 45 frames");
        assert!(stuck * 100 <= cols as usize * rows as usize, "{cols}x{rows}: {stuck} stale glyphs");
    }
}

#[test]
fn ascii_repeated_frames_converge_to_cold_glyphs() {
    for (cols, rows) in [(80, 24), (200, 56)] {
        let mut warm = player(Codec::Ascii, cols, rows);
        for f in 0..150 {
            warm.render_grid(f).unwrap();
        }
        for _ in 0..80 {
            warm.render_grid(149).unwrap();
        }
        let mut cold = player(Codec::Ascii, cols, rows);
        cold.render_grid(149).unwrap();
        let far = warm.grid().as_slice().iter().zip(cold.grid().as_slice())
            .filter(|(a, b)| ramp_gap(a.glyph(), b.glyph()) >= 2)
            .count();
        eprintln!("{cols}x{rows}: {far} cells two or more ramp steps from cold after 80 repeats");
        assert!(far * 200 <= cols as usize * rows as usize, "{cols}x{rows}: {far} stale glyphs");
    }
}

fn toggles(codec: Codec) -> usize {
    toggles_in(ASSET, codec, (200, 56), 60..150)
}

fn toggles_in(asset: &'static [u8], codec: Codec, (cols, rows): (u16, u16), frames: std::ops::Range<u32>) -> usize {
    let mut p = player_of(asset, codec, cols, rows);
    let mut prev: Vec<Cell> = Vec::new();
    let mut switches = 0;
    for f in frames {
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

fn assert_toggles_bounded_by_pixels(asset: &'static [u8], sizes: &[(u16, u16)]) {
    for &size in sizes {
        let ascii = toggles_in(asset, Codec::Ascii, size, 0..90);
        let pixels = toggles_in(asset, Codec::Pixels, size, 0..90);
        eprintln!("{size:?}: glyph switches ascii {ascii}, pixels {pixels}");
        assert!(ascii * 5 <= pixels * 6, "{size:?}: ascii {ascii}, pixels {pixels}");
    }
}

#[test]
fn ascii_toggle_rate_is_bounded_on_death_star() {
    assert_toggles_bounded_by_pixels(include_bytes!("fixtures/death-star-466.bin"), &[(80, 24), (200, 56)]);
}

#[test]
fn ascii_toggle_rate_is_bounded_on_millennium_falcon() {
    assert_toggles_bounded_by_pixels(include_bytes!("fixtures/falcon-3545.bin"), &[(80, 24), (200, 56)]);
}

#[test]
fn ascii_toggle_rate_is_bounded_on_techno_6() {
    assert_toggles_bounded_by_pixels(include_bytes!("fixtures/techno6-390.bin"), &[(80, 24)]);
}

#[test]
fn ascii_toggle_rate_is_bounded_on_darth_vader() {
    assert_toggles_bounded_by_pixels(include_bytes!("fixtures/vader-1126.bin"), &[(80, 24), (200, 56)]);
}

fn fg_lumas(codec: Codec, tier: GlyphTier) -> Vec<u32> {
    let mut p = Player::new(AsciiReader::open(ASSET).unwrap(), 2.0, false, ColorDepth::True, tier).unwrap();
    p.set_codec(codec);
    p.reflow_grid(200, 56);
    for f in 0..=40 {
        p.render_grid(f).unwrap();
    }
    let luma = |c: &Cell| (299 * c.fg.r as u32 + 587 * c.fg.g as u32 + 114 * c.fg.b as u32 + 500) / 1000;
    let mut v: Vec<u32> = p.grid().as_slice().iter().filter(|c| c.glyph() != ' ').map(luma).collect();
    v.sort_unstable();
    v
}

#[test]
fn ascii_glyph_brightness_range_matches_letters_on_the_ascii_tier() {
    let letters = fg_lumas(Codec::Letters, GlyphTier::Ascii);
    let ascii = fg_lumas(Codec::Ascii, GlyphTier::UnicodeBlocks);
    let pct = |v: &[u32], p: usize| v[(v.len() * p / 100).min(v.len() - 1)];
    let hi = |v: &[u32]| v.iter().filter(|&&l| l >= 240).count() as f64 / v.len() as f64;
    let (l, a) = ([5, 50, 95].map(|p| pct(&letters, p)), [5, 50, 95].map(|p| pct(&ascii, p)));
    eprintln!("fg luma p5/p50/p95 letters {l:?} ascii {a:?}, >=240 {:.3} {:.3}, cells {} {}",
        hi(&letters), hi(&ascii), letters.len(), ascii.len());
    for k in 0..3 {
        assert!(a[k].abs_diff(l[k]) * 20 <= l[k].max(40), "p{}: letters {}, ascii {}", [5, 50, 95][k], l[k], a[k]);
    }
    let (sl, sa) = (l[2] - l[0], a[2] - a[0]);
    assert!(sa.abs_diff(sl) * 20 <= sl, "spread95: letters {sl}, ascii {sa}");
    assert!((hi(&ascii) - hi(&letters)).abs() <= 0.02, ">=240: letters {:.3}, ascii {:.3}", hi(&letters), hi(&ascii));
}
