//! Factory configuration defaults, loading and validation.

use std::path::Path;

use serde::{Deserialize, Serialize};
use auto_ascii_eval::Tolerances;

use crate::ffmpeg::BoxErr;

pub const EMBEDDED_PARAMS: &str = include_str!("../../../params.toml");

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

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BuildParams {
    pub fps: u16,
    pub base_w: u16,
    pub base_h: u16,
    pub zstd_level: i32,
    pub keyframe_ivl: u32,
}

impl Default for BuildParams {
    fn default() -> BuildParams {
        let w = auto_ascii_format::WriterOptions::default();
        BuildParams {
            fps: 30,
            base_w: auto_ascii_format::BASE_W,
            base_h: auto_ascii_format::BASE_H,
            zstd_level: 15,
            keyframe_ivl: u32::from(w.keyframe_ivl),
        }
    }
}

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

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EdgesParams {
    pub scharr_shift: u32,
    pub bilateral_passes: u32,
    pub bilateral_radius: u32,
    pub t_hi: u32,
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

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HighlightsParams {
    pub tophat_radius: u32,
    pub tophat_thresh: u32,
    pub shadow_pct: u32,
    pub shadow_max_l: u32,
}

impl Default for HighlightsParams {
    fn default() -> HighlightsParams {
        HighlightsParams { tophat_radius: 3, tophat_thresh: 48, shadow_pct: 8, shadow_max_l: 40 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TemporalParams {
    pub ema_alpha_y_milli: u32,
    pub ema_alpha_e_milli: u32,
    pub ema_alpha_c_milli: u32,
}

impl Default for TemporalParams {
    fn default() -> TemporalParams {
        TemporalParams { ema_alpha_y_milli: 700, ema_alpha_e_milli: 500, ema_alpha_c_milli: 700 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ComposeTable {
    pub edge_t_on: u32,
    pub edge_t_off: u32,
    pub coh_min_q8: u32,
    pub coh_dir_q8: u32,
    pub hi_cut_q8: u32,
    pub edge_white_cut_q8: u32,
    pub halfblock_min_delta: u32,
    pub edge_strong: u32,
    pub quad_e_on: u32,
    pub quad_e_off: u32,
    pub idx_hyst_q8: u32,
    pub shadow_lift: u32,
}

impl Default for ComposeTable {
    fn default() -> ComposeTable {
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
            idx_hyst_q8: auto_ascii_core::hysteresis::IDX_HYST_DEFAULT_Q8 as u32,
            shadow_lift: 0,
        }
    }
}

impl ComposeTable {
    pub fn to_core(self) -> Result<auto_ascii_core::ComposeParams, BoxErr> {
        Ok(auto_ascii_core::ComposeParams {
            edge_t_on: compose_u8("edge_t_on", self.edge_t_on)?,
            edge_t_off: compose_u8("edge_t_off", self.edge_t_off)?,
            coh_min_q8: compose_u8("coh_min_q8", self.coh_min_q8)?,
            coh_dir_q8: compose_u8("coh_dir_q8", self.coh_dir_q8)?,
            hi_cut_q8: compose_u8("hi_cut_q8", self.hi_cut_q8)?,
            edge_white_cut_q8: compose_u8("edge_white_cut_q8", self.edge_white_cut_q8)?,
            halfblock_min_delta: compose_u8("halfblock_min_delta", self.halfblock_min_delta)?,
            edge_strong: compose_u8("edge_strong", self.edge_strong)?,
            quad_e_on: compose_u8("quad_e_on", self.quad_e_on)?,
            quad_e_off: compose_u8("quad_e_off", self.quad_e_off)?,
            idx_hyst_q8: compose_u8("idx_hyst_q8", self.idx_hyst_q8)?,
            shadow_lift: compose_u8("shadow_lift", self.shadow_lift)?,
        })
    }
}

fn compose_u8(name: &str, v: u32) -> Result<u8, BoxErr> {
    u8::try_from(v).map_err(|_| format!("params: compose.{name} must be in 0..=255").into())
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EvalParams {
    pub grid_cols: u16,
    pub grid_rows: u16,
    pub max_frames: u32,
    pub ssim_every: u32,
    pub contact_frames: u32,
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
    pub fn embedded() -> Params {
        toml::from_str(EMBEDDED_PARAMS).expect("committed params.toml must parse")
    }

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

    pub fn dump(&self) -> String {
        toml::to_string_pretty(self).expect("Params serializes to TOML")
    }

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
                "params: build.keyframe_ivl must be in 1..=255 (the .ascii header stores it \
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
        c.to_core()?;
        if c.idx_hyst_q8 > auto_ascii_core::hysteresis::IDX_HYST_MAX_Q8 as u32 {
            return Err("params: compose.idx_hyst_q8 must be in 0..=255".into());
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

    #[test]
    fn hysteresis_config_obeys_the_player_range() {
        let mut p = Params::default();
        assert_eq!(p.compose.idx_hyst_q8, 128);
        p.compose.idx_hyst_q8 = 255;
        p.validate().unwrap();
        p.compose.idx_hyst_q8 = 256;
        assert!(p.validate().unwrap_err().to_string().contains("idx_hyst_q8 must be in 0..=255"));
    }

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

        let partial: Params = toml::from_str("[build]\nkeyframe_ivl = 12\n").unwrap();
        assert_eq!(partial.build.keyframe_ivl, 12);
        assert_eq!(partial.build.fps, 30);
        assert_eq!(partial.shots, ShotParams::default());
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
        assert!(toml::from_str::<Params>("[build]\nfsp = 30\n").is_err());
        assert!(toml::from_str::<Params>("[bulid]\nfps = 30\n").is_err());
    }

    #[test]
    fn validation_rejects_degenerate_geometry_and_ranges() {
        let mut p = Params::default();
        p.build.base_w = 1;
        assert!(p.validate().unwrap_err().to_string().contains("even and >= 2"));
        p.build.base_w = 479;
        assert!(p.validate().is_err());
        p.build.base_w = 0;
        assert!(p.validate().is_err());

        let mut p = Params::default();
        p.build.fps = 0;
        assert!(p.validate().is_err());
        let mut p = Params::default();
        p.build.zstd_level = 23;
        assert!(p.validate().is_err());
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
        let mut p = Params::default();
        p.edges.t_lo = p.edges.t_hi + 1;
        assert!(p.validate().unwrap_err().to_string().contains("t_lo <= t_hi"));
        let mut p = Params::default();
        p.edges.t_lo = 0;
        assert!(p.validate().is_err());
        let mut p = Params::default();
        p.edges.t_hi = 300;
        assert!(p.validate().unwrap_err().to_string().contains("1..=255"));
        let mut p = Params::default();
        p.edges.scharr_shift = 9;
        assert!(p.validate().is_err());
        let mut p = Params::default();
        p.edges.bilateral_radius = 5;
        assert!(p.validate().is_err());

        let mut p = Params::default();
        p.highlights.tophat_radius = 0;
        assert!(p.validate().is_err());
        let mut p = Params::default();
        p.highlights.shadow_pct = 51;
        assert!(p.validate().is_err());
        let mut p = Params::default();
        p.highlights.shadow_max_l = 256;
        assert!(p.validate().is_err());

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
        let mut b = Params::default();
        b.edges.t_hi = 99;
        assert_ne!(b.build_fingerprint(), fp, "edge knobs must invalidate the cache");
        let mut b = Params::default();
        b.highlights.tophat_radius = 5;
        assert_ne!(b.build_fingerprint(), fp, "highlight knobs must invalidate the cache");
        let mut b = Params::default();
        b.temporal.ema_alpha_y_milli = 999;
        assert_ne!(b.build_fingerprint(), fp, "temporal knobs must invalidate the cache");
        let mut b = Params::default();
        b.compose.edge_t_on = 40;
        assert_eq!(b.build_fingerprint(), fp, "compose knobs must not invalidate the cache");
    }

    #[test]
    fn compose_table_pins_core_defaults() {
        let t = ComposeTable::default().to_core().unwrap();
        let d = auto_ascii_core::ComposeParams::default();
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
        assert_eq!(t.shadow_lift, d.shadow_lift);
    }

    #[test]
    fn compose_table_validation() {
        let mut p = Params::default();
        p.compose.edge_t_on = 300;
        assert!(p.validate().unwrap_err().to_string().contains("0..=255"));
        let mut p = Params::default();
        p.compose.shadow_lift = 255;
        p.validate().unwrap();
        assert_eq!(p.compose.to_core().unwrap().shadow_lift, 255);
        p.compose.shadow_lift = 256;
        let err = p.validate().unwrap_err().to_string();
        assert_eq!(err, "params: compose.shadow_lift must be in 0..=255");
        assert!(p.compose.to_core().is_err(), "no silent truncation to 0");
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
