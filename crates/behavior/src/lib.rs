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
//! Values have one representation per source type (see [`Value`]): integers,
//! `Bool`, `Char`, `Color`, `()` and unit enum variants are `Int`; floats and
//! dimensional scalars are `Float`; `None` is `Nil` and `Some(x)` is `x`.

mod arith;
mod memo;
mod module;
pub mod native;
mod op;
mod value;
mod vm;
mod wire;

pub use memo::Reads;
pub use module::{Chunk, ChunkKind, Code, Component, Module, NativeImport, Span, VerifyError};
pub use op::{Arith, ArithOp, DisplayKind, Num, Op};
pub use value::{Aggregate, Closure, Value};
pub use vm::{Budget, Cost, Event, Fault, FaultKind, Instance, Location, Outcome, Vm};
pub use wire::LoadError;
