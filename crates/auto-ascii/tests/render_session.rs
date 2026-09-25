use std::fs;
use std::path::PathBuf;

use auto_ascii_eval::fixtures::{FIXTURE_FRAMES, Fixture, build_fixture};
use auto_ascii::{Cell, Error, Grid, PaletteChoice, RenderSession};

struct TmpFile(PathBuf);

impl TmpFile {
    fn with_fixture(fixture: Fixture, tag: &str) -> TmpFile {
        let mut p = std::env::temp_dir();
        p.push(format!("auto-ascii-session-{}-{tag}.ascii", std::process::id()));
        fs::write(&p, build_fixture(fixture)).expect("write fixture asset");
        TmpFile(p)
    }
}

impl Drop for TmpFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn glyphs(grid: &Grid<Cell>) -> String {
    (0..grid.rows())
        .flat_map(|r| grid.row(r).iter().map(|c| c.glyph()))
        .collect()
}

#[test]
fn open_exposes_asset_metadata() {
    let f = TmpFile::with_fixture(Fixture::GradientMotion, "meta");
    let session = RenderSession::open(&f.0).expect("open fixture");
    assert_eq!(session.frame_count(), FIXTURE_FRAMES);
    assert!((session.fps() - 30.0).abs() < 1e-9, "fixtures are 30 fps");
    assert!((session.aspect() - 16.0 / 9.0).abs() < 1e-9, "16:9 asset");
}

#[test]
fn render_fills_requested_grid_deterministically() {
    let f = TmpFile::with_fixture(Fixture::GradientMotion, "grid");
    let mut session = RenderSession::open(&f.0).unwrap();
    let first = glyphs(session.render(0, 80, 24).unwrap());
    let grid = session.render(0, 80, 24).unwrap();
    assert_eq!((grid.cols(), grid.rows()), (80, 24));
    assert_eq!(glyphs(grid), first, "same frame twice = identical grid");
    assert!(first.chars().any(|c| c != ' '), "rendered content expected");
}

#[test]
fn backward_jump_resets_to_cold_render() {
    let f = TmpFile::with_fixture(Fixture::GradientMotion, "jump");
    let mut warm = RenderSession::open(&f.0).unwrap();
    for idx in [0u32, 1, 2, 5, 40] {
        warm.render(idx, 100, 30).unwrap();
    }
    let jumped = warm.render(3, 100, 30).unwrap().as_slice().to_vec();

    let mut cold = RenderSession::open(&f.0).unwrap();
    let cold_cells = cold.render(3, 100, 30).unwrap().as_slice().to_vec();
    assert_eq!(jumped, cold_cells, "post-jump frame == cold-start frame, cell for cell");
}

#[test]
fn resize_reflows_and_below_minimum_renders_card() {
    let f = TmpFile::with_fixture(Fixture::CheckerDrift, "resize");
    let mut session = RenderSession::open(&f.0).unwrap();
    assert_eq!(session.render(0, 80, 24).unwrap().cols(), 80);
    let grid = session.render(1, 213, 58).unwrap();
    assert_eq!((grid.cols(), grid.rows()), (213, 58), "grid tracks the call");
    let tiny = session.render(2, 10, 3).unwrap();
    assert_eq!((tiny.cols(), tiny.rows()), (10, 3));
    assert_eq!(session.render(3, 80, 24).unwrap().cols(), 80);
}

#[test]
fn palette_and_cell_aspect_knobs() {
    let f = TmpFile::with_fixture(Fixture::GradientMotion, "knobs");
    let mut session = RenderSession::open(&f.0).unwrap();
    session.set_palette(PaletteChoice::Ascii);
    let ascii = glyphs(session.render(0, 80, 24).unwrap());
    assert!(ascii.is_ascii(), "ascii tier: {ascii:?}");
    session.set_cell_aspect(1.0).unwrap();
    let square = session.render(0, 80, 24).unwrap();
    for col in 0..18u16 {
        for row in 0..square.rows() {
            assert_eq!(square.get(col, row), Cell::BLANK, "pad at ({col},{row})");
        }
    }
    assert!(session.set_cell_aspect(0.0).is_err());
    assert!(session.set_cell_aspect(f64::NAN).is_err());
}

fn aspect_asset(aspect_num: u16, aspect_den: u16, base_w: u16, base_h: u16) -> Vec<u8> {
    use auto_ascii_format::header::plane_id;
    use auto_ascii_format::{Meta, PlaneRef, AsciiWriter, WriterOptions};
    let opts = WriterOptions {
        base_w,
        base_h,
        aspect_num,
        aspect_den,
        plane_ids: vec![plane_id::Y],
        keyframe_ivl: 4,
        ..WriterOptions::default()
    };
    let meta = Meta {
        factory_version: "render-session-aspect-test".to_owned(),
        source: format!("synthetic {aspect_num}:{aspect_den}"),
        palette_hints: Vec::new(),
    };
    let mut writer =
        AsciiWriter::new(std::io::Cursor::new(Vec::new()), opts, &meta).expect("writer options");
    let luma = vec![220u8; usize::from(base_w) * usize::from(base_h)];
    for _ in 0..4 {
        writer.write_frame(&[PlaneRef { id: plane_id::Y, data: &luma }]).expect("frame");
    }
    writer.finish().expect("finish").into_inner()
}

impl TmpFile {
    fn with_bytes(bytes: &[u8], tag: &str) -> TmpFile {
        let mut p = std::env::temp_dir();
        p.push(format!("auto-ascii-session-{}-{tag}.ascii", std::process::id()));
        fs::write(&p, bytes).expect("write synthetic asset");
        TmpFile(p)
    }
}

fn assert_letterbox(grid: &Grid<Cell>, l: u16, r: u16, t: u16, b: u16) {
    for row in 0..grid.rows() {
        for (col, cell) in grid.row(row).iter().enumerate() {
            let col = col as u16;
            let pad = col < l
                || col >= grid.cols() - r
                || row < t
                || row >= grid.rows() - b;
            if pad {
                assert_eq!(*cell, Cell::BLANK, "expected blank pad at ({col},{row})");
            } else {
                assert_ne!(cell.glyph(), ' ', "expected inked picture at ({col},{row})");
            }
        }
    }
}

#[test]
fn four_by_three_asset_letterboxes_to_its_own_aspect() {
    let f = TmpFile::with_bytes(&aspect_asset(4, 3, 160, 120), "aspect-4x3");
    let mut session = RenderSession::open(&f.0).unwrap();
    assert!((session.aspect() - 4.0 / 3.0).abs() < 1e-9, "aspect() reports 4:3");

    assert_letterbox(session.render(0, 80, 24).unwrap(), 8, 8, 0, 0);

    assert_letterbox(session.render(1, 40, 40).unwrap(), 0, 0, 12, 13);

    assert_letterbox(session.render(2, 64, 24).unwrap(), 0, 0, 0, 0);
}

#[test]
fn portrait_9_16_asset_letterboxes_to_its_own_aspect() {
    let f = TmpFile::with_bytes(&aspect_asset(9, 16, 90, 160), "aspect-9x16");
    let mut session = RenderSession::open(&f.0).unwrap();
    assert!((session.aspect() - 9.0 / 16.0).abs() < 1e-9, "aspect() reports 9:16");

    assert_letterbox(session.render(0, 80, 24).unwrap(), 26, 27, 0, 0);
}

#[test]
fn sixteen_by_nine_assets_are_unchanged() {
    let f = TmpFile::with_fixture(Fixture::GradientMotion, "aspect-16x9");
    let mut session = RenderSession::open(&f.0).unwrap();
    let grid = session.render(0, 80, 24).unwrap();
    assert!(
        grid.row(23).iter().all(|c| *c == Cell::BLANK),
        "80x24 on 16:9 still pads exactly the bottom row"
    );
    assert!(grid.row(0).iter().any(|c| c.glyph() != ' '), "top row is picture");
}

#[test]
fn open_rejects_a_valid_header_with_a_broken_chunk_table() {
    let bytes = build_fixture(Fixture::GradientMotion);
    let f = TmpFile::with_bytes(&bytes[..bytes.len() * 3 / 4], "truncated");
    let e = RenderSession::open(&f.0).unwrap_err();
    assert!(matches!(e, Error::Format { .. }), "{e}");
}

#[test]
fn error_surface_is_coherent() {
    let f = TmpFile::with_fixture(Fixture::HardCut, "errors");
    let mut session = RenderSession::open(&f.0).unwrap();
    let e = session.render(FIXTURE_FRAMES, 80, 24).unwrap_err();
    assert!(matches!(e, Error::Config(_)), "{e}");
    let e = RenderSession::open("/no/such/asset.ascii").unwrap_err();
    assert!(matches!(e, Error::Io { .. }), "{e}");
    let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
    let e = RenderSession::open(manifest).unwrap_err();
    assert!(matches!(e, Error::Format { .. }), "{e}");
}

#[test]
fn font_table_repertoire_vetoes_palette_tier() {
    let f = TmpFile::with_fixture(Fixture::GradientMotion, "font-veto");
    let mut session = RenderSession::open(&f.0).unwrap();

    let unicode = glyphs(session.render(10, 120, 40).unwrap());
    assert!(!unicode.is_ascii(), "default = unicode blocks");

    session.set_font_table(Some("ubuntu-mono")).unwrap();
    let vetoed = glyphs(session.render(10, 120, 40).unwrap());
    assert!(
        vetoed.chars().all(|c| c == ' ' || c.is_ascii_graphic()),
        "ubuntu-mono repertoire (no ‾╱╲, no half/quadrant blocks) must veto to ASCII"
    );
    assert!(vetoed.chars().any(|c| c != ' '), "still renders content");

    session.set_font_table(Some("dejavu-sans-mono")).unwrap();
    assert!(!glyphs(session.render(10, 120, 40).unwrap()).is_ascii());
    session.set_font_table(None).unwrap();
    assert!(!glyphs(session.render(10, 120, 40).unwrap()).is_ascii());

    session.set_palette(PaletteChoice::Ascii);
    session.set_font_table(Some("dejavu-sans-mono")).unwrap();
    let ascii = glyphs(session.render(10, 120, 40).unwrap());
    assert!(ascii.chars().all(|c| c == ' ' || c.is_ascii_graphic()));
}

#[test]
fn font_table_path_spec_loads_and_vetoes() {
    let f = TmpFile::with_fixture(Fixture::GradientMotion, "font-path");
    let mut session = RenderSession::open(&f.0).unwrap();

    let mut table = String::from("name = \"custom\"\nmissing = [\"╱\"]\n");
    for ch in auto_ascii_core_glyphs() {
        let esc = match ch {
            '"' => "\\\"".to_string(),
            '\\' => "\\\\".to_string(),
            c => c.to_string(),
        };
        let cov = if ch == '╱' { 0.0 } else { 0.1 };
        table.push_str(&format!("[[glyphs]]\nch = \"{esc}\"\ncoverage = {cov}\n"));
    }
    let mut p = std::env::temp_dir();
    p.push(format!("auto-ascii-font-table-{}.toml", std::process::id()));
    fs::write(&p, table).unwrap();

    session.set_font_table(Some(p.to_str().unwrap())).unwrap();
    let vetoed = glyphs(session.render(5, 120, 40).unwrap());
    assert!(vetoed.chars().all(|c| c == ' ' || c.is_ascii_graphic()), "missing ╱ vetoes unicode");
    let _ = fs::remove_file(&p);
}

#[test]
fn font_table_error_surface() {
    let f = TmpFile::with_fixture(Fixture::GradientMotion, "font-errors");
    let mut session = RenderSession::open(&f.0).unwrap();
    let before = glyphs(session.render(0, 100, 30).unwrap());

    let e = session.set_font_table(Some("comic-sans")).unwrap_err();
    assert!(matches!(e, Error::Config(_)), "{e}");
    assert!(e.to_string().contains("dejavu-sans-mono"), "error lists the built-ins: {e}");

    let mut p = std::env::temp_dir();
    p.push(format!("auto-ascii-bad-table-{}.toml", std::process::id()));
    fs::write(&p, "name = \"broken\"\n").unwrap();
    let e = session.set_font_table(Some(p.to_str().unwrap())).unwrap_err();
    assert!(matches!(e, Error::Config(_)), "{e}");
    let _ = fs::remove_file(&p);

    assert_eq!(glyphs(session.render(0, 100, 30).unwrap()), before);
}

fn auto_ascii_core_glyphs() -> Vec<char> {
    auto_ascii_core::palette::all_palette_glyphs()
}
