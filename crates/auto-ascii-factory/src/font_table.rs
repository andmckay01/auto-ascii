//! `auto-ascii-factory font-table` — offline glyph ink-coverage table generator
//! (PLAN §3.4, M5 item B).
//!
//! Rasterizes **every glyph the 8 shipped palettes can emit** (enumerated
//! from the palette data via `auto_ascii_core::palette::all_palette_glyphs` — never
//! a hardcoded list) into a 64×128 px cell (§3.4's raster size) with
//! `ab_glyph`, and emits a deterministic TOML coverage table:
//!
//! - `coverage` = Σ antialiased ink / (64·128) — the same fractional-ink
//!   integral the M2 `derive_coverage.py` reference used (mean pixel value,
//!   not a threshold), so per-font tables are directly comparable to the
//!   committed conservative constants.
//! - `lstar` = CIE L\* of that coverage taken as linear luminance (white ink
//!   on black): the §3.4 "coverage → L\*" axis ramps are picked on.
//!
//! **Cell model.** The font is scaled so its monospace advance equals the
//! 64 px cell width (how terminals actually size text: by advance, not em);
//! the glyph's ink box is centered in the cell and clipped to it. Fonts with
//! a line box taller than 2× the advance (e.g. Noto Sans Mono) overflow the
//! 1:2 cell exactly as they do in a real 1:2 terminal cell, so full blocks
//! may integrate slightly above/below 1.0× of their true cell — clipping
//! keeps coverage honest in `0..=1`.
//!
//! **Missing-glyph policy** (task contract): a codepoint the font has no
//! glyph for (`.notdef`) gets `coverage = 0`, joins the one-line `missing`
//! array (the font's repertoire gap — `--font-table` palette veto input) and
//! is WARN-listed on stderr.
//!
//! **Determinism** (acceptance: same font file → byte-identical table): the
//! output is a pure function of the font bytes + `--name`; entries are
//! sorted by codepoint, floats are emitted with fixed precision, and the
//! source path only contributes its basename + sha256. Unit-tested below;
//! `crates/auto-ascii-core/fonts/README.md` documents the regeneration commands.

use std::path::PathBuf;

use ab_glyph::{Font, FontRef, PxScale};

use crate::ffmpeg::BoxErr;
use crate::sha256::sha256_hex;

/// §3.4 raster cell: 64×128 px (1:2 — the engine's default cell aspect).
pub const CELL_W: u32 = 64;
/// See [`CELL_W`].
pub const CELL_H: u32 = 128;

pub struct FontTableArgs {
    /// Font file; `None` only with `conservative`.
    pub font: Option<PathBuf>,
    pub output: PathBuf,
    /// Table name (defaults to the font file stem).
    pub name: Option<String>,
    /// Emit the built-in conservative table (ASCII repertoire, DejaVu-derived
    /// constants from auto-ascii-eval) in the same format instead of rasterizing.
    pub conservative: bool,
}

pub fn run(args: &FontTableArgs) -> Result<(), BoxErr> {
    let (toml, warns) = match (&args.font, args.conservative) {
        (Some(_), true) => {
            return Err("font-table: pass a font file OR --conservative, not both".into());
        }
        (None, false) => {
            return Err("font-table: need a font file (or --conservative)".into());
        }
        (None, true) => {
            let name = args.name.as_deref().unwrap_or("conservative");
            generate_conservative(name)
        }
        (Some(font_path), false) => {
            let bytes = std::fs::read(font_path)
                .map_err(|e| format!("read {}: {e}", font_path.display()))?;
            let name = match &args.name {
                Some(n) => n.clone(),
                None => font_path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .ok_or("font-table: cannot derive a table name from the font path")?,
            };
            let source = font_path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            generate_from_font(&bytes, &name, &source)?
        }
    };
    for w in &warns {
        eprintln!("font-table: WARN {w}");
    }
    std::fs::write(&args.output, &toml)
        .map_err(|e| format!("write {}: {e}", args.output.display()))?;
    eprintln!(
        "font-table: wrote {} ({} glyphs, {} missing)",
        args.output.display(),
        toml.matches("\n[[glyphs]]").count(),
        warns.len()
    );
    Ok(())
}

/// CIE L\* (0..100) of a linear luminance `y` in `0..=1` — white ink covering
/// fraction `y` of a black cell reflects `y` of the light.
fn lstar(y: f64) -> f64 {
    // CIE 1976: L* = 116·f(Y/Yn) − 16 with the standard cube-root spline.
    let f = if y > 216.0 / 24389.0 { y.cbrt() } else { (24389.0 / 27.0 * y + 16.0) / 116.0 };
    116.0 * f - 16.0
}

/// TOML basic-string escape for a single glyph (the emitter's dual of
/// `auto_ascii_core::FontTable::parse`).
fn toml_ch(ch: char) -> String {
    match ch {
        '"' => "\"\\\"\"".into(),
        '\\' => "\"\\\\\"".into(),
        _ => format!("\"{ch}\""),
    }
}

/// One rasterized glyph.
struct GlyphRow {
    ch: char,
    coverage: f64,
    missing: bool,
}

/// Shared TOML emitter — one fixed field order + fixed float precision so
/// byte-identity is a property of the *data*, not the code path.
fn emit(
    name: &str,
    source: &str,
    source_sha256: &str,
    px_per_em: Option<f64>,
    rows: &[GlyphRow],
) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Glyph ink-coverage table (PLAN \u{a7}3.4) \u{2014} generated by `auto-ascii-factory \
         font-table`.\n\
         # DO NOT EDIT: regenerate with the command in crates/auto-ascii-core/fonts/README.md.\n\
         # coverage = fraction of a 64x128 px cell covered by ink (antialiased integral);\n\
         # lstar = CIE L* of that coverage as linear luminance (white ink on black).\n\
         # `missing` lists palette glyphs absent from the font (coverage 0) \u{2014} the\n\
         # repertoire gap the --font-table palette veto consumes.\n\
         schema = 1\n\
         name = \"{name}\"\n\
         source = \"{source}\"\n\
         source_sha256 = \"{source_sha256}\"\n\
         cell = [{CELL_W}, {CELL_H}]"
    );
    if let Some(p) = px_per_em {
        let _ = writeln!(out, "px_per_em = {p:.4}");
    }
    let miss: Vec<String> = rows.iter().filter(|r| r.missing).map(|r| toml_ch(r.ch)).collect();
    let _ = writeln!(out, "missing = [{}]", miss.join(", "));
    for r in rows {
        let _ = writeln!(
            out,
            "\n[[glyphs]]\nch = {}\ncp = 0x{:04X}\ncoverage = {:.6}\nlstar = {:.3}",
            toml_ch(r.ch),
            r.ch as u32,
            r.coverage,
            lstar(r.coverage)
        );
    }
    out
}

/// Rasterize every palette glyph through `font` (pure: font bytes + names in,
/// TOML string + warnings out — the determinism test calls this twice).
pub fn generate_from_font(
    bytes: &[u8],
    name: &str,
    source: &str,
) -> Result<(String, Vec<String>), BoxErr> {
    let font = FontRef::try_from_slice(bytes).map_err(|e| format!("parse font: {e}"))?;
    let sha = sha256_hex(bytes);

    // Terminals size monospace text by ADVANCE: scale so one advance == the
    // cell width. Reference advance from ' ' (every monospace glyph shares
    // it; verified against '@' below).
    let units_per_em =
        f64::from(font.units_per_em().ok_or("font has no units_per_em (not a scalable font)")?);
    let space = font.glyph_id(' ');
    if space.0 == 0 {
        return Err("font has no space glyph — not usable as a terminal font".into());
    }
    let adv_units = f64::from(font.h_advance_unscaled(space));
    if adv_units <= 0.0 {
        return Err("font reports a non-positive space advance".into());
    }
    let mut warns: Vec<String> = Vec::new();
    let at_units = f64::from(font.h_advance_unscaled(font.glyph_id('@')));
    if (at_units - adv_units).abs() > 0.5 {
        warns.push(format!(
            "font does not look monospace: advance(' ') = {adv_units} font units \
             but advance('@') = {at_units}"
        ));
    }
    // px per font unit so that advance == CELL_W; ab_glyph's PxScale is the
    // scaled line box (ascent − descent), so convert through height_unscaled.
    let px_per_unit = f64::from(CELL_W) / adv_units;
    let scale = PxScale::from((px_per_unit * f64::from(font.height_unscaled())) as f32);
    let px_per_em = px_per_unit * units_per_em;

    let mut rows: Vec<GlyphRow> = Vec::new();
    for ch in auto_ascii_core::palette::all_palette_glyphs() {
        let id = font.glyph_id(ch);
        if id.0 == 0 {
            // .notdef → repertoire gap: coverage 0 + missing + WARN (policy).
            warns.push(format!("U+{:04X} {ch:?} has no glyph in {source} (coverage 0)", ch as u32));
            rows.push(GlyphRow { ch, coverage: 0.0, missing: true });
            continue;
        }
        let glyph = id.with_scale(scale);
        let coverage = match font.outline_glyph(glyph) {
            None => 0.0, // present but blank (space)
            Some(og) => {
                let b = og.px_bounds();
                let (w, h) = (b.width().ceil() as i64, b.height().ceil() as i64);
                // Center the ink box in the cell; clip to the cell (see the
                // module docs — matches a real 1:2 terminal cell).
                let ox = (i64::from(CELL_W) - w) / 2;
                let oy = (i64::from(CELL_H) - h) / 2;
                let mut sum = 0.0f64;
                og.draw(|x, y, c| {
                    let (cx, cy) = (i64::from(x) + ox, i64::from(y) + oy);
                    if (0..i64::from(CELL_W)).contains(&cx) && (0..i64::from(CELL_H)).contains(&cy)
                    {
                        sum += f64::from(c.clamp(0.0, 1.0));
                    }
                });
                (sum / (f64::from(CELL_W) * f64::from(CELL_H))).min(1.0)
            }
        };
        rows.push(GlyphRow { ch, coverage, missing: false });
    }
    Ok((emit(name, source, &sha, Some(px_per_em), &rows), warns))
}

/// The conservative default in table form: the committed auto-ascii-eval DejaVu
/// constants for the ASCII repertoire; every non-ASCII palette glyph is a
/// deliberate repertoire gap (conservative = trust only ASCII — the same
/// posture as the probe's conservative caps default).
pub fn generate_conservative(name: &str) -> (String, Vec<String>) {
    let table = auto_ascii_eval::CoverageTable::conservative();
    let rows: Vec<GlyphRow> = auto_ascii_core::palette::all_palette_glyphs()
        .into_iter()
        .map(|ch| match table.coverage(ch) {
            Some(c) => GlyphRow { ch, coverage: f64::from(c), missing: false },
            None => GlyphRow { ch, coverage: 0.0, missing: true },
        })
        .collect();
    (emit(name, "builtin:CONSERVATIVE_COVERAGE", "-", None, &rows), Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEJAVU: &str = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf";

    fn dejavu_bytes() -> Option<Vec<u8>> {
        // System-font dependent: skip (not fail) where the font package is
        // absent — same posture as the corpus-gated eval stages. CI-critical
        // determinism is also covered by the conservative (font-free) half.
        std::fs::read(DEJAVU).ok()
    }

    /// Acceptance (M5 item B): same font file → byte-identical table.
    #[test]
    fn generator_is_deterministic() {
        let (a, wa) = generate_conservative("conservative");
        let (b, wb) = generate_conservative("conservative");
        assert_eq!(a, b);
        assert_eq!(wa, wb);
        let Some(bytes) = dejavu_bytes() else {
            eprintln!("skip: {DEJAVU} not installed");
            return;
        };
        let (a, _) = generate_from_font(&bytes, "dejavu-sans-mono", "DejaVuSansMono.ttf").unwrap();
        let (b, _) = generate_from_font(&bytes, "dejavu-sans-mono", "DejaVuSansMono.ttf").unwrap();
        assert_eq!(a, b, "same font bytes must produce a byte-identical table");
    }

    /// The emitted table round-trips through the auto-ascii-core parser and covers
    /// the full palette enumeration.
    #[test]
    fn table_roundtrips_through_core_parser() {
        let (toml, _) = generate_conservative("conservative");
        let t = auto_ascii_core::FontTable::parse(&toml).unwrap();
        assert_eq!(t.name(), "conservative");
        for ch in auto_ascii_core::palette::all_palette_glyphs() {
            assert!(t.coverage(ch).is_some(), "{ch:?} missing from emitted table");
            assert_eq!(t.has_glyph(ch), ch.is_ascii(), "conservative repertoire is ASCII");
        }
        let Some(bytes) = dejavu_bytes() else {
            eprintln!("skip: {DEJAVU} not installed");
            return;
        };
        let (toml, warns) =
            generate_from_font(&bytes, "dejavu-sans-mono", "DejaVuSansMono.ttf").unwrap();
        let t = auto_ascii_core::FontTable::parse(&toml).unwrap();
        // DejaVu Sans Mono covers the whole block/box-drawing surface but —
        // verified against fontconfig (`fc-list :charset=2809`) — has NO
        // braille block (that lives in DejaVu Sans/Serif): the poster child
        // for §3.4's "braille verified-support only" gate.
        for ch in auto_ascii_core::palette::all_palette_glyphs() {
            let braille = ('\u{2800}'..='\u{28FF}').contains(&ch);
            assert_eq!(t.has_glyph(ch), !braille, "{ch:?} repertoire surprise");
        }
        assert_eq!(warns.len(), t.missing().len());
        use auto_ascii_core::GlyphTier;
        assert_eq!(t.veto_tier(GlyphTier::BrailleVerified), GlyphTier::UnicodeBlocks);
        assert_eq!(t.veto_tier(GlyphTier::UnicodeBlocks), GlyphTier::UnicodeBlocks);
        // Sanity vs the committed conservative constants (same font, same
        // 64×128 fractional-ink model, different rasterizer): '@' within a
        // few percent, ramps ordered, space blank, full block near-solid.
        let c = |ch| t.coverage(ch).unwrap();
        assert_eq!(c(' '), 0.0);
        assert!((c('@') - 0.2627).abs() < 0.02, "@ = {}", c('@'));
        assert!(c('.') < c(':') && c(':') < c('#') && c('#') < c('@'));
        assert!(c('█') > 0.9, "full block = {}", c('█'));
        assert!(c('▓') > c('▒') && c('▒') > c('░'), "shade ramp ordered");
    }

    /// Missing-glyph policy: coverage 0 + `missing` + WARN. Liberation Mono
    /// has no braille block — exactly the veto case §3.4 exists for.
    #[test]
    fn missing_glyphs_warn_and_record() {
        let path = "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf";
        let Ok(bytes) = std::fs::read(path) else {
            eprintln!("skip: {path} not installed");
            return;
        };
        let (toml, warns) =
            generate_from_font(&bytes, "liberation-mono", "LiberationMono-Regular.ttf").unwrap();
        let t = auto_ascii_core::FontTable::parse(&toml).unwrap();
        let braille: Vec<char> =
            auto_ascii_core::palette::all_palette_glyphs()
                .into_iter()
                .filter(|c| ('\u{2800}'..='\u{28FF}').contains(c))
                .collect();
        assert!(!braille.is_empty());
        for ch in braille {
            assert!(!t.has_glyph(ch), "Liberation Mono has no braille ({ch:?})");
            assert_eq!(t.coverage(ch), Some(0.0));
            assert!(warns.iter().any(|w| w.contains(&format!("U+{:04X}", ch as u32))));
        }
    }
}
