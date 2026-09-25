//! Notifications as tray balloons, which Windows 10 and later show as
//! toasts. The icon lives on a message-only window, created with the first
//! notification and removed with the service.

use std::cell::{Cell, RefCell};

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, HWND_MESSAGE, IDI_APPLICATION, LoadIconW, WINDOW_EX_STYLE,
    WINDOW_STYLE,
};
use windows::core::w;

use crate::notifications::{Notification, Notifications};
use crate::reply::{Reply, ServiceError};

pub(super) struct TrayBalloons {
    app: String,
    window: Cell<Option<HWND>>,
    /// Whether the icon is in the tray.
    added: Cell<bool>,
    /// The id of the balloon showing.
    current: RefCell<Option<String>>,
}

impl TrayBalloons {
    pub(super) fn new(app: &str) -> Self {
        Self {
            app: app.to_owned(),
            window: Cell::new(None),
            added: Cell::new(false),
            current: RefCell::new(None),
        }
    }

    fn window(&self) -> windows::core::Result<HWND> {
        if let Some(window) = self.window.get() {
            return Ok(window);
        }
        // SAFETY: "STATIC" is a system class; a message-only window is never
        // shown and only receives the tray's messages.
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("viso-notifications"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                None,
                None,
            )
        }?;
        self.window.set(Some(window));
        Ok(window)
    }

    fn icon_data(window: HWND) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: window,
            uID: 1,
            ..Default::default()
        }
    }

    fn remove(&self) {
        let Some(window) = self.window.get() else {
            return;
        };
        if self.added.replace(false) {
            // SAFETY: the data names the icon this service added.
            let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &Self::icon_data(window)) };
        }
    }
}

/// Copy `text` into a fixed UTF-16 field, truncated to leave its NUL.
fn copy(field: &mut [u16], text: &str) {
    let room = field.len() - 1;
    let mut end = 0;
    for (slot, unit) in field[..room].iter_mut().zip(text.encode_utf16()) {
        *slot = unit;
        end += 1;
    }
    field[end] = 0;
}

impl Notifications for TrayBalloons {
    fn notify(&self, notification: Notification) -> Reply<()> {
        let window = match self.window() {
            Ok(window) => window,
            Err(e) => return Reply::err(ServiceError::Failed(e.message())),
        };
        let mut data = Self::icon_data(window);
        data.uFlags = NIF_ICON | NIF_TIP | NIF_INFO;
        // SAFETY: a stock system icon, shared and never destroyed.
        data.hIcon = unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or_default();
        copy(&mut data.szTip, &self.app);
        copy(&mut data.szInfoTitle, &notification.title);
        copy(&mut data.szInfo, &notification.body);
        data.dwInfoFlags = NIIF_INFO;
        let message = if self.added.get() {
            NIM_MODIFY
        } else {
            NIM_ADD
        };
        // SAFETY: the data is fully initialized and sized by `cbSize`.
        if !unsafe { Shell_NotifyIconW(message, &data) }.as_bool() {
            return Reply::err(ServiceError::Failed(
                "the tray refused the notification".into(),
            ));
        }
        self.added.set(true);
        *self.current.borrow_mut() = Some(notification.id);
        Reply::ready(Ok(()))
    }

    fn withdraw(&self, id: &str) {
        if self.current.borrow().as_deref() == Some(id) {
            self.current.borrow_mut().take();
            self.remove();
        }
    }
}

impl Drop for TrayBalloons {
    fn drop(&mut self) {
        self.remove();
        if let Some(window) = self.window.take() {
            // SAFETY: the message-only window this service created.
            let _ = unsafe { DestroyWindow(window) };
        }
    }
}
