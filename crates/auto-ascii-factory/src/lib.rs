//! `auto-ascii-factory` as a **library** (PLAN-M6-M8 §2).
//!
//! The factory was bin-only through M6. M7 needs the ingest path from a
//! second binary — `auto-ascii import` (crates/auto-ascii-cli) — and the
//! facade cannot host it, because the factory already depends on the
//! facade. So the modules moved here and `src/main.rs` kept nothing but the
//! clap surface and the `inspect` report.
//!
//! Every module stays `pub`: this crate is unpublished workspace plumbing,
//! not an API with a semver promise, and the split must not turn a
//! cross-module helper into dead code. The *supported* entry points are the
//! three items below — [`build`] (ingest one video), [`effective_params`]
//! (the params.toml merge the CLI flags override) and [`sha256_hex`] /
//! [`sha256_file`] (the provenance hash `import` records).
//!
//! Human progress and info lines are written to a caller-supplied
//! [`std::io::Write`], never to `println!`/`eprintln!`: the bin passes
//! `stderr` (so its bytes are byte-for-byte what M6 shipped) and a `--json`
//! caller passes stderr too, keeping stdout clean for one JSON object.

pub mod build;
pub mod edges;
pub mod eval;
pub mod extract;
pub mod features;
pub mod ffmpeg;
pub mod font_table;
pub mod highlights;
pub mod lut;
pub mod params;
pub mod reel;
pub mod sha256;
pub mod shots;
pub mod sweep;
pub mod temporal;

use std::io::Write;
use std::path::Path;

pub use build::BuildReport;
pub use ffmpeg::BoxErr;
pub use sha256::{sha256, sha256_file, sha256_hex};

/// One ingest job: the arguments `auto-ascii-factory build` and
/// `auto-ascii import` both reduce to (PLAN §5 CLI shape). `params` is a
/// path to a tunables file, not a parsed [`params::Params`] — the merge
/// order (embedded defaults, file, then `fps`/`res`) is part of the
/// contract and [`build`] owns it.
pub struct BuildRequest<'a> {
    /// Input video (any ffmpeg-readable container).
    pub input: &'a Path,
    /// Output `.ascii` path. Written via `<output>.part` and renamed.
    pub output: &'a Path,
    /// Optional tunables file; missing keys keep the embedded defaults.
    pub params: Option<&'a Path>,
    /// Start offset in seconds (ffmpeg `-ss`, input seeking).
    pub ss: Option<f64>,
    /// Duration limit in seconds (ffmpeg `-t`).
    pub t: Option<f64>,
    /// Output frame rate override (default: `[build].fps`).
    pub fps: Option<u16>,
    /// Stored plane resolution override (default: `[build].base_w/base_h`).
    pub res: Option<(u16, u16)>,
}

/// Ingest one video into an ASCI asset, returning what it produced.
///
/// `info` receives the human progress lines (`input: …`, `pass 1/2: …`,
/// `wrote …`) — pass `&mut std::io::stderr()` to reproduce the binary's
/// output exactly.
pub fn build(req: &BuildRequest<'_>, info: &mut dyn Write) -> Result<BuildReport, BoxErr> {
    let params = effective_params(req.params, req.fps, req.res)?;
    build::run(
        &build::BuildArgs {
            input: req.input.to_path_buf(),
            output: req.output.to_path_buf(),
            ss: req.ss,
            t: req.t,
            params,
        },
        info,
    )
}

/// Effective params: embedded defaults, `--params` file, then the CLI
/// overrides (the most specific wins); re-validated after the merge.
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

/// Parse a `--res WxH` spec. Shared with `auto-ascii import --res` so both
/// binaries reject the same shapes with the same words (§4 geometry: the
/// chroma plane C is stored at half res, hence "even").
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
