//! Every child the stream spawns (yt-dlp, both ffmpegs) lives in its own
//! process group and is owned here: shutdown kills each group and reaps it,
//! on quit, on error and on drop. The private scratch dir is the children's
//! working directory and is removed with the guard.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const SLOTS: usize = 16;
const REAP_POLL: std::time::Duration = std::time::Duration::from_millis(5);

static GROUPS: [AtomicI32; SLOTS] = [const { AtomicI32::new(0) }; SLOTS];

pub struct Spawned {
    pub id: u64,
    pub stdout: ChildStdout,
    pub stderr: ChildStderr,
}

#[derive(Default)]
struct Inner {
    children: Vec<(u64, Child)>,
    groups: Vec<(i32, bool)>,
    next: u64,
    closed: bool,
}

#[derive(Clone, Default)]
pub struct Procs {
    inner: Arc<Mutex<Inner>>,
}

impl Procs {
    pub fn new() -> Procs {
        Procs::default()
    }

    pub fn spawn(&self, cmd: &mut Command, tool: &str) -> Result<Spawned, String> {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if inner.closed {
            return Err("stream is shutting down".into());
        }
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd.spawn().map_err(|e| missing(tool, &e))?;
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let id = inner.next;
        inner.next += 1;
        let group = i32::try_from(child.id()).unwrap_or(0);
        inner.groups.push((group, false));
        remember(group);
        inner.children.push((id, child));
        Ok(Spawned { id, stdout, stderr })
    }

    pub fn reap(&self, id: u64) -> Option<ExitStatus> {
        loop {
            {
                let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
                let at = inner.children.iter().position(|(cid, _)| *cid == id)?;
                let status = match inner.children[at].1.try_wait() {
                    Ok(None) => None,
                    Ok(Some(status)) => Some(Some(status)),
                    Err(_) => Some(None),
                };
                if let Some(status) = status {
                    let child = inner.children.swap_remove(at).1;
                    kill_group(child.id(), libc_sigkill());
                    forget(i32::try_from(child.id()).unwrap_or(0));
                    latch_gone(&mut inner.groups);
                    return status;
                }
            }
            std::thread::sleep(REAP_POLL);
        }
    }

    pub fn kill(&self, id: u64) {
        let child = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let at = inner.children.iter().position(|(cid, _)| *cid == id);
            at.map(|at| inner.children.swap_remove(at).1)
        };
        if let Some(mut child) = child {
            kill_group(child.id(), libc_sigkill());
            let _ = child.kill();
            let _ = child.wait();
            forget(i32::try_from(child.id()).unwrap_or(0));
            latch_gone(&mut self.inner.lock().unwrap_or_else(|p| p.into_inner()).groups);
        }
    }

    pub fn shutdown(&self) {
        let children = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            inner.closed = true;
            std::mem::take(&mut inner.children)
        };
        for (_, mut child) in children {
            kill_group(child.id(), libc_sigkill());
            let _ = child.kill();
            let _ = child.wait();
            forget(i32::try_from(child.id()).unwrap_or(0));
        }
        latch_gone(&mut self.inner.lock().unwrap_or_else(|p| p.into_inner()).groups);
    }

    pub fn spawned(&self) -> usize {
        self.inner.lock().unwrap_or_else(|p| p.into_inner()).groups.len()
    }

    #[cfg(test)]
    pub fn running(&self) -> usize {
        self.inner.lock().unwrap_or_else(|p| p.into_inner()).children.len()
    }

    pub fn alive(&self) -> usize {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        latch_gone(&mut inner.groups);
        inner.groups.iter().filter(|(_, gone)| !gone).count()
    }
}

pub struct ProcsGuard(pub Procs);

impl Drop for ProcsGuard {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}

pub fn missing(tool: &str, e: &io::Error) -> String {
    auto_ascii_factory::ffmpeg::missing_tool(tool, e)
}

fn remember(group: i32) {
    if group <= 0 {
        return;
    }
    for slot in &GROUPS {
        if slot.compare_exchange(0, group, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
            return;
        }
    }
}

fn forget(group: i32) {
    if group <= 0 {
        return;
    }
    for slot in &GROUPS {
        let _ = slot.compare_exchange(group, 0, Ordering::SeqCst, Ordering::SeqCst);
    }
}

fn latch_gone(groups: &mut [(i32, bool)]) {
    for (group, gone) in groups.iter_mut() {
        if !*gone && !group_exists(*group) {
            *gone = true;
        }
    }
}

pub fn kill_registered_groups() {
    for slot in &GROUPS {
        let group = slot.swap(0, Ordering::SeqCst);
        if group > 0 {
            kill_group(group as u32, libc_sigkill());
        }
    }
}

#[cfg(unix)]
fn libc_sigkill() -> i32 {
    libc::SIGKILL
}

#[cfg(not(unix))]
fn libc_sigkill() -> i32 {
    9
}

#[cfg(unix)]
fn kill_group(pid: u32, sig: i32) {
    if let Ok(pid) = i32::try_from(pid)
        && pid > 0
    {
        unsafe {
            libc::kill(-pid, sig);
        }
    }
}

#[cfg(not(unix))]
fn kill_group(_pid: u32, _sig: i32) {}

#[cfg(unix)]
fn group_exists(group: i32) -> bool {
    if group <= 0 {
        return false;
    }
    let rc = unsafe { libc::kill(-group, 0) };
    rc == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn group_exists(_group: i32) -> bool {
    false
}

pub struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    pub fn new() -> io::Result<ScratchDir> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        loop {
            let seq = SEQ.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir()
                .join(format!("auto-ascii-stream-{}-{nanos:09}-{seq}", std::process::id()));
            match builder.create(&path) {
                Ok(()) => return Ok(ScratchDir { path }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn entries(&self) -> usize {
        std::fs::read_dir(&self.path).map(|d| d.count()).unwrap_or(0)
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn sleeper(dir: &Path) -> Command {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30 & sleep 30"]).current_dir(dir);
        cmd
    }

    fn wait_gone(procs: &Procs) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if procs.alive() == 0 {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn drop_kills_and_reaps_the_whole_group_and_removes_the_scratch_dir() {
        let scratch = ScratchDir::new().unwrap();
        let dir = scratch.path().to_path_buf();
        assert!(dir.is_dir());
        let procs = Procs::new();
        {
            let guard = ProcsGuard(procs.clone());
            let _spawned = guard.0.spawn(&mut sleeper(&dir), "sh").unwrap();
            assert_eq!(procs.running(), 1);
            assert_eq!(procs.alive(), 1);
        }
        assert_eq!(procs.running(), 0);
        assert!(wait_gone(&procs), "the group (a zombie would still count) outlived the guard");
        assert!(procs.spawn(&mut sleeper(&dir), "sh").is_err(), "a closed guard spawns nothing");
        drop(scratch);
        assert!(!dir.exists());
    }

    #[test]
    fn kill_on_quit_stops_one_child_and_leaves_the_rest() {
        let scratch = ScratchDir::new().unwrap();
        let procs = Procs::new();
        let guard = ProcsGuard(procs.clone());
        let a = procs.spawn(&mut sleeper(scratch.path()), "sh").unwrap();
        let _b = procs.spawn(&mut sleeper(scratch.path()), "sh").unwrap();
        procs.kill(a.id);
        assert_eq!(procs.running(), 1);
        drop(guard);
        assert!(wait_gone(&procs));
        assert_eq!(procs.spawned(), 2);
    }

    #[test]
    fn a_missing_tool_is_reported_by_name() {
        let procs = Procs::new();
        let err = procs
            .spawn(&mut Command::new("/nonexistent/auto-ascii-no-such-tool"), "yt-dlp")
            .err()
            .unwrap();
        assert!(err.starts_with("failed to run yt-dlp (is it installed and on PATH?)"), "{err}");
    }

    #[test]
    fn shutdown_ends_a_reap_waiting_on_a_child_that_closed_its_pipes() {
        let procs = Procs::new();
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "exec >&- 2>&-; sleep 30"]);
        let s = procs.spawn(&mut cmd, "sh").unwrap();
        let mut out = s.stdout;
        let mut sink = Vec::new();
        std::io::Read::read_to_end(&mut out, &mut sink).unwrap();
        let reaper = {
            let procs = procs.clone();
            std::thread::spawn(move || procs.reap(s.id))
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while procs.alive() == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(50));
        procs.shutdown();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !reaper.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(reaper.is_finished(), "a quit could not cancel a child that was being reaped");
        assert!(wait_gone(&procs));
        let _ = reaper.join();
    }

    #[test]
    fn a_panic_unwinds_through_the_guards() {
        let procs = Procs::new();
        let seen = std::sync::Mutex::new(PathBuf::new());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let scratch = ScratchDir::new().unwrap();
            *seen.lock().unwrap() = scratch.path().to_path_buf();
            let guard = ProcsGuard(procs.clone());
            let _child = guard.0.spawn(&mut sleeper(scratch.path()), "sh").unwrap();
            panic!("stream loop panicked");
        }));
        assert!(result.is_err());
        assert!(wait_gone(&procs), "children outlived a panic");
        assert!(!seen.lock().unwrap().exists(), "the scratch dir outlived a panic");
    }

    #[test]
    fn reap_returns_the_exit_status() {
        let procs = Procs::new();
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "exit 3"]);
        let s = procs.spawn(&mut cmd, "sh").unwrap();
        drop(s.stdout);
        let status = procs.reap(s.id).unwrap();
        assert_eq!(status.code(), Some(3));
        assert_eq!(procs.running(), 0);
    }
}
