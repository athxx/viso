//! `viso-ui` — the retained UI runtime (Part V).
//!
//! The UI is a real retained tree keyed by a compact generational
//! [`node::NodeId`] over a [`node::NodeArena`] — *not*
//! `Rc<RefCell<Box<dyn Widget>>>` (§14). Hot per-node data is stored in
//! partitioned arrays (§14.1) and separated into hot/warm/cold tiers (§14.2).
//! State changes drive *targeted* invalidation through typed dirty classes
//! (§11 dirty rules), never a full-tree rebuild.
//!
//! Phase 0 status: the identity, dirty, and phase-context *contracts*.
//! Layout/style/input/semantics implementations arrive in later phases.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod component;
pub mod context;
pub mod dirty;
pub mod layout;
pub mod node;
pub mod paint;
pub mod style;

pub use component::{BuildCx, Component, FlexStyle, Handle, LeafStyle, NodeStore};
pub use dirty::DirtyClass;
pub use layout::{Align, Axis, Inset, Length, Size};
pub use node::{NodeArena, NodeId, NodeLinks};
pub use paint::paint_tree;
pub use style::BoxStyle;
