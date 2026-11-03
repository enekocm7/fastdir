//! `Ctrl+C` handling: the first press asks the scan to stop cleanly (the
//! progress thread forwards it to the scanner's cancel flag); a second press
//! falls back to the default behaviour and ends the process.

use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
static INSTALL: Once = Once::new();

/// Whether `Ctrl+C` was pressed since the handler was installed.
pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::Relaxed)
}

/// Install the handler (idempotent).
pub fn install() {
    INSTALL.call_once(install_impl);
}

#[cfg(windows)]
fn install_impl() {
    use windows_sys::Win32::Foundation::{FALSE, TRUE};
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler,
    };

    unsafe extern "system" fn handler(event: u32) -> windows_sys::core::BOOL {
        if event != CTRL_C_EVENT && event != CTRL_BREAK_EVENT {
            return FALSE;
        }
        // Already asked once: let the default handler terminate us.
        if INTERRUPTED.swap(true, Ordering::Relaxed) {
            return FALSE;
        }
        TRUE
    }

    // SAFETY: `handler` is a valid `PHANDLER_ROUTINE` for the process lifetime.
    unsafe {
        SetConsoleCtrlHandler(Some(handler), TRUE);
    }
}

#[cfg(unix)]
fn install_impl() {
    extern "C" fn handler(_: libc::c_int) {
        if INTERRUPTED.swap(true, Ordering::Relaxed) {
            // SAFETY: `signal` and `raise` are async-signal-safe.
            unsafe {
                libc::signal(libc::SIGINT, libc::SIG_DFL);
                libc::raise(libc::SIGINT);
            }
        }
    }

    // SAFETY: the handler only touches an atomic and async-signal-safe calls.
    unsafe {
        libc::signal(
            libc::SIGINT,
            handler as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }
}

#[cfg(not(any(windows, unix)))]
fn install_impl() {}
