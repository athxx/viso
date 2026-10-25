//! The tray icon: `Shell_NotifyIconW` on a hidden window of the UI thread,
//! which takes the icon's callback messages and pops up its menu, and adds
//! the icon again when Explorer restarts (`TaskbarCreated`, a broadcast only
//! top-level windows receive, so the window is a hidden top-level one rather
//! than message-only).

use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use ::windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use ::windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject, HBITMAP,
};
use ::windows::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use ::windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION,
    NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use ::windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, ChangeWindowMessageFilterEx, CreateIconIndirect, CreatePopupMenu,
    CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow, GWLP_USERDATA,
    GetWindowLongPtrW, HICON, ICONINFO, MSGFLT_ALLOW, PostMessageW, RegisterClassExW,
    RegisterWindowMessageW, SM_CXSMICON, SetForegroundWindow, SetWindowLongPtrW, TPM_BOTTOMALIGN,
    TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx, WM_APP, WM_CONTEXTMENU,
    WM_LBUTTONDBLCLK, WM_NCCREATE, WM_NCDESTROY, WM_NULL, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_POPUP,
};
use ::windows::core::{PCWSTR, w};

use super::Shared;
use super::translate::{MenuAction, MenuPlan};
use crate::event::RawEvent;
use crate::menu::SystemAction;
use crate::shell::{IconImage, Tray, TrayEvent};

const CLASS_NAME: PCWSTR = w!("VisoTrayWindow");
/// The icon's callback message.
const WM_TRAY: u32 = WM_APP + 4;
/// The keyboard's select on the icon (`NIN_SELECT | NINF_KEY`).
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
/// The one icon the app shows.
const ICON_ID: u32 = 1;

/// The tray icon shown, with the hidden window that owns it.
pub(super) struct TrayIcon {
    state: Rc<TrayState>,
}

/// What the hidden window's procedure reads.
struct TrayState {
    shared: Shared,
    hwnd: Cell<HWND>,
    /// The broadcast Explorer sends when the taskbar comes back.
    taskbar_created: u32,
    shown: RefCell<Shown>,
    /// A double click was just reported: the select that follows it is the
    /// second press's, not a click of its own.
    after_double: Cell<bool>,
}

struct Shown {
    tray: Tray,
    plan: MenuPlan,
    icon: HICON,
}

impl TrayIcon {
    /// Shows `tray` in the notification area, or `None` when the window or
    /// the icon cannot be made.
    pub(super) fn new(hinstance: HINSTANCE, shared: &Shared, tray: &Tray) -> Option<TrayIcon> {
        register_class(hinstance)?;
        // SAFETY: a static, NUL-terminated name.
        let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        let state = Rc::new(TrayState {
            shared: shared.clone(),
            hwnd: Cell::new(HWND::default()),
            taskbar_created,
            shown: RefCell::new(Shown {
                tray: tray.clone(),
                plan: MenuPlan::build_items(&tray.menu),
                icon: hicon(&tray.icon, tray_icon_size())?,
            }),
            after_double: Cell::new(false),
        });
        // The procedure adopts this count at `WM_NCCREATE` and releases it
        // at `WM_NCDESTROY`.
        let param = Rc::into_raw(state.clone());
        // SAFETY: the class is registered; the window is never shown; `param`
        // is a leaked `Rc<TrayState>` the procedure takes ownership of.
        let created = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                CLASS_NAME,
                w!("Viso tray"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(hinstance),
                Some(param.cast()),
            )
        };
        if created.is_err() {
            if state.hwnd.get().is_invalid() {
                // SAFETY: `WM_NCCREATE` never ran, so the count leaked above
                // was never adopted; reclaim it.
                drop(unsafe { Rc::from_raw(param) });
            }
            // SAFETY: the icon this state created and alone holds.
            let _ = unsafe { DestroyIcon(state.shown.borrow().icon) };
            return None;
        }
        // An elevated app still hears Explorer's restart.
        // SAFETY: a filter change on our own live window.
        let _ = unsafe {
            ChangeWindowMessageFilterEx(state.hwnd.get(), taskbar_created, MSGFLT_ALLOW, None)
        };
        state.add();
        Some(TrayIcon { state })
    }

    /// Shows `tray` in place of what the icon shows, redoing only what
    /// changed.
    pub(super) fn update(&self, tray: &Tray) {
        let mut shown = self.state.shown.borrow_mut();
        if !tray.icon.same(&shown.tray.icon)
            && let Some(icon) = hicon(&tray.icon, tray_icon_size())
        {
            // SAFETY: the old icon is ours; the shell copied it when it was
            // shown, and the new one replaces it below.
            let _ = unsafe { DestroyIcon(shown.icon) };
            shown.icon = icon;
        }
        if tray.menu != shown.tray.menu {
            shown.plan = MenuPlan::build_items(&tray.menu);
        }
        shown.tray = tray.clone();
        drop(shown);
        self.state.notify(NIM_MODIFY);
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        self.state.notify(NIM_DELETE);
        // SAFETY: the window and icon this value created; the window's
        // `WM_NCDESTROY` releases the procedure's count of the state.
        unsafe {
            let _ = DestroyIcon(self.state.shown.borrow().icon);
            let _ = DestroyWindow(self.state.hwnd.get());
        }
    }
}

impl TrayState {
    /// Adds the icon to the notification area, with the version whose
    /// callbacks name the event and anchor.
    fn add(&self) {
        self.notify(NIM_ADD);
        self.notify(NIM_SETVERSION);
    }

    fn notify(&self, message: ::windows::Win32::UI::Shell::NOTIFY_ICON_MESSAGE) {
        let shown = self.shown.borrow();
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd.get(),
            uID: ICON_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: WM_TRAY,
            hIcon: shown.icon,
            ..Default::default()
        };
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        for (slot, unit) in data
            .szTip
            .iter_mut()
            .take(127)
            .zip(shown.tray.tooltip.encode_utf16())
        {
            *slot = unit;
        }
        // SAFETY: `data` is a fully initialized record naming our own window.
        let _ = unsafe { Shell_NotifyIconW(message, &data) };
    }

    /// The message the icon sent: `event` its kind, `(x, y)` its anchor in
    /// screen pixels.
    fn callback(&self, event: u32, x: i32, y: i32) {
        match event {
            NIN_SELECT | NIN_KEYSELECT => {
                if self.after_double.replace(false) {
                    return;
                }
                let on_click = {
                    let shown = self.shown.borrow();
                    shown.tray.menu_on_click && !shown.plan.entries.is_empty()
                };
                if on_click {
                    self.pop_menu(x, y);
                } else {
                    self.push(RawEvent::Tray(TrayEvent::Click));
                }
            }
            WM_LBUTTONDBLCLK => {
                self.after_double.set(true);
                self.push(RawEvent::Tray(TrayEvent::DoubleClick));
            }
            WM_CONTEXTMENU => self.pop_menu(x, y),
            _ => {}
        }
    }

    fn push(&self, event: RawEvent) {
        self.shared.borrow_mut().events.push_back(event);
    }

    /// Tracks the menu at `(x, y)` until the user picks or leaves it. No
    /// borrow is held across the tracking, whose loop dispatches messages
    /// that may change the tray; the pick resolves against the plan shown.
    fn pop_menu(&self, x: i32, y: i32) {
        let hwnd = self.hwnd.get();
        let plan = self.shown.borrow().plan.clone();
        if plan.entries.is_empty() {
            return;
        }
        // SAFETY: the popup is ours from creation to its destruction below;
        // the window is our own live one, made foreground so the menu closes
        // when the user clicks elsewhere.
        let picked = unsafe {
            let Ok(popup) = CreatePopupMenu() else {
                return;
            };
            super::menu::append(popup, &plan.entries);
            let _ = SetForegroundWindow(hwnd);
            let picked = TrackPopupMenuEx(
                popup,
                (TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_BOTTOMALIGN).0,
                x,
                y,
                hwnd,
                None,
            );
            let _ = DestroyMenu(popup);
            // The documented nudge that lets the next click on the icon
            // reopen the menu.
            let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            picked.0 as u32
        };
        match u16::try_from(picked).ok().and_then(|id| plan.action(id)) {
            Some(MenuAction::Command(id)) => self.push(RawEvent::MenuCommand { id }),
            Some(MenuAction::System(SystemAction::Quit)) => {
                self.shared.borrow_mut().should_exit = true;
            }
            // The window-bound actions have no window here.
            Some(MenuAction::System(_)) | None => {}
        }
    }
}

fn register_class(hinstance: HINSTANCE) -> Option<()> {
    thread_local! {
        static REGISTERED: Cell<bool> = const { Cell::new(false) };
    }
    if REGISTERED.with(Cell::get) {
        return Some(());
    }
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(tray_proc),
        hInstance: hinstance,
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };
    // SAFETY: `class` is fully initialized and its strings are static.
    if unsafe { RegisterClassExW(&class) } == 0 {
        return None;
    }
    REGISTERED.with(|r| r.set(true));
    Some(())
}

/// The tray icon's side in pixels at the system's scale.
fn tray_icon_size() -> u32 {
    // SAFETY: plain metric queries.
    let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()) };
    u32::try_from(size).ok().filter(|&s| s > 0).unwrap_or(16)
}

/// `icon` fitted into a `size`-pixel square, centered, as an `HICON` the
/// caller destroys.
pub(super) fn hicon(icon: &IconImage, size: u32) -> Option<HICON> {
    let (w, h) = (icon.width(), icon.height());
    let scale = f64::from(size) / f64::from(w.max(h));
    let fw = ((f64::from(w) * scale).round() as u32).clamp(1, size);
    let fh = ((f64::from(h) * scale).round() as u32).clamp(1, size);
    let fitted = icon.resized(fw, fh);
    let (ox, oy) = ((size - fw) / 2, (size - fh) / 2);
    let mut bgra = vec![0u8; (size * size * 4) as usize];
    for y in 0..fh {
        for x in 0..fw {
            let s = &fitted.rgba()[((y * fw + x) * 4) as usize..][..4];
            let d = &mut bgra[(((y + oy) * size + x + ox) * 4) as usize..][..4];
            d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
        }
    }
    let side = size as i32;
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: side,
            // Negative: rows run top-down, as the pixels do.
            biHeight: -side,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    // SAFETY: `info` describes a `size × size` 32-bit top-down DIB; on
    // success `bits` points at its `size * size * 4` bytes, which the copy
    // fills exactly. Each bitmap is deleted once the icon holds its copy.
    unsafe {
        let color: HBITMAP =
            CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        if bits.is_null() {
            let _ = DeleteObject(color.into());
            return None;
        }
        std::ptr::copy_nonoverlapping(bgra.as_ptr(), bits.cast::<u8>(), bgra.len());
        // An all-zero mask: the color bitmap's alpha decides what shows.
        let mask_bytes = vec![0u8; (size.div_ceil(16) * 2 * size) as usize];
        let mask = CreateBitmap(side, side, 1, 1, Some(mask_bytes.as_ptr().cast()));
        let made = CreateIconIndirect(&ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        });
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        made.ok()
    }
}

/// The hidden window's procedure.
unsafe extern "system" fn tray_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCCREATE {
        // SAFETY: for `WM_NCCREATE`, `lparam` points at the `CREATESTRUCTW`
        // whose `lpCreateParams` is the leaked `Rc<TrayState>` `TrayIcon::new`
        // passed; the window adopts that count here.
        unsafe {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            let state = create.lpCreateParams as *const TrayState;
            if !state.is_null() {
                (*state).hwnd.set(hwnd);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
            }
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
    }
    // SAFETY: `GWLP_USERDATA` is zero or the pointer adopted above, valid
    // until `WM_NCDESTROY` clears the slot and releases it; a count is held
    // for the call so nothing nested frees it under this frame.
    unsafe {
        let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const TrayState;
        if raw.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        if msg == WM_NCDESTROY {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            drop(Rc::from_raw(raw));
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        Rc::increment_strong_count(raw);
        let state = Rc::from_raw(raw);
        let handled = catch_unwind(AssertUnwindSafe(|| {
            if msg == WM_TRAY {
                // Version 4: the event in the low word of `lparam`, the
                // anchor's screen coordinates in `wparam`.
                let x = i32::from((wparam.0 & 0xFFFF) as u16 as i16);
                let y = i32::from(((wparam.0 >> 16) & 0xFFFF) as u16 as i16);
                state.callback((lparam.0 as usize & 0xFFFF) as u32, x, y);
                true
            } else if msg == state.taskbar_created && msg != 0 {
                state.add();
                true
            } else {
                false
            }
        }));
        match handled {
            Ok(true) => LRESULT(0),
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
