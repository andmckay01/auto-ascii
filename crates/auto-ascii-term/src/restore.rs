//! Process-wide terminal restoration.
//!
//! The alt-screen-leave, cursor-show and SGR-reset bytes are emitted on
//! Ctrl-C, SIGTERM and panic (asserted by a pty test).
//!
//! Design: `AnsiBackend::new` *arms* a process-global (tty fd + pre-raw
//! termios) before touching the terminal; `restore_now` disarms and
//! restores exactly once, from whichever path fires first — orderly
//! `shutdown`/`Drop`, the panic hook, SIGINT/SIGTERM, or atexit. Everything
//! on the signal path is async-signal-safe: atomics, raw `write(2)`,
//! `tcsetattr` — no locks, no allocation.

#[cfg(unix)]
use std::cell::UnsafeCell;
#[cfg(unix)]
use std::mem::MaybeUninit;
use std::sync::Once;
#[cfg(unix)]
use std::sync::atomic::AtomicI32;
#[cfg(windows)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

/// The restore byte sequence, in emit order: SGR reset, cursor show, autowrap
/// on, leave alt screen. Cooked-mode (termios) restoration happens alongside
/// but is not byte-visible.
pub const RESTORE_SEQ: &[u8] = b"\x1b[0m\x1b[?25h\x1b[?7h\x1b[?1049l";

#[cfg(windows)]
static ARMED: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
pub fn install_restore_hooks() {
    HOOKS.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_now();
            prev(info);
        }));
    });
}

#[cfg(windows)]
pub(crate) fn arm() {
    ARMED.store(true, Ordering::SeqCst);
}

#[cfg(windows)]
pub(crate) fn restore_now() {
    if !ARMED.swap(false, Ordering::SeqCst) {
        return;
    }
    use std::io::Write as _;
    let mut out = std::io::stdout();
    let _ = out.write_all(RESTORE_SEQ);
    let _ = out.flush();
    let _ = crossterm::terminal::disable_raw_mode();
}

#[cfg(unix)]
static RESTORE_FD: AtomicI32 = AtomicI32::new(-1);

#[cfg(unix)]
struct TermiosStore(UnsafeCell<MaybeUninit<libc::termios>>);
// SAFETY: written once by `arm` before RESTORE_FD is published, read only
// after observing RESTORE_FD >= 0 (same thread for the signal case).
#[cfg(unix)]
unsafe impl Sync for TermiosStore {}
#[cfg(unix)]
static SAVED_TERMIOS: TermiosStore = TermiosStore(UnsafeCell::new(MaybeUninit::uninit()));

static HOOKS: Once = Once::new();

/// Install restoration hooks: a panic hook, SIGINT/SIGTERM handlers, and
/// atexit — each runs `restore_now` (async-signal-safe raw `write(2)` of
/// [`RESTORE_SEQ`] + `tcsetattr`, no locks/allocation); the signal handlers
/// then re-raise with the default disposition so the exit status still
/// reports the signal.
///
/// Idempotent and safe to call before any backend exists; a no-op restore
/// when no session was ever entered. `AnsiBackend::shutdown`/`Drop` perform
/// the same restore on the orderly path (also exactly once — the armed fd is
/// consumed atomically).
#[cfg(unix)]
pub fn install_restore_hooks() {
    HOOKS.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_now();
            prev(info);
        }));
        unsafe {
            let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
            libc::signal(libc::SIGINT, handler);
            libc::signal(libc::SIGTERM, handler);
            libc::atexit(at_exit);
        }
    });
}

#[cfg(unix)]
pub(crate) fn arm(fd: libc::c_int, saved: libc::termios) {
    unsafe {
        (*SAVED_TERMIOS.0.get()).write(saved);
    }
    RESTORE_FD.store(fd, Ordering::SeqCst);
}

#[cfg(unix)]
pub(crate) fn restore_now() {
    let fd = RESTORE_FD.swap(-1, Ordering::SeqCst);
    if fd < 0 {
        return;
    }
    unsafe {
        let mut rem: &[u8] = RESTORE_SEQ;
        while !rem.is_empty() {
            let n = libc::write(fd, rem.as_ptr().cast(), rem.len());
            if n < 0 {
                if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                break;
            }
            if n == 0 {
                break;
            }
            rem = &rem[n as usize..];
        }
        let saved = (*SAVED_TERMIOS.0.get()).assume_init_ref();
        let _ = libc::tcsetattr(fd, libc::TCSANOW, saved);
    }
}

#[cfg(unix)]
extern "C" fn on_signal(sig: libc::c_int) {
    restore_now();
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

#[cfg(unix)]
extern "C" fn at_exit() {
    restore_now();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn restore_without_session_is_noop() {
        restore_now();
        assert_eq!(RESTORE_FD.load(Ordering::SeqCst), -1);
    }

    #[test]
    fn install_is_idempotent() {
        install_restore_hooks();
        install_restore_hooks();
    }
}
