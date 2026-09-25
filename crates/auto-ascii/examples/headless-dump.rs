//! Dump cell glyphs from an asset or composition without opening a terminal.
//! Accepts a frame count, grid size, codec, palette and starting frame.

use std::io::Write;

use auto_ascii::{Cell, Codec, Grid, PaletteChoice, RenderSession};

fn usage() -> String {
    format!(
        "usage: headless-dump <asset.ascii | composition.toml> [FRAMES] [COLSxROWS] \
         [--codec {}] [--palette ascii|unicode|braille] [--from FRAME]",
        Codec::names("|")
    )
}

fn bad() -> ! {
    panic!("{}", usage())
}

fn open(path: &str) -> Result<RenderSession, auto_ascii::Error> {
    #[cfg(feature = "compose")]
    if auto_ascii::Composition::is_toml_path(std::path::Path::new(path)) {
        let library = auto_ascii::Composition::default_library_dir();
        return RenderSession::open_composition(path, library.as_deref());
    }
    RenderSession::open(path)
}

fn parse_dims(s: &str) -> (u16, u16) {
    let (c, r) = s.split_once('x').unwrap_or_else(|| bad());
    (c.parse().unwrap_or_else(|_| bad()), r.parse().unwrap_or_else(|_| bad()))
}

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

fn parse_palette(s: &str) -> PaletteChoice {
    match s {
        "ascii" => PaletteChoice::Ascii,
        "unicode" => PaletteChoice::Unicode,
        "braille" => PaletteChoice::Braille,
        _ => bad(),
    }
}

fn main() -> Result<(), auto_ascii::Error> {
    let (mut codec, mut palette, mut from) = (Codec::default(), PaletteChoice::Ascii, None);
    let mut positional = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| bad());
        match arg.as_str() {
            "--codec" => codec = Codec::from_name(&value()).unwrap_or_else(|| bad()),
            "--palette" => palette = parse_palette(&value()),
            "--from" => from = Some(value().parse::<u32>().unwrap_or_else(|_| bad())),
            _ => positional.push(arg),
        }
    }
    let mut positional = positional.into_iter();
    let path = positional.next().unwrap_or_else(|| bad());
    let frames: u32 = positional.next().map_or(3, |s| s.parse().unwrap_or_else(|_| bad()));
    let (cols, rows) = positional.next().map_or((100, 28), |s| parse_dims(&s));

    let mut session = open(&path)?;
    session.set_palette(palette);
    session.set_codec(codec);

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let count = frames.clamp(1, session.frame_count());
    let (start, stride) = match from {
        Some(f) => (f.min(session.frame_count() - 1), 1),
        None => (0, (session.frame_count() / count).max(1)),
    };
    for n in 0..count {
        let frame = start + n * stride;
        if frame >= session.frame_count() {
            break;
        }
        let total = session.frame_count();
        let grid = session.render(frame, cols, rows)?;
        match dump_frame(&mut out, grid, frame, total) {
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => return Ok(()),
            other => other.expect("write to stdout"),
        }
    }
    Ok(())
}
