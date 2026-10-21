//! `viso-ui` — the retained UI runtime (Part V).
//!
//! The UI is a real retained tree keyed by a compact generational
//! [`node::NodeId`] over a [`node::NodeArena`] — *not*
//! `Rc<RefCell<Box<dyn Widget>>>`. Hot per-node data is stored in partitioned
//! arrays, separated into hot/warm/cold tiers by traversal frequency. State
//! changes drive *targeted* invalidation through typed dirty classes, never a
//! full-tree rebuild.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod adaptive;
pub mod animation;
pub mod aot;
pub mod asset;
pub mod binding;
pub mod check_mark;
pub mod component;
pub mod content;
pub mod context;
pub mod dirty;
pub mod flush;
pub mod frame_client;
pub mod grid;
pub mod hit_test;
pub mod input;
pub mod inspect;
pub mod interaction;
pub mod layout;
pub mod layout_value;
pub mod length;
pub mod node;
pub mod notice;
pub mod paint;
pub mod reactive;
pub mod semantics;
pub mod shader;
pub mod state;
#[cfg(feature = "hot-reload")]
pub mod state_origin;
pub mod structure;
pub mod style;
pub mod suspend;
pub mod task;
pub mod text_edit;
pub mod text_style;
pub mod timer;
pub mod token;
pub mod transform;
pub mod virtual_list;
pub mod window;

pub use animation::{AnimationRegistry, Easing, LookTransition, LookValue, Timing, TranslateAnim};
// The frame clock's time point (wasm-safe), named by timers and deadlines.
pub use binding::{
    Binding, BindingFootprint, BindingTable, InstanceBase, StateRef, Template, TemplateBinding,
};
pub use check_mark::{CheckMark, CheckShape};
pub use component::{
    AbsoluteStyle, BuildCx, Component, FlexStyle, FrameRecompute, Handle, LeafStyle, NodeStore,
    Painted, PointerHandler, ScrollRequest, ScrollStyle, StackStyle, VirtualListStyle,
};
pub use content::{BrushImage, Content, TextRequest};
pub use context::EventCx;
pub use dirty::{DirtyCause, DirtyClass, DirtyLists, DirtyMark};
pub use flush::{
    ADAPTIVE_ROUNDS, AdaptiveCycle, EffectsRan, ReactiveCycle, SETTLE_ROUNDS, Unsettled,
    run_effects, settle_adaptive, settle_states,
};
pub use frame_client::{FrameClient, SharedClient};
pub use grid::{
    AdaptiveColumns, AutoRepeat, GridAreas, GridPlacement, GridStyle, LineNames, TrackMax,
    TrackSizing, repeat, repeated,
};
pub use hit_test::{HitTestTree, hit_test};
pub use input::{
    DispatchPhase, ImeEvent, Key, KeyEvent, KeyRouter, Modifiers, PointerButtons, PointerContact,
    PointerEvent, PointerId, PointerPhase, PointerRouter, ScrollEvent, ScrollRouter, TOUCH_SLOP,
    blur_node, focus_next, focus_node, focus_quietly, route_contact, route_pointer, route_scroll,
};
pub use inspect::{
    InspectFlags, InspectNode, InspectSnapshot, InspectTree, PaintRange, PaintRanges, paint_ranges,
    snapshot_ui,
};
pub use interaction::Interaction;
pub use layout::{
    AbsoluteInsets, Align, AlignItems, Alignment2D, Axis, Basis, BoxRules, Inset, Justify,
    LayoutInput, Length, Placement, Size, Span, SpanInset, StackPlacement, Vec2,
};
pub use layout_value::{BoundSize, LayoutValue};
pub use length::{
    EdgeTerms, InsetTerms, LengthDeps, LengthEnv, LengthIssue, LengthStats, LengthTerms,
    LengthWarning, NodeLengths,
};
pub use node::{ArenaFull, ChildIds, Children, NodeArena, NodeId, NodeIndex, NodeLinks, TreeLinks};
pub use notice::{Notice, NoticeHandler, NoticeKinds, deliver_notices};
pub use paint::{paint_tree, paint_with_transforms};
pub use text_style::{
    FontFamily, FontSize, FontWeight, TextAlign, TextOverflow, TextSizing, TextStyle, WidthUse,
};
pub use transform::{TransformTable, Turn};
pub use viso_runtime::Instant;
// Render primitive data types that already appear in this crate's public API —
// `Content`'s payload fields (content.rs), `TextRequest::color`, and
// `BoxStyle::fill` (style.rs) are all typed with them. Re-export the exact set
// so a downstream crate depending only on `viso-ui` (e.g. `viso-widgets`, which
// the DAG allows to reach `viso-ui` alone) can name them to construct a
// `BoxStyle`, `TextRequest`, or `Content`. `viso-render` is already a `viso-ui`
// dependency, so this adds no new edge.
pub use asset::{Asset, AssetBytes, AssetDraw, AssetFit, AssetPayload, AssetState, AssetTable};
pub use reactive::{
    Cleanup, ComputeCx, ComputedId, ComputedStore, DepCursor, EffectCx, EffectId, EffectStore,
    ProjectId, SemanticProjector,
};
pub use semantics::{
    InspectKind, LiveRegion, Role, SemanticState, Semantics, SemanticsNode, SemanticsTree,
};
pub use shader::ShaderSlots;
pub use state::{StateId, StateStore, StateValue};
pub use structure::{StructureCx, StructureHookId, run_structure_hooks};
pub use style::{BoxStyle, InteractionStyle, StyleId};
pub use task::{Continuation, TaskFuture, TaskId, TaskOps};
pub use text_edit::{
    Buffer, EditGeometry, EditIntent, EditLayout, FieldLook, FieldSeed, Motion, TextEdits,
};
pub use timer::{TimerId, TimerRegistry, TimerRequest};
pub use token::{Theme, TokenId, TokenInterner, TokenNamespace};
pub use virtual_list::{
    HeightCache, HeightTree, ItemBuilder, VirtualListState, VirtualLists, absorb_measurements,
    reconcile, set_item_count,
};
/// The renderer's asset vocabulary — resource identity, formats, decoded
/// payloads — which the compiler names resources by and the facade decodes
/// into.
pub use viso_render::asset as resource;
pub use viso_render::{
    Border, LineJoin, PathCmd, Point, Primitive, Rect, Rgba, Srgb, Stroke, TextureId,
};
pub use viso_text::selection::Selection;
pub use viso_text::{CaretAffinity, TextOffset, TextPosition};
pub use window::{ChromeContext, WindowChrome, WindowConfig, WindowIdSlot, WindowOpenRequest};
