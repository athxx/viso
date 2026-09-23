//! System services the window procedure and the app share: the clipboard,
//! cursors, appearance and wheel settings, the dark title bar, fullscreen.

use std::ffi::c_void;
use std::time::Duration;

use ::windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use ::windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
use ::windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use ::windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData,
};
use ::windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use ::windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use ::windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use ::windows::Win32::UI::WindowsAndMessaging::{
    GWL_STYLE, GetWindowLongPtrW, GetWindowPlacement, HWND_TOP, LoadCursorW,
    SPI_GETCLIENTAREAANIMATION, SPI_GETHIGHCONTRAST, SPI_GETWHEELSCROLLCHARS,
    SPI_GETWHEELSCROLLLINES, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
    SWP_NOZORDER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetCursor, SetWindowLongPtrW,
    SetWindowPlacement, SetWindowPos, SystemParametersInfoW, WINDOWPLACEMENT, WS_OVERLAPPEDWINDOW,
};
use ::windows::core::{BOOL, PCWSTR, w};

use super::translate;
use super::{Restore, Shared, WindowState, menu};
use crate::event::{Appearance, ColorScheme, CursorIcon, RawEvent};

const CF_UNICODETEXT: u32 = 13;

/// The clipboard, open for the life of the value. Another process may hold
/// it briefly, so opening retries for a few milliseconds.
struct ClipboardSession;

impl ClipboardSession {
    fn open(owner: Option<HWND>) -> Option<Self> {
        for _ in 0..10 {
            // SAFETY: opening the clipboard for this thread; closed on drop.
            if unsafe { OpenClipboard(owner) }.is_ok() {
                return Some(Self);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        None
    }
}

impl Drop for ClipboardSession {
    fn drop(&mut self) {
        // SAFETY: this thread opened the clipboard in `open`.
        let _ = unsafe { CloseClipboard() };
    }
}

pub(super) fn write_clipboard(owner: Option<HWND>, text: &str) {
    let units = translate::clipboard_units(text);
    let Some(_session) = ClipboardSession::open(owner) else {
        return;
    };
    // SAFETY: the clipboard is open on this thread. The block is sized for
    // `units`, filled while locked, and handed to the clipboard, which owns
    // it once `SetClipboardData` succeeds; on any failure it is freed here.
    unsafe {
        if EmptyClipboard().is_err() {
            return;
        }
        let Ok(block) = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2) else {
            return;
        };
        let dst = GlobalLock(block).cast::<u16>();
        if dst.is_null() {
            let _ = GlobalFree(Some(block));
            return;
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), dst, units.len());
        let _ = GlobalUnlock(block);
        if SetClipboardData(CF_UNICODETEXT, Some(HANDLE(block.0))).is_err() {
            let _ = GlobalFree(Some(block));
        }
    }
}

pub(super) fn read_clipboard(owner: Option<HWND>) -> Option<String> {
    // SAFETY: a format query with no pointers.
    unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) }.ok()?;
    let _session = ClipboardSession::open(owner)?;
    // SAFETY: the clipboard is open on this thread, so its data handle stays
    // valid until the session closes. The locked block holds `GlobalSize`
    // bytes; the slice stays within them and ends before the unlock.
    unsafe {
        let data = GetClipboardData(CF_UNICODETEXT).ok()?;
        let block = HGLOBAL(data.0);
        let src = GlobalLock(block).cast::<u16>().cast_const();
        if src.is_null() {
            return None;
        }
        let units = std::slice::from_raw_parts(src, GlobalSize(block) / 2);
        let text = translate::clipboard_text(units);
        let _ = GlobalUnlock(block);
        Some(text)
    }
}

/// Show `icon` now. `WM_SETCURSOR` calls this again whenever the pointer
/// moves over the client area.
pub(super) fn apply_cursor(icon: CursorIcon) {
    // SAFETY: loading a shared system cursor by its integer resource id
    // (`MAKEINTRESOURCE`), which needs no module and is never freed.
    unsafe {
        let cursor = translate::cursor_resource(icon)
            .and_then(|id| LoadCursorW(None, PCWSTR(usize::from(id) as *const u16)).ok());
        SetCursor(cursor);
    }
}

pub(super) fn set_cursor(state: &WindowState, icon: CursorIcon) {
    state.cursor.set(icon);
    // Over the client area the change shows at once; elsewhere the next
    // `WM_SETCURSOR` applies it.
    if state.tracking_leave.get() {
        apply_cursor(icon);
    }
}

fn read_dword(key: PCWSTR, value: PCWSTR) -> Option<u32> {
    let mut data = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: `data` and `size` describe a writable 4-byte buffer.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            value,
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut data).cast::<c_void>()),
            Some(&mut size),
        )
    };
    status.is_ok().then_some(data)
}

fn read_parameter<T: Default>(
    action: ::windows::Win32::UI::WindowsAndMessaging::SYSTEM_PARAMETERS_INFO_ACTION,
    ui_param: u32,
) -> Option<T> {
    let mut out = T::default();
    // SAFETY: `out` is a writable `T`, the type this action fills.
    unsafe {
        SystemParametersInfoW(
            action,
            ui_param,
            Some((&raw mut out).cast::<c_void>()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .ok()?;
    Some(out)
}

pub(super) fn read_appearance() -> Appearance {
    let light = read_dword(
        w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
        w!("AppsUseLightTheme"),
    );
    let mut contrast = HIGHCONTRASTW {
        cbSize: size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    // SAFETY: `contrast` is a writable `HIGHCONTRASTW` with its size set.
    let high_contrast = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            contrast.cbSize,
            Some((&raw mut contrast).cast::<c_void>()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .is_ok()
        && contrast.dwFlags.0 & HCF_HIGHCONTRASTON.0 != 0;
    let animate = read_parameter::<BOOL>(SPI_GETCLIENTAREAANIMATION, 0).is_none_or(|b| b.as_bool());
    Appearance {
        color_scheme: if light == Some(0) {
            ColorScheme::Dark
        } else {
            ColorScheme::Light
        },
        high_contrast,
        reduce_motion: !animate,
    }
}

/// Wheel lines per vertical notch and characters per horizontal notch.
pub(super) fn read_wheel_settings() -> (u32, u32) {
    (
        read_parameter::<u32>(SPI_GETWHEELSCROLLLINES, 0).unwrap_or(3),
        read_parameter::<u32>(SPI_GETWHEELSCROLLCHARS, 0).unwrap_or(3),
    )
}

pub(super) fn set_dark_title(hwnd: HWND, dark: bool) {
    let value = BOOL::from(dark);
    // SAFETY: `value` is a live `BOOL`, the type this attribute takes.
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&raw const value).cast::<c_void>(),
            size_of::<BOOL>() as u32,
        )
    };
}

/// Re-read the settings a broadcast may have changed. Every top-level window
/// gets the broadcast; only the first to see a change reports it.
pub(super) fn refresh_settings(shared: &Shared) {
    let appearance = read_appearance();
    let wheel = read_wheel_settings();
    let windows = {
        let mut q = shared.borrow_mut();
        q.wheel = wheel;
        if q.appearance == appearance {
            return;
        }
        q.appearance = appearance;
        q.events.push_back(RawEvent::AppearanceChanged(appearance));
        q.windows.iter().map(|w| w.hwnd.get()).collect::<Vec<_>>()
    };
    let dark = appearance.color_scheme == ColorScheme::Dark;
    for hwnd in windows {
        set_dark_title(hwnd, dark);
    }
}

/// Borderless over the window's monitor, the menu bar hidden; leaving
/// restores the saved frame, style and bar.
pub(super) fn set_fullscreen(state: &WindowState, fullscreen: bool) {
    let hwnd = state.hwnd.get();
    if state.fullscreen.borrow().is_some() == fullscreen {
        return;
    }
    // SAFETY: style, placement and position changes on our own live window;
    // every out-struct is initialized with its size field.
    unsafe {
        if fullscreen {
            let mut placement = WINDOWPLACEMENT {
                length: size_of::<WINDOWPLACEMENT>() as u32,
                ..Default::default()
            };
            if GetWindowPlacement(hwnd, &mut placement).is_err() {
                return;
            }
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                return;
            }
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
            *state.fullscreen.borrow_mut() = Some(Restore { style, placement });
            menu::detach(hwnd);
            SetWindowLongPtrW(hwnd, GWL_STYLE, style & !(WS_OVERLAPPEDWINDOW.0 as isize));
            let r = info.rcMonitor;
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOP),
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
            );
        } else {
            let Some(restore) = state.fullscreen.borrow_mut().take() else {
                return;
            };
            SetWindowLongPtrW(hwnd, GWL_STYLE, restore.style);
            let bar = state.shared.borrow().menu.clone();
            if let Some(bar) = bar {
                bar.attach(hwnd);
            }
            let _ = SetWindowPlacement(hwnd, &restore.placement);
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
            );
        }
    }
    state.push(RawEvent::FullscreenChanged {
        window: state.id,
        fullscreen,
    });
}
