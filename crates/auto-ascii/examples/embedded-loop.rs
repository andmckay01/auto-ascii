//! Driving auto-ascii from *your* event loop with [`RenderSession`].
//!
//! `RenderSession` is terminal-free: you own the clock, the surface size and
//! the output layer (a game engine, a GUI widget, a web canvas, a test).
//! You hand it `(frame_idx, cols, rows)`; it hands back a composed
//! `Grid<Cell>` of glyphs + RGB colors. This example is a fake 60-tick app
//! loop that
//!
//! 1. picks the frame matching a virtual wall clock (skips are fine —
//!    monotonic advance keeps full hysteresis quality),
//! 2. "draws" each grid (here: counts ink, so there is something to print),
//! 3. resizes its surface halfway through — no reopen, no reallocation
//!    dance, just pass the new size,
//! 4. seeks backwards at the end, which resets temporal state automatically
//!    so the landing frame has no ghosts from before the seek.
//!
//! Run: `cargo run --example embedded-loop -- asset.ascii`
//!
//! [`RenderSession`]: auto_ascii::RenderSession

use std::time::Duration;

use auto_ascii::{Cell, Grid, PaletteChoice, RenderSession};

/// Stand-in for "blit this grid onto my surface": returns (non-blank cells,
/// brightest foreground channel) so the loop has something to report.
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
    session.set_cell_aspect(2.0)?; // our cells are twice as tall as wide

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
        // Your clock, not ours. A real app would use `Instant::now() - t0`
        // and sleep; we advance a virtual clock so the example stays fast.
        let elapsed = tick * t;
        let frame = (elapsed.as_secs_f64() * session.fps()) as u32 % session.frame_count();

        // Surface resize mid-run: state is reallocated and reset for the new
        // grid, and the very next frame is already correct.
        if t == ticks / 2 {
            (cols, rows) = (80, 24);
            println!("-- surface resized to {cols}x{rows} --");
        }

        let (ink, peak) = draw(session.render(frame, cols, rows)?);
        if t % 10 == 0 {
            println!("t={t:02} frame={frame:03} {cols}x{rows} ink={ink} peak_fg={peak}");
        }
    }

    // Backward jump = seek: temporal state resets, so this is cell-for-cell
    // what a freshly opened session would render for frame 0.
    let (ink, _) = draw(session.render(0, cols, rows)?);
    println!("seek back to frame 0: ink={ink}");
    Ok(())
}
