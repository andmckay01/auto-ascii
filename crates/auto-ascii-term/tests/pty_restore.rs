//! M0 acceptance (3): Ctrl-C / SIGTERM / SIGINT / panic / Drop all restore
//! the terminal — asserted on real pty output (alt-screen-leave, cursor-show,
//! SGR-reset bytes). Spawns the `auto-ascii-term-harness` bin under an openpty
//! pair with the pty as its controlling terminal.

#![cfg(unix)]

use std::io;
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use auto_ascii_term::RESTORE_SEQ;

const ALT_ENTER: &[u8] = b"\x1b[?1049h";
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

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_auto-ascii-term-harness"));
    cmd.arg(mode)
        .stdin(dup_stdio(slave))
        .stdout(dup_stdio(slave))
        .stderr(dup_stdio(slave));
    let slave_for_child = slave;
    unsafe {
        cmd.pre_exec(move || {
            // New session + make the pty the controlling terminal so
            // crossterm's /dev/tty resolution lands on it.
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(slave_for_child, libc::TIOCSCTTY as libc::c_ulong, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn().expect("spawn auto-ascii-term-harness");
    unsafe { libc::close(slave) };
    (Pty { master }, child)
}

enum Pump {
    Data,
    Quiet,
    Eof,
}

fn pump(master: RawFd, acc: &mut Vec<u8>, wait_ms: i32) -> Pump {
    let mut pfd = libc::pollfd { fd: master, events: libc::POLLIN, revents: 0 };
    let rc = unsafe { libc::poll(&mut pfd, 1, wait_ms) };
    if rc < 0 {
        return Pump::Eof;
    }
    if rc == 0 {
        return Pump::Quiet;
    }
    let mut buf = [0u8; 4096];
    let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
    if n > 0 {
        acc.extend_from_slice(&buf[..n as usize]);
        Pump::Data
    } else if n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
        Pump::Quiet
    } else {
        // 0, or EIO once the child side is gone: EOF.
        Pump::Eof
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn count(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

fn wait_until_contains(master: RawFd, acc: &mut Vec<u8>, needle: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(DEADLINE_SECS);
    while find(acc, needle).is_none() {
        assert!(
            Instant::now() < deadline,
            "timeout waiting for {:?}; pty output so far: {:?}",
            String::from_utf8_lossy(needle),
            String::from_utf8_lossy(acc)
        );
        if let Pump::Eof = pump(master, acc, 100) {
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

fn drain_to_eof(master: RawFd, acc: &mut Vec<u8>) {
    let deadline = Instant::now() + Duration::from_secs(DEADLINE_SECS);
    loop {
        match pump(master, acc, 100) {
            Pump::Eof => break,
            _ => assert!(
                Instant::now() < deadline,
                "timeout draining pty ({} bytes so far)",
                acc.len()
            ),
        }
    }
}

fn wait_child(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(DEADLINE_SECS);
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("harness did not exit in time");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn assert_restored(out: &[u8]) {
    assert!(
        find(out, RESTORE_SEQ).is_some(),
        "restore sequence missing from pty output: {:?}",
        String::from_utf8_lossy(out)
    );
    // The individual M0 acceptance bytes, spelled out:
    assert!(find(out, b"\x1b[0m").is_some(), "SGR reset missing");
    assert!(find(out, b"\x1b[?25h").is_some(), "cursor show missing");
    assert!(find(out, b"\x1b[?1049l").is_some(), "alt-screen leave missing");
}

#[test]
fn drop_restores_terminal_exactly_once() {
    let (pty, mut child) = spawn_harness("drop");
    let mut out = Vec::new();
    drain_to_eof(pty.master, &mut out);
    let status = wait_child(&mut child);
    assert!(status.success(), "harness exited with {status:?}");
    assert!(find(&out, ALT_ENTER).is_some(), "session never entered alt screen");
    assert_restored(&out);
    assert_eq!(
        count(&out, RESTORE_SEQ),
        1,
        "restore must run exactly once across Drop + atexit (idempotent)"
    );
}

#[test]
fn sigterm_restores_terminal() {
    let (pty, mut child) = spawn_harness("wait");
    let mut out = Vec::new();
    wait_until_contains(pty.master, &mut out, ALT_ENTER);
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    drain_to_eof(pty.master, &mut out);
    let status = wait_child(&mut child);
    assert_eq!(status.signal(), Some(libc::SIGTERM), "must die by re-raised SIGTERM");
    assert_restored(&out);
}

#[test]
fn sigint_restores_terminal() {
    let (pty, mut child) = spawn_harness("wait");
    let mut out = Vec::new();
    wait_until_contains(pty.master, &mut out, ALT_ENTER);
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
    drain_to_eof(pty.master, &mut out);
    let status = wait_child(&mut child);
    assert_eq!(status.signal(), Some(libc::SIGINT), "must die by re-raised SIGINT");
    assert_restored(&out);
}

#[test]
fn panic_restores_before_the_panic_message() {
    let (pty, mut child) = spawn_harness("panic");
    let mut out = Vec::new();
    drain_to_eof(pty.master, &mut out);
    let status = wait_child(&mut child);
    assert_eq!(status.code(), Some(101), "panic must exit 101, got {status:?}");
    assert_restored(&out);
    let restore_at = find(&out, RESTORE_SEQ).unwrap();
    let marker_at = find(&out, b"harness-panic-marker")
        .expect("panic message must reach the pty");
    assert!(
        marker_at > restore_at,
        "panic message must land on the restored (main-screen, cooked) terminal"
    );
}

#[test]
fn ctrl_c_key_quits_and_restores() {
    let (pty, mut child) = spawn_harness("loop");
    let mut out = Vec::new();
    wait_until_contains(pty.master, &mut out, ALT_ENTER);
    // Raw mode: 0x03 arrives as a key event, maps to Quit → orderly Drop.
    let ctrl_c = [0x03u8];
    let n = unsafe { libc::write(pty.master, ctrl_c.as_ptr().cast(), 1) };
    assert_eq!(n, 1, "failed to type Ctrl-C into the pty");
    drain_to_eof(pty.master, &mut out);
    let status = wait_child(&mut child);
    assert!(status.success(), "Ctrl-C key must quit cleanly, got {status:?}");
    assert_restored(&out);
}
