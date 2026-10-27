//! What a handler asks of the event it runs for: `e.stop_propagation()`,
//! `e.stop_immediate_propagation()` and `e.prevent_default()` are the actions
//! `viso::event` declares, which note the request on the [`EventControl`] the
//! view's host lends its VM. The host applies them once the handler returns.

use alloc::rc::Rc;
use core::cell::Cell;

use crate::native::NativeLibrary;

/// What the handlers of one dispatch asked of its event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EventAsks {
    /// No node further along the route hears the event.
    pub stop: bool,
    /// Not even the node's handlers after this one.
    pub stop_immediate: bool,
    /// The default action does not happen.
    pub prevent_default: bool,
}

/// The requests a view's handlers make of their event, shared by the view's
/// VM and its host.
#[derive(Debug, Clone, Default)]
pub struct EventControl(Rc<Cell<EventAsks>>);

impl EventControl {
    /// A control no handler has asked anything of.
    pub fn new() -> EventControl {
        EventControl::default()
    }

    fn ask(&self, f: impl FnOnce(&mut EventAsks)) {
        let mut asks = self.0.get();
        f(&mut asks);
        self.0.set(asks);
    }

    /// What was asked since the last call, taken.
    pub fn take(&self) -> EventAsks {
        self.0.take()
    }
}

/// The actions an event's methods lower to: without an [`EventControl`]
/// (nothing dispatches the handler) they do nothing.
pub(crate) static EVENT: NativeLibrary = NativeLibrary {
    path: "viso::event",
    version: 1,
    functions: &[
        crate::native!(action "stop_propagation" |cx| -> () {
            if let Ok(control) = cx.service::<EventControl>() {
                control.ask(|a| a.stop = true);
            }
            Ok(())
        }),
        crate::native!(action "stop_immediate_propagation" |cx| -> () {
            if let Ok(control) = cx.service::<EventControl>() {
                control.ask(|a| {
                    a.stop = true;
                    a.stop_immediate = true;
                });
            }
            Ok(())
        }),
        crate::native!(action "prevent_default" |cx| -> () {
            if let Ok(control) = cx.service::<EventControl>() {
                control.ask(|a| a.prevent_default = true);
            }
            Ok(())
        }),
    ],
    types: &[],
    traits: &[],
    derives: &[],
    widgets: &[],
};

/// The library the methods of an event call.
pub const EVENT_PATH: &str = "viso::event";

/// The methods every event a handler receives has, each the action of
/// [`EVENT_PATH`] of its name.
pub const EVENT_METHODS: [&str; 3] = [
    "stop_propagation",
    "stop_immediate_propagation",
    "prevent_default",
];
