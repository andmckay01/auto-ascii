//! Process-wide terminal restoration and shutdown hooks.

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
#[cfg(unix)]
unsafe impl Sync for TermiosStore {}
#[cfg(unix)]
static SAVED_TERMIOS: TermiosStore = TermiosStore(UnsafeCell::new(MaybeUninit::uninit()));

static HOOKS: Once = Once::new();

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
