//! Per-cell temporal state and ramp-index and edge-gate hysteresis.
//! `begin_frame` steps the shared phase clock once per frame.
//! Cell-by-cell compose reads the residue that tick issued, and `compose_frame` ticks the same clock.
//! `ramp_phase` is the stationary interleaved-gradient slot both ramps release on.

use crate::grid::Grid;
use crate::orient::BIN_UNSET;

pub const IDX_UNSET: u8 = 0xFF;

pub const IDX_HYST_Q8: u32 = 90;

pub const IDX_HYST_MAX_Q8: u8 = 255;

pub const IDX_HYST_DEFAULT_Q8: u8 = 128;

pub mod cell_flags {
    pub const WAS_EDGE: u8 = 1;
    pub const WAS_QUADRANT: u8 = 1 << 1;
    pub const SHARED_MASK: u8 = WAS_EDGE | WAS_QUADRANT;
    pub const STYLE_PRIVATE_MASK: u8 = !SHARED_MASK;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CellState {
    pub idx: u8,
    pub bin: u8,
    pub flags: u8,
    pub tone_candidate: u8,
    pub tone_age: u8,
}

impl Default for CellState {
    #[inline]
    fn default() -> CellState {
        CellState { idx: IDX_UNSET, bin: BIN_UNSET, flags: 0, tone_candidate: IDX_UNSET, tone_age: 0 }
    }
}

const _: () = assert!(core::mem::size_of::<CellState>() == 5);

#[derive(Clone, Debug)]
pub struct HysteresisState {
    cells: Grid<CellState>,
    issued: u8,
    next: u8,
}

impl HysteresisState {
    pub fn new(cols: u16, rows: u16) -> HysteresisState {
        HysteresisState { cells: Grid::new(cols, rows), issued: 0, next: 0 }
    }

    #[inline]
    pub fn cols(&self) -> u16 {
        self.cells.cols()
    }

    #[inline]
    pub fn rows(&self) -> u16 {
        self.cells.rows()
    }

    pub fn reset(&mut self) {
        self.cells.fill(CellState::default());
        self.issued = 0;
        self.next = 0;
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cells.resize(cols, rows);
        self.issued = 0;
        self.next = 0;
    }

    #[inline]
    pub(crate) fn frame(&self) -> u8 {
        self.issued
    }

    #[inline]
    pub fn begin_frame(&mut self) -> u8 {
        let issued = self.next;
        self.issued = issued;
        self.next = (issued + 1) % 3;
        issued
    }

    #[inline]
    pub fn cell(&self, col: u16, row: u16) -> CellState {
        self.cells.get(col, row)
    }

    #[inline]
    pub fn cell_mut(&mut self, col: u16, row: u16) -> &mut CellState {
        let i = row as usize * self.cells.cols() as usize + col as usize;
        &mut self.cells.as_mut_slice()[i]
    }
}

#[inline]
pub fn hysteresis_idx(n: u8, len: u8, prev: u8, hyst_q8: u32) -> u8 {
    debug_assert!(len >= 1);
    debug_assert!(hyst_q8 < 256);
    let p = n as u32 * len as u32;
    if prev >= len {
        return (p >> 8) as u8;
    }
    let lo = prev as u32 * 256;
    let hi = lo + 256;
    if p >= hi + hyst_q8 {
        ((p - hyst_q8) >> 8) as u8
    } else if p + hyst_q8 < lo {
        ((p + hyst_q8) >> 8) as u8
    } else {
        prev
    }
}

#[inline]
pub fn edge_gate(e: u8, was_edge: bool, t_on: u8, t_off: u8) -> bool {
    e > t_on || (was_edge && e > t_off)
}

const RAMP_PHASE: [u8; 256] = [
    0, 8, 1, 10, 3, 12, 5, 14, 7, 0, 8, 1, 10, 3, 12, 5, 4, 13, 6, 15, 8, 1, 10, 3, 12, 4, 13, 6,
    15, 8, 1, 10, 9, 2, 11, 4, 13, 6, 15, 8, 1, 9, 2, 11, 4, 13, 6, 15, 14, 7, 0, 9, 2, 11, 4, 13,
    5, 14, 7, 0, 9, 2, 11, 4, 3, 12, 5, 14, 7, 0, 9, 2, 10, 3, 12, 5, 14, 7, 0, 9, 8, 1, 10, 3, 12,
    5, 14, 6, 15, 8, 1, 10, 3, 12, 5, 14, 13, 6, 15, 8, 1, 10, 3, 11, 4, 13, 6, 15, 8, 1, 10, 3, 2,
    11, 4, 13, 6, 15, 7, 0, 9, 2, 11, 4, 13, 6, 15, 8, 7, 0, 9, 2, 11, 4, 12, 5, 14, 7, 0, 9, 2,
    11, 4, 13, 12, 5, 14, 7, 0, 8, 1, 10, 3, 12, 5, 14, 7, 0, 9, 2, 1, 10, 3, 12, 5, 13, 6, 15, 8,
    1, 10, 3, 12, 5, 13, 7, 6, 15, 8, 1, 9, 2, 11, 4, 13, 6, 15, 8, 1, 10, 3, 12, 11, 4, 13, 6, 14,
    7, 0, 9, 2, 11, 4, 13, 6, 14, 8, 1, 0, 9, 2, 11, 3, 12, 5, 14, 7, 0, 9, 2, 11, 3, 13, 5, 5, 14,
    7, 15, 8, 1, 10, 3, 12, 5, 14, 7, 15, 8, 2, 10, 10, 3, 12, 4, 13, 6, 15, 8, 1, 10, 3, 12, 4,
    13, 6, 15,
];

#[inline]
pub(crate) fn ramp_phase(col: u16, row: u16) -> u8 {
    RAMP_PHASE[((row as usize & 15) << 4) | (col as usize & 15)]
}

#[allow(clippy::too_many_arguments)]
#[inline]
pub(crate) fn pixels_ramp_idx(
    n: u8,
    len: u8,
    prev: u8,
    hyst_q8: u32,
    phase: u8,
    frame: u8,
    pending: &mut u8,
    age: &mut u8,
) -> u8 {
    debug_assert!(len >= 1);
    debug_assert!(hyst_q8 < 256);
    if prev >= len || hyst_q8 == 0 {
        *pending = IDX_UNSET;
        *age = 0;
        return hysteresis_idx(n, len, prev, hyst_q8);
    }
    let plain = ((n as u32 * len as u32) >> 8).min(len as u32 - 1) as u8;
    if plain.abs_diff(prev) >= 2 {
        *pending = IDX_UNSET;
        *age = 0;
        return plain;
    }
    let want = hysteresis_idx(n, len, prev, hyst_q8 / 4);
    if want == prev || want.abs_diff(prev) >= 2 {
        *pending = IDX_UNSET;
        *age = 0;
        return want;
    }
    if *pending != want {
        *pending = want;
        *age = 1;
    } else {
        *age = age.saturating_add(1);
    }
    let base = ((hyst_q8 * 2) / 128).clamp(1, 4) as u8;
    if *age >= base && frame % 3 == (phase & 15) % 3 {
        *pending = IDX_UNSET;
        *age = 0;
        want
    } else {
        prev
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idx_boundary_plus_minus_035() {
        assert_eq!(hysteresis_idx(110, 10, IDX_UNSET, IDX_HYST_Q8), 4);

        assert_eq!(hysteresis_idx(136, 10, 4, IDX_HYST_Q8), 4, "0.31 past boundary holds");
        assert_eq!(hysteresis_idx(137, 10, 4, IDX_HYST_Q8), 5, "0.35+ past boundary moves");

        assert_eq!(hysteresis_idx(94, 10, 4, IDX_HYST_Q8), 4, "0.33 below boundary holds");
        assert_eq!(hysteresis_idx(93, 10, 4, IDX_HYST_Q8), 3, "0.36 below boundary moves");

        assert_eq!(hysteresis_idx(255, 10, 0, IDX_HYST_Q8), 9);
        assert_eq!(hysteresis_idx(0, 10, 9, IDX_HYST_Q8), 0);
    }

    #[test]
    fn idx_width_is_parametric() {
        assert_eq!(hysteresis_idx(140, 10, 4, 128), 4, "0.47 past boundary holds at width 128");
        assert_eq!(hysteresis_idx(141, 10, 4, 128), 5, "0.5+ past boundary moves at width 128");
        assert_eq!(hysteresis_idx(128, 10, 4, 0), 5, "width 0 = plain step");
        assert_eq!(hysteresis_idx(127, 10, 4, 0), 4);
        assert_eq!(hysteresis_idx(90, 10, 4, 128), 4);
        assert_eq!(hysteresis_idx(89, 10, 4, 128), 3);
    }

    #[test]
    fn idx_unset_or_stale_prev_quantizes_plainly() {
        assert_eq!(hysteresis_idx(255, 16, IDX_UNSET, IDX_HYST_Q8), 15);
        assert_eq!(hysteresis_idx(0, 16, IDX_UNSET, IDX_HYST_Q8), 0);
        assert_eq!(hysteresis_idx(128, 8, 12, IDX_HYST_Q8), 4);
        assert_eq!(hysteresis_idx(200, 1, IDX_UNSET, IDX_HYST_Q8), 0);
    }

    #[test]
    fn edge_dual_threshold() {
        let (t_on, t_off) = (96, 48);
        assert!(!edge_gate(96, false, t_on, t_off), "T_on is strict");
        assert!(edge_gate(97, false, t_on, t_off));
        assert!(edge_gate(49, true, t_on, t_off));
        assert!(!edge_gate(48, true, t_on, t_off), "T_off is strict");
        assert!(!edge_gate(90, false, t_on, t_off));
    }

    #[test]
    fn scene_cut_reset() {
        let mut st = HysteresisState::new(4, 3);
        *st.cell_mut(2, 1) = CellState { idx: 5, bin: 3, flags: cell_flags::WAS_EDGE, tone_candidate: 120, tone_age: 17 };
        assert_eq!(st.begin_frame(), 0);
        assert_eq!(st.begin_frame(), 1);
        st.reset();
        assert_eq!(st.cell(2, 1), CellState::default());
        assert_eq!((st.cols(), st.rows()), (4, 3));
        assert_eq!(st.frame(), 0, "a cut clears the residue cell compose reads");
        assert_eq!(st.begin_frame(), 0, "a cut restarts the ramp phase clock");
        assert_eq!(st.frame(), 0);
    }

    fn drive(
        n: u8,
        prev: u8,
        hyst: u32,
        phase: u8,
        frame: u8,
        pending: &mut u8,
        age: &mut u8,
    ) -> u8 {
        pixels_ramp_idx(n, 6, prev, hyst, phase, frame, pending, age)
    }

    #[test]
    fn pixels_ramp_dial_zero_is_plain_and_a_wide_gap_snaps() {
        let mut pending = 4u8;
        let mut age = 9u8;
        assert_eq!(drive(140, 2, 0, 7, 0, &mut pending, &mut age), 3);
        assert_eq!((pending, age), (IDX_UNSET, 0));

        pending = 3;
        age = 4;
        assert_eq!(drive(171, 2, 128, 0, 0, &mut pending, &mut age), 4, "two ramp steps do not wait");
        assert_eq!((pending, age), (IDX_UNSET, 0));
        assert_eq!(drive(255, IDX_UNSET, 128, 4, 0, &mut pending, &mut age), 5);
    }

    #[test]
    fn pixels_ramp_ignores_a_one_frame_spike_and_confirms_a_held_step() {
        let mut pending = IDX_UNSET;
        let mut age = 0u8;
        assert_eq!(drive(134, 2, 128, 0, 0, &mut pending, &mut age), 2);
        assert_eq!(age, 1);
        assert_eq!(drive(120, 2, 128, 0, 1, &mut pending, &mut age), 2, "the spike is cancelled");
        assert_eq!((pending, age), (IDX_UNSET, 0));

        let mut early = (IDX_UNSET, 0u8);
        let mut late = (IDX_UNSET, 0u8);
        let mut ie = 2u8;
        let mut il = 2u8;
        for frame in 0..4u8 {
            ie = drive(140, ie, 128, 0, frame, &mut early.0, &mut early.1);
            il = drive(140, il, 128, 2, frame, &mut late.0, &mut late.1);
            match frame {
                0 | 1 => assert_eq!((ie, il), (2, 2), "frame {frame} holds both phases"),
                2 => {
                    assert_eq!(ie, 2, "phase 0 waits for a frame index divisible by 3");
                    assert_eq!(il, 3, "phase 2 releases on frame 2");
                }
                _ => assert_eq!(ie, 3, "phase 0 releases on frame 3"),
            }
        }
    }

    #[test]
    fn pixels_ramp_higher_dial_holds_a_step_the_default_releases() {
        let mut pending = IDX_UNSET;
        let mut age = 0u8;
        for _ in 0..8 {
            assert_eq!(drive(134, 2, 255, 0, 0, &mut pending, &mut age), 2);
        }
        assert_eq!(age, 0, "inside the wider band the confirm stays disarmed");
    }

    #[test]
    fn pixels_ramp_small_noise_does_not_flicker_more_than_the_legacy_band() {
        let mut state = 0x1234_5678u32;
        let mut noise = || {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            ((state >> 24) % 9) as i16 - 4
        };
        for base in [40u8, 100, 128, 140, 200, 250] {
            for phase in [0u8, 1, 2, 7, 15] {
                let mut legacy = IDX_UNSET;
                let mut idx = IDX_UNSET;
                let mut pending = IDX_UNSET;
                let mut age = 0u8;
                let mut legacy_changes = 0u32;
                let mut changes = 0u32;
                for frame in 0..80u8 {
                    let n = (i16::from(base) + noise()).clamp(0, 255) as u8;
                    let next_legacy = hysteresis_idx(n, 6, legacy, 128);
                    if legacy != IDX_UNSET && next_legacy != legacy {
                        legacy_changes += 1;
                    }
                    legacy = next_legacy;
                    let prev = idx;
                    idx = pixels_ramp_idx(n, 6, idx, 128, phase, frame, &mut pending, &mut age);
                    if prev != IDX_UNSET && idx != prev {
                        changes += 1;
                    }
                }
                assert!(
                    changes <= legacy_changes,
                    "base {base} phase {phase}: ramp {changes}, legacy {legacy_changes}",
                );
            }
        }
    }

    #[test]
    fn pixels_ramp_alternating_threshold_chatter_stays_quieter_than_legacy() {
        let mut legacy = IDX_UNSET;
        let mut idx = IDX_UNSET;
        let mut pending = IDX_UNSET;
        let mut age = 0u8;
        let mut legacy_changes = 0u32;
        let mut changes = 0u32;
        for frame in 0..40 {
            let n = if frame % 2 == 0 { 158 } else { 98 };
            let next_legacy = hysteresis_idx(n, 6, legacy, 128);
            if legacy != IDX_UNSET && next_legacy != legacy {
                legacy_changes += 1;
            }
            legacy = next_legacy;
            let prev = idx;
            idx = pixels_ramp_idx(n, 6, idx, 128, 0, frame as u8, &mut pending, &mut age);
            if prev != IDX_UNSET && idx != prev {
                changes += 1;
            }
        }
        assert!(legacy_changes > 10, "the legacy band tracks this swing");
        assert_eq!(changes, 0, "a one-frame swing never confirms");
    }

    #[test]
    fn resize_reallocs_and_resets() {
        let mut st = HysteresisState::new(2, 2);
        *st.cell_mut(1, 1) = CellState { idx: 7, bin: 1, flags: cell_flags::WAS_EDGE, tone_candidate: 120, tone_age: 17 };
        let _ = st.begin_frame();
        st.resize(5, 4);
        assert_eq!((st.cols(), st.rows()), (5, 4));
        assert_eq!(st.frame(), 0, "a resize clears the residue cell compose reads");
        assert_eq!(st.begin_frame(), 0, "a resize restarts the ramp phase clock");
        assert_eq!(st.frame(), 0);
        for r in 0..4 {
            for c in 0..5 {
                assert_eq!(st.cell(c, r), CellState::default());
            }
        }
    }

    #[test]
    fn ramp_phase_clock_never_repeats_a_residue() {
        let mut st = HysteresisState::new(1, 1);
        let mut prev = None;
        for i in 0..300 {
            let frame = st.begin_frame();
            assert_eq!(st.frame(), frame, "cell compose reads the residue this tick issued");
            assert!(frame < 3, "frame {frame} at step {i}");
            if let Some(p) = prev {
                assert_ne!(frame, p, "step {i} repeated residue {frame}");
            }
            prev = Some(frame);
        }
    }

    #[test]
    fn pixels_ramp_confirm_wait_stays_inside_one_phase_period() {
        for &(hyst, lo, hi) in &[(16u32, 1u8, 3u8), (128, 2, 4), (255, 3, 5)] {
            for phase in 0..3u8 {
                for arm in 0..6u8 {
                    let mut pending = IDX_UNSET;
                    let mut age = 0u8;
                    let idx = 2u8;
                    let mut waited = None;
                    for t in 0..8u8 {
                        let frame = arm.wrapping_add(t) % 3;
                        let next = drive(140, idx, hyst, phase, frame, &mut pending, &mut age);
                        if next != idx {
                            waited = Some(t + 1);
                            assert_eq!(next, 3, "hyst {hyst} phase {phase} arm {arm}");
                            assert_eq!(frame, phase, "released on another residue");
                            break;
                        }
                    }
                    let waited = waited.expect("confirm");
                    assert!(
                        (lo..=hi).contains(&waited),
                        "hyst {hyst} phase {phase} arm {arm} waited {waited}, want {lo}..={hi}",
                    );
                }
            }
        }
    }

}
