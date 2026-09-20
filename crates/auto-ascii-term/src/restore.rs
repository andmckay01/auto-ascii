//! Process-wide terminal restoration (PLAN §3.1 session hygiene).
//!
//! "A hosed terminal is the #1 TUI bug report and gets its own pty test" —
//! M0 acceptance (3) asserts the alt-screen-leave, cursor-show and SGR-reset
//! bytes on Ctrl-C, SIGTERM and panic.
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
/// on, leave alt screen. `pub` so the pty test asserts these exact bytes.
/// Cooked-mode (termios) restoration happens alongside but is not byte-visible.
pub const RESTORE_SEQ: &[u8] = b"\x1b[0m\x1b[?25h\x1b[?7h\x1b[?1049l";

// ---------------------------------------------------------------------------
// Windows (M5 item E, untested-cross — see scripts/release.sh): no termios,
// no POSIX signals. The session is restored by writing RESTORE_SEQ +
// crossterm's disable_raw_mode from the orderly shutdown/Drop path and the
// panic hook; Ctrl-C in raw mode arrives as a key event (mapped to Quit), so
// the orderly path covers it. Everything here is best-effort by design.
// ---------------------------------------------------------------------------

/// Armed flag: a live session exists whose terminal must be restored.
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

/// Armed tty fd to restore, or -1 when there is no live session.
#[cfg(unix)]
static RESTORE_FD: AtomicI32 = AtomicI32::new(-1);

/// Pre-raw termios saved by [`arm`]; valid whenever `RESTORE_FD >= 0`.
#[cfg(unix)]
struct TermiosStore(UnsafeCell<MaybeUninit<libc::termios>>);
// SAFETY: written once by `arm` before RESTORE_FD is published, read only
// after observing RESTORE_FD >= 0 (same thread for the signal case).
#[cfg(unix)]
unsafe impl Sync for TermiosStore {}
#[cfg(unix)]
static SAVED_TERMIOS: TermiosStore = TermiosStore(UnsafeCell::new(MaybeUninit::uninit()));

static HOOKS: Once = Once::new();

/// Install restoration hooks (PLAN §3.1): a panic hook, SIGINT/SIGTERM
/// handlers, and atexit — each runs `restore_now` (async-signal-safe raw
/// `write(2)` of [`RESTORE_SEQ`] + `tcsetattr`, no locks/allocation); the
/// signal handlers then re-raise with the default disposition so the exit
/// status still reports the signal.
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

/// Register the live session: `fd` is the tty, `saved` its pre-raw termios.
/// Called by `AnsiBackend::new` BEFORE entering raw mode/alt screen so no
/// window exists where a signal could leave the terminal hosed.
#[cfg(unix)]
pub(crate) fn arm(fd: libc::c_int, saved: libc::termios) {
    unsafe {
        (*SAVED_TERMIOS.0.get()).write(saved);
    }
    RESTORE_FD.store(fd, Ordering::SeqCst);
}

/// Restore the terminal now, if a session is armed: write [`RESTORE_SEQ`],
/// then `tcsetattr` back to the saved (cooked) termios. Atomically consumes
/// the armed fd, so every caller past the first is a no-op — idempotent
/// across Drop + panic hook + signal + atexit. Async-signal-safe.
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
                // errno via std: `libc::__errno_location` is glibc-only (macOS
                // exposes `__error`). Allocation-free, so still signal-safe.
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
    // Re-raise with the default disposition so wait() reports death-by-signal.
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
        // Nothing armed: must not write anywhere or crash.
        restore_now();
        assert_eq!(RESTORE_FD.load(Ordering::SeqCst), -1);
    }

    #[test]
    fn install_is_idempotent() {
        install_restore_hooks();
        install_restore_hooks();
    }
}
