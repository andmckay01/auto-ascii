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
