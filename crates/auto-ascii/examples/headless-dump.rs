//! Render frames to plain text on stdout — no terminal anywhere.
//!
//! The smallest useful thing you can build on [`RenderSession`]: open an
//! asset, render N frames spread across it, print the glyphs. Handy for
//! goldens, diffing two builds, piping into a file, or looking at output on
//! a box with no TTY at all. Colors are dropped here; `cell.fg` / `cell.bg`
//! carry them if you want them.
//!
//! Run: `cargo run --example headless-dump -- asset.slpy [FRAMES] [COLSxROWS]`
//!
//! [`RenderSession`]: auto_ascii::RenderSession

use std::io::Write;

use auto_ascii::{Cell, Grid, PaletteChoice, RenderSession};

const USAGE: &str = "usage: headless-dump <asset.slpy> [FRAMES] [COLSxROWS]";

/// `"100x28"` → `(100, 28)`.
fn parse_dims(s: &str) -> (u16, u16) {
    let (c, r) = s.split_once('x').expect(USAGE);
    (c.parse().expect(USAGE), r.parse().expect(USAGE))
}

/// Write one frame as text. Returns the underlying `io::Error` instead of
/// panicking (M5 fix 7): Rust ignores SIGPIPE, so when the reader goes away
/// (`headless-dump a.slpy | head`) every write fails with `BrokenPipe` —
/// `println!` would panic on it; `main` treats it as a normal early exit.
fn dump_frame(
    out: &mut impl Write,
    grid: &Grid<Cell>,
    frame: u32,
    total: u32,
) -> std::io::Result<()> {
    writeln!(out, "--- frame {frame}/{total} at {}x{} ---", grid.cols(), grid.rows())?;
    for row in 0..grid.rows() {
        let line: String = grid.row(row).iter().map(|cell| cell.glyph()).collect();
        writeln!(out, "{}", line.trim_end())?;
    }
    Ok(())
}

fn main() -> Result<(), auto_ascii::Error> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect(USAGE);
    let frames: u32 = args.next().map_or(3, |s| s.parse().expect(USAGE));
    let (cols, rows) = args.next().map_or((100, 28), |s| parse_dims(&s));

    let mut session = RenderSession::open(&path)?;
    // ASCII survives any pipe, pager or log file; drop this line for blocks.
    session.set_palette(PaletteChoice::Ascii);

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let count = frames.clamp(1, session.frame_count());
    let stride = (session.frame_count() / count).max(1);
    for n in 0..count {
        // Frame indices only ever advance here, so hysteresis stays warm and
        // the dump is exactly what playback would show at those frames.
        let frame = n * stride;
        let total = session.frame_count();
        let grid = session.render(frame, cols, rows)?;
        match dump_frame(&mut out, grid, frame, total) {
            // Reader closed the pipe (head, a quit pager): a normal way for
            // a dump to end, not an error — exit 0 without a panic message.
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => return Ok(()),
            other => other.expect("write to stdout"),
        }
    }
    Ok(())
}
