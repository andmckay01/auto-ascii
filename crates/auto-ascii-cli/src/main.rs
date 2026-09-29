//! The `auto-ascii` binary: parse the command line, dispatch it, and map the
//! outcome to an exit status: 0 done, 3 the viewer quit, 1 an error, 2 a
//! usage error (1 with --json, as a JSON error object).

#[macro_use]
mod output;
mod args;
mod commands;
mod composition;
mod deps;
mod dev;
mod help;
mod home;
mod import;
mod library;
mod play;
mod stream;

use std::ffi::OsString;
use std::process::ExitCode;

use auto_ascii::Stopped;
use auto_ascii::tools::Tool;
use clap::Parser;
use clap::error::ErrorKind;

pub use args::Cli;
use args::{Cmd, ComposeCmd};
use commands::{AddArgs, CutArgs, ExportArgs};
use home::Home;
use output::fail;
use play::Request;

pub type BoxErr = Box<dyn std::error::Error>;

const QUIT_STATUS: u8 = 3;

fn main() -> ExitCode {
    let argv: Vec<OsString> = std::env::args_os().collect();
    if let Some(text) = help::expanded(&argv) {
        output::emit(&text);
        return ExitCode::SUCCESS;
    }
    let json = argv.iter().any(|arg| arg == "--json");
    let cli = match Cli::try_parse_from(&argv) {
        Ok(cli) => cli,
        Err(e) => return usage_exit(&e, json),
    };
    let result = run(&cli);
    if let Err(e) = &result {
        fail(&e.to_string(), cli.json);
    }
    ExitCode::from(status(&result))
}

fn status(result: &Result<Stopped, BoxErr>) -> u8 {
    match result {
        Ok(Stopped::Ended) => 0,
        Ok(Stopped::Quit) => QUIT_STATUS,
        Err(_) => 1,
    }
}

fn usage_exit(e: &clap::Error, json: bool) -> ExitCode {
    if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
        let _ = e.print();
        return ExitCode::SUCCESS;
    }
    if !json {
        let _ = e.print();
        return ExitCode::from(2);
    }
    let rendered = e.render().to_string();
    let message = rendered.trim();
    fail(message.strip_prefix("error: ").unwrap_or(message), true);
    ExitCode::FAILURE
}

fn run(cli: &Cli) -> Result<Stopped, BoxErr> {
    let done = |result: Result<(), BoxErr>| result.map(|()| Stopped::Ended);
    match &cli.cmd {
        Cmd::Play(a) => play::play(&a.target, &Request::of(&a.opts), cli.json),
        Cmd::Stream(a) => done(commands::stream(cli, a)),
        Cmd::Import(a) => done(import::run(cli, a)),
        Cmd::List => done(commands::list(cli, &Home::resolve()?)),
        Cmd::Info { clip } => done(commands::info(cli, &Home::resolve()?, clip)),
        Cmd::Cut { clip, in_spec, out_spec, name, force } => {
            let args = CutArgs { clip, in_spec, out_spec, name: name.as_deref(), force: *force };
            done(commands::cut(cli, &Home::resolve()?, &args))
        }
        Cmd::Compose { cmd } => compose(cli, cmd),
        Cmd::Home => done(commands::home(cli, &Home::resolve()?)),
        Cmd::AgentGuide => done(commands::agent_guide(cli)),
        Cmd::Doctor { fetch } => done(commands::doctor(cli, *fetch)),
        Cmd::Dev { cmd } => dev::run(cli, cmd),
    }
}

fn compose(cli: &Cli, cmd: &ComposeCmd) -> Result<Stopped, BoxErr> {
    let done = |result: Result<(), BoxErr>| result.map(|()| Stopped::Ended);
    match cmd {
        ComposeCmd::New { name } => done(commands::compose_new(cli, &Home::resolve()?, name)),
        ComposeCmd::Add { name, clip, in_spec, out_spec, at_spec } => {
            let args = AddArgs {
                name,
                clip,
                in_spec: in_spec.as_deref(),
                out_spec: out_spec.as_deref(),
                at_spec: at_spec.as_deref(),
            };
            done(commands::compose_add(cli, &Home::resolve()?, &args))
        }
        ComposeCmd::Show { name } => done(commands::compose_show(cli, &Home::resolve()?, name)),
        ComposeCmd::Play { name, opts } => {
            play::play_composition(name, &Request::of(opts), cli.json)
        }
        ComposeCmd::Export { name, out, force } => {
            let args = ExportArgs { name, out: out.as_deref(), force: *force };
            done(commands::compose_export(cli, &Home::resolve()?, &args))
        }
    }
}

pub fn ensure_tools(cli: &Cli, tools: &[Tool]) -> Result<(deps::Ctx, deps::Resolved), BoxErr> {
    let ctx = deps::Ctx::new(cli.yes, cli.json);
    let found = deps::ensure(&ctx, tools, &mut ctx.live())?;
    Ok((ctx, found))
}

#[cfg(test)]
mod tests;
