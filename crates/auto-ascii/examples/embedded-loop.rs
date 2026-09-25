//! Render frames into a caller-owned loop without a terminal session.
//! Run with an asset path; `draw` demonstrates consuming the cell grid.

use std::time::Duration;

use auto_ascii::{Cell, Grid, PaletteChoice, RenderSession};

fn draw(grid: &Grid<Cell>) -> (usize, u8) {
    let mut ink = 0;
    let mut peak = 0;
    for row in 0..grid.rows() {
        for cell in grid.row(row) {
            if cell.glyph() != ' ' {
                ink += 1;
            }
            peak = peak.max(cell.fg.r.max(cell.fg.g).max(cell.fg.b));
        }
    }
    (ink, peak)
}

fn main() -> Result<(), auto_ascii::Error> {
    let path = std::env::args().nth(1).expect("usage: embedded-loop <asset.ascii>");

    let mut session = RenderSession::open(&path)?;
    session.set_palette(PaletteChoice::Unicode);
    session.set_cell_aspect(2.0)?;

    println!(
        "{path}: {} frames @ {:.3} fps, picture aspect {:.3}",
        session.frame_count(),
        session.fps(),
        session.aspect(),
    );

    let (mut cols, mut rows) = (120u16, 34u16);
    let tick = Duration::from_secs_f64(1.0 / session.fps());
    let ticks = 60;

    for t in 0..ticks {
        let elapsed = tick * t;
        let frame = (elapsed.as_secs_f64() * session.fps()) as u32 % session.frame_count();

        if t == ticks / 2 {
            (cols, rows) = (80, 24);
            println!("-- surface resized to {cols}x{rows} --");
        }

        let (ink, peak) = draw(session.render(frame, cols, rows)?);
        if t % 10 == 0 {
            println!("t={t:02} frame={frame:03} {cols}x{rows} ink={ink} peak_fg={peak}");
        }
    }

    let (ink, _) = draw(session.render(0, cols, rows)?);
    println!("seek back to frame 0: ink={ink}");
    Ok(())
}
