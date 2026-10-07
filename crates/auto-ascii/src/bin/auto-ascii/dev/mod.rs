//! `auto-ascii dev`: the tools for developing auto-ascii itself. Each command
//! is a thin adapter over the factory library, printing human text or, with
//! --json, exactly one JSON value on stdout.

pub mod eval;
pub mod font_table;
pub mod inspect;
pub mod reel;
pub mod sweep;

use std::path::Path;

use auto_ascii::Stopped;
use auto_ascii::tools::Tool;
use auto_ascii_factory::{Programs, effective_params};

use crate::args::DevCmd;
use crate::play::{self, Request};
use crate::{BoxErr, Cli};

pub fn run(cli: &Cli, cmd: &DevCmd) -> Result<Stopped, BoxErr> {
    match cmd {
        DevCmd::Inspect { asset, dump_planes, frame } => {
            let report = inspect::collect(asset, dump_planes.as_deref(), frame)?;
            if cli.json {
                outln!("{}", serde_json::to_string(&report.terminal_safe())?);
            } else {
                inspect::print(&report);
            }
        }
        DevCmd::Params { params, dump: _ } => {
            let p = effective_params(params.as_deref(), None, None)?;
            if cli.json {
                outln!("{}", serde_json::to_string(&p)?);
            } else {
                out!("{}", p.dump());
            }
        }
        DevCmd::Eval { corpus, params, baseline, out, html, reel, cache_dir, font_table } => {
            let params = effective_params(params.as_deref(), None, None)?;
            let programs = programs(cli)?;
            eval::run(&eval::EvalArgs {
                corpus: corpus.clone(),
                params,
                baseline: baseline.clone(),
                out: out.clone(),
                html: html.clone(),
                reel: reel.clone(),
                cache_dir: cache_dir.clone(),
                truecolor_only: false,
                font_table: font_table.clone(),
                programs,
            })?;
            echo_json(cli, out)?;
        }
        DevCmd::Sweep { corpus, params, grid, out, cache_dir } => {
            let base = effective_params(params.as_deref(), None, None)?;
            let programs = programs(cli)?;
            sweep::run(&sweep::SweepArgs {
                corpus: corpus.clone(),
                base,
                grid: grid.clone(),
                out_dir: out.clone(),
                cache_dir: cache_dir.clone(),
                programs,
            })?;
            echo_json(cli, &out.join("sweep.json"))?;
        }
        DevCmd::FontTable { font, output, name, conservative } => {
            font_table::run(&font_table::FontTableArgs {
                font: font.clone(),
                output: output.clone(),
                name: name.clone(),
                conservative: *conservative,
            })?;
            if cli.json {
                let obj = serde_json::json!({
                    "path": crate::library::absolute(output),
                    "name": table_name(output)?,
                });
                outln!("{obj}");
            }
        }
        DevCmd::Sim(a) => {
            return play::play(&a.target, &Request::sim(&a.playback, &a.terminal, &a.sim), cli.json);
        }
        DevCmd::BenchSeek(a) => {
            return play::play(
                &a.target,
                &Request::bench(&a.playback, &a.terminal, &a.bench),
                cli.json,
            );
        }
    }
    Ok(Stopped::Ended)
}

fn programs(cli: &Cli) -> Result<Programs, BoxErr> {
    let (_, found) = crate::ensure_tools(cli, &[Tool::Ffmpeg, Tool::Ffprobe])?;
    Ok(Programs { ffmpeg: found.path(Tool::Ffmpeg), ffprobe: found.path(Tool::Ffprobe) })
}

fn echo_json(cli: &Cli, report: &Path) -> Result<(), BoxErr> {
    if !cli.json {
        return Ok(());
    }
    let text = std::fs::read_to_string(report)
        .map_err(|e| format!("read {}: {e}", report.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not JSON: {e}", report.display()))?;
    outln!("{value}");
    Ok(())
}

fn table_name(output: &Path) -> Result<String, BoxErr> {
    let text = std::fs::read_to_string(output)
        .map_err(|e| format!("read {}: {e}", output.display()))?;
    let table = auto_ascii_core::FontTable::parse(&text)
        .map_err(|e| format!("{}: {e}", output.display()))?;
    Ok(table.name().to_string())
}
