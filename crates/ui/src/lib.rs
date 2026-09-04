//! `viso-ui` — the retained UI runtime (Part V).
//!
//! The UI is a real retained tree keyed by a compact generational
//! [`node::NodeId`] over a [`node::NodeArena`] — *not*
//! `Rc<RefCell<Box<dyn Widget>>>`. Hot per-node data is stored in partitioned
//! arrays, separated into hot/warm/cold tiers by traversal frequency. State
//! changes drive *targeted* invalidation through typed dirty classes, never a
//! full-tree rebuild.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod aot;
pub mod binding;
pub mod component;
pub mod content;
pub mod context;
pub mod dirty;
pub mod grid;
pub mod hit_test;
pub mod input;
pub mod layout;
pub mod node;
pub mod paint;
pub mod reactive;
pub mod semantics;
pub mod state;
pub mod style;
pub mod token;
pub mod virtual_list;

pub use binding::{Binding, BindingTable};
pub use component::{
    BuildCx, Component, FlexStyle, FrameRecompute, Handle, LeafStyle, NodeStore, PointerHandler,
    ScrollStyle, VirtualListStyle,
};
pub use content::{Content, TextRequest};
pub use context::EventCx;
pub use dirty::DirtyClass;
pub use grid::{GridPlacement, GridStyle, TrackSizing};
pub use hit_test::{HitTestTree, hit_test};
pub use input::{
    ImeEvent, Key, KeyEvent, KeyRouter, Modifiers, PointerButtons, PointerEvent, PointerPhase,
    PointerRouter, ScrollEvent, ScrollRouter, focus_next, route_pointer, route_scroll,
};
pub use layout::{Align, Axis, Inset, Length, Size, Vec2};
pub use node::{NodeArena, NodeId, NodeLinks};
pub use paint::paint_tree;
pub use reactive::{
    Cleanup, ComputeCx, ComputedId, ComputedStore, DepCursor, EffectId, EffectStore,
};
pub use semantics::{Role, Semantics, SemanticsNode, SemanticsTree};
pub use state::{StateId, StateStore, StateValue};
pub use style::{BoxStyle, StyleId};
pub use token::{Theme, TokenId, TokenInterner, TokenNamespace};
pub use virtual_list::{
    HeightCache, HeightTree, ItemBuilder, VirtualListState, VirtualLists, absorb_measurements,
    reconcile, set_item_count,
};
