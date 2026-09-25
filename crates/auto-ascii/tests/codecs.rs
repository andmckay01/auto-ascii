use std::io::Cursor;
use std::path::PathBuf;

use auto_ascii::deck::{ClipDeck, DeckConfig};
use auto_ascii::pipeline::{Player, ProgressContext, color_depth};
use auto_ascii::{Codec, Located, RenderSession};
use auto_ascii_core::cell::attrs;
use auto_ascii_core::codec::ascii::ascii_glyphs;
use auto_ascii_core::codec::letters::letters_glyphs;
use auto_ascii_core::{Cell, ColorDepth, GlyphTier, Grid, Rgb};
use auto_ascii_eval::fixtures::{Fixture, build_fixture};
use auto_ascii_format::header::plane_id;
use auto_ascii_format::{AsciiReader, AsciiWriter, Meta, PlaneRef, WriterOptions};
use auto_ascii_term::{Backend, Caps, ColorTier, Event, Key, SimBackend};

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
    assert_eq!(text, want, "codec render diverged from {}", path.display());
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

#[test]
fn letters_goldens_untinted_tiers() {
    let asset = full_asset();
    for (tier, color, name) in [
        (GlyphTier::UnicodeBlocks, ColorDepth::C16, "letters_80x24_unicode_16color.txt"),
        (GlyphTier::Ascii, ColorDepth::Mono, "letters_80x24_ascii_mono.txt"),
    ] {
        let mut backend = SimBackend::new(80, 24);
        let mut p = Player::new(AsciiReader::open(&asset).unwrap(), 2.0, true, color, tier).unwrap();
        p.set_codec(Codec::Letters);
        p.reflow(&mut backend, 80, 24);
        let grid = render(&mut p, &mut backend, FRAMES - 1);
        assert!(grid.as_slice().iter().all(|c| c.bg == Rgb::BLACK), "{name}: no background tint");
        let title = format!("letters codec, {tier:?} tier, {color:?}, 80x24, frame {}", FRAMES - 1);
        check_golden(name, &golden_text(&grid, &title));
    }
}

fn background_sgrs(bytes: &[u8]) -> Result<Vec<String>, String> {
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b != 0x1b {
            if !(0x20..=0x7e).contains(&b) {
                return Err(format!("byte {b:#04x} at {i} outside an escape sequence"));
            }
            i += 1;
            continue;
        }
        if bytes.get(i + 1) != Some(&b'[') {
            return Err(format!("non-CSI escape at {i}"));
        }
        let start = i + 2;
        let end = (start..bytes.len())
            .find(|&j| (0x40..=0x7e).contains(&bytes[j]))
            .ok_or_else(|| format!("unterminated CSI at {i}"))?;
        let params = std::str::from_utf8(&bytes[start..end]).map_err(|e| e.to_string())?;
        if bytes[end] == b'm' {
            let ps: Vec<&str> = params.split(';').collect();
            let mut k = 0;
            while k < ps.len() {
                let v: u32 = ps[k].parse().unwrap_or(0);
                if v == 38 || v == 48 {
                    let n = if ps.get(k + 1) == Some(&"2") { 5 } else { 3 };
                    if v == 48 {
                        found.push(ps[k..(k + n).min(ps.len())].join(";"));
                    }
                    k += n;
                    continue;
                }
                if (40..=47).contains(&v) || (100..=107).contains(&v) {
                    found.push(v.to_string());
                }
                k += 1;
            }
        }
        i = end + 1;
    }
    Ok(found)
}

const SIZES: [(u16, u16); 10] =
    [(80, 24), (1, 1), (8, 3), (5, 2), (400, 120), (213, 58), (31, 8), (240, 36), (120, 40), (100, 60)];

fn deck_stream(codec: Codec, tier: GlyphTier, color: ColorTier, overlays: bool) -> Vec<u8> {
    let tag = format!("{}-{tier:?}-{color:?}-{codec:?}-{overlays}", std::process::id());
    let dir = std::env::temp_dir().join(format!("auto-ascii-codecs-stream-{tag}"));
    std::fs::create_dir_all(&dir).unwrap();
    let bytes = full_asset();
    let paths: Vec<PathBuf> = (0..2)
        .map(|i| {
            let p = dir.join(format!("clip-{i}.ascii"));
            std::fs::write(&p, &bytes).unwrap();
            p
        })
        .collect();
    let cfg = DeckConfig { cell_aspect: 2.0, repaint_full: false, color: color_depth(color), glyph_tier: tier };
    let mut deck = ClipDeck::new(paths, cfg);
    deck.set_codec(codec);
    let mut backend = SimBackend::new(80, 24);
    backend.set_caps(Caps { color, ..Caps::default() });
    if overlays {
        deck.set_hint_overlay(true);
        deck.set_info_overlay(Some(" Caf\u{e9} clip   codec: ascii   settings: saved "));
        let ctx = ProgressContext { frame: 90, frame_count: 600, fps_num: 30, fps_den: 1, clip: Some((1, 2)) };
        deck.set_progress_context(Some(ctx));
    }
    let mut out = Vec::new();
    for (k, (cols, rows)) in SIZES.into_iter().enumerate() {
        backend.resize(cols, rows);
        deck.set_size(cols, rows);
        backend.invalidate();
        for (step, located) in [Some((0, 2)), None, Some((1, 3)), Some((1, 4))].into_iter().enumerate() {
            if overlays {
                let dial = (k + step) % 2 == 1;
                deck.set_progress_overlay(!dial);
                deck.set_dial_overlay(dial.then_some(("shadow lift", 64, 255)));
                deck.set_paused(step == 3);
            }
            let at = located.map(|(clip_idx, local_frame)| Located { clip_idx, local_frame });
            deck.present_at(&mut backend, at).unwrap();
            out.extend(backend.take_output());
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn ascii_draws_nothing_but_printable_ascii_on_the_terminal_background() {
    for tier in [GlyphTier::Ascii, GlyphTier::UnicodeBlocks, GlyphTier::BrailleVerified] {
        for color in [ColorTier::True, ColorTier::C256, ColorTier::C16, ColorTier::Mono] {
            for overlays in [false, true] {
                let what = format!("{tier:?} {color:?} overlays {overlays}");
                let out = deck_stream(Codec::Ascii, tier, color, overlays);
                let bg = background_sgrs(&out).unwrap_or_else(|e| panic!("{what}: {e}"));
                assert!(bg.is_empty(), "{what}: background SGR {bg:?}");
                let text = String::from_utf8_lossy(&out);
                assert!(text.contains('@') && text.contains('/'), "{what}: a real picture");
                assert!(text.contains("AUTO-ASCII"), "{what}: the enlarge card was drawn");
                if overlays {
                    for want in ["v controls", "shadow lift", "PAUSED", "Caf? clip", "400x120 cells", "zoom out"] {
                        assert!(text.contains(want), "{what}: overlay {want:?} drawn");
                    }
                }
                if color != ColorTier::Mono {
                    assert!(text.contains("49m"), "{what}: the default background is set");
                }
            }
        }
    }
}

#[test]
fn other_codecs_keep_their_backgrounds_and_big_overlay_text() {
    let pixels = deck_stream(Codec::Pixels, GlyphTier::Ascii, ColorTier::True, true);
    let bg = background_sgrs(&pixels).unwrap();
    assert!(bg.contains(&"48;2;0;0;0".to_string()) && bg.contains(&"48;2;24;24;40".to_string()), "{bg:?}");
    let letters = deck_stream(Codec::Letters, GlyphTier::UnicodeBlocks, ColorTier::C16, true);
    assert!(background_sgrs(&letters).is_err(), "letters keeps blocks and big overlay text");
    assert!(String::from_utf8_lossy(&letters).contains('\u{2580}'), "big text at 400x120");
}

#[test]
fn ascii_goldens() {
    let asset = full_asset();
    for (tier, color, name) in [
        (GlyphTier::UnicodeBlocks, ColorDepth::True, "ascii_80x24_unicode.txt"),
        (GlyphTier::Ascii, ColorDepth::Mono, "ascii_80x24_ascii_mono.txt"),
    ] {
        let mut backend = SimBackend::new(80, 24);
        let mut p = Player::new(AsciiReader::open(&asset).unwrap(), 2.0, true, color, tier).unwrap();
        p.set_codec(Codec::Ascii);
        p.reflow(&mut backend, 80, 24);
        let grid = render(&mut p, &mut backend, FRAMES - 1);
        let allowed = ascii_glyphs();
        assert!(grid.as_slice().iter().all(|c| allowed.contains(&c.glyph()) && c.attrs == attrs::DEFAULT_BG));
        let text: String = grid.as_slice().iter().map(|c| c.glyph()).collect();
        assert!(text.contains(['|', '/', '\\']), "{name}: edge strokes");
        assert!(text.contains('@'), "{name}: the disc core is the densest glyph");
        let title = format!("ascii codec, {tier:?} tier, {color:?}, 80x24, frame {}", FRAMES - 1);
        check_golden(name, &golden_text(&grid, &title));
    }
}
