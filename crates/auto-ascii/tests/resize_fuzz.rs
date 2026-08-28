//! Resize fuzzing (PLAN §6, M2 item D): random (cols, rows) sequences
//! 1×1..=1000×1000 — including mid-playback storms — driven through
//! `SimBackend` and the REAL `auto_ascii::pipeline::Player` (M2 review
//! fix: the §6 invariants gate the shipping renderer, not a test-harness
//! replica; the fuzz moved here from slpy-eval and now goes through
//! `Player::drain_events`/`reflow`/`render_present` — the exact code the
//! interactive loop runs). Invariant set:
//!
//! 1. no panic, no OOB (any panic fails the case);
//! 2. viewport ⊆ terminal (and `None` exactly below the 32×9 minimum);
//! 3. aspect error minimal-among-candidates;
//! 4. letterbox pads symmetric ±1, remainder right/bottom;
//! 5. prev-grid / tap tables realloc'd consistently (backend cells, player
//!    grid, resampler dst dims — luma at Vc×2Vr since M3 — viewport AND the
//!    hysteresis state all agree after every reflow; state realloc+reset on
//!    resize is the §3.5 M3 invariant);
//! 6. tap rebuild < 1 ms (min of 3 builds — scheduler spikes on this shared
//!    4-core box don't survive a min, a real regression does);
//! 7. the next present after a resize is a full valid frame (damage ==
//!    every cell, nonzero bytes).
//!
//! Resize events go through `SimBackend::push_event` and are drained by
//! `Player::drain_events` (coalesce to the latest, one reflow) — the same
//! event path and the same reflow the interactive loop uses.
//!
//! Case count: 256 by default (fast `cargo test`), env-scaled in scripts:
//! `PROPTEST_CASES=10000 cargo test -p auto-ascii --test resize_fuzz`
//! (M2 acceptance 5 runs 10k). The test prints cases + wall time + the
//! worst observed tap rebuild.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestError, TestRunner};
use auto_ascii::pipeline::Player;
use slpy_core::{Resampler, Viewport};
use slpy_eval::fixtures::{FIXTURE_BASE_H, FIXTURE_BASE_W, Fixture, build_fixture};
use slpy_format::SlpyReader;
use slpy_term::{Backend, Event, SimBackend};

/// One storm step.
#[derive(Clone, Debug)]
enum Op {
    /// Push a Resize event (may pile up — the drain coalesces, latest wins).
    Push(u16, u16),
    /// Drain events (applying any pending resize), then render+present
    /// `advance`-spaced frames; advance > 1 exercises the FIDX seek path
    /// mid-storm (latest-frame-wins skips).
    Render { frames: u8, advance: u8 },
    /// Jump playback to an arbitrary frame (seek path), then render once.
    Jump(u32),
}

/// Dimension strategy: full legal 1..=1000 range, biased toward the small
/// and mid sizes real terminals use, with the extremes always reachable.
fn dim() -> impl Strategy<Value = u16> {
    prop_oneof![
        8 => 1u16..=300,
        3 => 300u16..=1000,
        1 => Just(1u16),
        1 => Just(1000u16),
    ]
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (dim(), dim()).prop_map(|(c, r)| Op::Push(c, r)),
        4 => (1u8..=2, 1u8..=3).prop_map(|(frames, advance)| Op::Render { frames, advance }),
        1 => (0u32..72).prop_map(Op::Jump),
    ]
}

/// The gradient fixture asset, built once per process.
fn asset() -> &'static [u8] {
    static ASSET: OnceLock<Vec<u8>> = OnceLock::new();
    ASSET.get_or_init(|| build_fixture(Fixture::GradientMotion))
}

/// Worst tap-rebuild time across all cases (reported at the end).
static WORST_TAP_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// §6 invariants 2–4 on a fresh viewport result (invariant 3 recomputes
/// both PLAN §3.2 candidates from the spec formula). The 16/9 below is the
/// FIXTURE ASSET's header aspect, not a global constant: since M5 fix 2 the
/// player letterboxes to the asset's `aspect_num/den`, so the replication
/// must use the same ratio — `run_storm` asserts the fixture header is 16:9
/// (non-16:9 targeting is covered by `slpy-core` viewport tests and the
/// `render_session` letterbox tests).
fn check_viewport(cols: u16, rows: u16, vp: Option<Viewport>) -> Result<(), TestCaseError> {
    let a = slpy_core::DEFAULT_CELL_ASPECT;
    let Some(v) = vp else {
        prop_assert!(cols < 32 || rows < 9, "None for viable {cols}x{rows}");
        return Ok(());
    };
    prop_assert!(v.cols >= 1 && v.rows >= 1 && v.cols <= cols && v.rows <= rows);
    prop_assert_eq!(v.cols + v.pad_left + v.pad_right, cols);
    prop_assert_eq!(v.rows + v.pad_top + v.pad_bottom, rows);
    prop_assert!(v.pad_right == v.pad_left || v.pad_right == v.pad_left + 1);
    prop_assert!(v.pad_bottom == v.pad_top || v.pad_bottom == v.pad_top + 1);

    let r_ratio = (16.0 / 9.0) * a;
    let clamp1 = |x: f64| -> u16 { (x.round().max(1.0)) as u16 };
    let err = |c: u16, r: u16| ((f64::from(c) / (f64::from(r) * a)) * (9.0 / 16.0)).ln().abs();
    let r1 = clamp1(f64::from(cols) / r_ratio);
    let c2 = clamp1(f64::from(rows) * r_ratio);
    let mut best = f64::INFINITY;
    if r1 <= rows {
        best = best.min(err(cols, r1));
    }
    if c2 <= cols {
        best = best.min(err(c2, rows));
    }
    if best.is_finite() {
        prop_assert!(
            err(v.cols, v.rows) <= best + 1e-12,
            "aspect error not minimal for {cols}x{rows}: chose {}x{}",
            v.cols,
            v.rows
        );
    }
    Ok(())
}

/// Drain the backend's event queue through the REAL `Player::drain_events`
/// (coalesce to latest, one reflow), then run invariants 2–6. `queued` is
/// the latest resize the storm pushed since the last drain — the expected
/// coalescing outcome. Returns whether a resize was applied (arming the
/// invariant-7 check on the next present).
fn drain_and_check(
    backend: &mut SimBackend,
    player: &mut Player<'_>,
    queued: &mut Option<(u16, u16)>,
) -> Result<bool, TestCaseError> {
    let drained = player.drain_events(backend);
    prop_assert!(!drained.quit, "no Quit was pushed");
    let Some((cols, rows)) = queued.take() else { return Ok(false) };

    // Invariants 2–4 (viewport math).
    check_viewport(cols, rows, player.viewport())?;

    // Invariant 5: everything realloc'd consistently by Player::reflow.
    prop_assert_eq!(backend.caps().cells, (cols, rows), "backend cells");
    prop_assert_eq!(
        (player.grid().cols(), player.grid().rows()),
        (cols, rows),
        "grid dims"
    );
    match (player.viewport(), player.resampler_dims()) {
        (Some(vp), Some((src, dst))) => {
            prop_assert_eq!(src, (FIXTURE_BASE_W, FIXTURE_BASE_H), "resampler src dims");
            // M3 (§3.3): the luma tap tables are built at Vc × 2Vr — two
            // vertical samples per cell through the one separable path.
            prop_assert_eq!(dst, (vp.cols, 2 * vp.rows), "luma resampler dst == Vc x 2Vr");
            // M3 hysteresis invariant (§3.5): state realloc'd to the new
            // viewport on every resize (reset is guaranteed by
            // HysteresisState::resize, unit-tested in slpy-core).
            prop_assert_eq!(
                player.hysteresis_dims(),
                (vp.cols, vp.rows),
                "hysteresis state == viewport cells"
            );
        }
        (None, None) => {
            prop_assert_eq!(
                player.hysteresis_dims(),
                (0, 0),
                "below minimum: hysteresis state must be emptied"
            );
        }
        (vp, dims) => {
            return Err(TestCaseError::fail(format!(
                "viewport {vp:?} but resampler dims {dims:?}"
            )));
        }
    }

    // Invariant 6: tap table rebuild < 1 ms (PLAN §6). Min of 3 builds so a
    // scheduler preemption on this shared box can't flake the gate; a real
    // O(n²)/allocation regression slows all three.
    if let Some(vp) = player.viewport() {
        let mut best = Duration::MAX;
        for _ in 0..3 {
            let t = Instant::now();
            let r = Resampler::build(FIXTURE_BASE_W, FIXTURE_BASE_H, vp.cols, vp.rows);
            best = best.min(t.elapsed());
            std::hint::black_box(&r);
        }
        WORST_TAP_NS.fetch_max(
            best.as_nanos() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        prop_assert!(
            best < Duration::from_millis(1),
            "tap rebuild {best:?} >= 1 ms for {}x{} viewport",
            vp.cols,
            vp.rows
        );
    }
    Ok(true)
}

/// Render+present `frame` through the real pipeline; if this present
/// directly follows a resize, assert invariant 7 (full valid frame: every
/// cell damaged, bytes flow).
fn render_present(
    backend: &mut SimBackend,
    player: &mut Player<'_>,
    frame: u32,
    after_resize: bool,
) -> Result<(), TestCaseError> {
    let stats = player
        .render_present(backend, frame)
        .map_err(|e| TestCaseError::fail(format!("render_present({frame}): {e}")))?;
    let cells = u32::from(player.grid().cols()) * u32::from(player.grid().rows());
    prop_assert!(stats.cells_damaged <= cells, "damage cannot exceed the grid");
    if after_resize {
        // Invariant 7: full repaint — every cell emitted, real bytes out.
        prop_assert_eq!(
            stats.cells_damaged,
            cells,
            "present after resize must repaint every cell"
        );
        prop_assert!(stats.bytes > 0, "present after resize must emit bytes");
    }
    // Don't accumulate multi-MB escape streams across a long storm.
    let out = backend.take_output();
    prop_assert_eq!(out.len() as u32, stats.bytes);
    Ok(())
}

fn run_storm(ops: &[Op]) -> Result<(), TestCaseError> {
    let reader = SlpyReader::open(asset()).expect("fixture asset is valid");
    // check_viewport replicates the spec candidates at 16:9 — valid only
    // because THIS asset's header says 16:9 (see check_viewport docs).
    let header = reader.header();
    assert_eq!(
        (header.aspect_num, header.aspect_den),
        (16, 9),
        "fixture header aspect changed: update check_viewport's replication"
    );
    // Diff mode (repaint_full = false) so invariant 7 proves reflow's
    // invalidate, not a blanket every-frame repaint; chroma on (the C-plane
    // realloc path is part of invariant 5's surface); unicode tier so the
    // M3 half-block/quadrant compose paths run under the storm.
    let mut player = Player::new(
        reader,
        slpy_core::DEFAULT_CELL_ASPECT,
        false,
        slpy_core::ColorDepth::True,
        slpy_core::GlyphTier::UnicodeBlocks,
    )
    .expect("player over the fixture");
    let mut backend = SimBackend::new(80, 24);
    let nframes = player.frame_count();

    // Initial reflow + first frame (playback in progress before the storm).
    player.reflow(&mut backend, 80, 24);
    check_viewport(80, 24, player.viewport())?;
    let mut frame: u32 = 0;
    render_present(&mut backend, &mut player, frame, true)?;

    let mut queued: Option<(u16, u16)> = None; // latest pushed, not yet drained
    let mut pending_full = false; // a resize was applied, next present must be full
    for op in ops {
        match *op {
            Op::Push(c, r) => {
                backend.push_event(Event::Resize(c, r));
                queued = Some((c, r));
            }
            Op::Render { frames, advance } => {
                for _ in 0..frames {
                    let resized = drain_and_check(&mut backend, &mut player, &mut queued)?;
                    pending_full |= resized;
                    frame = (frame + u32::from(advance)) % nframes;
                    render_present(&mut backend, &mut player, frame, pending_full)?;
                    pending_full = false;
                }
            }
            Op::Jump(f) => {
                let resized = drain_and_check(&mut backend, &mut player, &mut queued)?;
                pending_full |= resized;
                frame = f % nframes;
                render_present(&mut backend, &mut player, frame, pending_full)?;
                pending_full = false;
            }
        }
    }
    // Storm tail: whatever is still queued must recover within one frame.
    let resized = drain_and_check(&mut backend, &mut player, &mut queued)?;
    frame = (frame + 1) % nframes;
    render_present(&mut backend, &mut player, frame, resized || pending_full)?;
    Ok(())
}

/// 256 cases under plain `cargo test`; scripts scale with PROPTEST_CASES
/// (M2 acceptance 5: 10_000). Uses `TestRunner` directly so the run can
/// report cases, wall time and the worst tap rebuild it saw.
#[test]
fn resize_storm_fuzz() {
    let cases: u32 = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256);
    let mut runner = TestRunner::new(Config {
        cases,
        // Deterministic-by-default CI runs still explore: proptest reseeds
        // per run; failures persist to proptest-regressions/ for replay.
        ..Config::default()
    });
    let strategy = proptest::collection::vec(op(), 1..12);

    let t0 = Instant::now();
    let result = runner.run(&strategy, |ops| run_storm(&ops));
    let wall = t0.elapsed();
    let worst_tap = WORST_TAP_NS.load(std::sync::atomic::Ordering::Relaxed);
    eprintln!(
        "resize_storm_fuzz: {cases} cases in {:.1}s ({:.2} ms/case), worst tap rebuild {:.3} ms",
        wall.as_secs_f64(),
        wall.as_secs_f64() * 1000.0 / f64::from(cases.max(1)),
        worst_tap as f64 / 1e6,
    );
    match result {
        Ok(()) => {}
        Err(TestError::Fail(reason, ops)) => {
            panic!("resize storm invariant failed: {reason}\nminimal ops: {ops:#?}")
        }
        Err(TestError::Abort(reason)) => panic!("fuzz aborted: {reason}"),
    }
}

/// Directed non-random regression: the PLAN worked examples plus the nastiest
/// corners (1×1, 1000×1, 1×1000, 1000×1000, min-viable 32×9, one-off 31×9 /
/// 32×8) as one deterministic storm — always runs, even at PROPTEST_CASES=1.
#[test]
fn resize_storm_directed_corners() {
    let ops: Vec<Op> = [
        (80u16, 24u16),
        (1, 1),
        (320, 90),
        (1000, 1),
        (213, 58),
        (1, 1000),
        (32, 9),
        (31, 9),
        (32, 8),
        (1000, 1000),
        (206, 58),
        (33, 10),
        (80, 23),
    ]
    .iter()
    .flat_map(|&(c, r)| {
        [Op::Push(c, r), Op::Render { frames: 1, advance: 1 }]
    })
    .collect();
    run_storm(&ops).unwrap_or_else(|e| panic!("directed corners failed: {e}"));
}
