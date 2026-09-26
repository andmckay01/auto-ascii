use std::fs;
use std::path::PathBuf;

use auto_ascii::{Composition, Error, RenderSession};
use auto_ascii_eval::fixtures::{Fixture, build_fixture};

struct TmpFile(PathBuf);

impl TmpFile {
    fn with_fixture(tag: &str) -> TmpFile {
        let mut p = std::env::temp_dir();
        p.push(format!("auto-ascii-examples-{}-{tag}.ascii", std::process::id()));
        fs::write(&p, build_fixture(Fixture::GradientMotion)).expect("write fixture asset");
        TmpFile(p)
    }
}

impl Drop for TmpFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(feature = "terminal")]
fn play_in_the_terminal() -> Result<(), Error> {
    auto_ascii::Player::builder()
        .asset("intro.ascii")
        .looping(true)
        .build()?
        .run()?;
    Ok(())
}

#[cfg(feature = "terminal")]
#[test]
fn terminal_quickstart_typechecks_without_running() {
    let _never_called: fn() -> Result<(), Error> = play_in_the_terminal;
}

#[test]
fn render_session_quickstart_drives_its_own_loop() -> Result<(), Error> {
    let f = TmpFile::with_fixture("quickstart");
    let mut session = RenderSession::open(&f.0)?;
    let fps = session.fps();
    let grid = session.render(0, 120, 40)?;
    for row in 0..grid.rows() {
        for cell in grid.row(row) {
            let _: (char, auto_ascii::Rgb) = (cell.glyph(), cell.fg);
        }
    }
    assert_eq!((grid.cols(), grid.rows(), fps), (120, 40, 30.0));
    Ok(())
}

#[test]
fn render_session_rows_contain_content() -> Result<(), Error> {
    let f = TmpFile::with_fixture("rows");
    let mut session = RenderSession::open(&f.0)?;
    let grid = session.render(0, 120, 40)?;
    let lines: Vec<String> = (0..grid.rows())
        .map(|row| grid.row(row).iter().map(|c| c.glyph()).collect())
        .collect();
    assert_eq!(lines.len(), 40);
    assert!(lines.iter().any(|line| line.chars().any(|g| g != ' ')), "asset rendered");
    Ok(())
}

#[test]
fn composition_quickstart_locates_one_second() -> Result<(), Error> {
    let f = TmpFile::with_fixture("composition");
    let mut comp = Composition::single(&f.0);
    comp.resolve()?;
    let at_one_second = comp.locate(1.0).expect("inside the clip");
    assert_eq!(at_one_second.clip_idx, 0);
    assert_eq!(at_one_second.local_frame, 30);
    Ok(())
}
