//! `sleepy-factory sweep` — the parameter-sweep half of the agent socket
//! (PLAN §5 CLI: `sweep --params params.toml --grid sweeps/edge_thresholds.toml`).
//!
//! A sweep file declares **axes** — named lists of param-override sets — and
//! optional composite-score weights. Combos are the cartesian product across
//! axes (values *within* one axis vary together, so correlated pairs like
//! `edges.t_hi`/`edges.t_lo` stay sane); coordinate descent = one or two axes
//! per file, winner folded into the next file's base. Every combo runs the
//! real eval pipeline ([`eval::eval_clip`]) over the corpus in sweep mode
//! (truecolor pass only, no contact PNGs, source-Canny truth memoized across
//! combos) and shares the M2 asset cache — combos differing only in renderer
//! (`[compose]`) or eval knobs never rebuild assets.
//!
//! ```toml
//! [score]                  # optional; defaults below
//! ssim = 0.4
//! edge_f1 = 0.4
//! flicker = 0.2            # subtracted
//! flicker_norm = 2.0       # flicker is divided by this before weighting
//!
//! [[axes]]
//! name = "edge-runtime"
//! values = [
//!   { "compose.edge_t_on" = 24, "compose.edge_t_off" = 12 },
//!   { "compose.edge_t_on" = 32, "compose.edge_t_off" = 16 },
//! ]
//! ```
//!
//! **Composite score** (documented default, PLAN §5 Tune):
//! `0.4·mean(ssim) + 0.4·mean(edge_f1) − 0.2·mean(flicker / flicker_norm)`
//! with means over clips; `flicker_norm` = 2.0 = the §6 flicker gate, so a
//! clip sitting exactly at the gate costs its full flicker weight. Combos
//! whose merged params fail validation are recorded as skipped (an axis
//! cross may legally produce e.g. `t_lo > t_hi`), never silently dropped.
//!
//! Outputs under `--out DIR`: `combo-NN.json` (full [`EvalReport`] per
//! combo), `sweep.json` (ranked results, deterministic layout) and
//! `leaderboard.html` (compact human half).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use slpy_eval::EvalReport;

use crate::eval::{self, EvalArgs, TruthCache};
use crate::ffmpeg::BoxErr;
use crate::params::Params;
use crate::sha256::sha256_hex;

pub struct SweepArgs {
    pub corpus: PathBuf,
    /// Base params (embedded defaults + `--params` file) that every combo
    /// starts from.
    pub base: Params,
    /// The sweep spec file (`--grid`).
    pub grid: PathBuf,
    /// Output directory (`--out`).
    pub out_dir: PathBuf,
    pub cache_dir: PathBuf,
}

/// Composite-score weights (see module docs for the formula).
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScoreWeights {
    pub ssim: f64,
    pub edge_f1: f64,
    /// Weight on normalized flicker; SUBTRACTED from the score.
    pub flicker: f64,
    /// Flicker normalizer (glyph switches/cell/s); default = the §6 gate.
    pub flicker_norm: f64,
}

impl Default for ScoreWeights {
    fn default() -> ScoreWeights {
        ScoreWeights { ssim: 0.4, edge_f1: 0.4, flicker: 0.2, flicker_norm: 2.0 }
    }
}

/// One axis: a named list of override sets (dotted param path → TOML value).
/// Values within an axis travel together; axes cross.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Axis {
    name: String,
    values: Vec<toml::map::Map<String, toml::Value>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SweepSpec {
    #[serde(default)]
    score: ScoreWeights,
    axes: Vec<Axis>,
}

/// One combo's flattened overrides, in axis order.
#[derive(Clone, Debug)]
struct Combo {
    /// `(dotted path, value)` pairs, e.g. `("edges.t_hi", 28)`.
    overrides: Vec<(String, toml::Value)>,
}

impl Combo {
    /// Canonical human/JSON label: `edges.t_hi=28 compose.edge_t_on=32`.
    fn label(&self) -> String {
        if self.overrides.is_empty() {
            return "(base params)".into();
        }
        self.overrides
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Per-clip metric row in `sweep.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SweepClipRow {
    pub clip: String,
    pub ssim: Option<f64>,
    pub edge_f1: Option<f64>,
    pub flicker: Option<f64>,
}

/// One ranked result in `sweep.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SweepResult {
    /// Combo index in enumeration order (stable across reruns).
    pub id: u32,
    pub combo: String,
    /// `None` = combo skipped (validation error, recorded in `skip_reason`).
    pub score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    pub ssim_mean: Option<f64>,
    pub edge_f1_mean: Option<f64>,
    pub flicker_mean: Option<f64>,
    #[serde(default)]
    pub clips: Vec<SweepClipRow>,
    /// The per-combo `EvalReport` file name (relative to the out dir).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SweepReport {
    pub schema_version: u32,
    pub generator: String,
    pub score_weights: ScoreWeights,
    /// Ranked: best score first, skipped combos last (by id).
    pub results: Vec<SweepResult>,
}

pub const SWEEP_SCHEMA_VERSION: u32 = 1;

/// Set `value` at a dotted path inside a TOML table tree. Every intermediate
/// table must already exist (the tree is the full serialized [`Params`], so
/// a missing table is a typo'd table name).
fn set_dotted(
    root: &mut toml::Value,
    path: &str,
    value: &toml::Value,
) -> Result<(), BoxErr> {
    let mut cur = root;
    let parts: Vec<&str> = path.split('.').collect();
    if parts.iter().any(|p| p.is_empty()) {
        return Err(format!("sweep: bad param path {path:?}").into());
    }
    for part in &parts[..parts.len() - 1] {
        cur = cur
            .as_table_mut()
            .and_then(|t| t.get_mut(*part))
            .ok_or_else(|| format!("sweep: unknown params table {part:?} in {path:?}"))?;
    }
    let leaf = *parts.last().expect("split never empty");
    let table = cur
        .as_table_mut()
        .ok_or_else(|| format!("sweep: {path:?} does not name a table entry"))?;
    // The leaf must already exist too — Params is #[serde(default)], so the
    // serialized base names every legal key; a new key is a typo (and
    // deny_unknown_fields would reject it later with a worse message).
    if !table.contains_key(leaf) {
        return Err(format!("sweep: unknown param {path:?} (typo?)").into());
    }
    table.insert(leaf.to_string(), value.clone());
    Ok(())
}

/// Base params + one combo's overrides → validated `Params`.
/// `Err` = malformed path/typo (hard error); `Ok(Err(reason))` = the merged
/// params failed validation (a legal skip under crossed axes).
fn apply_combo(base: &Params, combo: &Combo) -> Result<Result<Params, String>, BoxErr> {
    let mut root = toml::Value::try_from(base).map_err(|e| format!("serialize params: {e}"))?;
    for (path, value) in &combo.overrides {
        set_dotted(&mut root, path, value)?;
    }
    let params: Params = root
        .try_into()
        .map_err(|e| format!("sweep: merged params did not deserialize: {e}"))?;
    Ok(match params.validate() {
        Ok(()) => Ok(params),
        Err(e) => Err(e.to_string()),
    })
}

/// Cartesian product across axes, enumeration order = file order (later axes
/// vary fastest). A param named by two axes in the same combo is a hard
/// error — silent last-writer-wins would corrupt coordinate descent.
fn enumerate_combos(spec: &SweepSpec) -> Result<Vec<Combo>, BoxErr> {
    let mut combos: Vec<Combo> = vec![Combo { overrides: Vec::new() }];
    for axis in &spec.axes {
        if axis.values.is_empty() {
            return Err(format!("sweep: axis {:?} has no values", axis.name).into());
        }
        let mut next = Vec::with_capacity(combos.len() * axis.values.len());
        for c in &combos {
            for set in &axis.values {
                let mut combo = c.clone();
                for (k, v) in set {
                    if combo.overrides.iter().any(|(ek, _)| ek == k) {
                        return Err(format!(
                            "sweep: param {k:?} set by two axes in one combo (axis {:?})",
                            axis.name
                        )
                        .into());
                    }
                    combo.overrides.push((k.clone(), v.clone()));
                }
                next.push(combo);
            }
        }
        combos = next;
    }
    Ok(combos)
}

/// Mean of the present values; `None` when none are present.
fn mean(vals: impl Iterator<Item = Option<f64>>) -> Option<f64> {
    let (mut sum, mut n) = (0.0f64, 0u32);
    for v in vals.flatten() {
        sum += v;
        n += 1;
    }
    (n > 0).then(|| sum / f64::from(n))
}

/// The composite score (module docs): missing metrics contribute zero to
/// their term rather than poisoning the whole combo.
fn score_of(w: &ScoreWeights, ssim: Option<f64>, f1: Option<f64>, flicker: Option<f64>) -> f64 {
    let norm = if w.flicker_norm > 0.0 { w.flicker_norm } else { 1.0 };
    w.ssim * ssim.unwrap_or(0.0) + w.edge_f1 * f1.unwrap_or(0.0)
        - w.flicker * (flicker.unwrap_or(0.0) / norm)
}

pub fn run(args: &SweepArgs) -> Result<(), BoxErr> {
    let text = std::fs::read_to_string(&args.grid)
        .map_err(|e| format!("read {}: {e}", args.grid.display()))?;
    let spec: SweepSpec =
        toml::from_str(&text).map_err(|e| format!("parse {}: {e}", args.grid.display()))?;
    let combos = enumerate_combos(&spec)?;
    let clips = eval::discover_corpus(&args.corpus)?;
    std::fs::create_dir_all(&args.out_dir)
        .map_err(|e| format!("create {}: {e}", args.out_dir.display()))?;
    std::fs::create_dir_all(&args.cache_dir)
        .map_err(|e| format!("create {}: {e}", args.cache_dir.display()))?;

    eprintln!(
        "sweep: {} combo(s) x {} clip(s) (grid {})",
        combos.len(),
        clips.len(),
        args.grid.display()
    );

    let mut truth_cache = TruthCache::new();
    let mut results: Vec<SweepResult> = Vec::new();
    for (id, combo) in combos.iter().enumerate() {
        let id = id as u32;
        let label = combo.label();
        let mut params = match apply_combo(&args.base, combo)? {
            Ok(p) => p,
            Err(reason) => {
                eprintln!("sweep: combo {id} [{label}] SKIPPED: {reason}");
                results.push(SweepResult {
                    id,
                    combo: label,
                    score: None,
                    skip_reason: Some(reason),
                    ssim_mean: None,
                    edge_f1_mean: None,
                    flicker_mean: None,
                    clips: Vec::new(),
                    report: None,
                });
                continue;
            }
        };
        // Sweep mode: metrics only — no contact PNGs (ffmpeg subprocesses),
        // no reel, truecolor pass only. Deliberately NOT persisted into the
        // combo's params: these are eval-output knobs, not tunables.
        params.eval.contact_frames = 0;
        eprintln!("sweep: combo {id} [{label}]");

        let params_sha = sha256_hex(params.build_fingerprint().as_bytes());
        let combo_out = args.out_dir.join(format!("combo-{id:02}.json"));
        let eval_args = EvalArgs {
            corpus: args.corpus.clone(),
            params,
            baseline: None,
            out: combo_out,
            html: None,
            reel: None,
            cache_dir: args.cache_dir.clone(),
            truecolor_only: true,
            // Sweeps score with the same conservative table as the baseline;
            // per-font scoring (--font-table) is an eval-only mode.
            font_table: None,
        };

        let mut report =
            EvalReport::new(format!("sleepy-factory {} sweep", env!("CARGO_PKG_VERSION")));
        let mut rows: Vec<SweepClipRow> = Vec::new();
        for (name, path) in &clips {
            let ev = eval::eval_clip(name, path, &eval_args, &params_sha, Some(&mut truth_cache))?;
            rows.push(SweepClipRow {
                clip: name.clone(),
                ssim: ev.report.metrics.ssim,
                edge_f1: ev.report.metrics.edge_f1,
                flicker: ev.report.metrics.flicker_switches_per_cell_sec,
            });
            report.clips.push(ev.report);
        }
        let report_name = format!("combo-{id:02}.json");
        std::fs::write(args.out_dir.join(&report_name), report.to_json())
            .map_err(|e| format!("write combo report: {e}"))?;

        let ssim_mean = mean(rows.iter().map(|r| r.ssim));
        let edge_f1_mean = mean(rows.iter().map(|r| r.edge_f1));
        let flicker_mean = mean(rows.iter().map(|r| r.flicker));
        let score = score_of(&spec.score, ssim_mean, edge_f1_mean, flicker_mean);
        eprintln!(
            "sweep: combo {id} score {score:.4} (ssim {} | edge F1 {} | flicker {})",
            fmt(ssim_mean),
            fmt(edge_f1_mean),
            fmt(flicker_mean)
        );
        results.push(SweepResult {
            id,
            combo: label,
            score: Some(score),
            skip_reason: None,
            ssim_mean,
            edge_f1_mean,
            flicker_mean,
            clips: rows,
            report: Some(report_name),
        });
    }

    rank(&mut results);

    let sweep_report = SweepReport {
        schema_version: SWEEP_SCHEMA_VERSION,
        generator: format!("sleepy-factory {}", env!("CARGO_PKG_VERSION")),
        score_weights: spec.score,
        results,
    };
    let json = serde_json::to_string_pretty(&sweep_report).expect("sweep report serializes");
    std::fs::write(args.out_dir.join("sweep.json"), json)
        .map_err(|e| format!("write sweep.json: {e}"))?;
    let html = render_leaderboard(&sweep_report);
    std::fs::write(args.out_dir.join("leaderboard.html"), html)
        .map_err(|e| format!("write leaderboard.html: {e}"))?;
    eprintln!(
        "sweep: wrote {}/sweep.json + leaderboard.html",
        args.out_dir.display()
    );

    for (rank, r) in sweep_report.results.iter().take(5).enumerate() {
        match r.score {
            Some(s) => eprintln!("sweep: #{} combo {} [{}] score {s:.4}", rank + 1, r.id, r.combo),
            None => eprintln!("sweep: --  combo {} [{}] skipped", r.id, r.combo),
        }
    }
    Ok(())
}

fn fmt(v: Option<f64>) -> String {
    v.map_or("n/a".into(), |v| format!("{v:.4}"))
}

/// Rank: scored combos best-first (ties by id for determinism), skipped
/// combos at the tail by id.
fn rank(results: &mut [SweepResult]) {
    results.sort_by(|a, b| match (a.score, b.score) {
        (Some(x), Some(y)) => y.total_cmp(&x).then(a.id.cmp(&b.id)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.id.cmp(&b.id),
    });
}

/// Compact leaderboard (PLAN §6 "the human loop" applied to sweeps) —
/// self-contained, no external requests, same dark styling as the contact
/// sheet.
fn render_leaderboard(report: &SweepReport) -> String {
    let mut h = String::with_capacity(1 << 14);
    h.push_str(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>auto-ascii sweep leaderboard</title>\n<style>\n\
         body{font-family:system-ui,sans-serif;margin:2rem auto;max-width:1100px;\
         background:#14151a;color:#d8dae2;line-height:1.45}\n\
         h1{font-size:1.4rem}\n\
         table{border-collapse:collapse;margin:.6rem 0;font-size:.85rem;\
         font-variant-numeric:tabular-nums}\n\
         th,td{border:1px solid #33363f;padding:.25rem .6rem;text-align:right}\n\
         th{background:#1d1f26;text-align:center}\n\
         td.combo{text-align:left;font-family:ui-monospace,monospace;font-size:.8rem}\n\
         tr.best td{background:#15321b}\n\
         tr.skip td{color:#9aa0ae}\n\
         .meta{color:#9aa0ae;font-size:.85rem}\n\
         </style>\n</head>\n<body>\n<h1>auto-ascii sweep leaderboard</h1>\n",
    );
    let w = &report.score_weights;
    h.push_str(&format!(
        "<p class=\"meta\">{} | schema v{} | score = {}·ssim + {}·edgeF1 − {}·(flicker/{})</p>\n",
        crate::eval::html_escape(&report.generator),
        report.schema_version,
        w.ssim, w.edge_f1, w.flicker, w.flicker_norm
    ));
    h.push_str(
        "<table><tr><th>rank</th><th>id</th><th>combo</th><th>score</th>\
         <th>ssim</th><th>edge F1</th><th>flicker</th><th>per-clip (ssim / F1 / flicker)</th></tr>\n",
    );
    for (rank, r) in report.results.iter().enumerate() {
        let cls = if r.skip_reason.is_some() {
            " class=\"skip\""
        } else if rank == 0 {
            " class=\"best\""
        } else {
            ""
        };
        let per_clip = if let Some(reason) = &r.skip_reason {
            format!("skipped: {}", crate::eval::html_escape(reason))
        } else {
            r.clips
                .iter()
                .map(|c| {
                    format!(
                        "{}: {} / {} / {}",
                        crate::eval::html_escape(&c.clip),
                        fmt(c.ssim),
                        fmt(c.edge_f1),
                        fmt(c.flicker)
                    )
                })
                .collect::<Vec<_>>()
                .join("<br>")
        };
        h.push_str(&format!(
            "<tr{cls}><td>{}</td><td>{}</td><td class=\"combo\">{}</td><td>{}</td>\
             <td>{}</td><td>{}</td><td>{}</td><td class=\"combo\">{per_clip}</td></tr>\n",
            if r.skip_reason.is_some() { "—".into() } else { (rank + 1).to_string() },
            r.id,
            crate::eval::html_escape(&r.combo),
            r.score.map_or("—".into(), |s| format!("{s:.4}")),
            fmt(r.ssim_mean),
            fmt(r.edge_f1_mean),
            fmt(r.flicker_mean),
        ));
    }
    h.push_str("</table>\n</body>\n</html>\n");
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(text: &str) -> SweepSpec {
        toml::from_str(text).expect("spec parses")
    }

    #[test]
    fn score_weights_default_and_formula() {
        let w = ScoreWeights::default();
        assert_eq!((w.ssim, w.edge_f1, w.flicker, w.flicker_norm), (0.4, 0.4, 0.2, 2.0));
        // 0.4·0.5 + 0.4·0.75 − 0.2·(3.0/2.0) = 0.2 + 0.3 − 0.3 = 0.2.
        let s = score_of(&w, Some(0.5), Some(0.75), Some(3.0));
        assert!((s - 0.2).abs() < 1e-12, "{s}");
        // Missing metrics contribute zero to their term.
        assert_eq!(score_of(&w, None, None, None), 0.0);
    }

    #[test]
    fn axes_cross_and_values_travel_together() {
        let s = spec(
            "[[axes]]\nname = \"edge\"\nvalues = [\n\
             { \"edges.t_hi\" = 20, \"edges.t_lo\" = 8 },\n\
             { \"edges.t_hi\" = 28, \"edges.t_lo\" = 12 },\n]\n\
             [[axes]]\nname = \"runtime\"\nvalues = [\n\
             { \"compose.edge_t_on\" = 24 },\n\
             { \"compose.edge_t_on\" = 32 },\n\
             { \"compose.edge_t_on\" = 40 },\n]\n",
        );
        let combos = enumerate_combos(&s).unwrap();
        assert_eq!(combos.len(), 6, "2 x 3 cross");
        // Later axes vary fastest; pairs travel together.
        assert_eq!(
            combos[0].label(),
            "edges.t_hi=20 edges.t_lo=8 compose.edge_t_on=24"
        );
        assert_eq!(
            combos[5].label(),
            "edges.t_hi=28 edges.t_lo=12 compose.edge_t_on=40"
        );
    }

    #[test]
    fn conflicting_axes_are_a_hard_error() {
        let s = spec(
            "[[axes]]\nname = \"a\"\nvalues = [ { \"edges.t_hi\" = 20 } ]\n\
             [[axes]]\nname = \"b\"\nvalues = [ { \"edges.t_hi\" = 28 } ]\n",
        );
        let err = enumerate_combos(&s).unwrap_err().to_string();
        assert!(err.contains("two axes"), "{err}");
    }

    #[test]
    fn apply_combo_sets_nested_values_and_keeps_base() {
        let base = Params::default();
        let combo = Combo {
            overrides: vec![
                ("edges.t_hi".into(), toml::Value::Integer(40)),
                ("edges.t_lo".into(), toml::Value::Integer(18)),
                ("compose.edge_t_on".into(), toml::Value::Integer(24)),
                (
                    "temporal.ema_alpha_y_milli".into(),
                    toml::Value::Integer(500),
                ),
            ],
        };
        let p = apply_combo(&base, &combo).unwrap().unwrap();
        assert_eq!(p.edges.t_hi, 40);
        assert_eq!(p.edges.t_lo, 18);
        assert_eq!(p.compose.edge_t_on, 24);
        assert_eq!(p.temporal.ema_alpha_y_milli, 500);
        // Untouched keys keep base values.
        assert_eq!(p.edges.scharr_shift, base.edges.scharr_shift);
        assert_eq!(p.build, base.build);
    }

    #[test]
    fn typo_paths_are_hard_errors() {
        let base = Params::default();
        for path in ["edges.t_high", "egdes.t_hi", "edges", "compose.edge_t_on.x", ""] {
            let combo = Combo {
                overrides: vec![(path.to_string(), toml::Value::Integer(1))],
            };
            assert!(
                apply_combo(&base, &combo).is_err(),
                "path {path:?} must be rejected"
            );
        }
    }

    #[test]
    fn invalid_merged_params_become_a_skip_not_an_error() {
        let base = Params::default();
        // t_lo > t_hi — a legal outcome of crossing axes.
        let combo = Combo {
            overrides: vec![
                ("edges.t_hi".into(), toml::Value::Integer(10)),
                ("edges.t_lo".into(), toml::Value::Integer(20)),
            ],
        };
        let skipped = apply_combo(&base, &combo).unwrap().unwrap_err();
        assert!(skipped.contains("t_lo <= t_hi"), "{skipped}");
    }

    #[test]
    fn ranking_is_deterministic_and_skips_sink() {
        let mut report = SweepReport {
            schema_version: SWEEP_SCHEMA_VERSION,
            generator: "test".into(),
            score_weights: ScoreWeights::default(),
            results: vec![
                SweepResult {
                    id: 0,
                    combo: "a".into(),
                    score: Some(0.5),
                    skip_reason: None,
                    ssim_mean: None,
                    edge_f1_mean: None,
                    flicker_mean: None,
                    clips: vec![],
                    report: None,
                },
                SweepResult {
                    id: 1,
                    combo: "b".into(),
                    score: None,
                    skip_reason: Some("nope".into()),
                    ssim_mean: None,
                    edge_f1_mean: None,
                    flicker_mean: None,
                    clips: vec![],
                    report: None,
                },
                SweepResult {
                    id: 2,
                    combo: "c".into(),
                    score: Some(0.7),
                    skip_reason: None,
                    ssim_mean: Some(0.9),
                    edge_f1_mean: Some(0.8),
                    flicker_mean: Some(1.0),
                    clips: vec![SweepClipRow {
                        clip: "x".into(),
                        ssim: Some(0.9),
                        edge_f1: Some(0.8),
                        flicker: Some(1.0),
                    }],
                    report: Some("combo-02.json".into()),
                },
            ],
        };
        rank(&mut report.results);
        let ids: Vec<u32> = report.results.iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![2, 0, 1]);

        let html = render_leaderboard(&report);
        assert!(html.contains("<title>auto-ascii sweep leaderboard</title>"));
        assert!(html.contains("class=\"best\""));
        assert!(html.contains("skipped: nope"));
        assert!(
            !html.contains("http://") && !html.contains("https://"),
            "leaderboard must be self-contained"
        );
        // JSON roundtrip (agent half of the socket).
        let json = serde_json::to_string(&report).unwrap();
        let back: SweepReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back.results.len(), 3);
        assert_eq!(back.results[0].id, 2);
    }
}
