//! Behavior lowering: typed `fn`/`action` bodies, computed values, state
//! initializers, input defaults, constants and record field defaults become
//! functions of the register-based [`ir`], ready for bytecode generation.
//!
//! Lowering runs after type checking, over a body whose every expression has a
//! type; a body with a type error, or one using a construct the IR does not
//! represent yet, lowers to a function whose body is an [`ir::Unsupported`]
//! reason instead, and every function that calls it inherits that reason.

pub mod ir;

mod codegen;
mod dump;
mod instance;
pub(crate) mod lower;

pub(crate) use instance::{hidden_state, inline_instances};
pub use ir::{
    Body, ComponentLayout, FuncId, Function, FunctionKind, Inst, Program, Reg, RegionalStates,
    Site, Unsupported,
};
