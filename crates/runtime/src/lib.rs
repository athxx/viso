//! `viso-runtime` — the application execution kernel.
//!
//! Owns the highest-level control of the UI main loop and frame scheduler:
//! OS event pump, timers, task completions, animation tick, UI update,
//! render/vsync. An external async runtime (tokio/smol) is an *adapter* that
//! plugs in here — it never owns the frame lifecycle.
//!
//! This crate defines the frame-phase and scheduling *contract* only. Widget
//! implementation, layout, and paint live above it and are not visible here.
//!
//! The [`Scheduler`] implements [`viso_platform::AppHandler`] and drives
//! 12-phase frames against a real platform window, staying UI-agnostic via the
//! [`FrameDriver`] inversion.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod clock;
pub mod context;
pub mod driver;
pub mod frame;
pub mod input;
pub mod phase;
pub mod schedule;
pub mod scheduler;
pub mod task;

pub use clock::{FixedStepClock, FrameClock, ManualClock, WallClock};
pub use context::RuntimeCx;
pub use driver::{FrameDriver, Lifecycle};
pub use frame::run_frame;
pub use input::{
    ClipboardData, ClipboardImage, ClipboardKinds, ClipboardReply, CopySample, DropAction,
    DropOffer, DropPhase, DropReply, DropSample, EditMenuSample, GesturePhase, GestureSample,
    ImePreeditSample, InputSample, Key, KeySample, Modifiers, PasteReply, PasteRequestSample,
    PasteSample, PointerId, PointerKind, PointerPhase, PointerSample, ScrollPhase, ScrollSample,
    TextSample, primary_is_logo,
};
pub use phase::FramePhase;
pub use schedule::{FrameDecision, RedrawReason, RedrawReasons};
pub use scheduler::Scheduler;
pub use task::{TaskId, TaskSet};
pub use viso_platform::{
    Appearance, ColorScheme, CursorIcon, DisplayFeature, DisplayFeatureKind, EditLabels, EditMenu,
    EditMenuAction, GamepadButton, GamepadEvent, GamepadId, GamepadInfo, GamepadState, InputKind,
    Instant, LogicalInsets, LoopWaker, Loupe, Rumble,
};
