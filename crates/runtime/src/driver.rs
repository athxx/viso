//! The frame-driver contract: how the runtime calls *up* into the app layer
//! without depending on it.
//!
//! `viso-runtime` must not depend on `viso-ui`/widgets/dsl. But the frame loop
//! has to drive user code: build layout, produce paint, etc. We invert the
//! edge the same way [`viso_platform::AppHandler`] does — a trait defined here,
//! implemented above (by the `viso` facade's `AppDriver`, which owns the user
//! `Application` and its `AppCx`). The scheduler is generic over this trait, so
//! the runtime orchestrates frames while staying UI-agnostic.

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

    /// Whether the driver wants continuous animation frames right now. When
    /// true, the scheduler keeps requesting redraw beats even with no input.
    fn wants_animation(&self) -> bool {
        false
    }
}
