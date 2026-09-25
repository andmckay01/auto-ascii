#![cfg(unix)]

mod common;

use common::{
    field, probe_done_line, spawn_harness_with, wait_child_success, wait_until_contains,
    winsize_cells_only, winsize_with_cell_px, write_master,
};

struct Identity {
    name: &'static str,
    env: &'static [(&'static str, Option<&'static str>)],
    ws: libc::winsize,
    replies: &'static [u8],
    want_color: &'static str,
    want_sync: bool,
    want_cell_px: &'static str,
    want_support: &'static str,
    want_glyphs: u8,
}

const UTF8_LOCALE: &[(&str, Option<&str>)] =
    &[("LC_ALL", None), ("LC_CTYPE", None), ("LANG", Some("en_US.UTF-8")), ("TMUX", None)];

macro_rules! fixture_env {
    ($($k:literal => $v:expr),* $(,)?) => {
        &[("LC_ALL", None), ("LC_CTYPE", None), ("LANG", Some("en_US.UTF-8")), ("TMUX", None),
          $(($k, $v)),*]
    };
}

const KITTY: Identity = Identity {
    name: "kitty",
    env: fixture_env!["TERM" => Some("xterm-kitty"), "COLORTERM" => Some("truecolor"),
              "TERM_PROGRAM" => None],
    ws: winsize_with_cell_px(80, 24, 8, 16),
    replies: b"\x1bP>|kitty(0.42.2)\x1b\\\
\x1b[?2026;2$y\
\x1bP0+r524742\x1b\\\
\x1b[6;20;10t\
\x1b[?62;52;c",
    want_color: "True",
    want_sync: true,
    want_cell_px: "10x20",
    want_support: "UnicodeCore",
    want_glyphs: 1 | 2 | 4,
};

const ALACRITTY: Identity = Identity {
    name: "alacritty",
    env: fixture_env!["TERM" => Some("alacritty"), "COLORTERM" => Some("truecolor"),
              "TERM_PROGRAM" => None],
    ws: winsize_with_cell_px(80, 24, 10, 20),
    replies: b"\x1b[?2026;2$y\x1b[?6c",
    want_color: "True",
    want_sync: true,
    want_cell_px: "10x20",
    want_support: "UnicodeCore",
    want_glyphs: 1 | 2 | 4,
};

const WEZTERM: Identity = Identity {
    name: "wezterm",
    env: fixture_env!["TERM" => Some("wezterm"), "COLORTERM" => Some("truecolor"),
              "TERM_PROGRAM" => Some("WezTerm")],
    ws: winsize_cells_only(80, 24),
    replies: b"\x1bP>|WezTerm 20240203-110809-5046fc22\x1b\\\
\x1b[?2026;2$y\
\x1bP1+r524742=382F382F38\x1b\\\
\x1b[6;22;11t\
\x1b[?65;4;6;18;22;52c",
    want_color: "True",
    want_sync: true,
    want_cell_px: "11x22",
    want_support: "UnicodeCore",
    want_glyphs: 1 | 2 | 4,
};

const GNOME_VTE: Identity = Identity {
    name: "gnome-terminal (VTE)",
    env: fixture_env!["TERM" => Some("xterm-256color"), "COLORTERM" => Some("truecolor"),
              "TERM_PROGRAM" => None],
    ws: winsize_with_cell_px(80, 24, 9, 19),
    replies: b"\x1b[?2026;4$y\x1b[?61;1;4;21;22;28c",
    want_color: "True",
    want_sync: false,
    want_cell_px: "9x19",
    want_support: "UnicodeCore",
    want_glyphs: 1 | 2 | 4,
};

const XTERM: Identity = Identity {
    name: "xterm",
    env: fixture_env!["TERM" => Some("xterm-256color"), "COLORTERM" => None,
              "TERM_PROGRAM" => None],
    ws: winsize_with_cell_px(80, 24, 8, 16),
    replies: b"\x1bP>|XTerm(390)\x1b\\\
\x1b[?2026;0$y\
\x1bP1+r524742=2D31\x1b\\\
\x1b[6;13;6t\
\x1b[?63;1;2;4;6;9;15;16;17;18;21;22;28;29c",
    want_color: "C256",
    want_sync: false,
    want_cell_px: "6x13",
    want_support: "UnicodeCore",
    want_glyphs: 1 | 2 | 4,
};

const XTERM_DIRECT: Identity = Identity {
    name: "xterm (direct color)",
    env: fixture_env!["TERM" => Some("xterm-direct"), "COLORTERM" => None,
              "TERM_PROGRAM" => None],
    ws: winsize_with_cell_px(80, 24, 8, 16),
    replies: b"\x1bP>|XTerm(390)\x1b\\\
\x1b[?2026;0$y\
\x1bP1+r524742=38\x1b\\\
\x1b[6;16;8t\
\x1b[?63;1;2;4;6;9;15;16;17;18;21;22;28;29c",
    want_color: "True",
    want_sync: false,
    want_cell_px: "8x16",
    want_support: "UnicodeCore",
    want_glyphs: 1 | 2 | 4,
};

const XTERM_DIRECT_256: Identity = Identity {
    name: "xterm (direct color, TERM=xterm-256color)",
    env: fixture_env!["TERM" => Some("xterm-256color"), "COLORTERM" => None,
              "TERM_PROGRAM" => None],
    ws: winsize_with_cell_px(80, 24, 8, 16),
    replies: b"\x1bP>|XTerm(390)\x1b\\\
\x1b[?2026;0$y\
\x1bP1+r524742=38\x1b\\\
\x1b[6;16;8t\
\x1b[?63;1;2;4;6;9;15;16;17;18;21;22;28;29c",
    want_color: "True",
    want_sync: false,
    want_cell_px: "8x16",
    want_support: "UnicodeCore",
    want_glyphs: 1 | 2 | 4,
};

const LINUX_CONSOLE: Identity = Identity {
    name: "Linux console",
    env: fixture_env!["TERM" => Some("linux"), "COLORTERM" => None, "TERM_PROGRAM" => None],
    ws: winsize_cells_only(80, 24),
    replies: b"\x1b[?6c",
    want_color: "C16",
    want_sync: false,
    want_cell_px: "none",
    want_support: "Cp437",
    want_glyphs: 1 | 2,
};

const KITTY_STRIPPED: Identity = Identity {
    name: "kitty (COLORTERM stripped)",
    env: fixture_env!["TERM" => Some("xterm-kitty"), "COLORTERM" => None,
              "TERM_PROGRAM" => None],
    ws: winsize_with_cell_px(80, 24, 8, 16),
    replies: b"\x1bP>|kitty(0.42.2)\x1b\\\
\x1b[?2026;2$y\
\x1bP0+r524742\x1b\\\
\x1b[6;20;10t\
\x1b[?62;52;c",
    want_color: "True",
    want_sync: true,
    want_cell_px: "10x20",
    want_support: "UnicodeCore",
    want_glyphs: 1 | 2 | 4,
};

const XTERM_COLORTERM_LIE: Identity = Identity {
    name: "xterm (COLORTERM=truecolor lie)",
    env: fixture_env!["TERM" => Some("xterm-256color"), "COLORTERM" => Some("truecolor"),
              "TERM_PROGRAM" => None],
    ws: winsize_with_cell_px(80, 24, 8, 16),
    replies: b"\x1bP>|XTerm(390)\x1b\\\
\x1b[?2026;0$y\
\x1bP1+r524742=2D31\x1b\\\
\x1b[6;13;6t\
\x1b[?63;1;2;4;6;9;15;16;17;18;21;22;28;29c",
    want_color: "C256",
    want_sync: false,
    want_cell_px: "6x13",
    want_support: "UnicodeCore",
    want_glyphs: 1 | 2 | 4,
};

fn assert_identity(id: &Identity) {
    assert_identity_mode(id, "probe-reply");
}

fn assert_identity_mode(id: &Identity, mode: &str) {
    let (pty, mut child) = spawn_harness_with(mode, id.ws, id.env);
    let mut out = Vec::new();
    wait_until_contains(pty.master, &mut out, b"\x1b[c");
    write_master(pty.master, id.replies);
    wait_until_contains(pty.master, &mut out, b"PROBE-DONE");
    wait_child_success(&mut child);

    let line = probe_done_line(&out);
    let ctx = format!("{}: {line}", id.name);
    assert_eq!(field(&line, "color"), id.want_color, "color tier — {ctx}");
    assert_eq!(field(&line, "sync"), id.want_sync.to_string(), "sync 2026 — {ctx}");
    assert_eq!(field(&line, "cellpx"), id.want_cell_px, "cell px — {ctx}");
    assert_eq!(field(&line, "support"), id.want_support, "glyph support tier — {ctx}");
    assert_eq!(field(&line, "glyphs"), id.want_glyphs.to_string(), "glyph flags — {ctx}");
    assert_eq!(field(&line, "can_query"), "true", "{ctx}");
    assert_eq!(field(&line, "stray"), "0", "reply bytes leaked — {ctx}");
}

#[test]
fn kitty_identity() {
    assert_identity(&KITTY);
}

#[test]
fn alacritty_identity() {
    assert_identity(&ALACRITTY);
}

#[test]
fn wezterm_identity() {
    assert_identity(&WEZTERM);
}

#[test]
fn gnome_terminal_vte_identity() {
    assert_identity(&GNOME_VTE);
}

#[test]
fn xterm_identity() {
    assert_identity(&XTERM);
}

#[test]
fn xterm_direct_color_identity() {
    assert_identity(&XTERM_DIRECT);
}

#[test]
fn xterm_direct_color_behind_256color_term_identity() {
    assert_identity(&XTERM_DIRECT_256);
}

#[test]
fn linux_console_identity() {
    assert_identity(&LINUX_CONSOLE);
}

#[test]
fn kitty_stripped_colorterm_quirk_and_escape_hatch() {
    assert_identity(&KITTY_STRIPPED);
    let raw = Identity { want_color: "C256", ..KITTY_STRIPPED };
    assert_identity_mode(&raw, "probe-reply-noquirks");
}

#[test]
fn xterm_colorterm_lie_quirk_and_escape_hatch() {
    assert_identity(&XTERM_COLORTERM_LIE);
    let raw = Identity { want_color: "True", ..XTERM_COLORTERM_LIE };
    assert_identity_mode(&raw, "probe-reply-noquirks");
}

fn run_cached_probe(
    id: &Identity,
    mode: &str,
    cache_dir: &std::path::Path,
    answer_volley: bool,
) -> (String, Vec<u8>) {
    let dir = cache_dir.to_str().expect("utf8 cache dir");
    let mut env: Vec<(&str, Option<&str>)> = id.env.to_vec();
    env.push(("ASCII_HARNESS_CACHE_DIR", Some(dir)));
    let (pty, mut child) = spawn_harness_with(mode, id.ws, &env);
    let mut out = Vec::new();
    if answer_volley {
        wait_until_contains(pty.master, &mut out, b"\x1b[c");
        write_master(pty.master, id.replies);
    }
    wait_until_contains(pty.master, &mut out, b"PROBE-DONE");
    wait_child_success(&mut child);
    let line = probe_done_line(&out);
    (field(&line, "color").to_owned(), out)
}

fn temp_cache_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ascii-idcache-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn xterm_colorterm_lie_quirk_survives_the_cache() {
    let dir = temp_cache_dir("xterm-lie");

    let (cold, _) = run_cached_probe(&XTERM_COLORTERM_LIE, "probe-cached", &dir, true);
    assert_eq!(cold, "C256", "cold run: quirk clamps the COLORTERM lie");

    let (warm, out) = run_cached_probe(&XTERM_COLORTERM_LIE, "probe-cached", &dir, false);
    assert!(
        common::find(&out, b"\x1b[>0q").is_none(),
        "warm run must be a cache hit (no volley written)"
    );
    assert_eq!(warm, "C256", "the quirk clamp must survive the cache hit");

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn no_quirks_bypasses_the_cached_quirk_result() {
    let dir = temp_cache_dir("noquirks");

    let (cold, _) = run_cached_probe(&XTERM_COLORTERM_LIE, "probe-cached", &dir, true);
    assert_eq!(cold, "C256", "seed the cache with the quirk-clamped entry");

    let (raw, _) = run_cached_probe(&XTERM_COLORTERM_LIE, "probe-cached-noquirks", &dir, true);
    assert_eq!(raw, "True", "--no-quirks: replies at face value, cache ignored");

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn identity_matrix_is_diverse() {
    let all = [
        KITTY,
        ALACRITTY,
        WEZTERM,
        GNOME_VTE,
        XTERM,
        XTERM_DIRECT,
        XTERM_DIRECT_256,
        LINUX_CONSOLE,
        KITTY_STRIPPED,
        XTERM_COLORTERM_LIE,
    ];
    assert_eq!(all.len(), 10);
    assert!(all.iter().any(|i| i.want_color == "C256"), "a 256-color terminal");
    assert!(all.iter().any(|i| i.want_color == "C16"), "a 16-color terminal");
    assert!(all.iter().any(|i| i.want_sync), "a terminal with synchronized output");
    assert!(all.iter().any(|i| !i.want_sync), "a terminal without it");
    assert!(all.iter().any(|i| i.want_cell_px == "none"), "a terminal with no cell size");
    assert!(all.iter().any(|i| i.want_support == "Cp437"), "the CP437 floor");
    for id in &all {
        assert!(
            UTF8_LOCALE.iter().all(|kv| id.env.contains(kv)),
            "{}: fixture env must pin the locale",
            id.name
        );
    }
}
