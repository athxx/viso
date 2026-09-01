//! `viso-ui` — the retained UI runtime (Part V).
//!
//! The UI is a real retained tree keyed by a compact generational
//! [`node::NodeId`] over a [`node::NodeArena`] — *not*
//! `Rc<RefCell<Box<dyn Widget>>>`. Hot per-node data is stored in partitioned
//! arrays, separated into hot/warm/cold tiers by traversal frequency. State
//! changes drive *targeted* invalidation through typed dirty classes, never a
//! full-tree rebuild.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod binding;
pub mod component;
pub mod context;
pub mod dirty;
pub mod layout;
pub mod node;
pub mod paint;
pub mod reactive;
pub mod state;
pub mod style;

pub use binding::{Binding, BindingTable};
pub use component::{BuildCx, Component, FlexStyle, FrameRecompute, Handle, LeafStyle, NodeStore};
pub use dirty::DirtyClass;
pub use layout::{Align, Axis, Inset, Length, Size};
pub use node::{NodeArena, NodeId, NodeLinks};
pub use paint::paint_tree;
pub use reactive::{
    Cleanup, ComputeCx, ComputedId, ComputedStore, DepCursor, EffectId, EffectStore,
};
pub use state::{StateId, StateStore, StateValue};
pub use style::BoxStyle;
