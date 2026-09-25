use std::time::{Duration, Instant};

use auto_ascii_core::{Cell, Grid, Rgb};
use auto_ascii_term::{
    AnsiBackend, Backend, Caps, Event, Key, ProbeOptions, install_restore_hooks, probe_caps,
};

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

fn run_probe(timeout: Duration, no_query: bool, no_quirks: bool) {
    run_probe_opts(ProbeOptions {
        no_cache: true,
        no_query,
        no_quirks,
        timeout,
        ..ProbeOptions::default()
    });
}

fn run_probe_cached(no_quirks: bool) {
    let dir = std::env::var_os("ASCII_HARNESS_CACHE_DIR")
        .map(std::path::PathBuf::from)
        .expect("probe-cached modes require ASCII_HARNESS_CACHE_DIR");
    run_probe_opts(ProbeOptions {
        no_cache: false,
        no_quirks,
        cache_dir: Some(dir),
        timeout: Duration::from_secs(2),
        ..ProbeOptions::default()
    });
}

fn run_probe_opts(opts: ProbeOptions) {
    let start = Instant::now();
    let caps = probe_caps(&opts);
    let ms = start.elapsed().as_millis();
    let stray = stdin_pending_bytes();
    let cell_px = match caps.cell_px {
        Some((w, h)) => format!("{w}x{h}"),
        None => "none".to_string(),
    };
    println!(
        "PROBE-DONE ms={ms} color={:?} sync={} can_query={} cellpx={cell_px} \
         support={:?} glyphs={} stray={stray}",
        caps.color, caps.sync_2026, caps.can_query, caps.glyph_support, caps.glyphs.0
    );
}

fn run_straggler_session() {
    let opts = ProbeOptions {
        no_cache: true,
        timeout: Duration::from_millis(150),
        ..ProbeOptions::default()
    };
    let caps = probe_caps(&opts);
    println!("PROBE-DONE color={:?}", caps.color);

    install_restore_hooks();
    let mut backend = AnsiBackend::new(caps).expect("harness requires a tty");
    println!("SESSION-READY");

    let deadline = Instant::now() + Duration::from_secs(8);
    'run: while Instant::now() < deadline {
        while let Some(ev) = backend.events().pop() {
            match ev {
                Event::Quit => {
                    println!("EV=quit");
                    break 'run;
                }
                Event::Key(Key::Char(c)) => println!("EV=char:{c}"),
                Event::Key(Key::Ctrl(c)) => println!("EV=ctrl:{c}"),
                Event::Key(Key::Esc) => println!("EV=esc"),
                Event::Key(Key::Left) => println!("EV=left"),
                Event::Key(Key::Right) => println!("EV=right"),
                Event::Resize(c, r) => println!("EV=resize:{c}x{r}"),
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    backend.shutdown();
    println!("SESSION-DONE");
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "drop".to_string());

    match mode.as_str() {
        "probe-silent" | "caps" => {
            return run_probe(auto_ascii_term::DEFAULT_PROBE_TIMEOUT, false, false);
        }
        "probe-reply" => return run_probe(Duration::from_secs(2), false, false),
        "probe-reply-noquirks" => return run_probe(Duration::from_secs(2), false, true),
        "probe-cached" => return run_probe_cached(false),
        "probe-cached-noquirks" => return run_probe_cached(true),
        "probe-noquery" => return run_probe(auto_ascii_term::DEFAULT_PROBE_TIMEOUT, true, false),
        "probe-latereply" => return run_probe(auto_ascii_term::DEFAULT_PROBE_TIMEOUT, false, false),
        "probe-straggler" => return run_straggler_session(),
        _ => {}
    }

    install_restore_hooks();
    let mut backend = AnsiBackend::new(Caps::default()).expect("harness requires a tty");

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
}
