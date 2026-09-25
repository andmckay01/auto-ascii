use std::io::Cursor;
use std::path::PathBuf;

use auto_ascii::deck::{ClipDeck, DeckConfig};
use auto_ascii::pipeline::Player;
use auto_ascii::{Codec, Located, RenderSession};
use auto_ascii_core::codec::letters::letters_glyphs;
use auto_ascii_core::{Cell, ColorDepth, GlyphTier, Grid};
use auto_ascii_eval::fixtures::{Fixture, build_fixture};
use auto_ascii_format::header::plane_id;
use auto_ascii_format::{AsciiReader, AsciiWriter, Meta, PlaneRef, WriterOptions};
use auto_ascii_term::{Event, Key, SimBackend};

const W: usize = 192;
const H: usize = 108;
const FRAMES: u32 = 6;

fn full_asset() -> Vec<u8> {
    let opts = WriterOptions {
        base_w: W as u16,
        base_h: H as u16,
        plane_ids: vec![plane_id::Y, plane_id::E, plane_id::EX, plane_id::EY, plane_id::H, plane_id::C],
        zstd_level: 3,
        ..WriterOptions::default()
    };
    let meta = Meta { factory_version: "codecs-test".into(), source: "synthetic".into(), palette_hints: vec![] };
    let mut writer = AsciiWriter::new(Cursor::new(Vec::new()), opts, &meta).unwrap();
    for f in 0..FRAMES as usize {
        let luma = |x: usize, y: usize| -> u8 {
            let (cx, cy) = (70.0 + 6.0 * f as f64, 60.0);
            let d = ((x as f64 - cx).powi(2) + (y as f64 - cy).powi(2)).sqrt();
            if (8..14).contains(&y) && x > 20 && x < 170 {
                return 250;
            }
            if d < 30.0 {
                return (255.0 - d * 2.0) as u8;
            }
            ((x + y) as f64 * 0.6).min(150.0) as u8
        };
        let y: Vec<u8> = (0..W * H).map(|i| luma(i % W, i / W)).collect();
        let (mut e, mut ex, mut ey) = (vec![0u8; W * H], vec![128u8; W * H], vec![128u8; W * H]);
        for yy in 1..H - 1 {
            for xx in 1..W - 1 {
                let gx = f64::from(y[yy * W + xx + 1]) - f64::from(y[yy * W + xx - 1]);
                let gy = f64::from(y[(yy + 1) * W + xx]) - f64::from(y[(yy - 1) * W + xx]);
                let mag = (gx * gx + gy * gy).sqrt();
                if mag < 1.0 {
                    continue;
                }
                let i = yy * W + xx;
                e[i] = mag.min(255.0) as u8;
                ex[i] = (128.0 + (gx * gx - gy * gy) / (2.0 * mag)).clamp(0.0, 255.0) as u8;
                ey[i] = (128.0 + gx * gy / mag).clamp(0.0, 255.0) as u8;
            }
        }
        let h: Vec<u8> = (0..W * H)
            .map(|i| {
                let (x, yy) = (i % W, i / W);
                if x % 37 == 5 && yy % 23 == 7 {
                    1
                } else if x > 170 && yy > 90 {
                    2
                } else {
                    0
                }
            })
            .collect();
        let (cw, ch) = (W / 2, H / 2);
        let mut c = Vec::with_capacity(cw * ch * 2);
        for yy in 0..ch {
            for x in 0..cw {
                let l = u16::from(luma(2 * x, 2 * yy));
                let (r, g, b) = (l, (l * (cw - x) as u16 / cw as u16), (l * x as u16 / cw as u16));
                let v = ((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3);
                c.extend_from_slice(&v.to_le_bytes());
            }
        }
        writer
            .write_frame(&[
                PlaneRef { id: plane_id::Y, data: &y },
                PlaneRef { id: plane_id::E, data: &e },
                PlaneRef { id: plane_id::EX, data: &ex },
                PlaneRef { id: plane_id::EY, data: &ey },
                PlaneRef { id: plane_id::H, data: &h },
                PlaneRef { id: plane_id::C, data: &c },
            ])
            .unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn player(bytes: &[u8], tier: GlyphTier) -> Player<'_> {
    Player::new(AsciiReader::open(bytes).unwrap(), 2.0, true, ColorDepth::True, tier).unwrap()
}

fn render(p: &mut Player<'_>, backend: &mut SimBackend, last: u32) -> Grid<Cell> {
    for f in 0..=last {
        p.render_present(backend, f).unwrap();
        backend.take_output();
    }
    p.grid().clone()
}

fn row(grid: &Grid<Cell>, r: u16) -> String {
    grid.row(r).iter().map(|c| c.glyph()).collect()
}

#[test]
fn slash_and_s_surface_through_the_event_queue() {
    let asset = build_fixture(Fixture::GradientMotion);
    let mut backend = SimBackend::new(80, 24);
    let mut p = player(&asset, GlyphTier::Ascii);
    p.reflow(&mut backend, 80, 24);

    for _ in 0..3 {
        backend.push_event(Event::Key(Key::Char('/')));
    }
    let d = p.drain_events(&mut backend);
    assert_eq!(d.codec_cycle, 3);
    assert!(!d.save && !d.toggle_hints && !d.toggle_pause && !d.quit);
    assert_eq!((d.jump_digit, d.seek_steps, d.dial_cycle, d.dial_delta), (None, 0, 0, 0));

    backend.push_event(Event::Key(Key::Char('s')));
    backend.push_event(Event::Key(Key::Char('s')));
    let d = p.drain_events(&mut backend);
    assert!(d.save);
    assert_eq!((d.codec_cycle, d.dial_cycle, d.dial_delta), (0, 0, 0));

    for key in ['q', ' ', '0', '9', 'd', '[', ']', 'v', 'x', '?'] {
        backend.push_event(Event::Key(Key::Char(key)));
        let d = p.drain_events(&mut backend);
        assert_eq!((d.codec_cycle, d.save), (0, false), "{key:?} must not cycle or save");
    }
    for key in [Key::Left, Key::Right] {
        backend.push_event(Event::Key(key));
        let d = p.drain_events(&mut backend);
        assert_eq!((d.codec_cycle, d.save), (0, false));
    }
}

#[test]
fn letters_fade_to_black_without_a_shadow_plane() {
    let levels = [120, 80, 40, 32, 0, 0, 0, 0, 120, 40, 0];
    let opts = WriterOptions {
        base_w: W as u16,
        base_h: H as u16,
        plane_ids: vec![plane_id::Y],
        zstd_level: 3,
        ..WriterOptions::default()
    };
    let meta = Meta { factory_version: "fade-test".into(), source: "synthetic".into(), palette_hints: vec![] };
    let mut writer = AsciiWriter::new(Cursor::new(Vec::new()), opts, &meta).unwrap();
    for n in levels {
        writer.write_frame(&[PlaneRef { id: plane_id::Y, data: &vec![n; W * H] }]).unwrap();
    }
    let bytes = writer.finish().unwrap().into_inner();
    for tier in [GlyphTier::Ascii, GlyphTier::UnicodeBlocks, GlyphTier::BrailleVerified] {
        for color in [ColorDepth::True, ColorDepth::C256, ColorDepth::C16, ColorDepth::Mono] {
            for hyst in [160, 255] {
                let mut p = Player::new(AsciiReader::open(&bytes).unwrap(), 2.0, true, color, tier).unwrap();
                p.set_codec(Codec::Letters);
                p.set_compose_params(auto_ascii_core::ComposeParams { idx_hyst_q8: hyst, ..Default::default() });
                let mut backend = SimBackend::new(80, 24);
                p.reflow(&mut backend, 80, 24);
                for (frame, n) in levels.into_iter().enumerate() {
                    p.render_present(&mut backend, frame as u32).unwrap();
                    backend.take_output();
                    let lit = p.grid().as_slice().iter().any(|c| c.glyph() != ' ');
                    if n == 0 {
                        assert!(!lit, "black frame {frame}: {tier:?}, {color:?}, hysteresis {hyst}");
                    } else if frame == 0 || frame == 8 {
                        assert!(lit, "the sequence must warm visible glyphs before black");
                    }
                }
            }
        }
    }
}

#[test]
fn codec_switch_is_a_cold_start_and_pixels_comes_back_exactly() {
    let asset = full_asset();
    for tier in [GlyphTier::Ascii, GlyphTier::UnicodeBlocks] {
        let (mut b1, mut b2) = (SimBackend::new(80, 24), SimBackend::new(80, 24));
        let mut p = player(&asset, tier);
        p.reflow(&mut b1, 80, 24);
        let pixels = render(&mut p, &mut b1, 3);

        p.set_codec(Codec::Letters);
        assert_eq!(p.codec(), Codec::Letters);
        p.render_present(&mut b1, 4).unwrap();
        let mut fresh = player(&asset, tier);
        fresh.set_codec(Codec::Letters);
        fresh.reflow(&mut b2, 80, 24);
        fresh.render_present(&mut b2, 4).unwrap();
        assert_eq!(p.grid().as_slice(), fresh.grid().as_slice(), "{tier:?}: switch = cold start");
        assert_ne!(p.grid().as_slice(), pixels.as_slice(), "letters is a different picture");

        p.set_codec(Codec::Pixels);
        p.render_present(&mut b1, 3).unwrap();
        let mut cold = player(&asset, tier);
        cold.reflow(&mut b2, 80, 24);
        cold.render_present(&mut b2, 3).unwrap();
        assert_eq!(p.grid().as_slice(), cold.grid().as_slice(), "{tier:?}: pixels restored");
    }
}

#[test]
fn deck_carries_the_codec_across_clip_switches() {
    let dir = std::env::temp_dir().join(format!("auto-ascii-codecs-deck-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bytes = full_asset();
    let paths: Vec<PathBuf> = (0..2)
        .map(|i| {
            let p = dir.join(format!("clip-{i}.ascii"));
            std::fs::write(&p, &bytes).unwrap();
            p
        })
        .collect();
    let cfg = DeckConfig {
        cell_aspect: 2.0,
        repaint_full: false,
        color: ColorDepth::True,
        glyph_tier: GlyphTier::Ascii,
    };
    let at = |clip_idx| Some(Located { clip_idx, local_frame: 2 });
    let mut deck = ClipDeck::new(paths.clone(), cfg);
    deck.set_size(80, 24);
    deck.render_at(at(0)).unwrap();
    let pixels = deck.showing().as_slice().to_vec();

    deck.set_codec(Codec::Letters);
    deck.render_at(at(1)).unwrap();
    let letters = deck.showing().as_slice().to_vec();
    assert_ne!(letters, pixels);
    let allowed = letters_glyphs(false);
    assert!(letters.iter().all(|c| allowed.contains(&c.glyph())), "clip 1 composes in letters");
    deck.render_at(at(0)).unwrap();
    assert_eq!(deck.showing().as_slice(), &letters[..], "clip 0 switched with the deck");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hints_row_names_the_new_keys_where_there_is_room() {
    let asset = build_fixture(Fixture::GradientMotion);
    let (cols, rows) = (120u16, 30u16);
    let mut backend = SimBackend::new(cols, rows);
    let mut p = player(&asset, GlyphTier::Ascii);
    p.reflow(&mut backend, cols, rows);
    p.set_hint_overlay(true);
    p.render_present(&mut backend, 0).unwrap();
    let hints = row(p.grid(), rows - 2);
    assert_eq!(
        hints.trim_end(),
        " q quit   space pause   0-9 jump   <- -> 5s   d dial   [ ] adjust   / codec   s save   v controls"
    );
    let mut backend = SimBackend::new(80, 24);
    let mut p = player(&asset, GlyphTier::Ascii);
    p.reflow(&mut backend, 80, 24);
    p.set_hint_overlay(true);
    p.render_present(&mut backend, 0).unwrap();
    assert!(!row(p.grid(), 22).contains("codec"), "80 columns keep the M6 row");
}

#[test]
fn info_row_rides_with_the_hints() {
    let asset = build_fixture(Fixture::GradientMotion);
    let (cols, rows) = (80u16, 24u16);
    let mut backend = SimBackend::new(cols, rows);
    let mut p = Player::new(
        AsciiReader::open(&asset).unwrap(),
        2.0,
        false,
        ColorDepth::True,
        GlyphTier::Ascii,
    )
    .unwrap();
    p.reflow(&mut backend, cols, rows);
    let mut reference = player(&asset, GlyphTier::Ascii);
    let mut b_ref = SimBackend::new(cols, rows);
    reference.reflow(&mut b_ref, cols, rows);
    reference.render_present(&mut b_ref, 0).unwrap();

    p.set_info_overlay(Some(" Café   codec: letters   settings: saved "));
    p.render_present(&mut backend, 0).unwrap();
    backend.take_output();
    assert_eq!(p.grid().as_slice(), reference.grid().as_slice(), "no hints, no info row");

    p.set_hint_overlay(true);
    p.render_present(&mut backend, 1).unwrap();
    backend.take_output();
    let info = row(p.grid(), rows - 3);
    assert!(info.starts_with(" Caf?   codec: letters   settings: saved "), "{info:?}");
    assert_eq!(info.chars().count(), cols as usize, "painted edge to edge");
    assert!(row(p.grid(), rows - 2).contains("v controls"), "hints below it");

    p.set_info_overlay(None);
    let stats = p.render_present(&mut backend, 2).unwrap();
    assert_eq!(stats.cells_damaged, u32::from(cols) * u32::from(rows), "hide → full repaint");
    assert!(!row(p.grid(), rows - 3).contains("codec"));
}

#[test]
fn render_session_selects_the_codec() {
    let path = std::env::temp_dir().join(format!("auto-ascii-codecs-session-{}.ascii", std::process::id()));
    std::fs::write(&path, full_asset()).unwrap();
    let mut s = RenderSession::open(&path).unwrap();
    assert_eq!(s.codec(), Codec::Pixels);
    let pixels = s.render(2, 100, 30).unwrap().as_slice().to_vec();
    s.set_codec(Codec::Letters);
    let letters = s.render(3, 100, 30).unwrap().as_slice().to_vec();
    assert_ne!(pixels, letters);
    let allowed = letters_glyphs(true);
    assert!(letters.iter().all(|c| allowed.contains(&c.glyph())));
    assert!(letters.iter().any(|c| c.glyph() == '█'), "the disc core fills");
    let _ = std::fs::remove_file(&path);
}

fn golden_text(grid: &Grid<Cell>, title: &str) -> String {
    let mut s = format!("{title}\n");
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for r in 0..grid.rows() {
        s.push('|');
        for cell in grid.row(r) {
            s.push(cell.glyph());
            for b in [cell.fg.r, cell.fg.g, cell.fg.b, cell.bg.r, cell.bg.g, cell.bg.b] {
                h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
            }
        }
        s.push_str("|\n");
    }
    s.push_str(&format!("colors fnv1a64 {h:016x}\n"));
    s
}

fn check_golden(name: &str, text: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/goldens").join(name);
    if std::env::var_os("ASCII_UPDATE_GOLDENS").is_some() {
        std::fs::write(&path, text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("missing golden {} ({e}); bless with ASCII_UPDATE_GOLDENS=1", path.display())
    });
    assert_eq!(text, want, "letters render diverged from {}", path.display());
}

#[test]
fn letters_goldens() {
    let asset = full_asset();
    for (tier, name) in [
        (GlyphTier::Ascii, "letters_80x24_ascii.txt"),
        (GlyphTier::UnicodeBlocks, "letters_80x24_unicode.txt"),
    ] {
        let mut backend = SimBackend::new(80, 24);
        let mut p = player(&asset, tier);
        p.set_codec(Codec::Letters);
        p.reflow(&mut backend, 80, 24);
        let grid = render(&mut p, &mut backend, FRAMES - 1);
        let allowed = letters_glyphs(tier != GlyphTier::Ascii);
        assert!(grid.as_slice().iter().all(|c| allowed.contains(&c.glyph())));
        let text: String = grid.as_slice().iter().map(|c| c.glyph()).collect();
        assert!(text.contains(['|', '/', '\\']), "{name}: edge strokes");
        if tier != GlyphTier::Ascii {
            assert!(text.contains('█'), "{name}: dense fill");
        }
        let title = format!("letters codec, {tier:?} tier, truecolor, 80x24, frame {}", FRAMES - 1);
        check_golden(name, &golden_text(&grid, &title));
    }
}
