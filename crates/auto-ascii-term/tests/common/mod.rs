#![allow(dead_code)]

use std::io;
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const DEADLINE_SECS: u64 = 10;

pub struct Pty {
    pub master: RawFd,
}

impl Drop for Pty {
    fn drop(&mut self) {
        unsafe { libc::close(self.master) };
    }
}

pub const fn winsize_cells_only(cols: u16, rows: u16) -> libc::winsize {
    libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 }
}

pub const fn winsize_with_cell_px(
    cols: u16,
    rows: u16,
    cell_w: u16,
    cell_h: u16,
) -> libc::winsize {
    libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: cols * cell_w,
        ws_ypixel: rows * cell_h,
    }
}

pub fn spawn_harness(mode: &str) -> (Pty, Child) {
    spawn_harness_with(mode, winsize_cells_only(80, 24), &[])
}

pub fn spawn_harness_with(
    mode: &str,
    mut ws: libc::winsize,
    env: &[(&str, Option<&str>)],
) -> (Pty, Child) {
    let mut master: libc::c_int = 0;
    let mut slave: libc::c_int = 0;
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut ws,
        )
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
    for (key, value) in env {
        match value {
            Some(v) => cmd.env(key, v),
            None => cmd.env_remove(key),
        };
    }
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
    let child = cmd.spawn().expect("spawn auto-ascii-term-harness");
    unsafe { libc::close(slave) };
    (Pty { master }, child)
}

pub fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

pub fn wait_until_contains(master: RawFd, acc: &mut Vec<u8>, needle: &[u8]) {
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

pub fn wait_child_success(child: &mut Child) {
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

pub fn field<'a>(line: &'a str, key: &str) -> &'a str {
    let pat = format!("{key}=");
    let at = line.find(&pat).unwrap_or_else(|| panic!("{key} missing in {line:?}"));
    let rest = &line[at + pat.len()..];
    rest.split_whitespace().next().unwrap_or(rest)
}

pub fn probe_done_line(out: &[u8]) -> String {
    let text = String::from_utf8_lossy(out);
    text.lines()
        .find(|l| l.contains("PROBE-DONE"))
        .unwrap_or_else(|| panic!("no PROBE-DONE line in {text:?}"))
        .to_string()
}

pub fn write_master(master: RawFd, bytes: &[u8]) {
    let n = unsafe { libc::write(master, bytes.as_ptr().cast(), bytes.len()) };
    assert_eq!(n as usize, bytes.len(), "failed to type into the pty");
}

pub fn session_events(out: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(out)
        .lines()
        .filter_map(|l| l.trim().strip_prefix("EV=").map(str::to_owned))
        .collect()
}
