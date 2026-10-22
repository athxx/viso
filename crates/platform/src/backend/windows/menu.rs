//! The menu bar: one `HMENU` tree per window built from a shared plan, and
//! one accelerator table for the app.

use ::windows::Win32::Foundation::{HWND, POINT};
use ::windows::Win32::Graphics::Gdi::ClientToScreen;
use ::windows::Win32::UI::Input::KeyboardAndMouse::VkKeyScanW;
use ::windows::Win32::UI::WindowsAndMessaging::{
    ACCEL, ACCEL_VIRT_FLAGS, AppendMenuW, CreateAcceleratorTableW, CreateMenu, CreatePopupMenu,
    DestroyAcceleratorTable, DestroyMenu, DrawMenuBar, FALT, FCONTROL, FSHIFT, FVIRTKEY, GetMenu,
    HACCEL, HMENU, MENU_ITEM_FLAGS, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING,
    SetForegroundWindow, SetMenu, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TPM_TOPALIGN,
    TrackPopupMenuEx,
};
use ::windows::core::PCWSTR;

use super::translate::{self, MenuAction, MenuEntry, MenuPlan};
use crate::menu::Menu;
use crate::{EditLabels, EditMenu};

pub(super) struct NativeMenu {
    plan: MenuPlan,
    accel: Option<HACCEL>,
}

impl NativeMenu {
    /// `None` for a menu with no entries: the windows then show no bar.
    pub(super) fn build(menu: &Menu) -> Option<Self> {
        let plan = MenuPlan::build(menu);
        if plan.entries.is_empty() {
            return None;
        }
        let rows = plan.accels.iter().filter_map(accel_row).collect::<Vec<_>>();
        let accel = if rows.is_empty() {
            None
        } else {
            // SAFETY: `rows` is a valid slice of initialized `ACCEL`s.
            unsafe { CreateAcceleratorTableW(&rows) }.ok()
        };
        Some(Self { plan, accel })
    }

    pub(super) fn accel(&self) -> Option<HACCEL> {
        self.accel
    }

    pub(super) fn action(&self, id: u16) -> Option<MenuAction> {
        self.plan.action(id)
    }

    /// Give `hwnd` its own copy of the bar, replacing any previous one. The
    /// caller restores the client size the bar's height changed.
    pub(super) fn attach(&self, hwnd: HWND) {
        // SAFETY: menus built here are owned by `hwnd` once `SetMenu`
        // succeeds (destroyed with it); on failure the new tree is destroyed
        // at once. The replaced bar is ours and no longer attached.
        unsafe {
            let Ok(bar) = CreateMenu() else { return };
            append(bar, &self.plan.entries);
            let old = GetMenu(hwnd);
            if SetMenu(hwnd, Some(bar)).is_err() {
                let _ = DestroyMenu(bar);
                return;
            }
            if !old.is_invalid() {
                let _ = DestroyMenu(old);
            }
            let _ = DrawMenuBar(hwnd);
        }
    }
}

impl Drop for NativeMenu {
    fn drop(&mut self) {
        if let Some(accel) = self.accel {
            // SAFETY: the table this value created and alone owns.
            let _ = unsafe { DestroyAcceleratorTable(accel) };
        }
    }
}

/// The commands of a context menu's edits.
pub(super) const CUT: u32 = 1;
pub(super) const COPY: u32 = 2;
pub(super) const PASTE: u32 = 3;
pub(super) const SELECT_ALL: u32 = 4;

/// Track `menu` titled by `labels` with its corner at client pixel `at` of
/// `hwnd` until the user picks or leaves it: the command picked, or zero.
pub(super) fn track_context(hwnd: HWND, at: POINT, menu: &EditMenu, labels: &EditLabels) -> u32 {
    let items = [
        (CUT, labels.cut, "Ctrl+X", menu.cut),
        (COPY, labels.copy, "Ctrl+C", menu.copy),
        (PASTE, labels.paste, "Ctrl+V", menu.paste),
        (SELECT_ALL, labels.select_all, "Ctrl+A", menu.select_all),
    ];
    // SAFETY: the popup is ours from creation to its destruction below;
    // every label is NUL-terminated and outlives its `AppendMenuW`; the
    // point and window are our own live client's.
    unsafe {
        let Ok(popup) = CreatePopupMenu() else {
            return 0;
        };
        for (id, title, keys, enabled) in items {
            if id == SELECT_ALL {
                let _ = AppendMenuW(popup, MF_SEPARATOR, 0, PCWSTR::null());
            }
            let text: Vec<u16> = format!("{title}\t{keys}")
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let flags = if enabled {
                MF_STRING
            } else {
                MENU_ITEM_FLAGS(MF_STRING.0 | MF_GRAYED.0)
            };
            let _ = AppendMenuW(popup, flags, id as usize, PCWSTR(text.as_ptr()));
        }
        let mut screen = at;
        let _ = ClientToScreen(hwnd, &mut screen);
        let _ = SetForegroundWindow(hwnd);
        let picked = TrackPopupMenuEx(
            popup,
            (TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_TOPALIGN).0,
            screen.x,
            screen.y,
            hwnd,
            None,
        );
        let _ = DestroyMenu(popup);
        picked.0 as u32
    }
}

/// Remove `hwnd`'s bar.
pub(super) fn detach(hwnd: HWND) {
    // SAFETY: the detached bar is ours and no longer attached to anything.
    unsafe {
        let old = GetMenu(hwnd);
        if old.is_invalid() {
            return;
        }
        if SetMenu(hwnd, None).is_ok() {
            let _ = DestroyMenu(old);
        }
        let _ = DrawMenuBar(hwnd);
    }
}

/// # Safety
///
/// `menu` must be a live menu handle owned by the caller.
unsafe fn append(menu: HMENU, entries: &[MenuEntry]) {
    for entry in entries {
        // SAFETY: every label is NUL-terminated and outlives its call; a
        // popup handle appended to `menu` becomes owned by it.
        unsafe {
            match entry {
                MenuEntry::Popup { name, items } => {
                    let Ok(popup) = CreatePopupMenu() else {
                        continue;
                    };
                    append(popup, items);
                    if AppendMenuW(menu, MF_POPUP, popup.0 as usize, PCWSTR(name.as_ptr())).is_err()
                    {
                        let _ = DestroyMenu(popup);
                    }
                }
                MenuEntry::Item { id, text, enabled } => {
                    let flags = if *enabled {
                        MF_STRING
                    } else {
                        MENU_ITEM_FLAGS(MF_STRING.0 | MF_GRAYED.0)
                    };
                    let _ = AppendMenuW(menu, flags, usize::from(*id), PCWSTR(text.as_ptr()));
                }
                MenuEntry::Separator => {
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                }
            }
        }
    }
}

/// Resolve a row's key to a virtual key. Named keys map directly; a
/// punctuation key is looked up in the active layout, which may also require
/// Shift (`+` on US layouts).
fn accel_row(spec: &translate::AccelSpec) -> Option<ACCEL> {
    let (vk, mut shift) = match translate::accel_vk(&spec.key) {
        Some(vk) => (vk, false),
        None => {
            let mut chars = spec.key.chars();
            let c = chars.next()?;
            if chars.next().is_some() || !c.is_ascii() {
                return None;
            }
            // SAFETY: a pure keyboard-layout query.
            let scan = unsafe { VkKeyScanW(c as u16) };
            if scan == -1 {
                return None;
            }
            let scan = scan as u16;
            (scan & 0xFF, scan & 0x100 != 0)
        }
    };
    shift |= spec.shift;
    let mut flags = FVIRTKEY.0;
    if spec.control {
        flags |= FCONTROL.0;
    }
    if shift {
        flags |= FSHIFT.0;
    }
    if spec.alt {
        flags |= FALT.0;
    }
    Some(ACCEL {
        fVirt: ACCEL_VIRT_FLAGS(flags),
        key: vk,
        cmd: spec.id,
    })
}
