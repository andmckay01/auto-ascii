//! M1 color tiers + DEC 2026 (PLAN §3.1, acceptance 5/6), byte-level via
//! SimBackend: 256 output is `38;5;N`-only, 16 is `30–37/90–97`-only, mono
//! has no color SGR at all, truecolor is unchanged M0 bytes; quantization
//! happens BEFORE diff (equal-after-quantize ⇒ zero damage); frames wrap in
//! `?2026h…l` exactly when caps say so.

use slpy_core::{Cell, Grid, Rgb};
use slpy_term::{Backend, Caps, ColorTier, SimBackend};

fn sim_with_tier(cols: u16, rows: u16, tier: ColorTier, sync: bool) -> SimBackend {
    let mut sim = SimBackend::new(cols, rows);
    let caps = Caps { color: tier, sync_2026: sync, ..Caps::default() };
    sim.set_caps(caps);
    sim
}

fn grid_of(cols: u16, rows: u16, f: impl Fn(u16, u16) -> Cell) -> Grid<Cell> {
    let mut g = Grid::new(cols, rows);
    for r in 0..rows {
        for c in 0..cols {
            g.set(c, r, f(c, r));
        }
    }
    g
}

/// Colorful test frame (hits cube, gray ramp and saturated corners).
fn colorful(cols: u16, rows: u16) -> Grid<Cell> {
    grid_of(cols, rows, |c, r| {
        let v = ((u32::from(c) * 37 + u32::from(r) * 91) % 256) as u8;
        Cell::new('#', Rgb::new(v, v.wrapping_mul(3), 255 - v), Rgb::new(v / 2, 0, v))
    })
}

/// Every SGR (CSI … m) parameter list in `out`, split into `;` tokens.
fn sgr_bodies(out: &[u8]) -> Vec<Vec<u32>> {
    let mut bodies = Vec::new();
    let mut i = 0;
    while i + 1 < out.len() {
        if out[i] == 0x1b && out[i + 1] == b'[' {
            let mut j = i + 2;
            while j < out.len() && (out[j].is_ascii_digit() || out[j] == b';' || out[j] == b'?') {
                j += 1;
            }
            if j < out.len() && out[j] == b'm' {
                let body = std::str::from_utf8(&out[i + 2..j]).unwrap();
                bodies.push(body.split(';').map(|t| t.parse::<u32>().unwrap()).collect());
            }
            i = j;
        } else {
            i += 1;
        }
    }
    bodies
}

fn count(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

/// M1 acceptance 5: 256-color output contains ONLY `38;5;N` / `48;5;N`
/// color SGRs — never truecolor `38;2` or 16-color codes.
#[test]
fn c256_output_is_385n_only() {
    let mut sim = sim_with_tier(24, 8, ColorTier::C256, false);
    sim.present(&colorful(24, 8));
    let out = sim.take_output();

    let bodies = sgr_bodies(&out);
    assert!(!bodies.is_empty(), "a colorful frame must emit SGRs");
    for body in &bodies {
        let mut t = body.iter();
        while let Some(&sel) = t.next() {
            assert!(sel == 38 || sel == 48, "unexpected SGR selector {sel} in {body:?}");
            assert_eq!(t.next(), Some(&5), "must be indexed-color form in {body:?}");
            let n = *t.next().expect("index after 38;5/48;5");
            assert!(n <= 255);
        }
    }
    assert_eq!(count(&out, b"38;2"), 0, "no truecolor SGR on the 256 tier");
    assert_eq!(count(&out, b"48;2"), 0);
    assert!(count(&out, b"38;5;") > 0);
}

/// 16-color output: every SGR token is one of 30–37/90–97 (fg) or
/// 40–47/100–107 (bg).
#[test]
fn c16_output_is_standard16_only() {
    let mut sim = sim_with_tier(24, 8, ColorTier::C16, false);
    sim.present(&colorful(24, 8));
    let out = sim.take_output();

    let bodies = sgr_bodies(&out);
    assert!(!bodies.is_empty());
    for body in &bodies {
        for &code in body {
            assert!(
                (30..=37).contains(&code)
                    || (90..=97).contains(&code)
                    || (40..=47).contains(&code)
                    || (100..=107).contains(&code),
                "non-16-color SGR code {code} in {body:?}"
            );
        }
    }
    assert_eq!(count(&out, b"38;"), 0);
    assert_eq!(count(&out, b"48;"), 0);
}

/// M1 acceptance 5: mono contains no color SGR at all — glyphs only.
#[test]
fn mono_output_has_no_color_sgr() {
    let mut sim = sim_with_tier(24, 8, ColorTier::Mono, false);
    // Glyph deliberately not 'm' so the SGR scan below is airtight.
    let stats = sim.present(&colorful(24, 8));
    let out = sim.take_output();

    assert_eq!(stats.cells_damaged, 24 * 8);
    assert!(sgr_bodies(&out).is_empty(), "mono must emit zero SGR sequences");
    assert_eq!(count(&out, b"\x1b[38"), 0);
    assert_eq!(count(&out, b"\x1b[48"), 0);
    assert_eq!(count(&out, b"#"), 24 * 8, "glyphs still render");
}

/// M1 acceptance 5: truecolor output is byte-identical to the M0 path —
/// same `38;2;R;G;B` stream, no wrap by default.
#[test]
fn truecolor_unchanged_from_m0() {
    let mut m0 = SimBackend::new(16, 4);
    m0.present(&colorful(16, 4));
    let m0_out = m0.take_output();

    let mut m1 = sim_with_tier(16, 4, ColorTier::True, false);
    m1.present(&colorful(16, 4));
    assert_eq!(m1.take_output(), m0_out);
    assert!(count(&m0_out, b"38;2;") > 0);
}

/// THE quantize-before-diff test (M1 acceptance 5, PLAN §3.1): two frames
/// whose cells differ in RGB but quantize equal produce ZERO damage and
/// ZERO bytes in diff mode.
#[test]
fn equal_after_quantize_is_zero_damage() {
    let mut sim = sim_with_tier(12, 4, ColorTier::C256, false);
    // Gray 100 and gray 102 both quantize to xterm index 241 (ramp 98).
    let a = grid_of(12, 4, |_, _| Cell::new('x', Rgb::gray(100), Rgb::BLACK));
    let b = grid_of(12, 4, |_, _| Cell::new('x', Rgb::gray(102), Rgb::BLACK));

    let first = sim.present(&a);
    assert_eq!(first.cells_damaged, 48);
    assert!(!sim.take_output().is_empty());

    let second = sim.present(&b);
    assert_eq!(second.cells_damaged, 0, "equal-after-quantize must be zero damage");
    assert_eq!(second.bytes, 0);
    assert!(sim.take_output().is_empty());

    // Sanity: the same two frames on the truecolor tier DO differ.
    let mut tc = SimBackend::new(12, 4);
    tc.present(&a);
    tc.take_output();
    let tc_second = tc.present(&b);
    assert_eq!(tc_second.cells_damaged, 48, "truecolor sees the raw RGB change");
}

/// Same property on the 16 tier: distinct RGBs mapping to the same ANSI
/// color are invisible to the diff.
#[test]
fn equal_after_quantize_16_tier() {
    let mut sim = sim_with_tier(8, 2, ColorTier::C16, false);
    let a = grid_of(8, 2, |_, _| Cell::new('o', Rgb::new(200, 10, 10), Rgb::BLACK));
    let b = grid_of(8, 2, |_, _| Cell::new('o', Rgb::new(210, 0, 20), Rgb::BLACK));
    sim.present(&a);
    sim.take_output();
    let second = sim.present(&b);
    assert_eq!(second.cells_damaged, 0);
    assert_eq!(second.bytes, 0);
}

/// M1 acceptance 6: with `Caps::sync_2026`, every non-empty frame is wrapped
/// in `CSI ? 2026 h … l`; empty frames emit nothing (no bare wrap).
#[test]
fn sync_2026_wraps_frames() {
    let mut sim = sim_with_tier(10, 3, ColorTier::True, true);
    let g = colorful(10, 3);

    sim.present(&g);
    let out = sim.take_output();
    assert!(out.starts_with(b"\x1b[?2026h"), "frame must open with BSU");
    assert!(out.ends_with(b"\x1b[?2026l"), "frame must close with ESU");
    assert_eq!(count(&out, b"\x1b[?2026h"), 1);
    assert_eq!(count(&out, b"\x1b[?2026l"), 1);

    // Unchanged frame: nothing at all — not even the wrap.
    let idle = sim.present(&g);
    assert_eq!(idle.bytes, 0);
    assert!(sim.take_output().is_empty());

    // Invalidate → full repaint, wrapped again.
    sim.invalidate();
    sim.present(&g);
    let full = sim.take_output();
    assert!(full.starts_with(b"\x1b[?2026h") && full.ends_with(b"\x1b[?2026l"));
}

/// Caps without sync_2026 (the default) never wrap — truecolor M0 parity.
#[test]
fn no_sync_flag_no_wrap() {
    let mut sim = SimBackend::new(10, 3);
    sim.present(&colorful(10, 3));
    let out = sim.take_output();
    assert_eq!(count(&out, b"2026"), 0);
}

/// Mono + sync: wrap applies to glyph-only output too.
#[test]
fn sync_2026_wraps_mono_frames() {
    let mut sim = sim_with_tier(6, 2, ColorTier::Mono, true);
    sim.present(&colorful(6, 2));
    let out = sim.take_output();
    assert!(out.starts_with(b"\x1b[?2026h") && out.ends_with(b"\x1b[?2026l"));
    assert!(sgr_bodies(&out).is_empty());
}

/// `--tier` parsing (M1 acceptance 4): canonical names and rejection.
#[test]
fn tier_parsing() {
    assert_eq!("truecolor".parse::<ColorTier>(), Ok(ColorTier::True));
    assert_eq!("256".parse::<ColorTier>(), Ok(ColorTier::C256));
    assert_eq!("16".parse::<ColorTier>(), Ok(ColorTier::C16));
    assert_eq!("mono".parse::<ColorTier>(), Ok(ColorTier::Mono));
    assert_eq!("TRUECOLOR".parse::<ColorTier>(), Ok(ColorTier::True));
    assert!("42".parse::<ColorTier>().is_err());
    assert!("".parse::<ColorTier>().is_err());
}
