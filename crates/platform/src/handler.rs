//! The platform→runtime callback funnel (dependency inversion).
//!
//! `viso-platform` is the bottom of the DAG and cannot name any runtime type,
//! yet the OS event pump must call *up* into the runtime for every event. We
//! invert the edge with a trait *defined here* and *implemented above*: the
//! backend owns a `&mut dyn AppHandler` and calls [`AppHandler::handle`] for
//! each event, receiving a [`ControlFlow`] telling it whether to block, poll,
//! or exit — one named trait rather than a boxed closure. The backend lends
//! itself to every call, so the handler reaches the app only through that
//! borrow, never through a pointer of its own that aliases the pump's.

use crate::PlatformApp;
use crate::control::ControlFlow;
use crate::event::RawEvent;

/// The single entry point the platform pump calls for every raw event.
///
/// Implemented by `viso-runtime`'s scheduler. The return value drives the
/// pump's next blocking decision.
pub trait AppHandler {
    /// Handle one raw event against `app` (the pump that delivered it) and
    /// report how the pump should proceed.
    fn handle(&mut self, app: &mut dyn PlatformApp, event: RawEvent) -> ControlFlow;
}
