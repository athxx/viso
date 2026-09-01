//! The platform→runtime callback funnel (dependency inversion).
//!
//! `viso-platform` is the bottom of the DAG and cannot name any runtime type,
//! yet the OS event pump must call *up* into the runtime for every event. We
//! invert the edge with a trait *defined here* and *implemented above*: the
//! backend owns a `&mut dyn AppHandler` and calls [`AppHandler::handle`] for
//! each event, receiving a [`ControlFlow`] telling it whether to block, poll,
//! or exit. Mirrors makepad's `Box<dyn FnMut(PlatformEvent) -> EventFlow>`
//! funnel, but as a named trait rather than a boxed closure.

use crate::control::ControlFlow;
use crate::event::RawEvent;

/// The single entry point the platform pump calls for every raw event.
///
/// Implemented by `viso-runtime`'s scheduler. The return value drives the
/// pump's next blocking decision.
pub trait AppHandler {
    /// Handle one raw event and report how the pump should proceed.
    fn handle(&mut self, event: RawEvent) -> ControlFlow;
}
