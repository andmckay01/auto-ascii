use std::ffi::OsString;

use auto_ascii::Style;
use auto_ascii_term::ColorTier;
use clap::CommandFactory;
use clap::error::ErrorKind;

use super::*;
use crate::args::{DevCmd, PaletteArg, RepaintArg, SimSpec, parse_sim_spec, parse_size, parse_style};

const ROOT_HELP: &str = "\
Play, stream, and make ASCII-art video.

Usage: auto-ascii [OPTIONS] <COMMAND>

Commands:
  play         Play a file, library clip, or composition
  stream       Stream a video link or search with sound
  add          Add a YouTube link or a video file to the library, ready to play
  import       Turn a video into a .ascii asset
  list         List library clips
  info         Show clip details
  cut          Save part of a clip
  compose      Create, edit, play, or export a composition
  home         Show and create the library folders
  agent-guide  Print the guide for agents
  doctor       Check or fetch media tools
  dev          Advanced inspection, evaluation, and benchmarks
  help         Print this message or the help of the given subcommand(s)

Options:
      --json      Use JSON output and errors
  -y, --yes       Download missing media tools without asking
      --help-all  Show all commands and advanced options
  -h, --help      Print help
  -V, --version   Print version

Start with: auto-ascii add <link-or-file>, then auto-ascii play <name>
More options: auto-ascii play --help-all
";

const HIDDEN_PLAY_FLAGS: [&str; 16] = [
    "--tier",
    "--no-query",
    "--no-cache",
    "--no-quirks",
    "--no-backdrop",
    "--font-table",
    "--repaint",
    "--fps-cap",
    "--cell-aspect",
    "--duration-secs",
    "--sim ",
    "--sim-tier",
    "--sim-dump",
    "--sim-resize",
    "--sim-audio",
    "--bench-seek",
];

const VISIBLE_PLAY_FLAGS: [&str; 6] =
    ["--loop", "--seek", "--style", "--palette", "--mute", "--no-audio"];

fn parse(args: &[&str]) -> Cli {
    let argv = std::iter::once("auto-ascii").chain(args.iter().copied());
    Cli::try_parse_from(argv).unwrap_or_else(|e| panic!("{args:?}: {e}"))
}

fn reject(args: &[&str]) -> ErrorKind {
    let argv = std::iter::once("auto-ascii").chain(args.iter().copied());
    match Cli::try_parse_from(argv) {
        Ok(_) => panic!("{args:?} must be rejected"),
        Err(e) => e.kind(),
    }
}

fn help_of(args: &[&str]) -> String {
    let argv = std::iter::once("auto-ascii").chain(args.iter().copied());
    match Cli::try_parse_from(argv) {
        Err(e) if e.kind() == ErrorKind::DisplayHelp => e.render().to_string(),
        _ => panic!("{args:?} is not a help request"),
    }
}

fn help_all(args: &[&str]) -> String {
    let argv: Vec<OsString> =
        std::iter::once("auto-ascii").chain(args.iter().copied()).map(OsString::from).collect();
    help::expanded(&argv).unwrap_or_else(|| panic!("{args:?} is not a --help-all request"))
}

fn play_args(cli: Cli) -> args::PlayArgs {
    match cli.cmd {
        Cmd::Play(a) => a,
        _ => panic!("not play"),
    }
}

fn dev(cli: Cli) -> DevCmd {
    match cli.cmd {
        Cmd::Dev { cmd } => cmd,
        _ => panic!("not dev"),
    }
}

#[test]
fn the_command_graphs_are_consistent() {
    Cli::command().debug_assert();
    help::reveal(Cli::command()).debug_assert();
}

#[test]
fn root_help_is_pinned() {
    assert_eq!(help_of(&["--help"]), ROOT_HELP);
    assert_eq!(help_of(&["-h"]), ROOT_HELP);
}

#[test]
fn play_takes_every_visible_flag() {
    let a = play_args(parse(&[
        "play", "clip", "--loop", "--seek", "1:30", "--style", "letters", "--palette", "braille",
        "--mute",
    ]));
    assert_eq!(a.target, "clip");
    let pb = &a.opts.playback;
    assert!(pb.loop_playback && pb.mute && !pb.no_audio);
    assert_eq!(pb.seek.as_deref(), Some("1:30"));
    assert_eq!(pb.style, Some(Style::Letters));
    assert_eq!(pb.palette, PaletteArg::Braille);
    assert!(play_args(parse(&["play", "clip", "--no-audio"])).opts.playback.no_audio);

    let a = play_args(parse(&["play", "clip"]));
    assert_eq!(a.opts.playback.style, None, "no --style keeps the saved style");
    assert_eq!(a.opts.playback.palette, PaletteArg::Auto);
    assert_eq!(a.opts.terminal.repaint, RepaintArg::Full);
    assert!(a.opts.sim.sim.is_none() && a.opts.bench.bench_seek.is_none());
}

#[test]
fn play_accepts_the_hidden_flags() {
    let a = play_args(parse(&[
        "play", "clip", "--tier", "256", "--no-query", "--no-cache", "--no-quirks",
        "--no-backdrop", "--font-table", "dejavu-sans-mono", "--repaint", "diff", "--fps-cap",
        "12", "--cell-aspect", "2.1", "--duration-secs", "3", "--sim", "80x24:30", "--sim-tier",
        "mono", "--sim-dump", "/tmp/x", "--sim-resize", "--sim-audio",
    ]));
    let t = &a.opts.terminal;
    assert_eq!(t.tier, Some(ColorTier::C256));
    assert!(t.no_query && t.no_cache && t.no_quirks && t.no_backdrop);
    assert_eq!(t.font_table.as_deref(), Some("dejavu-sans-mono"));
    assert_eq!(t.repaint, RepaintArg::Diff);
    assert_eq!((t.fps_cap, t.cell_aspect, t.duration_secs), (Some(12.0), Some(2.1), Some(3.0)));
    let s = &a.opts.sim;
    assert_eq!(s.sim, Some(SimSpec { cols: 80, rows: 24, frames: 30 }));
    assert_eq!(s.sim_tier, Some(ColorTier::Mono));
    assert_eq!(s.sim_resize, Some((100, 40)), "a bare --sim-resize means 100x40");
    assert!(s.sim_audio);

    let a = play_args(parse(&["play", "clip", "--sim", "80x24:3", "--sim-resize", "60x20"]));
    assert_eq!(a.opts.sim.sim_resize, Some((60, 20)));
    let a = play_args(parse(&["play", "clip", "--bench-seek", "50"]));
    assert_eq!(a.opts.bench.bench_seek, Some(50));
}

#[test]
fn play_flag_constraints_hold() {
    assert_eq!(reject(&["play", "c", "--mute", "--no-audio"]), ErrorKind::ArgumentConflict);
    assert_eq!(
        reject(&["play", "c", "--sim", "80x24:3", "--bench-seek", "5"]),
        ErrorKind::ArgumentConflict
    );
    for flag in [&["--sim-audio"][..], &["--sim-tier", "16"], &["--sim-dump", "x"], &["--sim-resize"]]
    {
        let args = [&["play", "c"][..], flag].concat();
        assert_eq!(reject(&args), ErrorKind::MissingRequiredArgument, "{flag:?} needs --sim");
    }
    assert_eq!(reject(&["play", "c", "--bench-seek", "0"]), ErrorKind::ValueValidation);
    assert_eq!(reject(&["play", "c", "--sim", "80x24"]), ErrorKind::ValueValidation);
    assert_eq!(reject(&["play", "c", "--sim", "80x24:0"]), ErrorKind::ValueValidation);
    assert_eq!(reject(&["play", "c", "--style", "runes"]), ErrorKind::ValueValidation);
    assert_eq!(reject(&["play", "c", "--tier", "8bit"]), ErrorKind::ValueValidation);
    assert_eq!(reject(&["play"]), ErrorKind::MissingRequiredArgument);
}

#[test]
fn the_old_codec_flag_is_gone_everywhere() {
    let codec = concat!("--", "codec");
    for args in [
        &["play", "c", codec, "ascii"][..],
        &["compose", "play", "c", codec, "ascii"],
        &["stream", codec, "ascii", "zoo"],
        &["dev", "sim", "c", "--sim", "8x8:1", codec, "ascii"],
        &["dev", "bench-seek", "c", "--bench-seek", "1", codec, "ascii"],
    ] {
        assert_eq!(reject(args), ErrorKind::UnknownArgument, "{args:?}");
    }
}

#[test]
fn compose_play_shares_the_play_flags() {
    let cli = parse(&["compose", "play", "demo", "--loop", "--style", "pixels", "--sim", "8x8:2"]);
    let Cmd::Compose { cmd: ComposeCmd::Play { name, opts } } = cli.cmd else { panic!() };
    assert_eq!(name, "demo");
    assert!(opts.playback.loop_playback);
    assert_eq!(opts.playback.style, Some(Style::Pixels));
    assert_eq!(opts.sim.sim, Some(SimSpec { cols: 8, rows: 8, frames: 2 }));
}

#[test]
fn stream_keeps_its_flags_and_defaults() {
    let cli = parse(&["stream", "zoo", "keeper", "--style", "letters", "--max-height", "720"]);
    let Cmd::Stream(s) = cli.cmd else { panic!() };
    assert_eq!(s.input, ["zoo", "keeper"]);
    assert_eq!(s.style, Style::Letters);
    assert_eq!(s.max_height, 720);
    let cli = parse(&["stream", "zoo", "--sim", "120x40:5", "--sim-dump", "d", "--no-audio"]);
    let Cmd::Stream(s) = cli.cmd else { panic!() };
    assert_eq!((s.style, s.palette, s.sim.as_deref()), (Style::Ascii, PaletteArg::Auto, Some("120x40:5")));
    assert!(s.no_audio);
    assert_eq!(reject(&["stream", "zoo", "--max-height", "100"]), ErrorKind::ValueValidation);
    assert_eq!(reject(&["stream"]), ErrorKind::MissingRequiredArgument);
    let e = Cli::try_parse_from(["auto-ascii", "stream", "--style", "runes", "zoo"]).err().unwrap();
    assert!(e.to_string().contains("unknown style \"runes\""), "{e}");
}

#[test]
fn import_covers_the_library_and_the_path_modes() {
    let cli = parse(&[
        "import", "in.mp4", "--name", "Clip A", "--ss", "1:00", "--t", "5", "--fps", "24",
        "--res", "320x180", "--params", "p.toml", "--force",
    ]);
    let Cmd::Import(a) = cli.cmd else { panic!() };
    assert_eq!(a.name.as_deref(), Some("Clip A"));
    assert!(a.output.is_none() && a.force);
    assert_eq!((a.build.ss.as_deref(), a.build.t.as_deref()), (Some("1:00"), Some("5")));
    assert_eq!((a.build.fps, a.build.res.as_deref()), (Some(24), Some("320x180")));
    assert_eq!(a.build.params.as_deref(), Some(std::path::Path::new("p.toml")));

    for flag in ["-o", "--output"] {
        let Cmd::Import(a) = parse(&["import", "in.mp4", flag, "out.ascii"]).cmd else { panic!() };
        assert_eq!(a.output.as_deref(), Some(std::path::Path::new("out.ascii")));
    }
    assert_eq!(reject(&["import", "in.mp4", "--name", "a", "-o", "b"]), ErrorKind::ArgumentConflict);
    assert_eq!(reject(&["import", "in.mp4", "--fps", "0"]), ErrorKind::ValueValidation);
    assert_eq!(reject(&["import", "in.mp4", "--fps", "1001"]), ErrorKind::ValueValidation);
}

#[test]
fn the_everyday_commands_parse() {
    assert!(matches!(parse(&["list"]).cmd, Cmd::List));
    assert!(matches!(parse(&["info", "c"]).cmd, Cmd::Info { .. }));
    assert!(matches!(parse(&["home"]).cmd, Cmd::Home));
    assert!(matches!(parse(&["agent-guide"]).cmd, Cmd::AgentGuide));
    assert!(matches!(parse(&["doctor", "--fetch"]).cmd, Cmd::Doctor { fetch: true }));
    let Cmd::Cut { in_spec, out_spec, force, .. } =
        parse(&["cut", "c", "--in", "0:01", "--out", "0:02", "--force"]).cmd
    else {
        panic!()
    };
    assert_eq!((in_spec.as_str(), out_spec.as_str(), force), ("0:01", "0:02", true));
    assert_eq!(reject(&["cut", "c", "--in", "0"]), ErrorKind::MissingRequiredArgument);
    for args in [
        &["compose", "new", "d"][..],
        &["compose", "add", "d", "c", "--in", "1", "--out", "2", "--at", "3"],
        &["compose", "show", "d"],
        &["compose", "export", "d", "-o", "x.ascii", "--force"],
    ] {
        parse(args);
    }
}

#[test]
fn global_flags_go_before_or_after_the_command() {
    for args in [&["--json", "-y", "list"][..], &["list", "--json", "--yes"], &["-y", "list", "--json"]] {
        let cli = parse(args);
        assert!(cli.json && cli.yes, "{args:?}");
    }
    let cli = parse(&["dev", "params", "--dump", "--json"]);
    assert!(cli.json);
}

#[test]
fn the_dev_commands_parse() {
    let DevCmd::Inspect { frame, dump_planes, .. } =
        dev(parse(&["dev", "inspect", "a.ascii", "--frame", "1", "--frame", "9", "--dump-planes", "d"]))
    else {
        panic!()
    };
    assert_eq!(frame, [1, 9]);
    assert!(dump_planes.is_some());
    assert!(matches!(dev(parse(&["dev", "params", "--dump"])), DevCmd::Params { dump: true, .. }));
    assert_eq!(reject(&["dev", "params"]), ErrorKind::MissingRequiredArgument);
    let DevCmd::Eval { cache_dir, .. } = dev(parse(&["dev", "eval", "--corpus", "c", "--out", "o.json"]))
    else {
        panic!()
    };
    assert_eq!(cache_dir, std::path::Path::new("runs/cache"));
    assert_eq!(reject(&["dev", "eval", "--corpus", "c"]), ErrorKind::MissingRequiredArgument);
    assert!(matches!(
        dev(parse(&["dev", "sweep", "--corpus", "c", "--grid", "g.toml", "--out", "o"])),
        DevCmd::Sweep { .. }
    ));
    assert!(matches!(
        dev(parse(&["dev", "font-table", "--conservative", "-o", "t.toml"])),
        DevCmd::FontTable { conservative: true, .. }
    ));
    let DevCmd::Sim(s) = dev(parse(&["dev", "sim", "c", "--sim", "8x8:2", "--sim-audio", "--tier", "16"]))
    else {
        panic!()
    };
    assert_eq!(s.sim.sim, Some(SimSpec { cols: 8, rows: 8, frames: 2 }));
    assert_eq!(s.terminal.tier, Some(ColorTier::C16));
    assert_eq!(reject(&["dev", "sim", "c"]), ErrorKind::MissingRequiredArgument);
    assert_eq!(reject(&["dev", "sim", "c", "--sim", "8x8:1", "--bench-seek", "3"]), ErrorKind::UnknownArgument);
    let DevCmd::BenchSeek(b) = dev(parse(&["dev", "bench-seek", "c", "--bench-seek", "7"])) else { panic!() };
    assert_eq!(b.bench.bench_seek, Some(7));
    assert_eq!(reject(&["dev", "bench-seek", "c"]), ErrorKind::MissingRequiredArgument);
}

#[test]
fn play_help_hides_the_advanced_flags_and_help_all_shows_them() {
    for flag in ["--help", "-h"] {
        let help = help_of(&["play", flag]);
        for visible in VISIBLE_PLAY_FLAGS {
            assert!(help.contains(visible), "{visible} missing from play {flag}:\n{help}");
        }
        for hidden in HIDDEN_PLAY_FLAGS {
            assert!(!help.contains(hidden), "{hidden} leaked into play {flag}:\n{help}");
        }
        assert!(help.contains("3 when the viewer quits"), "{help}");
    }
    for args in [&["play", "--help-all"][..], &["play", "some-clip", "--help-all"], &["--help-all", "play"]] {
        let help = help_all(args);
        assert!(help.starts_with("Play a file"), "{args:?}:\n{help}");
        for flag in VISIBLE_PLAY_FLAGS.iter().chain(&HIDDEN_PLAY_FLAGS) {
            assert!(help.contains(flag), "{flag} missing from {args:?}:\n{help}");
        }
    }
    let help = help_all(&["compose", "play", "--help-all"]);
    assert!(help.starts_with("Play a composition") && help.contains("--sim-resize"), "{help}");
    let help = help_all(&["stream", "--help-all"]);
    assert!(help.contains("--sim-dump"), "{help}");
    assert!(!help_of(&["stream", "--help"]).contains("--sim"));
}

#[test]
fn help_all_walks_past_option_values() {
    let help = help_all(&["--json", "import", "--fps", "play", "--help-all"]);
    assert!(help.starts_with("Turn a video"), "{help}");
    let help = help_all(&["dev", "sim", "-y", "--help-all"]);
    assert!(help.starts_with("Headless playback"), "{help}");
    assert!(help::expanded(&["auto-ascii".into(), "play".into(), "--".into(), "--help-all".into()]).is_none());
    assert!(help::expanded(&["auto-ascii".into(), "list".into()]).is_none());
}

#[test]
fn root_help_all_lists_every_command_page() {
    let help = help_all(&["--help-all"]);
    assert!(help.starts_with("Play, stream, and make ASCII-art video."), "{help}");
    for page in [
        "== auto-ascii play ==",
        "== auto-ascii compose play ==",
        "== auto-ascii dev inspect ==",
        "== auto-ascii dev bench-seek ==",
        "== auto-ascii doctor ==",
    ] {
        assert!(help.contains(page), "{page} missing");
    }
    for flag in HIDDEN_PLAY_FLAGS {
        assert!(help.contains(flag), "{flag} missing from --help-all");
    }
}

#[test]
fn dev_pages_show_their_advanced_flags() {
    let help = help_of(&["dev", "sim", "--help"]);
    for flag in HIDDEN_PLAY_FLAGS.iter().filter(|f| **f != "--bench-seek") {
        assert!(help.contains(flag), "{flag} missing from dev sim --help:\n{help}");
    }
    assert!(help_of(&["dev", "bench-seek", "--help"]).contains("--bench-seek <N>"));
    assert!(help_of(&["--help"]).contains("dev          Advanced"));
}

#[test]
fn no_help_page_names_the_removed_binaries() {
    let mut pages = vec![help_all(&["--help-all"]), help_of(&["--help"])];
    let mut stack = vec![Cli::command()];
    while let Some(cmd) = stack.pop() {
        stack.extend(cmd.get_subcommands().cloned());
        pages.push(cmd.clone().render_long_help().to_string());
    }
    for page in pages {
        let removed = [concat!("auto-ascii-", "player"), concat!("auto-ascii-", "factory"), concat!("--", "codec")];
        for old in removed.into_iter().chain([concat!("ASCI", " asset")]) {
            assert!(!page.contains(old), "{old} in help:\n{page}");
        }
    }
}

#[test]
fn exit_statuses_map_the_way_play_with_sound_expects() {
    assert_eq!(status(&Ok(Stopped::Ended)), 0);
    assert_eq!(status(&Ok(Stopped::Quit)), 3);
    assert_eq!(status(&Err("boom".into())), 1);
}

#[test]
fn style_flag_parsing() {
    assert_eq!(parse_style("pixels"), Ok(Style::Pixels));
    assert_eq!(parse_style("letters"), Ok(Style::Letters));
    assert_eq!(parse_style("ascii"), Ok(Style::Ascii));
    let e = parse_style("ASCII").unwrap_err();
    assert!(e.contains("pixels, letters, ascii"), "the error lists the registry: {e}");
}

#[test]
fn size_and_spec_parsing() {
    assert_eq!(parse_size("213x58").unwrap(), (213, 58));
    assert_eq!(parse_size("320X90").unwrap(), (320, 90));
    assert!(parse_size("0x9").is_err());
    assert!(parse_size("213").is_err());
    assert_eq!(parse_sim_spec("213x58:900").unwrap(), SimSpec { cols: 213, rows: 58, frames: 900 });
    assert!(parse_sim_spec("213x58").is_err());
    assert!(parse_sim_spec("213x58:0").is_err());
}
