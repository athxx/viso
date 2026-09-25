//! The Linux accessibility bridge (ADR 0030): an AccessKit AT-SPI adapter per
//! window, shared by the X11 and Wayland backends.
//!
//! The adapter talks to the accessibility bus from a thread of its own and
//! calls every handler there, so the handlers only hand the request to the
//! loop through its [`Waker`]. The activation handler answers with no tree;
//! the facade's next frame sends the full one through
//! [`LinuxAccess::update`].

use accesskit::{ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, TreeUpdate};
use accesskit_unix::Adapter;

use super::{Wake, Waker};
use crate::accessibility::AccessRequest;
use crate::control::WindowId;

/// One window's adapter.
pub(crate) struct LinuxAccess {
    adapter: Adapter,
}

impl LinuxAccess {
    pub(crate) fn attach(window: WindowId, waker: &Waker) -> Self {
        let requests = Requests {
            window,
            waker: waker.clone(),
        };
        Self {
            adapter: Adapter::new(requests.clone(), requests.clone(), requests),
        }
    }

    /// Apply `update` if an assistive technology has asked for the tree.
    pub(crate) fn update(&mut self, update: TreeUpdate) {
        self.adapter.update_if_active(|| update);
    }

    /// Tell the adapter whether the window has keyboard focus, which decides
    /// whether its focused node is announced.
    pub(crate) fn set_focused(&mut self, focused: bool) {
        self.adapter.update_window_focus_state(focused);
    }
}

/// All three handlers: each request becomes a queued `RawEvent::Accessibility`.
#[derive(Clone)]
struct Requests {
    window: WindowId,
    waker: Waker,
}

impl Requests {
    fn send(&self, request: AccessRequest) {
        self.waker.send(Wake::Access {
            window: self.window,
            request,
        });
    }
}

impl ActivationHandler for Requests {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        self.send(AccessRequest::Activated);
        None
    }
}

impl ActionHandler for Requests {
    fn do_action(&mut self, request: ActionRequest) {
        if let Some(request) = AccessRequest::from_accesskit(&request) {
            self.send(request);
        }
    }
}

impl DeactivationHandler for Requests {
    fn deactivate_accessibility(&mut self) {
        self.send(AccessRequest::Deactivated);
    }
}
