use std::io::Cursor;
use std::path::PathBuf;

use auto_ascii::deck::{ClipDeck, DeckConfig};
use auto_ascii::pipeline::{OverlayScale, Player, ProgressContext, UiRows, color_depth};
use auto_ascii::{Codec, Located, RenderSession};
use auto_ascii_core::codec::ascii::{
    SHADE_FLOOR, SHADES_256, ascii_glyphs, backing_within_cap, cell_within_cap, in_hue_family,
};
use auto_ascii_core::codec::letters::letters_glyphs;
use auto_ascii_core::{Cell, ColorDepth, GlyphTier, Grid, Rgb};
use auto_ascii_eval::fixtures::{Fixture, build_fixture};
use auto_ascii_format::header::plane_id;
use auto_ascii_format::{AsciiReader, AsciiWriter, Meta, PlaneRef, WriterOptions};
use auto_ascii_term::{Backend, Caps, ColorTier, Event, Key, SimBackend, quant};

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
        " q quit   space pause   0-9 jump   <- -> 5s   d dial   [ ] adjust   / codec   m sound   s save   v controls"
    );
    let mut backend = SimBackend::new(80, 24);
    let mut p = player(&asset, GlyphTier::Ascii);
    p.reflow(&mut backend, 80, 24);
    p.set_hint_overlay(true);
    p.render_present(&mut backend, 0).unwrap();
    assert!(!row(p.grid(), 22).contains("codec"), "80 columns keep the M6 row");
    for (cols, sound, codec, save) in [(89u16, true, false, false), (99, true, true, false), (108, true, true, true), (88, false, false, false)] {
        let mut backend = SimBackend::new(cols, 24);
        let mut p = player(&asset, GlyphTier::Ascii);
        p.reflow(&mut backend, cols, 24);
        p.set_hint_overlay(true);
        p.render_present(&mut backend, 0).unwrap();
        let hints = row(p.grid(), 22);
        assert_eq!(
            (hints.contains("m sound"), hints.contains("/ codec"), hints.contains("s save")),
            (sound, codec, save),
            "{cols} columns: m sound outlasts / codec, s save goes first: {hints:?}"
        );
        assert!(hints.contains("v controls"));
    }
}

#[test]
fn info_row_reads_the_sound_state_on_every_codec() {
    let asset = build_fixture(Fixture::GradientMotion);
    let (cols, rows) = (200u16, 56u16);
    let allowed = ascii_glyphs();
    for sound in ["on", "off", "wait", "none"] {
        for codec in Codec::ALL {
            let mut backend = SimBackend::new(cols, rows);
            let mut p = player(&asset, GlyphTier::UnicodeBlocks);
            p.set_codec(codec);
            p.reflow(&mut backend, cols, rows);
            let text = format!(" Interstellar   codec: {}   settings: saved   sound: {sound} ", codec.name());
            p.set_info_overlay(Some(&text));
            p.set_hint_overlay(true);
            p.render_present(&mut backend, 0).unwrap();
            let info = row(p.grid(), rows - 3);
            let pad = " ".repeat(cols as usize - text.len() - " 200x56 cells ".len());
            assert_eq!(info, format!("{text}{pad} 200x56 cells "), "{codec:?} sound {sound}");
            assert!(row(p.grid(), rows - 2).contains("m sound"), "the hint names the key");
            if codec == Codec::Ascii {
                let ui = p.ui_rows();
                for r in (0..rows).filter(|&r| !ui.contains(r)) {
                    assert!(p.grid().row(r).iter().all(|c| allowed.contains(&c.glyph())), "row {r} stays picture");
                }
            }
        }
    }
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
            return Err(format!("escape at {i} is not a CSI: {:?}", bytes.get(i + 1).map(|&b| b as char)));
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

struct Printed {
    row: u16,
    col: u16,
    ch: char,
    fg: Option<Rgb>,
    bg: Option<Rgb>,
}

fn sgr_color(ps: &[&str]) -> Option<Rgb> {
    let n = |k: usize| ps.get(k).and_then(|v| v.parse::<u8>().ok());
    match ps.get(1) {
        Some(&"2") => Some(Rgb::new(n(2)?, n(3)?, n(4)?)),
        Some(&"5") => Some(quant::ansi256_to_rgb(n(2)?)),
        _ => None,
    }
}

struct Screen {
    row: u16,
    col: u16,
    fg: Option<Rgb>,
    bg: Option<Rgb>,
}

impl Screen {
    fn new() -> Screen {
        Screen { row: 0, col: 0, fg: None, bg: None }
    }

    fn print(&mut self, bytes: &[u8]) -> Result<Vec<Printed>, String> {
        let mut out = Vec::new();
        let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
        let mut chars = text.char_indices().peekable();
        while let Some((i, ch)) = chars.next() {
            if ch != '\x1b' {
                if ch.is_control() {
                    return Err(format!("control {ch:?} at {i} outside an escape sequence"));
                }
                out.push(Printed { row: self.row, col: self.col, ch, fg: self.fg, bg: self.bg });
                self.col += 1;
                continue;
            }
            if chars.next().map(|(_, c)| c) != Some('[') {
                return Err(format!("escape at {i} is not a CSI"));
            }
            let mut params = String::new();
            let fin = loop {
                match chars.next() {
                    Some((_, c @ '\x40'..='\x7e')) => break c,
                    Some((_, c)) => params.push(c),
                    None => return Err(format!("unterminated CSI at {i}")),
                }
            };
            let ps: Vec<&str> = params.split(';').collect();
            match fin {
                'H' => {
                    let n = |k: usize| ps.get(k).and_then(|v| v.parse::<u16>().ok()).unwrap_or(1).max(1) - 1;
                    (self.row, self.col) = (n(0), n(1));
                }
                'm' if !params.starts_with('?') => {
                    let mut k = 0;
                    while k < ps.len() {
                        match ps[k].parse::<u32>().unwrap_or(0) {
                            v @ (38 | 48) => {
                                let n = if ps.get(k + 1) == Some(&"2") { 5 } else { 3 };
                                let c = sgr_color(&ps[k..(k + n).min(ps.len())]);
                                if v == 38 { self.fg = c } else { self.bg = c }
                                k += n;
                                continue;
                            }
                            0 => (self.fg, self.bg) = (None, None),
                            39 => self.fg = None,
                            49 => self.bg = None,
                            v @ (30..=37 | 90..=97) => self.fg = Some(quant::ansi16_to_rgb(ansi16(v, 30, 90))),
                            v @ (40..=47 | 100..=107) => self.bg = Some(quant::ansi16_to_rgb(ansi16(v, 40, 100))),
                            _ => {}
                        }
                        k += 1;
                    }
                }
                _ => {}
            }
        }
        Ok(out)
    }
}

fn ansi16(v: u32, dim: u32, bright: u32) -> u8 {
    if v >= bright { (v - bright + 8) as u8 } else { (v - dim) as u8 }
}

const SIZES: [(u16, u16); 14] = [
    (80, 24), (1, 1), (8, 3), (5, 2), (400, 120), (213, 58), (31, 8), (240, 36), (120, 40), (100, 60),
    (200, 56), (239, 36), (320, 90), (1000, 300),
];

struct Frame {
    bytes: Vec<u8>,
    ui: UiRows,
}

fn joined(frames: &[Frame]) -> Vec<u8> {
    frames.iter().flat_map(|f| f.bytes.iter().copied()).collect()
}

fn deck_stream(codec: Codec, tier: GlyphTier, color: ColorTier, overlays: bool) -> Vec<Frame> {
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
        deck.set_info_overlay(Some(" Caf\u{e9} clip   codec: ascii   settings: saved   sound: on "));
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
            out.push(Frame { bytes: backend.take_output(), ui: deck.ui_rows() });
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn ascii_picture_cells_are_printable_ascii_over_a_capped_shade() {
    for tier in [GlyphTier::Ascii, GlyphTier::UnicodeBlocks, GlyphTier::BrailleVerified] {
        for color in [ColorTier::True, ColorTier::C256, ColorTier::C16, ColorTier::Mono] {
            for overlays in [false, true] {
                let what = format!("{tier:?} {color:?} overlays {overlays}");
                let frames = deck_stream(Codec::Ascii, tier, color, overlays);
                let mut screen = Screen::new();
                let (mut shaded, mut ui_cells, mut ui_blocks, mut ui_backed) = (0, 0, 0, 0);
                for (n, frame) in frames.iter().enumerate() {
                    assert_eq!(frame.ui.is_empty(), !overlays, "{what} frame {n}");
                    let cells = screen.print(&frame.bytes).unwrap_or_else(|e| panic!("{what} frame {n}: {e}"));
                    for Printed { row, col, ch, fg, bg } in cells {
                        let at = format!("{what} frame {n} ({col},{row}) {ch:?}");
                        if frame.ui.contains(row) {
                            ui_cells += 1;
                            ui_blocks += usize::from(!ch.is_ascii());
                            ui_backed += usize::from(bg.is_some());
                            continue;
                        }
                        assert!(ch == ' ' || ch.is_ascii_graphic(), "{at}: a picture cell outside printable ASCII");
                        let Some(back) = bg else { continue };
                        assert!(
                            matches!(color, ColorTier::True | ColorTier::C256),
                            "{at}: a background on a picture cell at {color:?}: {back:?}"
                        );
                        shaded += 1;
                        let fg = fg.unwrap_or_else(|| panic!("{at}: a shade needs its own color"));
                        assert!(backing_within_cap(ch, fg, back), "{at}: {fg:?} on {back:?}");
                        assert!(back.r.max(back.g).max(back.b) >= SHADE_FLOOR, "{at}: {back:?} reads as black");
                        if color == ColorTier::C256 {
                            assert!(SHADES_256.contains(&back), "{at}: {back:?} is not a 256-color shade");
                            assert!(in_hue_family(fg, back), "{at}: {back:?} leaves {fg:?}'s hue family");
                        }
                    }
                }
                if matches!(color, ColorTier::True | ColorTier::C256) {
                    assert!(shaded > 0, "{what}: the picture is shaded");
                }
                let text = String::from_utf8_lossy(&joined(&frames)).into_owned();
                assert!(text.contains('@') && text.contains('/'), "{what}: a real picture");
                assert!(text.contains("AUTO-ASCII"), "{what}: the enlarge card was drawn");
                if color != ColorTier::Mono {
                    assert!(text.contains("49m"), "{what}: pads, gaps and unshaded cells keep the default background");
                }
                if !overlays {
                    assert_eq!(ui_cells, 0, "{what}: no overlay, no UI");
                    continue;
                }
                assert!(ui_cells > 0, "{what}: the overlay is UI");
                assert_eq!(ui_blocks > 0, tier != GlyphTier::Ascii, "{what}: big block text only on block tiers");
                assert_eq!(ui_backed > 0, color != ColorTier::Mono, "{what}: the overlay keeps its own background");
                for want in ["v controls", "shadow lift", "PAUSED", "Caf? clip", "sound: on", "213x58 cells", "zoom out"] {
                    assert!(text.contains(want), "{what}: overlay {want:?} drawn");
                }
            }
        }
    }
}

fn hud(asset: &[u8], codec: Codec, tier: GlyphTier, (cols, rows): (u16, u16), on: &[&str]) -> (Grid<Cell>, UiRows) {
    let mut p = player(asset, tier);
    p.set_codec(codec);
    p.reflow_grid(cols, rows);
    p.set_progress_overlay(on.contains(&"progress"));
    p.set_hint_overlay(on.contains(&"hints") || on.contains(&"info"));
    p.set_info_overlay(on.contains(&"info").then_some(" The Architect   codec: ascii   settings: default   sound: off "));
    p.set_dial_overlay(on.contains(&"dial").then_some(("edge on", 32, 255)));
    p.set_paused(true);
    p.render_grid(3).unwrap();
    (p.grid().clone(), p.ui_rows())
}

#[test]
fn ascii_draws_the_same_hud_as_pixels_and_letters() {
    let asset = full_asset();
    let sets: [&[&str]; 6] =
        [&["progress"], &["hints"], &["info"], &["dial"], &["progress", "dial", "info"], &["progress", "hints", "info"]];
    let sizes = [(80, 24), (200, 56), (239, 36), (240, 35), (240, 36), (320, 90), (400, 120), (1000, 300)];
    let allowed = ascii_glyphs();
    for tier in [GlyphTier::Ascii, GlyphTier::UnicodeBlocks, GlyphTier::BrailleVerified] {
        for size in sizes {
            let big = OverlayScale::for_grid(size.0, size.1, tier) == OverlayScale::Big;
            assert_eq!(big, tier != GlyphTier::Ascii && size.0 >= 240 && size.1 >= 36);
            for on in sets {
                let what = format!("{tier:?} {}x{} {on:?}", size.0, size.1);
                let (ascii, ui) = hud(&asset, Codec::Ascii, tier, size, on);
                assert!(!ui.is_empty(), "{what}");
                for codec in [Codec::Pixels, Codec::Letters] {
                    let (other, other_ui) = hud(&asset, codec, tier, size, on);
                    assert_eq!(other_ui, ui, "{what}: {codec:?} draws the same rows");
                    for r in (0..size.1).filter(|&r| ui.contains(r)) {
                        assert_eq!(ascii.row(r), other.row(r), "{what}: row {r} differs from {codec:?}");
                    }
                }
                let hud_text: String =
                    (0..size.1).filter(|&r| ui.contains(r)).flat_map(|r| row(&ascii, r).chars().collect::<Vec<_>>()).collect();
                assert_eq!(hud_text.contains(['▀', '▄', '█']), big, "{what}: big text exactly where pixels draws it");
                for r in (0..size.1).filter(|&r| !ui.contains(r)) {
                    for c in ascii.row(r) {
                        assert!(allowed.contains(&c.glyph()) && cell_within_cap(c, ColorDepth::True), "{what}: row {r} {c:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn ascii_without_overlays_is_all_picture_on_big_grids() {
    let asset = full_asset();
    let allowed = ascii_glyphs();
    for tier in [GlyphTier::Ascii, GlyphTier::UnicodeBlocks, GlyphTier::BrailleVerified] {
        for color in [ColorDepth::True, ColorDepth::C256, ColorDepth::C16, ColorDepth::Mono] {
            for (cols, rows) in [(200, 56), (239, 36), (240, 36), (320, 90), (400, 120), (1000, 300)] {
                let mut p = Player::new(AsciiReader::open(&asset).unwrap(), 2.0, false, color, tier).unwrap();
                p.set_codec(Codec::Ascii);
                p.reflow_grid(cols, rows);
                for f in 0..FRAMES {
                    p.render_grid(f).unwrap();
                }
                assert!(p.ui_rows().is_empty());
                for c in p.grid().as_slice() {
                    assert!(allowed.contains(&c.glyph()), "{tier:?} {color:?} {cols}x{rows}: {c:?}");
                    assert!(cell_within_cap(c, color), "{tier:?} {color:?} {cols}x{rows}: {c:?}");
                }
            }
        }
    }
}

#[test]
fn other_codecs_keep_their_backgrounds_and_big_overlay_text() {
    let pixels = joined(&deck_stream(Codec::Pixels, GlyphTier::Ascii, ColorTier::True, true));
    let bg = background_sgrs(&pixels).unwrap();
    assert!(bg.contains(&"48;2;0;0;0".to_string()) && bg.contains(&"48;2;24;24;40".to_string()), "{bg:?}");
    let letters = joined(&deck_stream(Codec::Letters, GlyphTier::UnicodeBlocks, ColorTier::C16, true));
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
        assert!(grid.as_slice().iter().all(|c| allowed.contains(&c.glyph()) && cell_within_cap(c, color)));
        let text: String = grid.as_slice().iter().map(|c| c.glyph()).collect();
        assert!(text.contains(['|', '/', '\\']), "{name}: edge strokes");
        assert!(text.contains('@'), "{name}: the disc core is the densest glyph");
        let title = format!("ascii codec, {tier:?} tier, {color:?}, 80x24, frame {}", FRAMES - 1);
        check_golden(name, &golden_text(&grid, &title));
    }
}
