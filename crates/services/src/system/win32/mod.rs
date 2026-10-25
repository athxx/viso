//! The Windows services: the shell opening URLs. Windows has no haptics.

mod shell_open;

use std::rc::Rc;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

use crate::registry::Services;
use crate::unsupported::Unsupported;

pub(crate) fn services(_app: &str) -> Services {
    Services::from_parts(Rc::new(Unsupported), Rc::new(shell_open::ShellOpen))
}

/// The window an opening belongs to: this thread's active window, else the
/// foreground one.
fn owner() -> HWND {
    // SAFETY: both calls only read the window manager's state.
    let active = unsafe { GetActiveWindow() };
    if active.is_invalid() {
        // SAFETY: as above.
        unsafe { GetForegroundWindow() }
    } else {
        active
    }
}
