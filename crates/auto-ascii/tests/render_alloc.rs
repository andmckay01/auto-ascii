use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use auto_ascii::Codec;
use auto_ascii::pipeline::{Player, color_depth};
use auto_ascii_core::GlyphTier;
use auto_ascii_eval::fixtures::{FIXTURE_FRAMES, Fixture, build_fixture};
use auto_ascii_format::AsciiReader;
use auto_ascii_term::ColorTier;

struct CountingAlloc;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

fn note_allocation() {
    let counting = COUNTING.try_with(Cell::get).unwrap_or(false);
    if counting {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
    }
}

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_allocation();
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note_allocation();
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note_allocation();
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

fn allocations_during(f: impl FnOnce()) -> u64 {
    ALLOCATIONS.with(|n| n.set(0));
    COUNTING.with(|c| c.set(true));
    f();
    COUNTING.with(|c| c.set(false));
    ALLOCATIONS.with(Cell::get)
}

#[test]
fn the_counter_sees_heap_allocations() {
    let allocations = allocations_during(|| drop(std::hint::black_box(vec![0u8; 64])));
    assert_eq!(allocations, 1);
}

#[test]
fn warmed_sequential_render_grid_does_not_allocate() {
    let asset = build_fixture(Fixture::HardCut);
    for codec in Codec::ALL {
        let reader = AsciiReader::open(&asset).unwrap();
        let mut player = Player::new(
            reader,
            2.0,
            false,
            color_depth(ColorTier::True),
            GlyphTier::UnicodeBlocks,
        )
        .unwrap();
        player.set_codec(codec);
        player.set_progress_overlay(false);
        player.reflow_grid(120, 40);
        let warm = 4;
        for frame in 0..warm {
            player.render_grid(frame).unwrap();
        }
        let allocations = allocations_during(|| {
            for frame in warm..FIXTURE_FRAMES {
                player.render_grid(frame).unwrap();
            }
        });
        assert_eq!(allocations, 0, "{codec:?}");
    }
}
