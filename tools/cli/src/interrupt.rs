//! The interrupts a long-running command takes over: Ctrl-C and a request to
//! terminate (SIGINT, SIGTERM and SIGHUP on Unix; the console's Ctrl-C,
//! Ctrl-Break and close on Windows). Once caught they no longer end the
//! process; the command counts them, so the first stops it cooperatively —
//! killing the builds it started, asking its app to close — and the second
//! forces its children down.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

static COUNT: AtomicU32 = AtomicU32::new(0);
static CAUGHT: AtomicBool = AtomicBool::new(false);

/// How many interrupts arrived since [`catch`].
pub fn count() -> u32 {
    COUNT.load(Ordering::Relaxed)
}

/// Whether [`catch`] took the interrupts over.
pub fn caught() -> bool {
    CAUGHT.load(Ordering::Relaxed)
}

#[cfg(unix)]
extern "C" fn counted(_: libc::c_int) {
    COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Counts each interrupt from now on instead of ending the process.
#[cfg(unix)]
pub fn catch() {
    CAUGHT.store(true, Ordering::Relaxed);
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: `action` is zeroed plain data with its handler and flags
        // set; the handler only touches an atomic, which is signal safe.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = counted as extern "C" fn(libc::c_int) as libc::sighandler_t;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}

/// Counts each interrupt from now on instead of ending the process.
#[cfg(windows)]
pub fn catch() {
    CAUGHT.store(true, Ordering::Relaxed);
    use windows::Win32::System::Console::SetConsoleCtrlHandler;
    use windows_core::BOOL;

    unsafe extern "system" fn counted(_: u32) -> BOOL {
        COUNT.fetch_add(1, Ordering::Relaxed);
        BOOL::from(true)
    }
    // SAFETY: the handler lives for the process and only touches an atomic.
    let _ = unsafe { SetConsoleCtrlHandler(Some(counted), true) };
}

/// Counts each interrupt from now on instead of ending the process.
#[cfg(not(any(unix, windows)))]
pub fn catch() {}
