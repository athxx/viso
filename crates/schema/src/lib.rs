//! `viso-schema` — the data that crosses the compile boundary.
//!
//! The compiler (`viso-dsl`, the LSP, `viso check`) produces these types and
//! the runtime (`viso-ui`, `viso-view`) consumes them, so neither links the
//! other: the compiler builds without GPU, platform or runtime code. Each type
//! is plain data with its wire codec and the small pure functions that define
//! its meaning; the runtime crates re-export them at their historical paths.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]

extern crate alloc;

pub mod aot;
pub mod carry;
mod codec;
pub mod dirty;
pub mod effect;
pub mod env;
pub mod game;
pub mod id;
pub mod interaction;
pub mod layout;
pub mod length;
pub mod number;
pub mod plate;
pub mod resource;
pub mod retype;
pub mod semantics;
pub mod state;
pub mod view;
pub mod widget;
pub mod widgets;

pub use carry::MigratableState;
pub use dirty::DirtyClass;
pub use effect::{ComponentEffect, EffectRun, ResourceLoad};
pub use env::{Avoid, EnvField, LayoutDirection};
pub use game::Rebuild;
pub use id::StableId;
pub use interaction::Interaction;
pub use layout::{Align, Axis, Inset, Justify, Length};
pub use length::{EdgeTerms, InsetTerms, LengthDeps, LengthEnv, LengthTerms, NodeLengths};
pub use resource::{AssetBytes, AssetFormat, AssetKind, ResourceId, ResourceRevision};
pub use semantics::{InspectKind, LiveRegion, Role, SemanticState};
pub use state::{FINGERPRINT_VERSION, StateKey, StateValue};
