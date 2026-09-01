//! `viso-runtime` — the application execution kernel.
//!
//! Owns the highest-level control of the UI main loop and frame scheduler
//! (§11.1): OS event pump, timers, task completions, animation tick, UI
//! update, render/vsync. An external async runtime (tokio/smol) is an
//! *adapter* that plugs in here — it never owns the frame lifecycle.
//!
//! This crate defines the frame-phase and scheduling *contract* only. Widget
//! implementation, layout, and paint live above it and are not visible here.
//!
//! Phase 1 status: the frame scheduler and event pump are live. The
//! [`Scheduler`] implements [`viso_platform::AppHandler`] and drives blank
//! 12-phase frames against a real platform window, staying UI-agnostic via the
//! [`FrameDriver`] inversion.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod context;
pub mod driver;
pub mod frame;
pub mod phase;
pub mod schedule;
pub mod scheduler;

pub use context::RuntimeCx;
pub use driver::FrameDriver;
pub use frame::run_frame;
pub use phase::FramePhase;
pub use schedule::{FrameDecision, RedrawReason, RedrawReasons};
pub use scheduler::Scheduler;
