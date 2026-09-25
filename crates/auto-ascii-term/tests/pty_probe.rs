#![cfg(unix)]

mod common;

use std::time::Duration;

use common::{
    field, find, probe_done_line, session_events, spawn_harness, wait_child_success,
    wait_until_contains, write_master,
};

#[test]
fn probe_with_no_replies_times_out_to_defaults() {
    let (pty, mut child) = spawn_harness("probe-silent");
    let mut out = Vec::new();
    wait_until_contains(pty.master, &mut out, b"PROBE-DONE");
    wait_child_success(&mut child);

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

#[test]
fn probe_grace_drain_consumes_replies_dribbling_past_deadline() {
    let (pty, mut child) = spawn_harness("probe-latereply");
    let mut out = Vec::new();
    wait_until_contains(pty.master, &mut out, b"\x1b[c");

    write_master(pty.master, b"\x1bP>|kitty(0.32.2)\x1b\\");
    for b in b"\x1b[?2026;2$y" {
        std::thread::sleep(Duration::from_millis(40));
        write_master(pty.master, &[*b]);
    }
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

#[test]
fn late_probe_replies_never_surface_as_key_events() {
    let (pty, mut child) = spawn_harness("probe-straggler");
    let mut out = Vec::new();

    wait_until_contains(pty.master, &mut out, b"PROBE-DONE");
    wait_until_contains(pty.master, &mut out, b"SESSION-READY");

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

#[test]
fn split_esc_p_straggler_burst_never_quits_or_leaks_keys() {
    let (pty, mut child) = spawn_harness("probe-straggler");
    let mut out = Vec::new();
    wait_until_contains(pty.master, &mut out, b"PROBE-DONE");
    wait_until_contains(pty.master, &mut out, b"SESSION-READY");

    write_master(pty.master, b"\x1b");
    std::thread::sleep(Duration::from_millis(50));
    write_master(pty.master, b"P>|kitty(0.32.2)");
    std::thread::sleep(Duration::from_millis(50));
    write_master(pty.master, b"\x1b");
    std::thread::sleep(Duration::from_millis(50));
    write_master(pty.master, b"\\");

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

#[test]
fn straggler_burst_after_two_seconds_still_filtered() {
    let (pty, mut child) = spawn_harness("probe-straggler");
    let mut out = Vec::new();
    wait_until_contains(pty.master, &mut out, b"PROBE-DONE");
    wait_until_contains(pty.master, &mut out, b"SESSION-READY");

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

#[test]
fn probe_with_scripted_replies_detects_caps() {
    let (pty, mut child) = spawn_harness("probe-reply");
    let mut out = Vec::new();
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
