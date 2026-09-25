use std::sync::OnceLock;
use std::time::{Duration, Instant};

use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestError, TestRunner};
use auto_ascii::pipeline::Player;
use auto_ascii_core::{Resampler, Viewport};
use auto_ascii_eval::fixtures::{FIXTURE_BASE_H, FIXTURE_BASE_W, Fixture, build_fixture};
use auto_ascii_format::AsciiReader;
use auto_ascii_term::{Backend, Event, SimBackend};

#[derive(Clone, Debug)]
enum Op {
    Push(u16, u16),
    Render { frames: u8, advance: u8 },
    Jump(u32),
    Overlays(bool),
}

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
        1 => any::<bool>().prop_map(Op::Overlays),
    ]
}

fn asset() -> &'static [u8] {
    static ASSET: OnceLock<Vec<u8>> = OnceLock::new();
    ASSET.get_or_init(|| build_fixture(Fixture::GradientMotion))
}

static WORST_TAP_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn check_viewport(cols: u16, rows: u16, vp: Option<Viewport>) -> Result<(), TestCaseError> {
    let a = auto_ascii_core::DEFAULT_CELL_ASPECT;
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

fn drain_and_check(
    backend: &mut SimBackend,
    player: &mut Player<'_>,
    queued: &mut Option<(u16, u16)>,
) -> Result<bool, TestCaseError> {
    let drained = player.drain_events(backend);
    prop_assert!(!drained.quit, "no Quit was pushed");
    let Some((cols, rows)) = queued.take() else { return Ok(false) };

    check_viewport(cols, rows, player.viewport())?;

    prop_assert_eq!(backend.caps().cells, (cols, rows), "backend cells");
    prop_assert_eq!(
        (player.grid().cols(), player.grid().rows()),
        (cols, rows),
        "grid dims"
    );
    match (player.viewport(), player.resampler_dims()) {
        (Some(vp), Some((src, dst))) => {
            prop_assert_eq!(src, (FIXTURE_BASE_W, FIXTURE_BASE_H), "resampler src dims");
            prop_assert_eq!(dst, (vp.cols, 2 * vp.rows), "luma resampler dst == Vc x 2Vr");
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
        prop_assert_eq!(
            stats.cells_damaged,
            cells,
            "present after resize must repaint every cell"
        );
        prop_assert!(stats.bytes > 0, "present after resize must emit bytes");
    }
    let out = backend.take_output();
    prop_assert_eq!(out.len() as u32, stats.bytes);
    Ok(())
}

fn run_storm(ops: &[Op]) -> Result<(), TestCaseError> {
    let reader = AsciiReader::open(asset()).expect("fixture asset is valid");
    let header = reader.header();
    assert_eq!(
        (header.aspect_num, header.aspect_den),
        (16, 9),
        "fixture header aspect changed: update check_viewport's replication"
    );
    let mut player = Player::new(
        reader,
        auto_ascii_core::DEFAULT_CELL_ASPECT,
        false,
        auto_ascii_core::ColorDepth::True,
        auto_ascii_core::GlyphTier::UnicodeBlocks,
    )
    .expect("player over the fixture");
    let mut backend = SimBackend::new(80, 24);
    let nframes = player.frame_count();

    player.reflow(&mut backend, 80, 24);
    check_viewport(80, 24, player.viewport())?;
    let mut frame: u32 = 0;
    render_present(&mut backend, &mut player, frame, true)?;

    let mut queued: Option<(u16, u16)> = None;
    let mut pending_full = false;
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
            Op::Overlays(on) => {
                player.set_progress_overlay(on);
                player.set_dial_overlay(on.then_some(("edge on", 32, 255)));
                player.set_hint_overlay(on);
                player.set_info_overlay(on.then_some(" clip   codec: pixels   settings: default "));
            }
        }
    }
    let resized = drain_and_check(&mut backend, &mut player, &mut queued)?;
    frame = (frame + 1) % nframes;
    render_present(&mut backend, &mut player, frame, resized || pending_full)?;
    Ok(())
}

#[test]
fn resize_storm_fuzz() {
    let cases: u32 = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256);
    let mut runner = TestRunner::new(Config {
        cases,
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

#[test]
fn resize_storm_directed_corners() {
    let corners = [
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
    ];
    let ops: Vec<Op> = std::iter::once(Op::Overlays(true))
        .chain(corners.iter().flat_map(|&(c, r)| {
            [Op::Push(c, r), Op::Render { frames: 1, advance: 1 }]
        }))
        .collect();
    run_storm(&ops).unwrap_or_else(|e| panic!("directed corners failed: {e}"));
}
