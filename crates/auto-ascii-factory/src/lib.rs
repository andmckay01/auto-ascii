//! Factory library entry points for video ingestion and configuration.

pub mod build;
pub mod edges;
pub mod extract;
pub mod features;
pub mod ffmpeg;
pub mod highlights;
pub mod live;
pub mod lut;
pub mod params;
pub mod sha256;
pub mod shots;
pub mod temporal;

use std::io::Write;
use std::path::Path;

pub use build::BuildReport;
pub use ffmpeg::{BoxErr, Programs};
pub use sha256::{sha256, sha256_file, sha256_hex};

pub const PIPELINE_FINGERPRINT: &str = env!("ASCII_PIPELINE_FINGERPRINT");
pub const PIPELINE_VERSION: &str = "0.1.0";

pub struct BuildRequest<'a> {
    pub input: &'a Path,
    pub output: &'a Path,
    pub params: Option<&'a Path>,
    pub ss: Option<f64>,
    pub t: Option<f64>,
    pub fps: Option<u16>,
    pub res: Option<(u16, u16)>,
    pub programs: &'a Programs,
}

pub fn build(req: &BuildRequest<'_>, info: &mut dyn Write) -> Result<BuildReport, BoxErr> {
    let params = effective_params(req.params, req.fps, req.res)?;
    build::run(
        &build::BuildArgs {
            input: req.input.to_path_buf(),
            output: req.output.to_path_buf(),
            ss: req.ss,
            t: req.t,
            params,
            programs: req.programs.clone(),
        },
        info,
    )
}

pub fn effective_params(
    path: Option<&Path>,
    fps: Option<u16>,
    res: Option<(u16, u16)>,
) -> Result<params::Params, BoxErr> {
    let mut p = params::Params::load(path)?;
    if let Some(fps) = fps {
        p.build.fps = fps;
    }
    if let Some((w, h)) = res {
        p.build.base_w = w;
        p.build.base_h = h;
    }
    p.validate()?;
    Ok(p)
}

pub fn parse_res(s: &str) -> Result<(u16, u16), String> {
    let (w, h) = s
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("bad --res {s:?}: expected WxH, e.g. 480x270"))?;
    let w: u16 = w.trim().parse().map_err(|e| format!("bad --res width: {e}"))?;
    let h: u16 = h.trim().parse().map_err(|e| format!("bad --res height: {e}"))?;
    if w == 0 || h == 0 {
        return Err("--res dimensions must be nonzero".into());
    }
    if !w.is_multiple_of(2) || !h.is_multiple_of(2) {
        return Err("--res dimensions must be even (chroma plane C is stored at half res)".into());
    }
    Ok((w, h))
}
