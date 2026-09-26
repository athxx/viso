//! The macOS accessibility bridge (ADR 0030): an AccessKit subclassing adapter
//! on each window's content view.
//!
//! AppKit asks for the tree from inside the pump's `nextEventMatchingMask:`,
//! while the adapter holds its own state borrowed, so the tree cannot be built
//! there. The activation handler answers with no tree, queues
//! [`AccessRequest::Activated`] and posts an application-defined event so the
//! blocked pump wakes and delivers it; the facade's next frame then sends the
//! full tree through [`MacAccess::update`]. Actions are queued the same way.

use std::rc::Rc;

use accesskit::{ActionHandler, ActionRequest, ActivationHandler, TreeUpdate};
use accesskit_macos::SubclassingAdapter;
use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSView};
use objc2_foundation::{MainThreadMarker, NSPoint};

use crate::accessibility::AccessRequest;
use crate::control::WindowId;
use crate::event::RawEvent;

/// Where the adapter's handlers put the events they raise: the pump's queue.
pub(super) type Sink = Rc<dyn Fn(RawEvent)>;

/// One window's adapter.
pub(super) struct MacAccess {
    adapter: SubclassingAdapter,
}

impl MacAccess {
    /// Subclass `view` for accessibility. Call before the window is first
    /// shown or made key.
    pub(super) fn attach(view: &NSView, window: WindowId, sink: Sink) -> Self {
        let requests = Requests { window, sink };
        // SAFETY: `view` is a live `NSView` borrowed for the call; the adapter
        // retains it for as long as the adapter lives.
        let adapter = unsafe {
            SubclassingAdapter::new(
                (view as *const NSView).cast_mut().cast(),
                requests.clone(),
                requests,
            )
        };
        Self { adapter }
    }

    /// Apply `update` if an assistive technology has asked for the tree.
    pub(super) fn update(&mut self, update: TreeUpdate) {
        if let Some(events) = self.adapter.update_if_active(|| update) {
            events.raise();
        }
    }

    /// Tell the adapter whether the window is key, which decides whether its
    /// focused node is announced.
    pub(super) fn set_focused(&mut self, focused: bool) {
        if let Some(events) = self.adapter.update_view_focus_state(focused) {
            events.raise();
        }
    }
}

/// Both handlers: each request becomes a queued `RawEvent::Accessibility`.
#[derive(Clone)]
struct Requests {
    window: WindowId,
    sink: Sink,
}

impl Requests {
    fn send(&self, request: AccessRequest) {
        (self.sink)(RawEvent::Accessibility {
            window: self.window,
            request,
        });
        wake_pump();
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

/// Post an application-defined event so a pump blocked in
/// `nextEventMatchingMask:` returns and drains the queue. AppKit answers
/// accessibility queries inside that call without returning from it.
pub(super) fn wake_pump() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined,
        NSPoint::new(0.0, 0.0),
        NSEventModifierFlags::empty(),
        0.0,
        0,
        None,
        0,
        0,
        0,
    );
    if let Some(event) = event {
        NSApplication::sharedApplication(mtm).postEvent_atStart(&event, false);
    }
}
