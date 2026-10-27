//! Viso behavior bytecode and its interpreter.
//!
//! The DSL compiler lowers every `fn`/`action` body, computed value, state
//! initializer, input default, constant and record field default to a
//! [`Chunk`] of 8-byte [`Op`]s over a register frame. A [`Module`] holds the
//! chunks and each component's [`Component`] layout, verified once when built
//! so the interpreter never meets an out-of-range register, jump or operand.
//!
//! A [`Vm`] runs chunks against an [`Instance`] (one component's state and
//! inputs) under a [`Budget`]. Every outermost call is one transaction: state
//! writes and emitted events are pending until it returns, then commit together
//! as one revision; a [`Fault`] discards them and leaves the instance as it was.
//!
//! The [`game`] module drives a module's `system`s on a fixed-step clock.
//!
//! A module crosses the build as bytes ([`Module::encode`]). A build running
//! it as Rust ([`aot::lower_module`]) carries it [`compiled`](Module::compiled):
//! each [`Body`] only the constant pool and [`BodyReads`] that code and the VM
//! read, no bytecode. [`Module::decode_static`] reads a `'static` blob — an
//! embedded module, mapped flash — in place: every name, key and string
//! constant is a [`Text`] borrowing it.
//!
//! Values have one representation per source type (see [`Value`]): integers,
//! `Bool`, `Char`, `Color`, `()` and unit enum variants are `Int`; floats and
//! dimensional scalars are `Float`; `None` is `Nil` and `Some(x)` is `x`.
//!
//! The crate is `no_std` + `alloc`. The `std` feature (on by default) adds
//! what needs an operating system:
//!
//! - the timer thread `viso::time::sleep` falls back to when the host installs
//!   no [`native::Timers`], and [`native::sleep`]; without it the native needs
//!   the host's timers;
//! - the monotonic clock behind `viso::time::Stopwatch`; without it starting
//!   one fails;
//! - [`game::DirStore`], the directory persistence backend;
//! - [`game::AudioHost`] and its [`game::AudioLink`], with
//!   [`game::Scheduler::attach_audio`]; without them `send_audio` is false;
//! - turning a native's panic into a [`FaultKind::NativeFailure`] fault;
//! - one shared [`native::Natives::standard`] registry and one cached
//!   [`theme::default_theme`] per thread, which a build without it makes anew
//!   on each call;
//! - the platform's float functions, where a build without it computes
//!   them with `libm`.

#![no_std]

extern crate alloc;
#[cfg(any(feature = "std", test))]
extern crate std;

pub mod aot;
mod arith;
pub mod event;
#[cfg(not(feature = "std"))]
mod float;
pub mod game;
pub mod i18n;
mod memo;
mod module;
pub mod native;
pub mod nav;
mod op;
pub mod retype;
pub mod theme;
mod value;
mod vm;
mod wire;

pub use memo::Reads;
pub use module::{
    Body, BodyReads, Chunk, ChunkKind, Code, CollisionDelivery, Compiled, Component,
    ComponentEffect, DEFAULT_TICK_RATE, EffectRun, Migrator, Module, NativeImport, PersistSlot,
    ResourceLoad, SnapshotSlot, Span, StableId, System, TaskPolicy, VerifyError,
};
pub use op::{Arith, ArithOp, DisplayKind, NodeAction, Num, Op};
pub use value::{Aggregate, Closure, Text, Value};
pub use vm::{
    Budget, Cost, Deferred, Event, Fault, FaultKind, Fiber, Instance, Location, NodeRequest,
    Outcome, Start, TaskStep, Vm, animation_of,
};
pub use wire::LoadError;
