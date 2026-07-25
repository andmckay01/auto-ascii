//! Test-only pty harness for the M0 acceptance-(3) restore tests
//! (`tests/pty_restore.rs`). Enters a real `AnsiBackend` session on the pty
//! it is spawned under, presents one frame, then follows `argv[1]`:
//!
//! - `drop`  — return from main (orderly `Drop` path)
//! - `panic` — panic after entering (panic-hook path)
//! - `wait`  — sleep forever (the test delivers SIGTERM/SIGINT)
//! - `loop`  — drain events until `Quit` (the test types Ctrl-C / q)
//!
//! Not part of the shipped product; it exists so a hosed terminal is a red
//! test, not a bug report (PLAN §3.1).

use slpy_core::{Cell, Grid, Rgb};
use slpy_term::{AnsiBackend, Backend, Caps, Event, install_restore_hooks};

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "drop".to_string());

    install_restore_hooks();
    let mut backend = AnsiBackend::new(Caps::default()).expect("harness requires a tty");

    // One real frame of traffic so the session is exercised end-to-end.
    let (cols, rows) = backend.caps().cells;
    let mut grid = Grid::new(cols, rows);
    for r in 0..rows {
        for c in 0..cols {
            let v = ((u32::from(c) * 3 + u32::from(r) * 5) % 256) as u8;
            grid.set(c, r, Cell::new('#', Rgb::gray(v), Rgb::BLACK));
        }
    }
    backend.present(&grid);

    match mode.as_str() {
        "drop" => {}
        "panic" => panic!("harness-panic-marker"),
        "wait" => loop {
            std::thread::sleep(std::time::Duration::from_millis(20));
        },
        "loop" => 'run: loop {
            while let Some(ev) = backend.events().pop() {
                if matches!(ev, Event::Quit) {
                    break 'run;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        },
        other => {
            drop(backend);
            eprintln!("unknown harness mode: {other}");
            std::process::exit(2);
        }
    }
    // `backend` drops here: shutdown -> restore (exactly once).
}
