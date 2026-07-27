//! M4 item D — per-terminal **pty identity fixtures**.
//!
//! GUI terminals cannot run on this box (headless), so "verified on kitty /
//! alacritty / wezterm / gnome-terminal / xterm" is established the only
//! honest way available: each terminal's *identity* — the environment it
//! exports, the `TIOCGWINSZ` it sets, and the exact bytes it answers our
//! volley with — is replayed through the real [`slpy_term::probe_caps`] on a
//! real pty, and the [`slpy_term::Caps`] it must conclude are asserted.
//! Nothing is stubbed: same binary, same parser, same termios dance. What is
//! canned is only what the terminal would have typed back.
//!
//! Every reply stream below is derived from that terminal's source or its
//! reference documentation, cited inline. Where a terminal answers *nothing*
//! (alacritty has no XTVERSION handler, VTE has no XTGETTCAP handler), the
//! absence is part of the fixture — silence is a capability signal too.
//!
//! Companion doc for the owner's real-terminal pass: `docs/TERMINAL-CHECKLIST.md`.
//! Parser-level (byte → `ProbeReplies`) coverage: `tests/probe_parser.rs`.

#![cfg(unix)]

mod common;

use common::{
    field, probe_done_line, spawn_harness_with, wait_child_success, wait_until_contains,
    winsize_cells_only, winsize_with_cell_px, write_master,
};

/// One terminal's identity: what it exports, what the kernel knows about its
/// window, what it answers — and the caps we must conclude from all three.
struct Identity {
    name: &'static str,
    /// Child environment (`None` = unset: an unexported COLORTERM is as much
    /// a fixture as an exported one). Locale is pinned explicitly so the
    /// glyph tier never depends on the machine running the tests.
    env: &'static [(&'static str, Option<&'static str>)],
    ws: libc::winsize,
    /// The canned reply stream, written in one go once the volley lands.
    replies: &'static [u8],
    want_color: &'static str,
    want_sync: bool,
    /// `"WxH"` or `"none"` — the probe's `cell_px`.
    want_cell_px: &'static str,
    want_support: &'static str,
    /// `GlyphFlags` bits: 1 = ASCII, 2 = BLOCKS, 4 = BOX_DRAWING, 8 = BRAILLE.
    want_glyphs: u8,
}

/// A UTF-8 desktop session's locale, pinned per fixture (the probe reads
/// LC_ALL > LC_CTYPE > LANG; leaving them to the test runner's environment
/// would make the glyph tier machine-dependent).
const UTF8_LOCALE: &[(&str, Option<&str>)] =
    &[("LC_ALL", None), ("LC_CTYPE", None), ("LANG", Some("en_US.UTF-8")), ("TMUX", None)];

/// Build a fixture env: the UTF-8 locale base + this terminal's own exports.
macro_rules! fixture_env {
    ($($k:literal => $v:expr),* $(,)?) => {
        &[("LC_ALL", None), ("LC_CTYPE", None), ("LANG", Some("en_US.UTF-8")), ("TMUX", None),
          $(($k, $v)),*]
    };
}

// ---------------------------------------------------------------------------
// The five local terminals of PLAN §7 M4 + two floors (xterm-direct, console)
// + the M5 RGB-parse failing guard (xterm-direct behind TERM=xterm-256color)
// ---------------------------------------------------------------------------

/// **kitty** — sets `TERM=xterm-kitty` and `COLORTERM=truecolor`.
///
/// * XTVERSION: `DCS >| kitty(<version>) ST` (kitty/screen.c
///   `screen_xtversion`: `">|kitty(" XT_VERSION ")"`).
/// * DECRQM 2026: kitty's `report_mode_status` answers 1/2 for its known
///   modes, 2026 being `PENDING_UPDATE` (kitty/screen.c) → settable ⇒ sync.
/// * XTGETTCAP `RGB`: kitty answers **`0+r`** — its terminfo tables have no
///   `RGB` boolean (kitty/terminfo.py: `bool_capabilities` carries `Tc`, not
///   `RGB`; unknown names fall through to `result(name)` = `0+r<name>`).
///   Truecolor for kitty therefore comes from `COLORTERM`, not the query —
///   the fixture exists to keep that path honest.
/// * `CSI 16 t`: supported, `CSI 6 ; height ; width t` (kitty/screen.c
///   `screen_report_size` case 16).
/// * DA1: `CSI ? 62 ; 52 ; c` — VT220 plus 52 (clipboard) with kitty's
///   default `clipboard_control` (kitty/window.py `da1()`).
const KITTY: Identity = Identity {
    name: "kitty",
    env: fixture_env!["TERM" => Some("xterm-kitty"), "COLORTERM" => Some("truecolor"),
              "TERM_PROGRAM" => None],
    // kitty reports pixel geometry to the kernel too; the CSI 16 t answer
    // must WIN over it (10×20 here vs 8×16 from the winsize).
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

/// **alacritty** — `TERM=alacritty`, `COLORTERM=truecolor`.
///
/// The quiet one: it answers only two of our five queries.
/// * XTVERSION: **no reply** — alacritty has no XTVERSION handler (vtdn.dev
///   XTVERSION support table; alacritty's `identify_terminal` implements DA1
///   and DA2 only, alacritty_terminal/src/term/mod.rs).
/// * DECRQM 2026: **supported**, answered `2` (reset) —
///   `report_private_mode`: `NamedPrivateMode::SyncUpdate => ModeState::Reset`
///   (alacritty_terminal/src/term/mod.rs); DECRQM/DECRPM landed in 0.13.0
///   (CHANGELOG: "Support for `DECRQM`/`DECRPM` escape sequences",
///   "Synchronized updates now use `CSI 2026`"). The M4 brief's "no 2026"
///   guess is stale — current alacritty *does* support it.
/// * XTGETTCAP: **no reply** (no `DCS + q` handler).
/// * `CSI 16 t`: **no reply** — alacritty implements 14 t (text area px) and
///   18 t (chars) only (CHANGELOG 0.6.0; `text_area_size_pixels` /
///   `text_area_size_chars`). Cell size therefore comes from `TIOCGWINSZ`.
/// * DA1: `CSI ? 6 c` (VT102), `identify_terminal`.
const ALACRITTY: Identity = Identity {
    name: "alacritty",
    env: fixture_env!["TERM" => Some("alacritty"), "COLORTERM" => Some("truecolor"),
              "TERM_PROGRAM" => None],
    ws: winsize_with_cell_px(80, 24, 10, 20),
    replies: b"\x1b[?2026;2$y\x1b[?6c",
    want_color: "True",
    want_sync: true,
    // No CSI 16 t answer → the winsize pixel fields are the only source.
    want_cell_px: "10x20",
    want_support: "UnicodeCore",
    want_glyphs: 1 | 2 | 4,
};

/// **wezterm** — `TERM=wezterm`, `COLORTERM=truecolor`, `TERM_PROGRAM=WezTerm`.
///
/// The chatty one: every query answered.
/// * XTVERSION: `DCS >| WezTerm <build> ST` (vtdn.dev sample:
///   `WezTerm 20240203-110809-5046fc22`).
/// * DECRQM 2026: `decqrm_response(mode, /*supported=*/true, /*enabled=*/false)`
///   ⇒ `CSI ? 2026 ; 2 $ y` (wezterm term/src/terminalstate/mod.rs,
///   `QueryDecPrivateMode(SynchronizedOutput)`).
/// * XTGETTCAP `RGB`: answered `1+r524742=` hex("8/8/8") — the only one of
///   the five that upgrades the tier through the *query* path
///   (`xt_get_tcap`: `"RGB" => … hex::encode_upper("8/8/8")`).
/// * `CSI 16 t`: `Window::ReportCellSizePixels` → `CSI 6 ; h ; w t`.
/// * DA1: `CSI ? 65 ; 4 ; 6 ; 18 ; 22 ; 52 c` (`RequestPrimaryDeviceAttributes`).
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

/// **gnome-terminal / VTE** — `TERM=xterm-256color`, `COLORTERM=truecolor`.
///
/// The passive-path terminal, and the one that must NOT get `?2026`:
/// * XTVERSION: **no reply** (no handler in vte/src/vteseq.cc; vtdn.dev
///   notes it is "mentioned in terminfo but no handler found in code").
/// * XTGETTCAP: **no reply** — VTE implements no `DCS + q`. Truecolor is
///   therefore known only from `COLORTERM`, which VTE exports.
/// * DECRQM 2026: answered **4 = permanently reset**. VTE knows the mode
///   (vte/src/modes.py `mode_WHAT('CONTOUR_BATCHED_RENDERING', 2026,
///   default=False)`) but not as a writable mode, so the generator emits
///   `MODE_FIXED(…, ALWAYS_RESET)` and `Terminal::DECRQM_DEC` maps
///   `eALWAYS_RESET → 4` (vte/src/vteseq.cc). Per the synchronized-output
///   spec 4 means "not supported" — `sync_2026` must stay **false**.
/// * `CSI 16 t`: **no reply** — VTE's `XTERM_WM` handles 14/18/19 t, not 16.
///   Cell size comes from `TIOCGWINSZ`, which VTE fills in
///   (vte/src/pty.cc `Pty::set_size`: `ws_xpixel = ws_col * cell_width_px`).
/// * DA1: `CSI ? 61 ; 1 ; 4 ; 21 ; 22 ; 28 c` (vte/src/vteseq.cc
///   `Terminal::DA1`, level 61 outside its test mode, 4 = sixel when built
///   with sixel support).
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

/// **xterm** (`TERM=xterm-256color`, no `COLORTERM`) — the 256-color floor,
/// and the reason the RGB reply is parsed by *value*.
///
/// * XTVERSION: `DCS >| XTerm(<patch>) ST` (xterm ctlseqs, `CSI > 0 q`;
///   vtdn.dev sample `XTerm(370)`).
/// * DECRQM 2026: `0` — xterm has no mode 2026, and DECRPM's 0 is "not
///   recognized" (ctlseqs).
/// * XTGETTCAP `RGB`: xterm *does* know the name (ctlseqs: "RGB for the
///   ncurses direct-color extension") and answers the **valid** form
///   `DCS 1 + r 524742 = <hex> ST` — with the value `-1` when it is not in
///   direct-color mode (xterm misc.c: `if (TScreenOf(xw)->direct_color &&
///   xw->has_rgb) { … } else unparseputs(xw, "-1")`). A prefix-only check
///   would promote every plain xterm to truecolor; we pin C256, which is
///   also what xterm renders — SGR 38;2 is approximated into its palette
///   unless direct color is on.
/// * `CSI 16 t`: supported — "Report xterm character cell size in pixels.
///   Result is `CSI 6 ; height ; width t`" (ctlseqs).
/// * DA1: `CSI ? 63 ; … c` for a VT420-level `decTerminalID` (the digits
///   vary with the resource; the probe only uses DA1 as the sentinel).
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

/// **xterm -direct2** (`TERM=xterm-direct`) — the same terminal with direct
/// color enabled: now the `RGB` reply carries a real width (hex("8")) and
/// the *query* path is what upgrades the tier, with no `COLORTERM` in sight.
/// The pair (this vs [`XTERM`]) is the regression that keeps the RGB value
/// parse from collapsing back into a prefix test. NOTE (M5 fix 4): this
/// fixture's `want_color: True` is NOT a guard on the parse itself —
/// `TERM=xterm-direct` already proves truecolor passively; the parse's
/// failing guard is [`XTERM_DIRECT_256`] below.
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

/// **xterm -direct2 behind `TERM=xterm-256color`** — the *failing guard* for
/// the XTGETTCAP-RGB-by-value parse (M5 fix 4).
///
/// [`XTERM_DIRECT`] alone cannot catch a parse regression: its
/// `TERM=xterm-direct` already proves truecolor through the passive path
/// (`apply_passive`: `term.contains("direct")`), so its `want_color: True`
/// stays green even if the query parse silently dies. This fixture removes
/// that safety net: `TERM` is pinned to plain `xterm-256color` — the ncurses
/// user-level workaround xterm's own FAQ documents for direct-color xterm,
/// since many hosts lack the `xterm-direct` terminfo entry and programs
/// misbehave under it (invisible.island.net/ncurses/ncurses.faq: "why not
/// just set TERM to xterm-256color?"), and the value `ssh` carries to remote
/// hosts — no `COLORTERM`, and the same direct-color reply stream
/// `DCS 1 + r 524742 = 38 ST` (hex("8"): 8-bit channels, xterm/misc.c
/// `xtermGetTcap`). Passive evidence says C256; ONLY the by-value XTGETTCAP
/// parse can conclude True. If that parse regresses (dropped, or collapsed
/// into a prefix test that then mis-handles the value), this fixture fails.
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

/// **Linux console** (`TERM=linux`) — the legibility floor (PLAN §7 M4:
/// "`TERM=linux` legible with palette 8").
///
/// It answers DA1 and nothing else: the kernel VT emulates a VT102 and
/// replies `ESC [ ? 6 c` (console_codes(4), "ESC [ c … Answer: ESC [ ? 6 c
/// 'I am a VT102'"); it has no XTVERSION, no XTGETTCAP, no DECRQM 2026, no
/// `CSI 16 t`, and no pixel fields in its winsize. Result: the 16-color +
/// CP437 tier from passive hints, cell aspect falling back to 2.0.
///
/// The matching render golden lives in
/// `crates/sleepytime/tests/linux_console_golden.rs`.
const LINUX_CONSOLE: Identity = Identity {
    name: "Linux console",
    env: fixture_env!["TERM" => Some("linux"), "COLORTERM" => None, "TERM_PROGRAM" => None],
    ws: winsize_cells_only(80, 24),
    replies: b"\x1b[?6c",
    want_color: "C16",
    want_sync: false,
    want_cell_px: "none",
    want_support: "Cp437",
    // CP437 has the block/shade glyphs but no box-drawing diagonals.
    want_glyphs: 1 | 2,
};

/// **kitty with COLORTERM stripped** — the M5 quirk-table fixture (item C).
///
/// Same terminal, same replies as [`KITTY`], but launched through something
/// that scrubs `COLORTERM` (sudo, `env -i`, env-whitelisting session
/// managers). Passive evidence now says C256 and the query path cannot help:
/// kitty's XTGETTCAP tables carry `Tc` but no `RGB`, so our `+q524742` query
/// gets the invalid `0+r…` form back (kitty/terminfo.py). kitty itself is
/// unconditionally truecolor (its docs and its own COLORTERM export) — the
/// identity-keyed quirk `kitty-rgbless-xtgettcap` (src/quirks.rs) must
/// restore True from the XTVERSION reply alone.
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

/// **plain xterm behind a `.bashrc` `export COLORTERM=truecolor`** — the M5
/// quirk-table fixture, downgrade direction (item C).
///
/// The terminal is [`XTERM`] verbatim — and it *answered* the RGB query with
/// the valid form carrying "-1", "no direct color" (xterm/misc.c
/// `xtermGetTcap`). The globally exported COLORTERM would promote the
/// passive tier to True, but xterm approximates every SGR 38;2 into its
/// 256-color palette; the `xterm-no-direct-color` quirk (src/quirks.rs)
/// clamps the tier back to what the queried terminal can actually display.
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

/// Replay one identity through the real probe on a real pty.
fn assert_identity(id: &Identity) {
    assert_identity_mode(id, "probe-reply");
}

/// Same, with an explicit harness probe mode (`probe-reply-noquirks` = the
/// `--no-quirks` escape hatch — M5 item C).
fn assert_identity_mode(id: &Identity, mode: &str) {
    let (pty, mut child) = spawn_harness_with(mode, id.ws, id.env);
    let mut out = Vec::new();
    // The volley is ONE write ending in the DA1 query — wait for its tail,
    // then answer exactly as this terminal would.
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
    // Whatever the terminal said, none of it may be left for the app's
    // input stream (digits are seek bindings).
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

/// M5 fix 4: truecolor here is derivable ONLY from the XTGETTCAP reply
/// value (no COLORTERM, no "direct" in TERM) — the failing guard for the
/// by-value RGB parse. See [`XTERM_DIRECT_256`].
#[test]
fn xterm_direct_color_behind_256color_term_identity() {
    assert_identity(&XTERM_DIRECT_256);
}

#[test]
fn linux_console_identity() {
    assert_identity(&LINUX_CONSOLE);
}

/// M5 item C, upgrade direction: the quirk table (keyed on the queried
/// XTVERSION identity, applied post-probe) restores truecolor for a
/// COLORTERM-stripped kitty — and `--no-quirks` reproduces the raw
/// pre-quirk conclusion (C256).
#[test]
fn kitty_stripped_colorterm_quirk_and_escape_hatch() {
    assert_identity(&KITTY_STRIPPED);
    let raw = Identity { want_color: "C256", ..KITTY_STRIPPED };
    assert_identity_mode(&raw, "probe-reply-noquirks");
}

/// M5 item C, downgrade direction: the queried xterm denied direct color, so
/// the passive COLORTERM=truecolor claim is clamped to C256 — and
/// `--no-quirks` reproduces the raw (uncorrected) True.
#[test]
fn xterm_colorterm_lie_quirk_and_escape_hatch() {
    assert_identity(&XTERM_COLORTERM_LIE);
    let raw = Identity { want_color: "True", ..XTERM_COLORTERM_LIE };
    assert_identity_mode(&raw, "probe-reply-noquirks");
}

/// Spawn the harness for `id` in a cache-enabled probe mode with the probe
/// cache rooted at `cache_dir`, optionally answer the volley, and return the
/// concluded color tier plus the raw pty output. `answer_volley: false`
/// asserts the warm-cache path — PROBE-DONE must arrive with NO volley
/// written (a cache hit skips it).
fn run_cached_probe(
    id: &Identity,
    mode: &str,
    cache_dir: &std::path::Path,
    answer_volley: bool,
) -> (String, Vec<u8>) {
    let dir = cache_dir.to_str().expect("utf8 cache dir");
    let mut env: Vec<(&str, Option<&str>)> = id.env.to_vec();
    env.push(("SLPY_HARNESS_CACHE_DIR", Some(dir)));
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
    let dir = std::env::temp_dir().join(format!("slpy-idcache-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// M5 review high 1: the downgrade-direction quirk must survive the cache.
/// Cold run: volley answered, `xterm-no-direct-color` clamps the COLORTERM
/// lie to C256, entry stored. Warm run (same key, cache hit, NO volley):
/// the clamp must still hold — before the fix, the upgrade-only merge let
/// the passive COLORTERM=truecolor evidence win back True on every run
/// after the first.
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

/// M5 review medium 2: `--no-quirks` must bypass the cache in BOTH
/// directions. With a quirk-clamped entry already cached, a no-quirks run
/// must NOT serve it — it re-volleys and takes the replies at face value
/// (True under the COLORTERM lie). Before the fix the cache hit skipped the
/// volley and handed back the quirk-derived result the flag promises to
/// disable.
#[test]
fn no_quirks_bypasses_the_cached_quirk_result() {
    let dir = temp_cache_dir("noquirks");

    let (cold, _) = run_cached_probe(&XTERM_COLORTERM_LIE, "probe-cached", &dir, true);
    assert_eq!(cold, "C256", "seed the cache with the quirk-clamped entry");

    // answer_volley: true — a cache hit would hang here (no volley to
    // answer); the wait itself asserts the bypass.
    let (raw, _) = run_cached_probe(&XTERM_COLORTERM_LIE, "probe-cached-noquirks", &dir, true);
    assert_eq!(raw, "True", "--no-quirks: replies at face value, cache ignored");

    let _ = std::fs::remove_dir_all(dir);
}

/// The fixture set is not allowed to quietly collapse: the M4 matrix is five
/// named terminals plus the two floors, and they must not all agree (a probe
/// that returns one answer for everything would pass seven identical
/// assertions otherwise).
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
    // Locale is pinned in every fixture (glyph tier must not depend on the
    // machine running the tests).
    for id in &all {
        assert!(
            UTF8_LOCALE.iter().all(|kv| id.env.contains(kv)),
            "{}: fixture env must pin the locale",
            id.name
        );
    }
}
