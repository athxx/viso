//! One frame's worth of phase orchestration (§11.2).
//!
//! `run_frame` walks [`FramePhase::ORDER`] and calls the driver once per phase.
//! In Phase 1 every phase body is a no-op in the facade's driver — this is the
//! "blank frame": the 12-phase loop provably ticks against a live window, but
//! produces no pixels (the GPU swapchain is Phase 2). The `Submit` phase is a
//! plain hook here with no special handling; it becomes the GPU submit point
//! later.

use crate::context::RuntimeCx;
use crate::driver::FrameDriver;
use crate::phase::FramePhase;

/// Drive a single frame through all twelve phases, in order.
pub fn run_frame<D: FrameDriver>(driver: &mut D, cx: &mut RuntimeCx<'_>) {
    for phase in FramePhase::ORDER {
        driver.run_phase(phase, cx);
    }
}
