//! The frame-driver contract: how the runtime calls *up* into the app layer
//! without depending on it.
//!
//! `viso-runtime` must not depend on `viso-ui`/widgets/dsl. But the frame loop
//! has to drive user code: build layout, produce paint, etc. We invert the
//! edge the same way [`viso_platform::AppHandler`] does — a trait defined here,
//! implemented above (by the `viso` facade's `AppDriver`, which owns the user
//! `Application` and its `AppCx`). The scheduler is generic over this trait, so
//! the runtime orchestrates frames while staying UI-agnostic.

use std::time::Instant;

use viso_platform::WindowId;

use crate::context::RuntimeCx;
use crate::input::InputSample;
use crate::phase::FramePhase;

/// The app-layer hooks the frame scheduler drives.
///
/// Every method is a no-op-able seam: Phase 1 wires the *loop*, and the facade's
/// driver leaves the phase bodies empty ("blank frames"). Real layout/paint fill
/// these in as their subsystems land.
pub trait FrameDriver {
    /// Called exactly once, after the platform pump is live (on `AppLaunched`),
    /// before any window event. The driver constructs app state and its first
    /// window here.
    fn on_launch(&mut self, cx: &mut RuntimeCx<'_>);

    /// The window's geometry (size and/or scale factor) changed.
    fn on_geometry(&mut self, window: WindowId, scale: f64, width: u32, height: u32);

    /// A normalized input sample arrived. The scheduler has already resolved
    /// the window scale and converted the sample into physical-pixel space, so
    /// the driver can hit-test and route it directly without touching raw
    /// platform types.
    fn on_input(&mut self, sample: InputSample);

    /// Run one frame phase. Called once per phase, in [`FramePhase::ORDER`],
    /// for each frame the scheduler decides to run.
    fn run_phase(&mut self, phase: FramePhase, cx: &mut RuntimeCx<'_>);

    /// A window was closed — by the user (OS close button) or programmatically
    /// (via [`RuntimeCx::close_window`](crate::RuntimeCx::close_window)). Both
    /// arrive as the same [`WindowClosed`](viso_platform::RawEvent::WindowClosed)
    /// event, so the driver tears down that window's per-window state through
    /// this one hook. Called *before* the scheduler decrements its open-window
    /// count, so the teardown observes the pre-decrement state. Default no-op:
    /// a single-window driver holds no per-window state to release.
    fn on_window_closed(&mut self, _window: WindowId) {}

    /// Whether the driver wants continuous animation frames right now. When
    /// true, the scheduler keeps requesting redraw beats even with no input.
    fn wants_animation(&self) -> bool {
        false
    }

    /// The earliest instant at which a one-shot UI timer is due, or `None` when
    /// the driver holds no live timer.
    ///
    /// Consulted only when the driver is otherwise idle: the scheduler turns a
    /// `Some(deadline)` into [`ControlFlow::WaitUntil`](viso_platform::ControlFlow::WaitUntil),
    /// so the pump blocks until the nearest timer is due rather than polling.
    /// This is what keeps a waiting toast at zero frames — unlike animation,
    /// which spins a beat every frame, a timer costs nothing until it fires.
    fn next_timer_deadline(&self) -> Option<Instant> {
        None
    }
}
