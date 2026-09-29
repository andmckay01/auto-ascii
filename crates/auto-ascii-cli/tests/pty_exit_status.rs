#![cfg(unix)]

use std::fs;
use std::io;
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use auto_ascii_format::{AsciiWriter, Meta, PlaneRef, WriterOptions, header::plane_id};

const QUIT_STATUS: i32 = 3;
const DEADLINE: Duration = Duration::from_secs(20);
const ALT_SCREEN_ON: &[u8] = b"\x1b[?1049h";
const RIGHT: &[u8] = b"\x1b[C";

struct TmpFile(PathBuf);

impl TmpFile {
    fn asset(tag: &str, frames: u32) -> TmpFile {
        let path = std::env::temp_dir()
            .join(format!("auto-ascii-pty-exit-{}-{tag}.ascii", std::process::id()));
        let opts = WriterOptions { zstd_level: 1, ..WriterOptions::default() };
        let (w, h) = (opts.base_w as usize, opts.base_h as usize);
        let meta = Meta {
            factory_version: "test".into(),
            source: "synthetic".into(),
            palette_hints: vec![],
        };
        let file = fs::File::create(&path).unwrap();
        let mut writer = AsciiWriter::new(io::BufWriter::new(file), opts, &meta).unwrap();
        let mut plane = vec![0u8; w * h];
        for f in 0..frames {
            for (i, px) in plane.iter_mut().enumerate() {
                *px = ((i % w + i / w + 7 * f as usize) & 0xff) as u8;
            }
            writer.write_frame(&[PlaneRef { id: plane_id::Y, data: &plane }]).unwrap();
        }
        writer.finish().unwrap().into_inner().unwrap();
        TmpFile(path)
    }
}

impl Drop for TmpFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

struct Session {
    master: RawFd,
    child: Child,
    out: Arc<Mutex<Vec<u8>>>,
    reader: Option<JoinHandle<()>>,
    started: Instant,
}

impl Session {
    fn spawn(asset: &Path) -> Session {
        let mut ws = libc::winsize { ws_row: 24, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 };
        let (mut master, mut slave) = (0, 0);
        let rc = unsafe {
            libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &mut ws)
        };
        assert_eq!(rc, 0, "openpty: {}", io::Error::last_os_error());
        let stdio = |fd: RawFd| unsafe { Stdio::from_raw_fd(libc::dup(fd)) };
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_auto-ascii"));
        cmd.arg("play")
            .arg(asset)
            .args(["--tier", "truecolor", "--no-cache", "--fps-cap", "10", "--no-audio"])
            .stdin(stdio(slave))
            .stdout(stdio(slave))
            .stderr(stdio(slave));
        unsafe {
            cmd.pre_exec(move || {
                if libc::setsid() < 0
                    || libc::ioctl(slave, libc::TIOCSCTTY as libc::c_ulong, 0) < 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().expect("spawn auto-ascii play");
        unsafe { libc::close(slave) };
        let out = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&out);
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 65536];
            loop {
                let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
                if n <= 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&buf[..n as usize]);
            }
        });
        Session { master, child, out, reader: Some(reader), started: Instant::now() }
    }

    fn wait_for_session(&self) {
        while !self.out.lock().unwrap().windows(ALT_SCREEN_ON.len()).any(|w| w == ALT_SCREEN_ON) {
            assert!(self.started.elapsed() < DEADLINE, "player never entered the alt screen");
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    fn send(&self, bytes: &[u8]) {
        let n = unsafe { libc::write(self.master, bytes.as_ptr().cast(), bytes.len()) };
        assert_eq!(n, bytes.len() as isize, "pty write: {}", io::Error::last_os_error());
    }

    fn wait(mut self) -> (ExitStatus, Duration) {
        loop {
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                return (status, self.started.elapsed());
            }
            if self.started.elapsed() >= DEADLINE {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!("player did not exit within {DEADLINE:?}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        unsafe { libc::close(self.master) };
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
    }
}

#[test]
fn quit_keys_exit_with_the_quit_status() {
    let asset = TmpFile::asset("quit", 600);
    for (name, key) in [("q", &b"q"[..]), ("Esc", b"\x1b"), ("Ctrl-C", b"\x03")] {
        let s = Session::spawn(&asset.0);
        s.wait_for_session();
        s.send(key);
        let (status, _) = s.wait();
        assert_eq!(status.code(), Some(QUIT_STATUS), "{name} must exit {QUIT_STATUS}: {status:?}");
    }
}

#[test]
fn scrubbing_forward_to_the_end_exits_zero() {
    let asset = TmpFile::asset("scrub", 300);
    let s = Session::spawn(&asset.0);
    s.wait_for_session();
    for _ in 0..2 {
        s.send(RIGHT);
        std::thread::sleep(Duration::from_millis(150));
    }
    let (status, took) = s.wait();
    assert_eq!(status.code(), Some(0), "reaching the end after a scrub is not a quit: {status:?}");
    assert!(took < Duration::from_secs(8), "two scrubs skip 10 s of a 10 s clip, took {took:?}");
}

#[test]
fn playing_to_the_end_exits_zero() {
    let asset = TmpFile::asset("end", 30);
    let (status, _) = Session::spawn(&asset.0).wait();
    assert_eq!(status.code(), Some(0), "{status:?}");
}
