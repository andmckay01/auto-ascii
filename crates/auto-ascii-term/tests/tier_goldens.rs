use std::path::PathBuf;

use auto_ascii_core::{Cell, Grid};
use auto_ascii_eval::fixtures::{Fixture, FixtureRenderer, GoldenPalette, build_fixture};
use auto_ascii_term::{Backend, Caps, ColorTier, SimBackend};

const COLS: u16 = 48;
const ROWS: u16 = 12;
const FRAME: u32 = 10;

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/goldens").join(name)
}

fn fixture_grid() -> Grid<Cell> {
    let asset = build_fixture(Fixture::GradientMotion);
    let mut renderer = FixtureRenderer::new(&asset, GoldenPalette::Ascii);
    renderer.reflow(COLS, ROWS);
    renderer.render(FRAME).clone()
}

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
    if std::env::var_os("ASCII_UPDATE_GOLDENS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &out).unwrap();
        return;
    }
    let want = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("missing golden {} ({e}); bless with ASCII_UPDATE_GOLDENS=1", path.display()));
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
             re-bless DELIBERATELY with ASCII_UPDATE_GOLDENS=1 after review",
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
    for s in &streams {
        assert!(s.starts_with(b"\x1b[?2026h") && s.ends_with(b"\x1b[?2026l"));
    }
}
