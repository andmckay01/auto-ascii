//! M1 acceptance 4: the caps probe never hangs and never leaks bytes.
//! Runs the `auto-ascii-term-harness` probe modes under an openpty pair (same
//! harness pattern as `tests/pty_restore.rs`):
//!
//! - `probe-silent`: the test side never answers the volley → the probe must
//!   time out to conservative defaults in < 300 ms with zero stray bytes
//!   left on stdin.
//! - `probe-reply`: the test side scripts kitty-style replies → detected
//!   caps upgrade (truecolor, sync 2026, cell px) and still zero strays.
//!
//! The pty plumbing lives in `common/mod.rs` (shared with the M4
//! per-terminal identity fixtures, `tests/terminal_identity.rs`).

#![cfg(unix)]

mod common;

use std::time::Duration;

use common::{
    field, find, probe_done_line, session_events, spawn_harness, wait_child_success,
    wait_until_contains, write_master,
};

/// Silent terminal: probe must time out to the conservative default in
/// < 300 ms, with the volley written in order (DA1 last) and no stray bytes
/// left unconsumed on stdin.
#[test]
fn probe_with_no_replies_times_out_to_defaults() {
    let (pty, mut child) = spawn_harness("probe-silent");
    let mut out = Vec::new();
    wait_until_contains(pty.master, &mut out, b"PROBE-DONE");
    wait_child_success(&mut child);

    // The volley reached the terminal, one write, DA1 sentinel last.
    let volley_start = find(&out, b"\x1b[>0q").expect("XTVERSION query missing");
    let da1_at = find(&out, b"\x1b[c").expect("DA1 sentinel missing");
    assert!(volley_start < da1_at);
    assert!(find(&out, b"\x1b[?2026$p").is_some(), "DECRQM 2026 query missing");
    assert!(find(&out, b"\x1bP+q524742\x1b\\").is_some(), "XTGETTCAP RGB query missing");
    assert!(find(&out, b"\x1b[16t").is_some(), "CSI 16 t query missing");

    let line = probe_done_line(&out);
    let ms: u64 = field(&line, "ms").parse().unwrap();
    assert!(ms < 300, "probe must time out in < 300 ms, took {ms} ms: {line}");
    assert_eq!(field(&line, "color"), "C256", "conservative default tier: {line}");
    assert_eq!(field(&line, "sync"), "false");
    assert_eq!(field(&line, "can_query"), "true");
    assert_eq!(field(&line, "stray"), "0", "no unconsumed bytes after probe: {line}");
}

/// `--no-query` (M1 acceptance 4): passive hints only — NOT ONE volley byte
/// may reach the terminal, `can_query` is false, and it returns immediately
/// (no deadline wait).
#[test]
fn no_query_sends_no_volley_bytes() {
    let (pty, mut child) = spawn_harness("probe-noquery");
    let mut out = Vec::new();
    wait_until_contains(pty.master, &mut out, b"PROBE-DONE");
    wait_child_success(&mut child);

    assert_eq!(find(&out, b"\x1b["), None, "--no-query must write no escape bytes at all");
    assert_eq!(find(&out, b"\x1bP"), None);

    let line = probe_done_line(&out);
    assert_eq!(field(&line, "can_query"), "false");
    assert_eq!(field(&line, "stray"), "0");
}

/// Regression (M1 review low 2, grace drain): the terminal answers slowly,
/// dribbling reply bytes across the deadline. The probe's quiet-gap grace
/// drain must consume every straggling byte (stray=0 — nothing left for the
/// app's input stream) and still use the late DA1 to upgrade caps.
#[test]
fn probe_grace_drain_consumes_replies_dribbling_past_deadline() {
    let (pty, mut child) = spawn_harness("probe-latereply");
    let mut out = Vec::new();
    // Wait for the full volley (DA1 query is its tail), then answer slowly.
    wait_until_contains(pty.master, &mut out, b"\x1b[c");

    // XTVERSION promptly (inside the 200 ms window: got_bytes = true)...
    write_master(pty.master, b"\x1bP>|kitty(0.32.2)\x1b\\");
    // ...then the DECRPM reply one byte every 40 ms — the deadline passes
    // mid-reply, but every 40 ms gap is far below the 150 ms quiet gap.
    for b in b"\x1b[?2026;2$y" {
        std::thread::sleep(Duration::from_millis(40));
        write_master(pty.master, &[*b]);
    }
    // Finally XTGETTCAP + cell px + the DA1 sentinel, well past the deadline.
    std::thread::sleep(Duration::from_millis(40));
    write_master(pty.master, b"\x1bP1+r524742=38\x1b\\\x1b[6;20;10t\x1b[?62;c");

    wait_until_contains(pty.master, &mut out, b"PROBE-DONE");
    wait_child_success(&mut child);

    let line = probe_done_line(&out);
    let ms: u64 = field(&line, "ms").parse().unwrap();
    assert!(ms > 200, "probe must have kept draining past the 200 ms deadline: {line}");
    assert_eq!(field(&line, "color"), "True", "late replies still upgrade: {line}");
    assert_eq!(field(&line, "sync"), "true", "{line}");
    assert_eq!(field(&line, "cellpx"), "10x20", "{line}");
    assert_eq!(field(&line, "stray"), "0", "no straggler byte may leak to the app: {line}");
}

/// Regression (M1 review low 2, event filter): the reply burst arrives only
/// AFTER the probe gave up entirely — the classic straggler. Once the
/// session is up, the burst reaches crossterm's decoder, which would
/// surface the DCS payload as plain key events (digits = seek bindings,
/// 't'/'y' letters, Alt combos). The backend's straggler filter must
/// discard every one of them, and real playback keys typed after a quiet
/// gap must still arrive.
#[test]
fn late_probe_replies_never_surface_as_key_events() {
    let (pty, mut child) = spawn_harness("probe-straggler");
    let mut out = Vec::new();

    // Silence until the probe times out (150 ms harness deadline)...
    wait_until_contains(pty.master, &mut out, b"PROBE-DONE");
    // ...and the AnsiBackend session is up (raw mode: no echo loops).
    wait_until_contains(pty.master, &mut out, b"SESSION-READY");

    // NOW the terminal finally answers: the full kitty-style burst.
    write_master(
        pty.master,
        b"\x1bP>|kitty(0.32.2)\x1b\\\x1b[?2026;2$y\x1bP1+r524742=38\x1b\\\x1b[6;20;10t\x1b[?62;c",
    );

    // Quiet gap, then a real playback key — it must still work.
    std::thread::sleep(Duration::from_millis(450));
    write_master(pty.master, b"x");
    wait_until_contains(pty.master, &mut out, b"EV=char:x");

    // Quit still works too.
    write_master(pty.master, b"q");
    wait_until_contains(pty.master, &mut out, b"EV=quit");
    wait_until_contains(pty.master, &mut out, b"SESSION-DONE");
    wait_child_success(&mut child);

    // The ONLY events the app may ever have seen: the 'x' key and the quit.
    let text = String::from_utf8_lossy(&out);
    let events: Vec<&str> = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("EV="))
        .collect();
    assert_eq!(
        events,
        vec!["char:x", "quit"],
        "straggler reply fragments surfaced as events: {events:?}"
    );
}

/// Regression (M2 low fix a, part 1): the straggler burst arrives SPLIT
/// across reads — the lone `ESC` first (crossterm tokenizes it as the Esc
/// KEY, which maps to Quit and used to kill the session), the `P…` payload
/// in later writes, the ST split as `ESC` + `\` too. Nothing may surface:
/// no quit, no payload keys; real keys after the quiet gap still work.
#[test]
fn split_esc_p_straggler_burst_never_quits_or_leaks_keys() {
    let (pty, mut child) = spawn_harness("probe-straggler");
    let mut out = Vec::new();
    wait_until_contains(pty.master, &mut out, b"PROBE-DONE");
    wait_until_contains(pty.master, &mut out, b"SESSION-READY");

    // The reply dribbles in: ESC | payload | ESC | '\' as separate writes.
    write_master(pty.master, b"\x1b");
    std::thread::sleep(Duration::from_millis(50));
    write_master(pty.master, b"P>|kitty(0.32.2)");
    std::thread::sleep(Duration::from_millis(50));
    write_master(pty.master, b"\x1b");
    std::thread::sleep(Duration::from_millis(50));
    write_master(pty.master, b"\\");

    // Quiet gap, then a real playback key, then quit.
    std::thread::sleep(Duration::from_millis(450));
    write_master(pty.master, b"x");
    wait_until_contains(pty.master, &mut out, b"EV=char:x");
    write_master(pty.master, b"q");
    wait_until_contains(pty.master, &mut out, b"EV=quit");
    wait_until_contains(pty.master, &mut out, b"SESSION-DONE");
    wait_child_success(&mut child);

    assert_eq!(
        session_events(&out),
        vec!["char:x", "quit"],
        "split straggler burst surfaced as events"
    );
}

/// Regression (M2 low fix a, part 2): the reply burst lands LONG after
/// session start — past the old 2 s disarm window, which used to let the
/// whole payload (digits = seek bindings) through as key events. The filter
/// is session-long in armed sessions: still nothing may surface.
#[test]
fn straggler_burst_after_two_seconds_still_filtered() {
    let (pty, mut child) = spawn_harness("probe-straggler");
    let mut out = Vec::new();
    wait_until_contains(pty.master, &mut out, b"PROBE-DONE");
    wait_until_contains(pty.master, &mut out, b"SESSION-READY");

    // Well past the removed 2 s window.
    std::thread::sleep(Duration::from_millis(2500));
    write_master(
        pty.master,
        b"\x1bP>|kitty(0.32.2)\x1b\\\x1b[?2026;2$y\x1bP1+r524742=38\x1b\\\x1b[6;20;10t\x1b[?62;c",
    );

    std::thread::sleep(Duration::from_millis(450));
    write_master(pty.master, b"x");
    wait_until_contains(pty.master, &mut out, b"EV=char:x");
    write_master(pty.master, b"q");
    wait_until_contains(pty.master, &mut out, b"EV=quit");
    wait_until_contains(pty.master, &mut out, b"SESSION-DONE");
    wait_child_success(&mut child);

    assert_eq!(
        session_events(&out),
        vec!["char:x", "quit"],
        "late (post-2s) straggler burst surfaced as events"
    );
}

/// The cost of the split-intro hold is bounded: a REAL lone Esc keypress in
/// an armed session still quits (delayed ≤ the hold window, never eaten).
#[test]
fn lone_esc_still_quits_in_armed_session() {
    let (pty, mut child) = spawn_harness("probe-straggler");
    let mut out = Vec::new();
    wait_until_contains(pty.master, &mut out, b"PROBE-DONE");
    wait_until_contains(pty.master, &mut out, b"SESSION-READY");

    write_master(pty.master, b"\x1b");
    wait_until_contains(pty.master, &mut out, b"EV=quit");
    wait_until_contains(pty.master, &mut out, b"SESSION-DONE");
    wait_child_success(&mut child);
    assert_eq!(session_events(&out), vec!["quit"], "lone Esc must map to exactly one quit");
}

/// Scripted kitty-style replies: caps upgrade to truecolor + sync 2026 +
/// cell px, the probe returns as soon as DA1 lands, and the replies are
/// fully consumed (no strays for the app's event loop to choke on).
#[test]
fn probe_with_scripted_replies_detects_caps() {
    let (pty, mut child) = spawn_harness("probe-reply");
    let mut out = Vec::new();
    // Wait for the full volley (DA1 sentinel is its tail), then answer.
    wait_until_contains(pty.master, &mut out, b"\x1b[c");
    let replies: &[u8] = b"\x1bP>|kitty(0.32.2)\x1b\\\
\x1b[?2026;2$y\
\x1bP1+r524742=38\x1b\\\
\x1b[6;20;10t\
\x1b[?62;c";
    write_master(pty.master, replies);

    wait_until_contains(pty.master, &mut out, b"PROBE-DONE");
    wait_child_success(&mut child);

    let line = probe_done_line(&out);
    assert_eq!(field(&line, "color"), "True", "XTGETTCAP RGB must upgrade the tier: {line}");
    assert_eq!(field(&line, "sync"), "true", "DECRPM 2026;2 must enable sync: {line}");
    assert_eq!(field(&line, "cellpx"), "10x20", "CSI 16 t reply must land: {line}");
    assert_eq!(field(&line, "stray"), "0", "replies must be fully consumed: {line}");
}
