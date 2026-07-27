//! `params.toml` — every factory tunable as data (PLAN §5: "**Every tunable
//! lives in `params.toml`** — this is the agent socket").
//!
//! The committed repo-root `params.toml` is embedded via `include_str!` and
//! is the default configuration; `--params FILE` overrides any subset
//! (missing keys keep the embedded defaults via serde defaults);
//! `sleepy-factory params --dump` prints the effective merged config as
//! TOML. The in-code `Default` impls and the committed file are pinned to
//! each other by a unit test — drift is a build break, not a surprise.
//!
//! Cache identity: [`Params::build_fingerprint`] serializes exactly the
//! tables that affect asset bytes (`[build]`, `[shots]`, `[levels]`) so
//! `sleepy-factory eval` can key its asset cache on (input sha, params sha)
//! without eval-only knobs invalidating built assets.

use std::path::Path;

use serde::{Deserialize, Serialize};
use slpy_eval::Tolerances;

use crate::ffmpeg::BoxErr;

/// The committed repo-root defaults, compiled in (M2 item B contract).
pub const EMBEDDED_PARAMS: &str = include_str!("../../../params.toml");

/// Effective factory configuration. Every field has a serde default equal to
/// the embedded `params.toml` value, so a `--params` file may name any
/// subset of keys.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Params {
    pub build: BuildParams,
    pub shots: ShotParams,
    pub levels: LevelParams,
    pub edges: EdgesParams,
    pub highlights: HighlightsParams,
    pub temporal: TemporalParams,
    pub compose: ComposeTable,
    pub eval: EvalParams,
}

/// `[build]` — encode profile (PLAN §4/§5 stage 6).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BuildParams {
    pub fps: u16,
    pub base_w: u16,
    pub base_h: u16,
    pub zstd_level: i32,
    /// Keyframe cadence. Wider than the wire type on purpose (M2 review
    /// fix): the SLPY header stores u8, but an agent sweep writing
    /// `keyframe_ivl = 600` must get the validate() range error below, not
    /// a serde type error.
    pub keyframe_ivl: u32,
}

impl Default for BuildParams {
    fn default() -> BuildParams {
        // Single source of truth: the SLPY v1 writer profile + base res
        // (PLAN §4) — params defaults can never drift from the format crate.
        let w = slpy_format::WriterOptions::default();
        BuildParams {
            fps: 30,
            base_w: slpy_format::BASE_W,
            base_h: slpy_format::BASE_H,
            zstd_level: w.zstd_level,
            keyframe_ivl: u32::from(w.keyframe_ivl),
        }
    }
}

/// `[shots]` — shot detection (PLAN §5 stage 2).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShotParams {
    pub sad_threshold_milli: u64,
    pub min_shot_frames: u32,
}

impl Default for ShotParams {
    fn default() -> ShotParams {
        ShotParams {
            sad_threshold_milli: crate::shots::SHOT_SAD_THRESHOLD_MILLI,
            min_shot_frames: crate::shots::MIN_SHOT_FRAMES,
        }
    }
}

/// `[levels]` — per-shot NORM percentiles (PLAN §5 stage 5).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LevelParams {
    pub lo_pct: u64,
    pub hi_pct: u64,
}

impl Default for LevelParams {
    fn default() -> LevelParams {
        LevelParams { lo_pct: crate::lut::LEVELS_LO_PCT, hi_pct: crate::lut::LEVELS_HI_PCT }
    }
}

/// `[edges]` — Scharr → doubled-angle field → orientation-aware bilateral
/// smoothing → hysteresis-thresholded unthinned E (PLAN §5 stage 3, M3).
/// All fields are wider than strictly needed (u32) on purpose: an agent
/// sweep writing an out-of-range value must get the validate() range error,
/// not a serde type error (the keyframe_ivl precedent).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EdgesParams {
    /// Right-shift applied to the raw Scharr magnitude `isqrt(gx²+gy²)`.
    /// At 4, a sharp step edge of L\* contrast Δ scores E ≈ Δ (the Scharr
    /// tap sum is 16), so edge thresholds read as L\* contrast.
    pub scharr_shift: u32,
    /// Orientation-aware bilateral smoothing passes on the doubled-angle
    /// field (PLAN §5: two; 0 disables smoothing).
    pub bilateral_passes: u32,
    /// Bilateral window radius in pixels (window = 2r+1 square).
    pub bilateral_radius: u32,
    /// Hysteresis strong seed threshold on the smoothed magnitude (≈ L\*
    /// contrast units, see `scharr_shift`).
    pub t_hi: u32,
    /// Hysteresis weak-keep threshold: pixels in `t_lo..t_hi` survive only
    /// when 8-connected to a strong seed (Canny-style, but UNTHINNED).
    pub t_lo: u32,
}

impl Default for EdgesParams {
    fn default() -> EdgesParams {
        EdgesParams {
            scharr_shift: 4,
            bilateral_passes: 2,
            bilateral_radius: 2,
            t_hi: 28,
            t_lo: 12,
        }
    }
}

/// `[highlights]` — top-hat highlight + percentile deep-shadow flags for the
/// H plane (PLAN §5 stage 3, M3). bit0 = highlight, bit1 = deep shadow.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HighlightsParams {
    /// Box structuring-element radius for the morphological opening (white
    /// top-hat = luma − opening). Features wider than ~2r+1 px are not
    /// highlights — they are just bright areas.
    pub tophat_radius: u32,
    /// Minimum top-hat response (L\* units above the local opening) to flag
    /// bit0.
    pub tophat_thresh: u32,
    /// Deep-shadow percentile: the darkest `shadow_pct`% of the frame is
    /// flag-eligible.
    pub shadow_pct: u32,
    /// Absolute L\* ceiling on the deep-shadow threshold, so bright scenes
    /// never flag midtones as shadow: flagged iff
    /// `y <= min(percentile(shadow_pct), shadow_max_l)`.
    pub shadow_max_l: u32,
}

impl Default for HighlightsParams {
    fn default() -> HighlightsParams {
        HighlightsParams { tophat_radius: 3, tophat_thresh: 48, shadow_pct: 8, shadow_max_l: 40 }
    }
}

/// `[temporal]` — per-plane EMA strength (PLAN §5 stage 4), reset at shot
/// cuts. Alpha = weight of the NEW frame in thousandths: 1000 = no
/// smoothing, smaller = heavier smoothing. H is not EMA'd (it is bitflags);
/// it inherits stability by being computed from the EMA'd Y plane.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TemporalParams {
    /// Y plane alpha (milli). Kept light: heavy smoothing ghosts motion.
    pub ema_alpha_y_milli: u32,
    /// E/Ex/Ey alpha (milli). Heavier: edge shimmer is the #1 flicker source
    /// and the player's dual-threshold gate rides the decay.
    pub ema_alpha_e_milli: u32,
    /// Chroma alpha (milli), applied per channel before RGB565 packing.
    pub ema_alpha_c_milli: u32,
}

impl Default for TemporalParams {
    fn default() -> TemporalParams {
        TemporalParams { ema_alpha_y_milli: 700, ema_alpha_e_milli: 500, ema_alpha_c_milli: 700 }
    }
}

/// `[compose]` — the player's §3.5 compositor tunables (M3). These are
/// RENDERER knobs: the eval driver maps them onto
/// `slpy_core::ComposeParams` and hands them to the `Player`, so an agent
/// sweep can tune the edge gate / coherence bands WITHOUT rebuilding assets
/// — deliberately excluded from [`Params::build_fingerprint`]. The in-code
/// defaults here are pinned to `ComposeParams::default()` by unit test
/// (single source of truth: interactive playback uses the core defaults).
/// Fields are u32-wide for the clean-range-error rule.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ComposeTable {
    /// Edge gate on-threshold (strict `e > T_on`) on the resampled E plane.
    pub edge_t_on: u32,
    /// Edge gate hold-threshold (strict `e > T_off` while `was_edge`).
    pub edge_t_off: u32,
    /// Coherence below this (Q8) suppresses the edge layer entirely.
    pub coh_min_q8: u32,
    /// Coherence at/above this (Q8) draws directional glyphs; the band in
    /// between draws the junction glyph.
    pub coh_dir_q8: u32,
    /// Highlight gate: fires only while `idx < len·hi_cut_q8/256`.
    pub hi_cut_q8: u32,
    /// Edge suppression on near-white base cells (Q8 of the ramp top).
    pub edge_white_cut_q8: u32,
    /// `|top − bottom|` at/above this is "large" (§3.5 sub-cell structure).
    pub halfblock_min_delta: u32,
    /// Edge magnitude at/above this upgrades an ASCII junction `+` to `#`.
    pub edge_strong: u32,
    /// Quadrant-refinement noise floor, arm threshold (strict `e >`).
    pub quad_e_on: u32,
    /// Quadrant-refinement noise floor, hold threshold (strict `e >`).
    pub quad_e_off: u32,
    /// Ramp-index hysteresis width in Q8 fractions of one step (§3.5
    /// "± 0.35·step" = 90). Promoted from a slpy-core constant at M3 Tune.
    pub idx_hyst_q8: u32,
}

impl Default for ComposeTable {
    fn default() -> ComposeTable {
        // Pinned to slpy_core::ComposeParams::default() by unit test.
        ComposeTable {
            edge_t_on: 32,
            edge_t_off: 16,
            coh_min_q8: 96,
            coh_dir_q8: 160,
            hi_cut_q8: 160,
            edge_white_cut_q8: 240,
            halfblock_min_delta: 64,
            edge_strong: 96,
            quad_e_on: 2,
            quad_e_off: 1,
            idx_hyst_q8: 160,
        }
    }
}

impl ComposeTable {
    /// The slpy-core shape (fields are validated to fit u8).
    pub fn to_core(self) -> slpy_core::ComposeParams {
        slpy_core::ComposeParams {
            edge_t_on: self.edge_t_on as u8,
            edge_t_off: self.edge_t_off as u8,
            coh_min_q8: self.coh_min_q8 as u8,
            coh_dir_q8: self.coh_dir_q8 as u8,
            hi_cut_q8: self.hi_cut_q8 as u8,
            edge_white_cut_q8: self.edge_white_cut_q8 as u8,
            halfblock_min_delta: self.halfblock_min_delta as u8,
            edge_strong: self.edge_strong as u8,
            quad_e_on: self.quad_e_on as u8,
            quad_e_off: self.quad_e_off as u8,
            idx_hyst_q8: self.idx_hyst_q8 as u8,
        }
    }
}

/// `[eval]` — the eval driver's knobs (M2 item B; PLAN §6).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EvalParams {
    pub grid_cols: u16,
    pub grid_rows: u16,
    /// Frames evaluated per clip per tier; 0 = all frames.
    pub max_frames: u32,
    /// SSIM sampling stride in frames.
    pub ssim_every: u32,
    /// Contact-sheet snapshots per clip.
    pub contact_frames: u32,
    /// Baseline-compare tolerances (slpy-eval; serde defaults let a params
    /// file override any subset — INTERFACES compare.rs contract).
    pub tolerances: Tolerances,
}

impl Default for EvalParams {
    fn default() -> EvalParams {
        EvalParams {
            grid_cols: 300,
            grid_rows: 80,
            max_frames: 900,
            ssim_every: 30,
            contact_frames: 3,
            tolerances: Tolerances::default(),
        }
    }
}

impl Params {
    /// The embedded repo-root `params.toml` (the defaults). Panics only if
    /// the committed file is invalid — a build-time bug, covered by tests.
    pub fn embedded() -> Params {
        toml::from_str(EMBEDDED_PARAMS).expect("committed params.toml must parse")
    }

    /// Effective params: embedded defaults, or `path` parsed with missing
    /// keys falling back to the defaults. Always validated.
    pub fn load(path: Option<&Path>) -> Result<Params, BoxErr> {
        let params: Params = match path {
            None => Params::embedded(),
            Some(p) => {
                let text = std::fs::read_to_string(p)
                    .map_err(|e| format!("read {}: {e}", p.display()))?;
                toml::from_str(&text).map_err(|e| format!("parse {}: {e}", p.display()))?
            }
        };
        params.validate()?;
        Ok(params)
    }

    /// Effective config as TOML (`sleepy-factory params --dump`).
    pub fn dump(&self) -> String {
        toml::to_string_pretty(self).expect("Params serializes to TOML")
    }

    /// Canonical serialization of exactly the byte-affecting tables — the
    /// "params sha" half of the eval cache key. Eval-only knobs are
    /// deliberately excluded (they never change asset bytes).
    pub fn build_fingerprint(&self) -> String {
        #[derive(Serialize)]
        struct Fingerprint<'a> {
            build: &'a BuildParams,
            shots: &'a ShotParams,
            levels: &'a LevelParams,
            edges: &'a EdgesParams,
            highlights: &'a HighlightsParams,
            temporal: &'a TemporalParams,
        }
        toml::to_string(&Fingerprint {
            build: &self.build,
            shots: &self.shots,
            levels: &self.levels,
            edges: &self.edges,
            highlights: &self.highlights,
            temporal: &self.temporal,
        })
        .expect("fingerprint serializes")
    }

    /// Range checks. The base-dim rule (even, >= 2) is the §4 C-plane
    /// geometry term — also enforced by the SLPY writer and reader (M1
    /// review fix 1); rejecting here gives the friendliest error first.
    pub fn validate(&self) -> Result<(), BoxErr> {
        let b = &self.build;
        if b.fps == 0 || b.fps > 1000 {
            return Err("params: build.fps must be in 1..=1000".into());
        }
        for (name, v) in [("base_w", b.base_w), ("base_h", b.base_h)] {
            if v < 2 || !v.is_multiple_of(2) {
                return Err(format!(
                    "params: build.{name} = {v} must be even and >= 2 \
                     (chroma plane C is stored at half res, PLAN §4)"
                )
                .into());
            }
        }
        if !(1..=22).contains(&b.zstd_level) {
            return Err("params: build.zstd_level must be in 1..=22".into());
        }
        if !(1..=255).contains(&b.keyframe_ivl) {
            return Err(
                "params: build.keyframe_ivl must be in 1..=255 (the SLPY header stores it \
                 as u8, PLAN §4)"
                    .into(),
            );
        }
        if self.shots.sad_threshold_milli == 0 {
            return Err("params: shots.sad_threshold_milli must be >= 1".into());
        }
        if self.shots.min_shot_frames == 0 {
            return Err("params: shots.min_shot_frames must be >= 1".into());
        }
        let l = &self.levels;
        if l.lo_pct >= l.hi_pct || l.hi_pct > 100 {
            return Err("params: levels must satisfy lo_pct < hi_pct <= 100".into());
        }
        let ed = &self.edges;
        if ed.scharr_shift > 8 {
            return Err("params: edges.scharr_shift must be in 0..=8".into());
        }
        if ed.bilateral_passes > 8 {
            return Err("params: edges.bilateral_passes must be in 0..=8".into());
        }
        if !(1..=4).contains(&ed.bilateral_radius) {
            return Err("params: edges.bilateral_radius must be in 1..=4".into());
        }
        if !(1..=255).contains(&ed.t_hi) {
            return Err("params: edges.t_hi must be in 1..=255".into());
        }
        if ed.t_lo < 1 || ed.t_lo > ed.t_hi {
            return Err("params: edges must satisfy 1 <= t_lo <= t_hi".into());
        }
        let hl = &self.highlights;
        if !(1..=15).contains(&hl.tophat_radius) {
            return Err("params: highlights.tophat_radius must be in 1..=15".into());
        }
        if !(1..=255).contains(&hl.tophat_thresh) {
            return Err("params: highlights.tophat_thresh must be in 1..=255".into());
        }
        if hl.shadow_pct > 50 {
            return Err("params: highlights.shadow_pct must be in 0..=50".into());
        }
        if hl.shadow_max_l > 255 {
            return Err("params: highlights.shadow_max_l must be in 0..=255".into());
        }
        for (name, v) in [
            ("ema_alpha_y_milli", self.temporal.ema_alpha_y_milli),
            ("ema_alpha_e_milli", self.temporal.ema_alpha_e_milli),
            ("ema_alpha_c_milli", self.temporal.ema_alpha_c_milli),
        ] {
            if !(1..=1000).contains(&v) {
                return Err(format!(
                    "params: temporal.{name} must be in 1..=1000 (1000 = no smoothing)"
                )
                .into());
            }
        }
        let c = &self.compose;
        for (name, v) in [
            ("edge_t_on", c.edge_t_on),
            ("edge_t_off", c.edge_t_off),
            ("coh_min_q8", c.coh_min_q8),
            ("coh_dir_q8", c.coh_dir_q8),
            ("hi_cut_q8", c.hi_cut_q8),
            ("edge_white_cut_q8", c.edge_white_cut_q8),
            ("halfblock_min_delta", c.halfblock_min_delta),
            ("edge_strong", c.edge_strong),
            ("quad_e_on", c.quad_e_on),
            ("quad_e_off", c.quad_e_off),
            ("idx_hyst_q8", c.idx_hyst_q8),
        ] {
            if v > 255 {
                return Err(format!("params: compose.{name} must be in 0..=255").into());
            }
        }
        if c.edge_t_off > c.edge_t_on {
            return Err("params: compose must satisfy edge_t_off <= edge_t_on".into());
        }
        if c.coh_min_q8 > c.coh_dir_q8 {
            return Err("params: compose must satisfy coh_min_q8 <= coh_dir_q8".into());
        }
        if c.quad_e_off > c.quad_e_on {
            return Err("params: compose must satisfy quad_e_off <= quad_e_on".into());
        }
        let e = &self.eval;
        if e.grid_cols == 0 || e.grid_rows == 0 {
            return Err("params: eval grid dimensions must be nonzero".into());
        }
        if e.ssim_every == 0 {
            return Err("params: eval.ssim_every must be >= 1".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed repo-root params.toml IS the in-code defaults — the two
    /// sources of truth may never drift (M2 item B contract).
    #[test]
    fn embedded_file_equals_in_code_defaults() {
        assert_eq!(Params::embedded(), Params::default());
        Params::default().validate().unwrap();
    }

    #[test]
    fn dump_roundtrips_and_partial_overrides_merge() {
        let p = Params::default();
        let back: Params = toml::from_str(&p.dump()).unwrap();
        assert_eq!(back, p);

        // A file naming one key overrides just that key.
        let partial: Params = toml::from_str("[build]\nkeyframe_ivl = 12\n").unwrap();
        assert_eq!(partial.build.keyframe_ivl, 12);
        assert_eq!(partial.build.fps, 30);
        assert_eq!(partial.shots, ShotParams::default());
        // Nested tolerance subsets merge the same way (compare.rs contract).
        let tol: Params =
            toml::from_str("[eval.tolerances]\nssim_max_drop = 0.001\n").unwrap();
        assert_eq!(tol.eval.tolerances.ssim_max_drop, 0.001);
        assert_eq!(
            tol.eval.tolerances.bytes_frac_max_increase,
            Tolerances::default().bytes_frac_max_increase
        );
    }

    #[test]
    fn unknown_keys_are_rejected() {
        // Typos must not silently no-op an agent's sweep.
        assert!(toml::from_str::<Params>("[build]\nfsp = 30\n").is_err());
        assert!(toml::from_str::<Params>("[bulid]\nfps = 30\n").is_err());
    }

    #[test]
    fn validation_rejects_degenerate_geometry_and_ranges() {
        let mut p = Params::default();
        p.build.base_w = 1; // the M1-review fix-1 case
        assert!(p.validate().unwrap_err().to_string().contains("even and >= 2"));
        p.build.base_w = 479; // odd
        assert!(p.validate().is_err());
        p.build.base_w = 0;
        assert!(p.validate().is_err());

        let mut p = Params::default();
        p.build.fps = 0;
        assert!(p.validate().is_err());
        let mut p = Params::default();
        p.build.zstd_level = 23;
        assert!(p.validate().is_err());
        // M2 review fix: keyframe_ivl 600 (the acceptance-4 drill value) must
        // be a clean range error naming the u8 wire limit, not a serde error.
        let mut p = Params::default();
        p.build.keyframe_ivl = 600;
        assert!(p.validate().unwrap_err().to_string().contains("1..=255"));
        assert!(toml::from_str::<Params>("[build]\nkeyframe_ivl = 600\n")
            .unwrap()
            .validate()
            .is_err());
        let mut p = Params::default();
        p.build.keyframe_ivl = 0;
        assert!(p.validate().is_err());
        let p = Params { levels: LevelParams { lo_pct: 98, hi_pct: 2 }, ..Params::default() };
        assert!(p.validate().is_err());
        let mut p = Params::default();
        p.eval.ssim_every = 0;
        assert!(p.validate().is_err());
    }

    #[test]
    fn validation_rejects_bad_m3_feature_ranges() {
        // [edges]: t_lo > t_hi, zero t_lo, oversized shift/radius.
        let mut p = Params::default();
        p.edges.t_lo = p.edges.t_hi + 1;
        assert!(p.validate().unwrap_err().to_string().contains("t_lo <= t_hi"));
        let mut p = Params::default();
        p.edges.t_lo = 0;
        assert!(p.validate().is_err());
        let mut p = Params::default();
        p.edges.t_hi = 300; // wide type, clean range error (sweep-agent contract)
        assert!(p.validate().unwrap_err().to_string().contains("1..=255"));
        let mut p = Params::default();
        p.edges.scharr_shift = 9;
        assert!(p.validate().is_err());
        let mut p = Params::default();
        p.edges.bilateral_radius = 5;
        assert!(p.validate().is_err());

        // [highlights].
        let mut p = Params::default();
        p.highlights.tophat_radius = 0;
        assert!(p.validate().is_err());
        let mut p = Params::default();
        p.highlights.shadow_pct = 51;
        assert!(p.validate().is_err());
        let mut p = Params::default();
        p.highlights.shadow_max_l = 256;
        assert!(p.validate().is_err());

        // [temporal]: 0 would freeze the first frame forever; > 1000 is
        // out of the milli domain.
        let mut p = Params::default();
        p.temporal.ema_alpha_e_milli = 0;
        assert!(p.validate().unwrap_err().to_string().contains("1..=1000"));
        let mut p = Params::default();
        p.temporal.ema_alpha_y_milli = 1001;
        assert!(p.validate().is_err());
    }

    #[test]
    fn build_fingerprint_ignores_eval_knobs() {
        let mut a = Params::default();
        let fp = a.build_fingerprint();
        a.eval.max_frames = 7;
        a.eval.tolerances.ssim_max_drop = 0.5;
        assert_eq!(a.build_fingerprint(), fp, "eval knobs must not invalidate the cache");
        a.build.zstd_level = 3;
        assert_ne!(a.build_fingerprint(), fp, "encode knobs must invalidate the cache");
        // M3 feature tables all change asset bytes → all must invalidate.
        let mut b = Params::default();
        b.edges.t_hi = 99;
        assert_ne!(b.build_fingerprint(), fp, "edge knobs must invalidate the cache");
        let mut b = Params::default();
        b.highlights.tophat_radius = 5;
        assert_ne!(b.build_fingerprint(), fp, "highlight knobs must invalidate the cache");
        let mut b = Params::default();
        b.temporal.ema_alpha_y_milli = 999;
        assert_ne!(b.build_fingerprint(), fp, "temporal knobs must invalidate the cache");
        // [compose] is a RENDERER knob (M3): tuning it must NOT rebuild
        // assets — that is the whole point of the player-side socket.
        let mut b = Params::default();
        b.compose.edge_t_on = 40;
        assert_eq!(b.build_fingerprint(), fp, "compose knobs must not invalidate the cache");
    }

    /// Single source of truth (M3): the `[compose]` defaults ARE
    /// `slpy_core::ComposeParams::default()` — interactive playback (which
    /// never reads params.toml) and the eval driver must start from the
    /// same untuned baseline.
    #[test]
    fn compose_table_pins_core_defaults() {
        let t = ComposeTable::default().to_core();
        let d = slpy_core::ComposeParams::default();
        assert_eq!(t.edge_t_on, d.edge_t_on);
        assert_eq!(t.edge_t_off, d.edge_t_off);
        assert_eq!(t.coh_min_q8, d.coh_min_q8);
        assert_eq!(t.coh_dir_q8, d.coh_dir_q8);
        assert_eq!(t.hi_cut_q8, d.hi_cut_q8);
        assert_eq!(t.edge_white_cut_q8, d.edge_white_cut_q8);
        assert_eq!(t.halfblock_min_delta, d.halfblock_min_delta);
        assert_eq!(t.edge_strong, d.edge_strong);
        assert_eq!(t.quad_e_on, d.quad_e_on);
        assert_eq!(t.quad_e_off, d.quad_e_off);
        assert_eq!(t.idx_hyst_q8, d.idx_hyst_q8);
    }

    #[test]
    fn compose_table_validation() {
        let mut p = Params::default();
        p.compose.edge_t_on = 300;
        assert!(p.validate().unwrap_err().to_string().contains("0..=255"));
        let mut p = Params::default();
        p.compose.edge_t_off = p.compose.edge_t_on + 1;
        assert!(p.validate().unwrap_err().to_string().contains("edge_t_off <= edge_t_on"));
        let mut p = Params::default();
        p.compose.coh_min_q8 = 200;
        assert!(p.validate().unwrap_err().to_string().contains("coh_min_q8 <= coh_dir_q8"));
        let mut p = Params::default();
        p.compose.quad_e_off = p.compose.quad_e_on + 1;
        assert!(p.validate().unwrap_err().to_string().contains("quad_e_off <= quad_e_on"));
    }
}
