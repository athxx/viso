//! Pointer lock: the cursor hidden and held in the window's middle, the
//! mouse's motion read from raw input.

use ::windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use ::windows::Win32::Graphics::Gdi::ClientToScreen;
use ::windows::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, MOUSE_MOVE_ABSOLUTE, RAWINPUT, RAWINPUTDEVICE,
    RAWINPUTDEVICE_FLAGS, RAWINPUTHEADER, RID_INPUT, RIDEV_REMOVE, RIM_TYPEMOUSE,
    RegisterRawInputDevices,
};
use ::windows::Win32::UI::WindowsAndMessaging::{
    ClipCursor, GetClientRect, GetForegroundWindow, SetCursor,
};

use super::{WindowState, system};
use crate::event::RawEvent;

/// The HID usage page and usage of a mouse.
const GENERIC_DESKTOP: u16 = 0x01;
const MOUSE: u16 = 0x02;

/// Locks the mouse's pointer to `state`'s window, or lets it go, reporting
/// the change. A lock is refused unless the window is the foreground one.
pub(super) fn set(state: &WindowState, lock: bool) {
    if lock == state.locked.get() {
        return;
    }
    let hwnd = state.hwnd.get();
    if !lock {
        release(state);
        return;
    }
    // SAFETY: plain queries and a registration for our own live window.
    let engaged = unsafe { GetForegroundWindow() == hwnd && register(hwnd, false) };
    if engaged {
        state.locked.set(true);
        clip(state);
        // SAFETY: clearing the cursor shape over our own window.
        unsafe {
            SetCursor(None);
        }
    }
    state.push(RawEvent::PointerLockChanged {
        window: state.id,
        locked: engaged,
    });
}

/// Lets the locked pointer go, reporting it; nothing if it was not locked.
pub(super) fn release(state: &WindowState) {
    if !state.locked.replace(false) {
        return;
    }
    // SAFETY: lifting the clip and the raw-input registration this lock
    // made; failing leaves the system as it was.
    unsafe {
        let _ = ClipCursor(None);
        register(HWND::default(), true);
    }
    system::apply_cursor(state.cursor.get(), state.dpi.get());
    state.push(RawEvent::PointerLockChanged {
        window: state.id,
        locked: false,
    });
}

/// Holds the cursor on the middle pixel of the window's client area, again
/// after the window moved or resized.
pub(super) fn clip(state: &WindowState) {
    if !state.locked.get() {
        return;
    }
    let hwnd = state.hwnd.get();
    let mut client = RECT::default();
    // SAFETY: reading our own live window's client rect and mapping a point
    // of it to the screen, then confining the cursor to that one pixel.
    unsafe {
        if GetClientRect(hwnd, &mut client).is_err() {
            return;
        }
        let mut mid = POINT {
            x: (client.left + client.right) / 2,
            y: (client.top + client.bottom) / 2,
        };
        if !ClientToScreen(hwnd, &mut mid).as_bool() {
            return;
        }
        let pin = RECT {
            left: mid.x,
            top: mid.y,
            right: mid.x + 1,
            bottom: mid.y + 1,
        };
        let _ = ClipCursor(Some(&pin));
    }
}

/// Reads the raw mouse motion `WM_INPUT` delivered and queues it as motion,
/// one raw count to a physical pixel.
pub(super) fn raw_input(state: &WindowState, lparam: LPARAM) {
    if !state.locked.get() {
        return;
    }
    let mut input = RAWINPUT::default();
    let mut size = size_of::<RAWINPUT>() as u32;
    // SAFETY: `lparam` is the `HRAWINPUT` of this `WM_INPUT`, and `input`
    // is a writable buffer of `size` bytes, as large as a mouse's record.
    let read = unsafe {
        GetRawInputData(
            HRAWINPUT(lparam.0 as *mut _),
            RID_INPUT,
            Some((&raw mut input).cast()),
            &mut size,
            size_of::<RAWINPUTHEADER>() as u32,
        )
    };
    if read == u32::MAX || input.header.dwType != RIM_TYPEMOUSE.0 {
        return;
    }
    // SAFETY: a record of type `RIM_TYPEMOUSE` holds the mouse member.
    let mouse = unsafe { input.data.mouse };
    if mouse.usFlags.0 & MOUSE_MOVE_ABSOLUTE.0 != 0 || (mouse.lLastX, mouse.lLastY) == (0, 0) {
        return;
    }
    let scale = state.scale();
    let (dx, dy) = (
        f64::from(mouse.lLastX) / scale,
        f64::from(mouse.lLastY) / scale,
    );
    crate::event::queue_motion(&mut state.shared.borrow_mut().events, state.id, dx, dy);
}

/// Asks raw mouse input for `hwnd`, or with `remove` stops it. Returns
/// whether the system took the request.
///
/// # Safety
///
/// `hwnd` is a live window of this thread, or null with `remove`.
unsafe fn register(hwnd: HWND, remove: bool) -> bool {
    let device = RAWINPUTDEVICE {
        usUsagePage: GENERIC_DESKTOP,
        usUsage: MOUSE,
        dwFlags: if remove {
            RIDEV_REMOVE
        } else {
            RAWINPUTDEVICE_FLAGS(0)
        },
        hwndTarget: hwnd,
    };
    // SAFETY: one well-formed device record of its own size; the caller
    // vouches for the window.
    unsafe { RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32).is_ok() }
}
