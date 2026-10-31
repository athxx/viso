//! `viso-ui` — the retained UI runtime.
//!
//! The UI is a real retained tree keyed by a compact generational
//! [`node::NodeId`] over a [`node::NodeArena`] — *not*
//! `Rc<RefCell<Box<dyn Widget>>>`. Hot per-node data is stored in partitioned
//! arrays, separated into hot/warm/cold tiers by traversal frequency. State
//! changes drive *targeted* invalidation through typed dirty classes, never a
//! full-tree rebuild.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod access;
pub mod adaptive;
pub mod anchored;
pub mod animation;
pub mod aot;
pub mod asset;
pub mod backdrop;
pub mod background;
pub mod binding;
pub mod calendar;
pub mod check_mark;
pub mod clicks;
pub mod clipboard;
pub mod color_picker;
pub mod color_plane;
pub mod combo_box;
pub mod component;
pub mod content;
pub mod content_inset;
pub mod context;
pub mod control;
pub mod cursor;
pub mod dirty;
pub mod drag;
pub mod drag_drop;
pub mod drawing;
mod edit_history;
pub mod feedback;
pub mod field_input;
mod field_marks;
pub mod flush;
pub mod focus_ring;
pub mod fold;
pub mod frame_client;
pub mod fullscreen;
pub mod glyphs;
pub mod grid;
pub mod hit_test;
pub mod input;
pub mod input_filter;
pub mod inspect;
pub mod interaction;
pub mod key_scroll;
pub mod kind;
pub mod layer;
pub mod layout;
pub mod layout_value;
pub mod length;
pub mod links;
pub mod long_press;
pub mod native;
pub mod navigation;
pub mod node;
pub mod notice;
pub mod number_format;
pub mod page_dots;
pub mod paint;
pub mod part_writes;
pub mod pinch;
pub mod plate;
pub mod plot;
pub mod pointer_lock;
pub mod progress;
pub mod reactive;
pub mod refresh;
pub mod router;
pub mod roving;
pub mod row_gesture;
pub mod scenes;
pub mod scroll_bar;
pub mod scroll_motion;
pub mod search_field;
pub mod segments;
pub mod semantics;
pub mod shader;
pub mod shadow;
pub mod sheet;
pub mod shimmer;
pub mod slider;
pub mod snap;
mod sparse;
pub mod spinner;
pub mod state;
#[cfg(feature = "hot-reload")]
pub mod state_origin;
pub mod stepper;
pub mod sticky;
pub mod structure;
pub mod style;
pub mod suspend;
pub mod task;
pub mod text_edit;
pub mod text_entry;
pub mod text_shadow;
pub mod text_style;
pub mod theme;
pub mod timer;
pub mod transform;
pub mod typed_state;
pub mod video;
pub mod video_controls;
pub mod viewer;
pub mod virtual_grid;
pub mod virtual_list;
pub mod wheel;
pub mod window;
pub mod work;

pub use animation::{
    AnimationEnd, AnimationEndHandler, AnimationId, CubicBezier, Curve, Easing, Keyframe,
    Keyframes, LookTransition, LookValue, Pace, PlayDirection, ReducedMotion, Repeat, Spring,
    Timing,
};
pub use theme::{ColorToken, Palette, fade};
// The frame clock's time point (wasm-safe), named by timers and deadlines.
pub use anchored::{AnchorAlign, AnchorPlacement, Anchoring, Side};
pub use binding::{
    Binding, BindingFootprint, BindingTable, InstanceBase, StateRef, Template, TemplateBinding,
};
pub use check_mark::{CheckMark, CheckShape};
pub use clipboard::{ClipboardData, ClipboardImage, ClipboardKinds};
pub use component::{
    AbsoluteStyle, AnyNode, BuildCx, Component, FlexStyle, FlowStyle, FrameRecompute, Handle,
    LeafStyle, Mount, MountedView, NodeMemory, NodeStore, Painted, PointerHandler, ScrollBehavior,
    ScrollRequest, ScrollStyle, StackStyle, VirtualListStyle,
};
pub use content::{BrushImage, Content, InkRun, LinkBox, TextInk, TextMark, TextRequest};
pub use context::{EventCx, FocusChange, StateWriter};
pub use control::{ControlHandle, ControlId, ControlTable, Controls};
pub use dirty::{DirtyCause, DirtyClass, DirtyLists, DirtyMark};
pub use drag::{DragPhase, DragSample, MOUSE_SLOP, SWIPE_SPEED, SwipeDirection};
pub use drag_drop::{
    ArrivalPhase, DragAccept, DragData, DragEnd, DragItem, DragKind, DropEvent, DropOperation,
    DropPhase, DropTarget,
};
pub use drawing::Drawing;
pub use flush::{
    ADAPTIVE_ROUNDS, AdaptiveCycle, EffectsRan, ReactiveCycle, SETTLE_ROUNDS, Unsettled,
    run_effects, settle_adaptive, settle_states,
};
pub use focus_ring::FocusRingStyle;
pub use frame_client::{FrameClient, SharedClient};
pub use grid::{
    AdaptiveColumns, AutoRepeat, GridAreas, GridPlacement, GridStyle, LineNames, TrackMax,
    TrackSizing, repeat, repeated,
};
pub use hit_test::{HitTestTree, hit_test};
pub use input::{
    DispatchPhase, ImeEvent, Key, KeyEvent, KeyRouter, Modifiers, PointerButtons, PointerContact,
    PointerEvent, PointerId, PointerPhase, PointerRouter, ScrollEvent, ScrollPhase, ScrollRouter,
    TOUCH_SLOP, blur_node, focus_next, focus_node, focus_quietly, route_contact, route_pointer,
    route_scroll,
};
pub use inspect::{
    InspectFlags, InspectNode, InspectSnapshot, InspectTree, PaintRange, PaintRanges, paint_ranges,
    snapshot_ui,
};
pub use interaction::Interaction;
pub use layout::{
    AbsoluteInsets, Align, AlignItems, Alignment2D, Axis, Basis, BoxRules, Inset, Justify,
    LayoutInput, Length, Placement, Sizing, Span, SpanInset, StackPlacement, Vec2,
};
pub use layout_value::{BoundSize, LayoutValue};
pub use length::{
    EdgeTerms, InsetTerms, LengthDeps, LengthEnv, LengthIssue, LengthStats, LengthTerms,
    LengthWarning, NodeLengths,
};
pub use long_press::{LONG_PRESS, LongPressSample, deliver_long_presses};
pub use node::{ArenaFull, ChildIds, Children, NodeArena, NodeId, NodeIndex, NodeLinks, TreeLinks};
pub use notice::{Notice, NoticeHandler, NoticeKinds, deliver_notices};
pub use paint::{paint_owned, paint_tree, paint_with_transforms};
pub use pinch::{
    PinchPhase, PinchSample, TrackpadPinch, WHEEL_PINCH_REST, deliver_pinch_ends,
    route_trackpad_pinch, route_wheel_zoom,
};
pub use row_gesture::{ReorderLift, RowGesture, SwipeEdge, SwipeParts};
pub use sheet::{SheetDetent, SheetEdge, SheetSpec};
pub use text_style::{
    FontFamily, FontSize, FontSlant, FontWeight, SpanStyle, TextAlign, TextOverflow, TextSizing,
    TextSpan, TextStyle, WidthUse,
};
pub use transform::{TransformTable, Turn};
pub use video::{
    Camera, CameraChoice, CameraFacing, CameraInfo, Playback, VideoEntry, VideoSource, VideoStatus,
    VideoTable,
};
pub use viso_runtime::{ImeEdit, InputKind, Instant, PreeditClauses, primary_is_logo};
pub use wheel::{WheelSample, route_wheel};
// Render primitive data types that already appear in this crate's public API —
// `Content`'s payload fields (content.rs), `TextRequest::color`, and
// `BoxStyle::fill` (style.rs) are all typed with them. Re-export the exact set
// so a downstream crate depending only on `viso-ui` (e.g. `viso-widgets`, which
// the DAG allows to reach `viso-ui` alone) can name them to construct a
// `BoxStyle`, `TextRequest`, or `Content`. `viso-render` is already a `viso-ui`
// dependency, so this adds no new edge.
pub use access::Access;
pub use asset::{
    Asset, AssetBytes, AssetDraw, AssetFit, AssetOrigin, AssetPayload, AssetSource, AssetState,
    AssetTable,
};
pub use backdrop::Backdrop;
pub use cursor::{CursorError, CursorIcon, CursorImage, CustomCursor};
pub use feedback::Haptic;
pub use field_input::{FieldEvent, FieldKey, field_input, field_key, number_input};
pub use field_marks::FieldMarks;
pub use input_filter::InputFilter;
pub use links::LinkPointer;
pub use navigation::PageTransition;
pub use number_format::{NumberFormat, number_kind};
pub use page_dots::{PageDots, PageDotsStyle};
pub use plot::{Plot, PlotBar, PlotGrid, PlotLayer, PlotMark};
pub use progress::{Progress, ProgressShape};
pub use reactive::{
    Cleanup, ComputeCx, ComputedId, ComputedStore, CycleEffect, DepCursor, EffectCx, EffectId,
    EffectOrigin, EffectStore, ProjectId, SemanticProjector,
};
pub use refresh::RefreshStyle;
pub use router::{BackRequest, Navigator, RouteEdit, RouterView, SharedRoutes, router_view};
pub use scenes::{SceneSlots, SceneSync, SceneTarget};
pub use scroll_bar::{ScrollBarStyle, ScrollBars};
pub use scroll_motion::{FlingCurve, OverscrollStyle, ScrollPhysics};
pub use semantics::{
    InspectKind, LiveRegion, Role, SemanticState, Semantics, SemanticsNode, SemanticsTree,
    SortOrder, TableFacts, Validity,
};
pub use shader::ShaderSlots;
pub use shadow::Shadow;
pub use shimmer::{Shimmer, ShimmerLook};
pub use snap::{ScrollSnap, SnapAlign};
pub use spinner::{Spinner, SpinnerLook};
pub use state::{StateId, StateStore, StateValue};
pub use structure::{StructureCx, StructureHookId, run_structure_hooks};
pub use style::{BoxStyle, InteractionStyle};
pub use task::{Continuation, TaskFuture, TaskId, TaskOps};
pub use text_edit::{
    Buffer, CARET_WIDTH, EditGeometry, EditIntent, EditLayout, FieldLook, FieldSeed, MarkState,
    Motion, Reach, SelectionEnd, TextEdits, TouchEdit, TouchState,
};
pub use text_entry::TextEntryRequest;
pub use timer::{TimerId, TimerRegistry, TimerRequest};
pub use typed_state::{State, StateKind, StateRead};
pub use viewer::{ViewerLimits, ViewerMove};
pub use virtual_grid::{Columns, ListGrid};
pub use virtual_list::{
    AcrossView, AcrossWatch, HeightCache, HeightTree, ItemBuilder, StickyOf, VirtualListState,
    VirtualLists, absorb_measurements, reconcile, set_item_count, set_sticky_of, splice_items,
    watch_across,
};
/// The renderer's asset vocabulary — resource identity, formats, decoded
/// payloads — which the compiler names resources by and the facade decodes
/// into.
pub use viso_render::asset as resource;
/// The 3D scene vocabulary a `Content::Scene` node's frame is written in.
pub use viso_render::scene3d;
pub use viso_render::{
    Border, LineCap, LineJoin, PathCmd, Point, Primitive, Rect, Rgba, Srgb, Stroke, TextureId,
};
pub use viso_text::selection::Selection;
pub use viso_text::{CaretAffinity, TextOffset, TextPosition};
pub use window::{ChromeContext, WindowChrome, WindowConfig, WindowIdSlot, WindowOpenRequest};
