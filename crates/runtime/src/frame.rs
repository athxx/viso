//! One frame's worth of phase orchestration.
//!
//! `run_frame` walks [`FramePhase::ORDER`] and calls the driver once per phase.
//! A driver whose phase bodies are all no-ops runs a "blank frame": the phase
//! loop ticks against a live window but produces no pixels. The `Submit` phase
//! is a plain hook here with no special handling.

use crate::context::RuntimeCx;
use crate::driver::FrameDriver;
use crate::phase::FramePhase;

/// Drive a single frame through every phase of [`FramePhase::ORDER`], in order.
pub fn run_frame<D: FrameDriver>(driver: &mut D, cx: &mut RuntimeCx<'_>) {
    for phase in FramePhase::ORDER {
        driver.run_phase(phase, cx);
    }
}
