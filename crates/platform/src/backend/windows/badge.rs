//! The badge on the app's taskbar buttons: an overlay icon
//! (`ITaskbarList3::SetOverlayIcon`) drawn here, a red disc with the badge's
//! text, set on every window's button and again whenever the taskbar makes
//! one (`TaskbarButtonCreated`: a new window, or Explorer restarting).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::OnceLock;

use ::windows::Win32::Foundation::{COLORREF, HWND, RECT};
use ::windows::Win32::Graphics::Gdi::{
    ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS,
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CENTER,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject, DrawTextW, FW_BOLD, GdiFlush,
    OUT_DEFAULT_PRECIS, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use ::windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use ::windows::Win32::UI::Shell::{ITaskbarList3, TaskbarList};
use ::windows::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, DestroyIcon, HICON, MSGFLT_ALLOW, RegisterWindowMessageW,
};
use ::windows::core::{PCWSTR, w};

use super::translate::{badge_pixels, wide};
use crate::shell::IconImage;

/// The message the taskbar sends a window once its button exists.
pub(super) fn button_created() -> u32 {
    static MESSAGE: OnceLock<u32> = OnceLock::new();
    // SAFETY: a static, NUL-terminated name.
    *MESSAGE.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarButtonCreated")) })
}

/// Lets the taskbar's message reach `hwnd` when the app runs elevated.
pub(super) fn allow(hwnd: HWND) {
    // SAFETY: a filter change on the caller's own live window.
    let _ = unsafe { ChangeWindowMessageFilterEx(hwnd, button_created(), MSGFLT_ALLOW, None) };
}

/// The badge shown, shared by the app and the window procedure. No borrow
/// is held across a call to the taskbar, which may dispatch messages.
#[derive(Default)]
pub(super) struct Badge {
    taskbar: RefCell<Option<ITaskbarList3>>,
    shown: RefCell<Option<Rc<Overlay>>>,
}

/// A drawn overlay and the text read out for it.
struct Overlay {
    icon: HICON,
    text: Vec<u16>,
}

impl Drop for Overlay {
    fn drop(&mut self) {
        // SAFETY: the icon this overlay drew and alone holds; the taskbar
        // keeps its own copy.
        let _ = unsafe { DestroyIcon(self.icon) };
    }
}

impl Badge {
    /// Shows `text` on the buttons of `windows`, or clears them with `None`.
    pub(super) fn set(&self, text: Option<&str>, windows: &[HWND]) {
        let overlay = text.and_then(|text| {
            let icon = draw(text, super::tray::tray_icon_size())?;
            Some(Rc::new(Overlay {
                icon,
                text: wide(text),
            }))
        });
        let shown = self.shown.replace(overlay.clone());
        if shown.is_none() && overlay.is_none() {
            return;
        }
        for &hwnd in windows {
            self.show(hwnd, overlay.as_deref());
        }
    }

    /// Shows the badge on `hwnd`'s button, which the taskbar just made.
    pub(super) fn button_made(&self, hwnd: HWND) {
        let shown = self.shown.borrow().clone();
        if let Some(overlay) = shown {
            self.show(hwnd, Some(&overlay));
        }
    }

    fn show(&self, hwnd: HWND, overlay: Option<&Overlay>) {
        let Some(taskbar) = self.taskbar() else {
            return;
        };
        let (icon, text) = match overlay {
            Some(o) => (o.icon, PCWSTR(o.text.as_ptr())),
            None => (HICON::default(), PCWSTR::null()),
        };
        // SAFETY: a live window of this thread; the icon and text outlive
        // the call, and a null icon clears the overlay.
        let _ = unsafe { taskbar.SetOverlayIcon(hwnd, icon, text) };
    }

    /// The taskbar, connected on first use.
    fn taskbar(&self) -> Option<ITaskbarList3> {
        if let Some(taskbar) = self.taskbar.borrow().as_ref() {
            return Some(taskbar.clone());
        }
        // SAFETY: the thread entered an apartment at startup; the object is
        // initialized before use as its contract asks.
        let taskbar: ITaskbarList3 = unsafe {
            let taskbar: ITaskbarList3 =
                CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER).ok()?;
            taskbar.HrInit().ok()?;
            taskbar
        };
        *self.taskbar.borrow_mut() = Some(taskbar.clone());
        Some(taskbar)
    }
}

/// `text` drawn as a `size`-pixel badge icon the caller destroys.
fn draw(text: &str, size: u32) -> Option<HICON> {
    let side = size as i32;
    let mut text = wide(text);
    // `DrawTextW` takes the units without the terminator.
    text.pop();
    let height = match text.len() {
        0 | 1 => side * 13 / 16,
        2 => side * 11 / 16,
        _ => side * 9 / 16,
    };
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: side,
            biHeight: -side,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    // SAFETY: every object made here is selected out and deleted before the
    // block ends; `bits` points at the `size * size * 4` bytes of the DIB,
    // zeroed (black) at creation, read only after `GdiFlush` finishes the
    // drawing.
    let coverage = unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            return None;
        }
        let Ok(bitmap) = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
        else {
            let _ = DeleteDC(dc);
            return None;
        };
        let font = CreateFontW(
            -height,
            0,
            0,
            0,
            FW_BOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            0,
            w!("Segoe UI"),
        );
        let old_bitmap = SelectObject(dc, bitmap.into());
        let old_font = SelectObject(dc, font.into());
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, COLORREF(0x00FF_FFFF));
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: side,
            bottom: side,
        };
        DrawTextW(
            dc,
            &mut text,
            &mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        let _ = GdiFlush();
        let pixels = std::slice::from_raw_parts(bits.cast::<u8>(), (size * size * 4) as usize);
        // White ink on black: any channel is the text's coverage.
        let coverage: Vec<u8> = pixels.as_chunks::<4>().0.iter().map(|p| p[1]).collect();
        SelectObject(dc, old_font);
        SelectObject(dc, old_bitmap);
        let _ = DeleteObject(font.into());
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(dc);
        coverage
    };
    let rgba = badge_pixels(size, &coverage);
    let image = IconImage::from_rgba(size, size, rgba)?;
    super::tray::hicon(&image, size)
}
