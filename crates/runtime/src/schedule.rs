//! Frame scheduling contract (architecture §12).
//!
//! `redraw()` is NOT an ordinary application responsibility. The runtime
//! aggregates redraw *reasons* and decides whether/when to run a frame.
//! Crucially, when nothing is dirty the runtime must go truly idle (§12.1):
//! no continuous layout, paint, submit, tree traversal, or per-component
//! polling.

use viso_platform::ControlFlow;

/// Why a frame might be needed. The scheduler aggregates these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RedrawReason {
    InputDirty,
    StateDirty,
    AnimationActive,
    TimerDue,
    AsyncCompletion,
    WindowResize,
    ExternalSurfaceInvalidation,
    /// Live editing / hot reload requested a redraw.
    HotReload,
}

impl RedrawReason {
    /// The bit this reason occupies in a [`RedrawReasons`] set.
    const fn bit(self) -> u8 {
        match self {
            RedrawReason::InputDirty => 1 << 0,
            RedrawReason::StateDirty => 1 << 1,
            RedrawReason::AnimationActive => 1 << 2,
            RedrawReason::TimerDue => 1 << 3,
            RedrawReason::AsyncCompletion => 1 << 4,
            RedrawReason::WindowResize => 1 << 5,
            RedrawReason::ExternalSurfaceInvalidation => 1 << 6,
            RedrawReason::HotReload => 1 << 7,
        }
    }
}

/// The scheduler's decision after aggregating [`RedrawReason`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameDecision {
    /// Stay idle. No CPU spent until a new reason arrives.
    NoFrame,
    /// Draw at the next vsync.
    FrameAtNextVsync,
    /// Draw immediately (e.g. resize).
    ImmediateFrame,
    /// Do low-priority background maintenance without a visible frame.
    BackgroundMaintenance,
}

impl FrameDecision {
    /// How the platform pump should behave given this decision.
    ///
    /// Idle ([`FrameDecision::NoFrame`]) blocks the pump ([`ControlFlow::Wait`])
    /// so an idle app spends zero CPU (§12.1); anything that wants a frame
    /// keeps the pump spinning ([`ControlFlow::Poll`]) so the next display beat
    /// arrives promptly.
    pub fn to_control_flow(self) -> ControlFlow {
        match self {
            FrameDecision::NoFrame => ControlFlow::Wait,
            FrameDecision::FrameAtNextVsync
            | FrameDecision::ImmediateFrame
            | FrameDecision::BackgroundMaintenance => ControlFlow::Poll,
        }
    }
}

/// An accumulated set of [`RedrawReason`]s awaiting a scheduling decision.
///
/// A packed `u8` bitset: aggregating a reason is a single OR, and going idle is
/// a compare-with-zero. `take` clears the set and returns what was pending, so
/// each reason drives exactly one decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RedrawReasons(u8);

impl RedrawReasons {
    /// An empty set (idle).
    pub const fn new() -> Self {
        Self(0)
    }

    /// Record that a frame is wanted for `reason`.
    pub fn add(&mut self, reason: RedrawReason) {
        self.0 |= reason.bit();
    }

    /// Whether no reason is pending.
    pub fn is_idle(self) -> bool {
        self.0 == 0
    }

    /// Whether `reason` is currently pending.
    pub fn contains(self, reason: RedrawReason) -> bool {
        self.0 & reason.bit() != 0
    }

    /// Clear the set and return the reasons that were pending.
    pub fn take(&mut self) -> RedrawReasons {
        let taken = *self;
        self.0 = 0;
        taken
    }

    /// Collapse the pending reasons into a single scheduling decision.
    ///
    /// Resize / external surface invalidation must be reflected *this* beat, so
    /// they escalate to [`FrameDecision::ImmediateFrame`]. Any other reason
    /// draws at the next vsync. An empty set stays idle.
    pub fn decide(self) -> FrameDecision {
        if self.is_idle() {
            return FrameDecision::NoFrame;
        }
        let immediate =
            RedrawReason::WindowResize.bit() | RedrawReason::ExternalSurfaceInvalidation.bit();
        if self.0 & immediate != 0 {
            FrameDecision::ImmediateFrame
        } else {
            FrameDecision::FrameAtNextVsync
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_set_is_idle_and_waits() {
        let reasons = RedrawReasons::new();
        assert!(reasons.is_idle());
        assert_eq!(reasons.decide(), FrameDecision::NoFrame);
        assert_eq!(reasons.decide().to_control_flow(), ControlFlow::Wait);
    }

    #[test]
    fn resize_escalates_to_immediate() {
        let mut reasons = RedrawReasons::new();
        reasons.add(RedrawReason::WindowResize);
        assert_eq!(reasons.decide(), FrameDecision::ImmediateFrame);
        assert_eq!(reasons.decide().to_control_flow(), ControlFlow::Poll);
    }

    #[test]
    fn external_surface_invalidation_is_immediate() {
        let mut reasons = RedrawReasons::new();
        reasons.add(RedrawReason::ExternalSurfaceInvalidation);
        assert_eq!(reasons.decide(), FrameDecision::ImmediateFrame);
    }

    #[test]
    fn ordinary_reasons_draw_at_next_vsync() {
        for reason in [
            RedrawReason::InputDirty,
            RedrawReason::StateDirty,
            RedrawReason::AnimationActive,
            RedrawReason::TimerDue,
            RedrawReason::AsyncCompletion,
            RedrawReason::HotReload,
        ] {
            let mut reasons = RedrawReasons::new();
            reasons.add(reason);
            assert_eq!(
                reasons.decide(),
                FrameDecision::FrameAtNextVsync,
                "{reason:?} should draw at next vsync"
            );
            assert_eq!(reasons.decide().to_control_flow(), ControlFlow::Poll);
        }
    }

    #[test]
    fn immediate_wins_when_mixed_with_ordinary() {
        let mut reasons = RedrawReasons::new();
        reasons.add(RedrawReason::InputDirty);
        reasons.add(RedrawReason::WindowResize);
        assert_eq!(reasons.decide(), FrameDecision::ImmediateFrame);
    }

    #[test]
    fn take_clears_the_set() {
        let mut reasons = RedrawReasons::new();
        reasons.add(RedrawReason::StateDirty);
        assert!(reasons.contains(RedrawReason::StateDirty));
        let taken = reasons.take();
        assert!(taken.contains(RedrawReason::StateDirty));
        assert!(reasons.is_idle(), "take must reset the set to idle");
    }
}
