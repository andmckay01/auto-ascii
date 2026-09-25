use auto_ascii_core::{Cell, Grid, Rgb};
use auto_ascii_term::{Backend, ColorTier, Event, Key, SimBackend};

fn grid_of(cols: u16, rows: u16, f: impl Fn(u16, u16) -> Cell) -> Grid<Cell> {
    let mut g = Grid::new(cols, rows);
    for r in 0..rows {
        for c in 0..cols {
            g.set(c, r, f(c, r));
        }
    }
    g
}

fn count(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

fn count_cups(out: &[u8]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i + 2 < out.len() {
        if out[i] == 0x1b && out[i + 1] == b'[' {
            let mut j = i + 2;
            while j < out.len() && (out[j].is_ascii_digit() || out[j] == b';') {
                j += 1;
            }
            if j < out.len() && out[j] == b'H' {
                n += 1;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    n
}

fn shaded(cols: u16, rows: u16) -> Grid<Cell> {
    grid_of(cols, rows, |c, r| {
        let v = ((u32::from(c) * 7 + u32::from(r) * 11) % 256) as u8;
        Cell::new('#', Rgb::gray(v), Rgb::BLACK)
    })
}

#[test]
fn single_cell_change_produces_tiny_frame() {
    let mut sim = SimBackend::new(80, 24);
    let base = shaded(80, 24);

    let first = sim.present(&base);
    assert_eq!(first.cells_damaged, 80 * 24, "first present is a full repaint");
    let full = sim.take_output();
    assert!(!full.is_empty());

    let mut next = base.clone();
    next.set(40, 12, Cell::new('@', Rgb::gray(250), Rgb::BLACK));
    let stats = sim.present(&next);
    let out = sim.take_output();

    assert_eq!(stats.cells_damaged, 1);
    assert_eq!(stats.bytes as usize, out.len());
    assert!(out.len() < 64, "single-cell frame must be < 64 B, got {}", out.len());
    assert_eq!(count_cups(&out), 1);
    assert!(count(&out, b"@") == 1);
}

#[test]
fn sgr_elision_one_sgr_for_uniform_run() {
    let mut sim = SimBackend::new(16, 4);
    let g = grid_of(16, 4, |_, _| Cell::new('x', Rgb::gray(200), Rgb::BLACK));
    let stats = sim.present(&g);
    let out = sim.take_output();

    assert_eq!(count(&out, b"38;2"), 1, "fg SGR must be run-length elided");
    assert_eq!(count(&out, b"48;2"), 1, "bg SGR must be run-length elided");
    assert_eq!(count(&out, b"x"), 64);
    assert_eq!(count_cups(&out), 4, "one CUP per row on a full repaint");
    assert_eq!(stats.cells_damaged, 64);
}

#[test]
fn unchanged_frame_emits_nothing_and_invalidate_emits_full_frame() {
    let mut sim = SimBackend::new(40, 10);
    let g = shaded(40, 10);

    sim.present(&g);
    let first = sim.take_output();

    let second = sim.present(&g);
    assert_eq!(second.bytes, 0);
    assert_eq!(second.cells_damaged, 0);
    assert!(sim.take_output().is_empty());

    sim.invalidate();
    let third = sim.present(&g);
    let out = sim.take_output();
    assert_eq!(third.cells_damaged, 400);
    assert_eq!(out, first, "full repaint after invalidate must reproduce the frame");
}

#[test]
fn gap_of_six_is_rewritten_not_moved() {
    let mut sim = SimBackend::new(40, 3);
    let base = grid_of(40, 3, |_, _| Cell::new('.', Rgb::gray(128), Rgb::BLACK));
    sim.present(&base);
    sim.take_output();

    let mut g = base.clone();
    g.set(0, 1, Cell::new('A', Rgb::gray(255), Rgb::BLACK));
    g.set(7, 1, Cell::new('B', Rgb::gray(255), Rgb::BLACK));
    let stats = sim.present(&g);
    let out = sim.take_output();
    assert_eq!(count_cups(&out), 1, "≤6-cell gap must be rewritten, not cursor-moved");
    assert_eq!(stats.cells_damaged, 8, "span rewrites the 6 gap cells too");
}

#[test]
fn gap_of_seven_splits_into_two_spans() {
    let mut sim = SimBackend::new(40, 3);
    let base = grid_of(40, 3, |_, _| Cell::new('.', Rgb::gray(128), Rgb::BLACK));
    sim.present(&base);
    sim.take_output();

    let mut g = base.clone();
    g.set(0, 1, Cell::new('A', Rgb::gray(255), Rgb::BLACK));
    g.set(8, 1, Cell::new('B', Rgb::gray(255), Rgb::BLACK));
    let stats = sim.present(&g);
    let out = sim.take_output();
    assert_eq!(count_cups(&out), 2, ">6-cell gap must emit a cursor move");
    assert_eq!(stats.cells_damaged, 2);
}

#[test]
fn resize_reallocs_and_forces_full_repaint() {
    let mut sim = SimBackend::new(20, 5);
    sim.present(&shaded(20, 5));
    sim.take_output();

    sim.resize(30, 8);
    assert_eq!(sim.caps().cells, (30, 8));
    let stats = sim.present(&shaded(30, 8));
    assert_eq!(stats.cells_damaged, 240, "resize implies invalidate → full repaint");
    assert!(!sim.take_output().is_empty());
}

#[test]
#[should_panic(expected = "call resize() first")]
fn present_with_stale_grid_size_panics() {
    let mut sim = SimBackend::new(20, 5);
    sim.resize(30, 8);
    let stale = shaded(20, 5);
    sim.present(&stale);
}

#[test]
fn throttle_accounts_simulated_drain_time() {
    let mut sim = SimBackend::new(80, 24);
    sim.set_throughput(Some(2_000_000));
    let stats = sim.present(&shaded(80, 24));
    assert!(stats.bytes > 0);
    let expected_ns = u64::from(stats.bytes) * 1_000_000_000 / 2_000_000;
    assert_eq!(stats.write_ns, expected_ns);

    sim.set_throughput(None);
    sim.invalidate();
    let stats = sim.present(&shaded(80, 24));
    assert!(stats.write_ns < expected_ns);
}

#[test]
fn events_roundtrip_and_caps() {
    let mut sim = SimBackend::new(213, 58);
    assert_eq!(sim.caps().cells, (213, 58));
    assert_eq!(sim.caps().color, ColorTier::True);

    sim.push_event(Event::Resize(320, 90));
    sim.push_event(Event::Key(Key::Char('p')));
    sim.push_event(Event::Quit);
    assert_eq!(sim.events().pop(), Some(Event::Resize(320, 90)));
    assert_eq!(sim.events().pop(), Some(Event::Key(Key::Char('p'))));
    assert_eq!(sim.events().pop(), Some(Event::Quit));
    assert_eq!(sim.events().pop(), None);
}
