//! The Windows accessibility bridge (ADR 0030): an AccessKit subclassing
//! adapter on each window.
//!
//! UI Automation asks for the tree inside `WM_GETOBJECT`, while the adapter
//! holds its own state borrowed, and calls the action handler on threads of
//! its own. Both handlers therefore post [`WM_ACCESS`] to the window: its
//! procedure turns the message into a queued [`RawEvent::Accessibility`] on the
//! UI thread, and the post itself wakes a loop blocked in `GetMessageW`. The
//! activation handler answers with no tree; the facade's next frame sends the
//! full one through `update_accessibility`.
//!
//! [`RawEvent::Accessibility`]: crate::RawEvent::Accessibility

use std::ffi::c_void;

use ::windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use ::windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use accesskit::{ActionHandler, ActionRequest, ActivationHandler, TreeUpdate};
use accesskit_windows::SubclassingAdapter;

use crate::accessibility::AccessRequest;

/// The private message carrying an [`AccessRequest`]: the kind in `WPARAM`,
/// the target id in `LPARAM` (Windows targets are 64-bit).
pub(super) const WM_ACCESS: u32 = WM_APP + 1;

/// Subclass `hwnd` for accessibility. Call before the window is first shown.
pub(super) fn attach(hwnd: HWND) -> SubclassingAdapter {
    let poster = Poster(hwnd.0 as usize);
    SubclassingAdapter::new(hwnd, poster, poster)
}

/// Apply `update` if an assistive technology has asked for the tree.
pub(super) fn update(adapter: &mut SubclassingAdapter, update: TreeUpdate) {
    if let Some(events) = adapter.update_if_active(|| update) {
        events.raise();
    }
}

/// The request a [`WM_ACCESS`] message carries.
pub(super) fn decode(wparam: WPARAM, lparam: LPARAM) -> Option<AccessRequest> {
    AccessRequest::from_words(wparam.0, lparam.0 as u64)
}

/// Both handlers. Holds the window handle as an integer so the action
/// handler can be sent to UI Automation's threads; posting to a window that
/// has since been destroyed fails harmlessly.
#[derive(Clone, Copy)]
struct Poster(usize);

impl Poster {
    fn post(self, request: AccessRequest) {
        let (kind, target) = request.to_words();
        // SAFETY: `PostMessageW` only queues the message; an invalid or
        // destroyed handle makes it return an error, which is ignored.
        let _ = unsafe {
            PostMessageW(
                Some(HWND(self.0 as *mut c_void)),
                WM_ACCESS,
                WPARAM(kind),
                LPARAM(target as isize),
            )
        };
    }
}

impl ActivationHandler for Poster {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        self.post(AccessRequest::Activated);
        None
    }
}

impl ActionHandler for Poster {
    fn do_action(&mut self, request: ActionRequest) {
        if let Some(request) = AccessRequest::from_accesskit(&request) {
            self.post(request);
        }
    }
}
