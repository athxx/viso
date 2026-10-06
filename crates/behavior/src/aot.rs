//! Release native lowering of System IR (§108.2): a game's systems compiled
//! to Rust and run by the same scheduler.
//!
//! The System IR is the verified bytecode of the chunks a module's systems
//! run: their hooks, their state initializers and every chunk those call or
//! close over. [`lower_systems`] writes it as a Rust module, op for op, for
//! the app to compile with itself; [`Vm::install_native`](crate::Vm::install_native)
//! then runs those chunks as that code. Bytecode stays the reference
//! semantics: compiled code spends the same fuel per op, charges the same
//! memory, calls natives through the same path, and faults where and as the
//! interpreter faults, so a game replays to the same snapshot hashes either
//! way. The rest of this module is the runtime the generated code calls; an
//! app does not call it itself.

mod lower;

pub use crate::vm::native_code::*;
pub use lower::lower_systems;
