//! Per-tier escape-stream byte goldens (PLAN §6 "per-palette golden
//! escape-byte streams", M2 item C): one small synthetic fixture frame
//! rendered to a 48×12 grid and presented through `SimBackend` on each color
//! tier (truecolor / 256 / 16 / mono), asserted byte-exact against committed
//! files under `tests/goldens/`.
//!
//! What these pin, end to end: the painter's quantize→diff→span→SGR-elide
//! assembly, the per-tier SGR forms (`38;2` / `38;5` / `30–37/90–97` / none),
//! the `?2026h…l` wrap, CUP emission, and — via the fixture render — the
//! resample/NORM/compose byte behavior feeding it. Reproducible without the
//! corpus (repo rule): the fixture is pure integer math through `SlpyWriter`
//! (dev-dep on slpy-eval; a legal dev-dependency cycle).
//!
//! Re-bless deliberately: `SLPY_UPDATE_GOLDENS=1 cargo test -p slpy-term
//! --test tier_goldens`, then review the diff.

use std::path::PathBuf;

use slpy_core::{Cell, Grid};
use slpy_eval::fixtures::{Fixture, FixtureRenderer, GoldenPalette, build_fixture};
use slpy_term::{Backend, Caps, ColorTier, SimBackend};

/// Small but non-trivial: 48×12 → 43×12 viewport with L2/R3 letterbox pads,
/// coarse ramp (< 70 cols), chroma fg exercising the full color range.
const COLS: u16 = 48;
const ROWS: u16 = 12;
/// Mid-motion gradient frame (same fixture family as the cell-grid goldens).
const FRAME: u32 = 10;

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/goldens").join(name)
}

/// Render the fixture frame once — the identical grid feeds all four tiers.
fn fixture_grid() -> Grid<Cell> {
    let asset = build_fixture(Fixture::GradientMotion);
    let mut renderer = FixtureRenderer::new(&asset, GoldenPalette::Ascii);
    renderer.reflow(COLS, ROWS);
    renderer.render(FRAME).clone()
}

/// First present after construction = full repaint; sync_2026 on so the wrap
/// is part of every golden.
fn present_on_tier(grid: &Grid<Cell>, tier: ColorTier) -> Vec<u8> {
    let mut sim = SimBackend::new(COLS, ROWS);
    sim.set_caps(Caps { color: tier, sync_2026: true, ..Caps::default() });
    let stats = sim.present(grid);
    assert_eq!(
        stats.cells_damaged,
        u32::from(COLS) * u32::from(ROWS),
        "first present must be a full repaint"
    );
    sim.take_output()
}

fn assert_golden(tier: ColorTier, file: &str) {
    let out = present_on_tier(&fixture_grid(), tier);
    let path = golden_path(file);
    if std::env::var_os("SLPY_UPDATE_GOLDENS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &out).unwrap();
        return;
    }
    let want = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("missing golden {} ({e}); bless with SLPY_UPDATE_GOLDENS=1", path.display()));
    if out != want {
        let first_diff = out
            .iter()
            .zip(&want)
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| out.len().min(want.len()));
        let ctx = |b: &[u8]| -> String {
            let lo = first_diff.saturating_sub(24);
            let hi = (first_diff + 24).min(b.len());
            b[lo..hi].escape_ascii().to_string()
        };
        panic!(
            "escape stream diverged from {} at byte {first_diff} \
             (got {} bytes, want {}):\n got …{}…\nwant …{}…\n\
             re-bless DELIBERATELY with SLPY_UPDATE_GOLDENS=1 after review",
            path.display(),
            out.len(),
            want.len(),
            ctx(&out),
            ctx(&want),
        );
    }
}

#[test]
fn golden_truecolor_stream() {
    assert_golden(ColorTier::True, "gradient_f10_48x12_truecolor.ansi");
}

#[test]
fn golden_256_stream() {
    assert_golden(ColorTier::C256, "gradient_f10_48x12_256.ansi");
}

#[test]
fn golden_16_stream() {
    assert_golden(ColorTier::C16, "gradient_f10_48x12_16.ansi");
}

#[test]
fn golden_mono_stream() {
    assert_golden(ColorTier::Mono, "gradient_f10_48x12_mono.ansi");
}

/// The four tier streams must be pairwise distinct (a regression collapsing
/// two tiers into one code path would otherwise still pass four identical
/// file compares after a careless re-bless).
#[test]
fn tier_streams_are_distinct() {
    let grid = fixture_grid();
    let streams = [
        present_on_tier(&grid, ColorTier::True),
        present_on_tier(&grid, ColorTier::C256),
        present_on_tier(&grid, ColorTier::C16),
        present_on_tier(&grid, ColorTier::Mono),
    ];
    for i in 0..streams.len() {
        for j in i + 1..streams.len() {
            assert_ne!(streams[i], streams[j], "tier streams {i} and {j} are identical");
        }
    }
    // And every one is wrapped (sync_2026 was set).
    for s in &streams {
        assert!(s.starts_with(b"\x1b[?2026h") && s.ends_with(b"\x1b[?2026l"));
    }
}
