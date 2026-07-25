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
        }
        toml::to_string(&Fingerprint {
            build: &self.build,
            shots: &self.shots,
            levels: &self.levels,
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
    fn build_fingerprint_ignores_eval_knobs() {
        let mut a = Params::default();
        let fp = a.build_fingerprint();
        a.eval.max_frames = 7;
        a.eval.tolerances.ssim_max_drop = 0.5;
        assert_eq!(a.build_fingerprint(), fp, "eval knobs must not invalidate the cache");
        a.build.zstd_level = 3;
        assert_ne!(a.build_fingerprint(), fp, "encode knobs must invalidate the cache");
    }
}
