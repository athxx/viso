//! The Windows services: the common item dialogs, the share sheet, tray
//! balloons, the Credential Manager. Windows gates none of these behind a
//! permission, and has no haptics.

mod credentials;
mod dialogs;
mod share;
mod tray;

use std::rc::Rc;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

use super::Ungated;
use crate::registry::Services;
use crate::unsupported::Unsupported;

pub(crate) fn services(app: &str) -> Services {
    Services::from_parts(
        Rc::new(dialogs::ItemDialogs),
        Rc::new(share::ShareSheet::new(app)),
        Rc::new(tray::TrayBalloons::new(app)),
        Rc::new(Ungated),
        Rc::new(credentials::Credentials::new(app)),
        Rc::new(Unsupported),
    )
}

/// The window a dialog or sheet belongs to: this thread's active window,
/// else the foreground one.
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
