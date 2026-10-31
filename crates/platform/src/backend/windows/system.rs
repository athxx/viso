//! System services the window procedure and the app share: cursors,
//! appearance and wheel settings, the dark title bar, fullscreen.

use std::cell::RefCell;
use std::ffi::c_void;
use std::time::Duration;

use ::windows::Win32::Foundation::{HWND, LPARAM, RECT};
use ::windows::Win32::Globalization::GetUserDefaultLocaleName;
use ::windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
use ::windows::Win32::Graphics::Gdi::{
    CreateBitmap, DeleteObject, EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use ::windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use ::windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use ::windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use ::windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
use ::windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, GWL_STYLE, GetWindowLongPtrW, GetWindowPlacement, HCURSOR, HWND_TOP,
    ICONINFO, LoadCursorW, SPI_GETCLIENTAREAANIMATION, SPI_GETHIGHCONTRAST,
    SPI_GETWHEELSCROLLCHARS, SPI_GETWHEELSCROLLLINES, SWP_FRAMECHANGED, SWP_NOMOVE,
    SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetCursor,
    SetWindowLongPtrW, SetWindowPlacement, SetWindowPos, SystemParametersInfoW, WINDOWPLACEMENT,
    WS_OVERLAPPEDWINDOW,
};
use ::windows::core::{BOOL, PCWSTR, w};

use super::translate;
use super::{Restore, Shared, WindowState, menu};
use crate::cursor::{CursorImage, CustomCursor};
use crate::event::{Appearance, ColorScheme, CursorIcon, RawEvent};
use crate::locale;
use crate::placement::DeskRect;

/// Show `icon` now, an image cursor at `dpi`. `WM_SETCURSOR` calls this
/// again whenever the pointer moves over the client area.
pub(super) fn apply_cursor(icon: CursorIcon, dpi: u32) {
    let cursor = match icon {
        CursorIcon::Custom(custom) => image_cursor(custom, dpi).or_else(arrow),
        _ => translate::cursor_resource(icon).and_then(system_cursor),
    };
    // SAFETY: `cursor` is a shared system cursor or one this thread built
    // and keeps alive for the process.
    unsafe {
        SetCursor(cursor);
    }
}

fn arrow() -> Option<HCURSOR> {
    translate::cursor_resource(CursorIcon::Default).and_then(system_cursor)
}

fn system_cursor(id: u16) -> Option<HCURSOR> {
    // SAFETY: loading a shared system cursor by its integer resource id
    // (`MAKEINTRESOURCE`), which needs no module and is never freed.
    unsafe { LoadCursorW(None, PCWSTR(usize::from(id) as *const u16)).ok() }
}

thread_local! {
    /// The image cursors built so far, each at the DPI it was built for.
    static IMAGE_CURSORS: RefCell<Vec<(CustomCursor, u32, Option<HCURSOR>)>> =
        const { RefCell::new(Vec::new()) };
}

/// The cursor showing `cursor`'s image at `dpi`, built the first time.
fn image_cursor(cursor: CustomCursor, dpi: u32) -> Option<HCURSOR> {
    IMAGE_CURSORS.with_borrow_mut(|built| {
        if let Some(&(_, _, h)) = built.iter().find(|(c, d, _)| *c == cursor && *d == dpi) {
            return h;
        }
        let h = build_cursor(&cursor.image(), f64::from(dpi) / 96.0);
        built.push((cursor, dpi, h));
        h
    })
}

/// A cursor of `image` at `scale` pixels to a point: a 32-bit colour bitmap
/// whose alpha shapes it, over an empty mask.
fn build_cursor(image: &CursorImage, scale: f64) -> Option<HCURSOR> {
    let image = image.at_scale(scale);
    let (w, h) = (image.width as i32, image.height as i32);
    let bgra: Vec<u8> = image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[2], p[1], p[0], p[3]])
        .collect();
    // A monochrome mask's rows are padded to 16 bits.
    let mask = vec![0u8; (image.width as usize).div_ceil(16) * 2 * image.height as usize];
    // SAFETY: both bitmaps are created from buffers of exactly their
    // `w × h` size at their bit depth, which `CreateBitmap` copies; the
    // icon copies the bitmaps in turn, so they are deleted after.
    unsafe {
        let color = CreateBitmap(w, h, 1, 32, Some(bgra.as_ptr().cast()));
        let mono = CreateBitmap(w, h, 1, 1, Some(mask.as_ptr().cast()));
        let info = ICONINFO {
            fIcon: BOOL(0),
            xHotspot: image.hotspot[0],
            yHotspot: image.hotspot[1],
            hbmMask: mono,
            hbmColor: color,
        };
        let icon = if color.is_invalid() || mono.is_invalid() {
            None
        } else {
            CreateIconIndirect(&info).ok()
        };
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mono.into());
        icon.map(|icon| HCURSOR(icon.0))
    }
}

pub(super) fn set_cursor(state: &WindowState, icon: CursorIcon) {
    state.cursor.set(icon);
    // Over the client area the change shows at once; elsewhere the next
    // `WM_SETCURSOR` applies it.
    if state.tracking_leave.get() && !state.locked.get() {
        apply_cursor(icon, state.dpi.get());
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

/// Whether the energy saver (battery saver before Windows 11) is on.
fn energy_saver() -> bool {
    let mut status = SYSTEM_POWER_STATUS::default();
    // SAFETY: `status` is a writable `SYSTEM_POWER_STATUS`.
    unsafe { GetSystemPowerStatus(&mut status) }.is_ok() && status.SystemStatusFlag & 1 != 0
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
        // Settings > Personalization > Colors > Transparency effects.
        reduce_transparency: read_dword(
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("EnableTransparency"),
        ) == Some(0),
        low_power: energy_saver(),
        text_scale: read_text_scale(),
        // Settings > Accessibility > Visual effects > Always show scrollbars.
        overlay_scroll_bars: read_dword(
            w!("Control Panel\\Accessibility"),
            w!("DynamicScrollbars"),
        ) != Some(0),
        // Settings > Personalization > Colors > Accent color, `0xAABBGGRR`.
        accent: read_dword(w!("Software\\Microsoft\\Windows\\DWM"), w!("AccentColor")).map(
            |abgr| {
                let channel = |shift: u32| ((abgr >> shift) & 0xff) as f32 / 255.0;
                [channel(0), channel(8), channel(16)]
            },
        ),
        // Settings > Bluetooth & devices > Mouse > Double-click speed.
        // SAFETY: `GetDoubleClickTime` takes nothing and only reads a
        // system setting.
        double_click: Some(Duration::from_millis(u64::from(unsafe {
            GetDoubleClickTime()
        })))
        .filter(|d| !d.is_zero()),
    }
}

/// Settings > Accessibility > Text size, a percentage from 100 to 225.
fn read_text_scale() -> f32 {
    read_dword(
        w!("Software\\Microsoft\\Accessibility"),
        w!("TextScaleFactor"),
    )
    .filter(|percent| (100..=500).contains(percent))
    .map_or(1.0, |percent| percent as f32 / 100.0)
}

/// The user's default locale, as a BCP 47 tag.
pub(super) fn read_locale() -> String {
    // LOCALE_NAME_MAX_LENGTH.
    let mut name = [0u16; 85];
    // SAFETY: `name` is a writable buffer of the length passed.
    let len = unsafe { GetUserDefaultLocaleName(&mut name) };
    if len <= 1 {
        return "und".to_owned();
    }
    locale::bcp47(&String::from_utf16_lossy(&name[..len as usize - 1]))
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
    let locale = read_locale();
    let windows = {
        let mut q = shared.borrow_mut();
        q.wheel = wheel;
        if q.locale != locale {
            q.locale.clone_from(&locale);
            q.events.push_back(RawEvent::LocaleChanged { locale });
        }
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
/// Each monitor's work area (outside the taskbar), in physical desktop
/// pixels.
pub(super) fn work_areas() -> Vec<DeskRect> {
    unsafe extern "system" fn each(
        monitor: HMONITOR,
        _dc: HDC,
        _bounds: *mut RECT,
        areas: LPARAM,
    ) -> BOOL {
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: `areas` is the `Vec` `work_areas` lent for the enumeration,
        // which runs synchronously inside it; `info` is sized.
        unsafe {
            if GetMonitorInfoW(monitor, &mut info).as_bool() {
                let r = info.rcWork;
                (*(areas.0 as *mut Vec<DeskRect>)).push(DeskRect::new(
                    f64::from(r.left),
                    f64::from(r.top),
                    f64::from(r.right - r.left),
                    f64::from(r.bottom - r.top),
                ));
            }
        }
        true.into()
    }
    let mut areas: Vec<DeskRect> = Vec::new();
    // SAFETY: the callback only pushes into `areas`, which outlives the
    // synchronous enumeration.
    let _ = unsafe { EnumDisplayMonitors(None, None, Some(each), LPARAM(&raw mut areas as isize)) };
    areas
}

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
