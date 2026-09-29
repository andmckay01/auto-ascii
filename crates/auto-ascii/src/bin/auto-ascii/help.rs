//! `--help-all`: the same clap command graph with every hidden flag shown,
//! rendered for the command the arguments name. At the root it appends every
//! command's page. Needs no target, home folder or media tools.

use std::ffi::OsString;

use clap::{Command, CommandFactory};

use crate::Cli;

pub const FLAG: &str = "--help-all";

pub fn expanded(argv: &[OsString]) -> Option<String> {
    let args = &argv[argv.len().min(1)..];
    let wanted = args.iter().take_while(|a| *a != "--").any(|a| a == FLAG);
    if !wanted {
        return None;
    }
    let mut root = reveal(Cli::command());
    root.build();
    let path = command_path(&root, args);
    let mut cmd = &root;
    for name in &path {
        cmd = cmd.find_subcommand(name).expect("command_path only returns known commands");
    }
    let mut text = page(cmd);
    if path.is_empty() {
        append_descendants(&root, &mut text);
    }
    Some(text)
}

pub fn reveal(cmd: Command) -> Command {
    cmd.mut_args(|a| a.hide(false)).mut_subcommands(reveal)
}

fn page(cmd: &Command) -> String {
    cmd.clone().render_help().to_string()
}

fn append_descendants(cmd: &Command, text: &mut String) {
    for sub in cmd.get_subcommands().filter(|s| s.get_name() != "help") {
        let name = sub.get_bin_name().unwrap_or(sub.get_name());
        text.push_str(&format!("\n== {name} ==\n\n"));
        text.push_str(&page(sub));
        append_descendants(sub, text);
    }
}

fn command_path(root: &Command, args: &[OsString]) -> Vec<String> {
    let mut path = Vec::new();
    let mut cmd = root;
    let mut tokens = args.iter().map(|a| a.to_string_lossy());
    while let Some(token) = tokens.next() {
        if token == "--" {
            break;
        }
        if let Some(long) = token.strip_prefix("--") {
            let takes_value = !long.contains('=')
                && cmd
                    .get_arguments()
                    .find(|a| a.get_long() == Some(long))
                    .is_some_and(|a| a.get_action().takes_values());
            if takes_value {
                tokens.next();
            }
            continue;
        }
        if let Some(shorts) = token.strip_prefix('-').filter(|s| !s.is_empty()) {
            let last = shorts.chars().last();
            let takes_value = shorts.chars().count() == 1
                && cmd
                    .get_arguments()
                    .find(|a| a.get_short() == last)
                    .is_some_and(|a| a.get_action().takes_values());
            if takes_value {
                tokens.next();
            }
            continue;
        }
        match cmd.find_subcommand(token.as_ref()) {
            Some(sub) => {
                path.push(sub.get_name().to_string());
                cmd = sub;
            }
            None => break,
        }
    }
    path
}
