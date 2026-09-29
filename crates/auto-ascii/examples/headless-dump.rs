//! Headless glyph-grid dump example.

use std::io::Write;

use auto_ascii::{Cell, ComposeParams, Grid, PaletteChoice, RenderSession, Style};

fn usage() -> String {
    format!(
        "usage: headless-dump <asset.ascii | composition.toml> [FRAMES] [COLSxROWS] \
         [--style {}] [--palette ascii|unicode|braille] [--from FRAME] \
         [--settings PLAYER.toml]",
        Style::names("|")
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

#[cfg(feature = "terminal")]
fn load_settings(path: &str) -> (Style, ComposeParams) {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let saved = auto_ascii::settings::VideoSettings::parse(&text)
        .unwrap_or_else(|e| panic!("{path}: {e}"));
    (saved.style, saved.compose)
}

#[cfg(not(feature = "terminal"))]
fn load_settings(path: &str) -> (Style, ComposeParams) {
    panic!("--settings {path} needs the `terminal` feature")
}

fn main() -> Result<(), auto_ascii::Error> {
    let (mut style, mut palette, mut from) = (None, PaletteChoice::Ascii, None);
    let mut compose = None;
    let mut positional = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| bad());
        match arg.as_str() {
            "--style" => style = Some(Style::from_name(&value()).unwrap_or_else(|| bad())),
            "--palette" => palette = parse_palette(&value()),
            "--from" => from = Some(value().parse::<u32>().unwrap_or_else(|_| bad())),
            "--settings" => {
                let (saved_style, saved_compose) = load_settings(&value());
                style = style.or(Some(saved_style));
                compose = Some(saved_compose);
            }
            _ if arg.starts_with("--") => bad(),
            _ => positional.push(arg),
        }
    }
    let mut positional = positional.into_iter();
    let path = positional.next().unwrap_or_else(|| bad());
    let frames: u32 = positional.next().map_or(3, |s| s.parse().unwrap_or_else(|_| bad()));
    let (cols, rows) = positional.next().map_or((100, 28), |s| parse_dims(&s));
    if positional.next().is_some() {
        bad();
    }

    let mut session = open(&path)?;
    session.set_palette(palette);
    session.set_style(style.unwrap_or_default());
    if let Some(params) = compose {
        session.set_compose_params(params);
    }

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
