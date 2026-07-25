//! M1 acceptance 4: the caps probe never hangs and never leaks bytes.
//! Runs the `slpy-term-harness` probe modes under an openpty pair (same
//! harness pattern as `tests/pty_restore.rs`):
//!
//! - `probe-silent`: the test side never answers the volley → the probe must
//!   time out to conservative defaults in < 300 ms with zero stray bytes
//!   left on stdin.
//! - `probe-reply`: the test side scripts kitty-style replies → detected
//!   caps upgrade (truecolor, sync 2026, cell px) and still zero strays.

#![cfg(unix)]

use std::io;
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const DEADLINE_SECS: u64 = 10;

struct Pty {
    master: RawFd,
}

impl Drop for Pty {
    fn drop(&mut self) {
        unsafe { libc::close(self.master) };
    }
}

fn spawn_harness(mode: &str) -> (Pty, Child) {
    let mut master: libc::c_int = 0;
    let mut slave: libc::c_int = 0;
    let ws = libc::winsize { ws_row: 24, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 };
    let rc = unsafe {
        libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(), &ws)
    };
    assert_eq!(rc, 0, "openpty failed: {}", io::Error::last_os_error());

    let dup_stdio = |fd: RawFd| -> Stdio {
        let d = unsafe { libc::dup(fd) };
        assert!(d >= 0, "dup failed: {}", io::Error::last_os_error());
        unsafe { Stdio::from_raw_fd(d) }
    };

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_slpy-term-harness"));
    cmd.arg(mode)
        .stdin(dup_stdio(slave))
        .stdout(dup_stdio(slave))
        .stderr(dup_stdio(slave));
    let slave_for_child = slave;
    unsafe {
        cmd.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(slave_for_child, libc::TIOCSCTTY as libc::c_ulong, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn().expect("spawn slpy-term-harness");
    unsafe { libc::close(slave) };
    (Pty { master }, child)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Pump master output until `needle` appears (or EOF/deadline).
fn wait_until_contains(master: RawFd, acc: &mut Vec<u8>, needle: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(DEADLINE_SECS);
    while find(acc, needle).is_none() {
        assert!(
            Instant::now() < deadline,
            "timeout waiting for {:?}; pty output so far: {:?}",
            String::from_utf8_lossy(needle),
            String::from_utf8_lossy(acc)
        );
        let mut pfd = libc::pollfd { fd: master, events: libc::POLLIN, revents: 0 };
        let rc = unsafe { libc::poll(&mut pfd, 1, 100) };
        if rc <= 0 {
            continue;
        }
        let mut buf = [0u8; 4096];
        let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            acc.extend_from_slice(&buf[..n as usize]);
        } else if n == 0 {
            break;
        }
    }
    assert!(
        find(acc, needle).is_some(),
        "pty closed before {:?} appeared; output: {:?}",
        String::from_utf8_lossy(needle),
        String::from_utf8_lossy(acc)
    );
}

fn wait_child_success(child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(DEADLINE_SECS);
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            assert!(status.success(), "harness exited with {status:?}");
            return;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("harness did not exit in time");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Extract `key=value` from the PROBE-DONE line.
fn field<'a>(line: &'a str, key: &str) -> &'a str {
    let pat = format!("{key}=");
    let at = line.find(&pat).unwrap_or_else(|| panic!("{key} missing in {line:?}"));
    let rest = &line[at + pat.len()..];
    rest.split_whitespace().next().unwrap_or(rest)
}

fn probe_done_line(out: &[u8]) -> String {
    let text = String::from_utf8_lossy(out);
    text.lines()
        .find(|l| l.contains("PROBE-DONE"))
        .unwrap_or_else(|| panic!("no PROBE-DONE line in {text:?}"))
        .to_string()
}

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

fn write_master(master: RawFd, bytes: &[u8]) {
    let n = unsafe { libc::write(master, bytes.as_ptr().cast(), bytes.len()) };
    assert_eq!(n as usize, bytes.len(), "failed to type into the pty");
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

/// Extract the `EV=` lines the harness session logged.
fn session_events(out: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(out)
        .lines()
        .filter_map(|l| l.trim().strip_prefix("EV=").map(str::to_owned))
        .collect()
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
    let n = unsafe { libc::write(pty.master, replies.as_ptr().cast(), replies.len()) };
    assert_eq!(n as usize, replies.len(), "failed to type replies into the pty");

    wait_until_contains(pty.master, &mut out, b"PROBE-DONE");
    wait_child_success(&mut child);

    let line = probe_done_line(&out);
    assert_eq!(field(&line, "color"), "True", "XTGETTCAP RGB must upgrade the tier: {line}");
    assert_eq!(field(&line, "sync"), "true", "DECRPM 2026;2 must enable sync: {line}");
    assert_eq!(field(&line, "cellpx"), "10x20", "CSI 16 t reply must land: {line}");
    assert_eq!(field(&line, "stray"), "0", "replies must be fully consumed: {line}");
}
