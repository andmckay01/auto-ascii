//! Test-only pty harness for the M0 acceptance-(3) restore tests
//! (`tests/pty_restore.rs`). Enters a real `AnsiBackend` session on the pty
//! it is spawned under, presents one frame, then follows `argv[1]`:
//!
//! - `drop`  — return from main (orderly `Drop` path)
//! - `panic` — panic after entering (panic-hook path)
//! - `wait`  — sleep forever (the test delivers SIGTERM/SIGINT)
//! - `loop`  — drain events until `Quit` (the test types Ctrl-C / q)
//!
//! M1 probe modes (`tests/pty_probe.rs` — no session entered, no restore):
//!
//! - `probe-silent` — run [`probe_caps`] against a pty that never replies:
//!   must time out to conservative defaults; prints one `PROBE-DONE` line
//!   with elapsed ms, resulting caps and leftover stdin byte count.
//! - `probe-reply` — same but with a 2 s deadline so the test can script
//!   replies (kitty-style) through the pty master.
//! - `probe-noquery` — `--no-query` escape hatch: passive hints only, no
//!   volley bytes may reach the terminal.
//!
//! Not part of the shipped product; it exists so a hosed terminal is a red
//! test, not a bug report (PLAN §3.1).

use std::time::{Duration, Instant};

use slpy_core::{Cell, Grid, Rgb};
use slpy_term::{AnsiBackend, Backend, Caps, Event, ProbeOptions, install_restore_hooks, probe_caps};

/// Count bytes still pending on stdin (raw, non-blocking) — the probe must
/// leave NO stray reply bytes behind (M1 acceptance 4).
fn stdin_pending_bytes() -> usize {
    let fd = libc::STDIN_FILENO;
    let mut saved = std::mem::MaybeUninit::<libc::termios>::uninit();
    let have_termios = unsafe { libc::tcgetattr(fd, saved.as_mut_ptr()) } == 0;
    let saved = if have_termios { Some(unsafe { saved.assume_init() }) } else { None };
    if let Some(saved) = &saved {
        let mut raw = *saved;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO);
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 0;
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) };
    }
    let mut total = 0usize;
    let mut buf = [0u8; 256];
    loop {
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        if unsafe { libc::poll(&mut pfd, 1, 0) } <= 0 {
            break;
        }
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            break;
        }
        total += n as usize;
    }
    if let Some(saved) = &saved {
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, saved) };
    }
    total
}

fn run_probe(timeout: Duration, no_query: bool) {
    let opts = ProbeOptions { no_cache: true, no_query, timeout, ..ProbeOptions::default() };
    let start = Instant::now();
    let caps = probe_caps(&opts);
    let ms = start.elapsed().as_millis();
    let stray = stdin_pending_bytes();
    let cell_px = match caps.cell_px {
        Some((w, h)) => format!("{w}x{h}"),
        None => "none".to_string(),
    };
    println!(
        "PROBE-DONE ms={ms} color={:?} sync={} can_query={} cellpx={cell_px} stray={stray}",
        caps.color, caps.sync_2026, caps.can_query
    );
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "drop".to_string());

    match mode.as_str() {
        // M1 acceptance 4: silent terminal → defaults, < 300 ms, no stray
        // bytes. Default deadline is 200 ms.
        "probe-silent" => return run_probe(slpy_term::DEFAULT_PROBE_TIMEOUT, false),
        // Scripted replies from the test side; generous deadline (deflaked).
        "probe-reply" => return run_probe(Duration::from_secs(2), false),
        // --no-query escape hatch: passive hints only, zero volley bytes.
        "probe-noquery" => return run_probe(slpy_term::DEFAULT_PROBE_TIMEOUT, true),
        _ => {}
    }

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
