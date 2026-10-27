//! The release AOT loader: instantiate a compact [`AotPackage`] into a live tree.
//!
//! This is the runtime twin of the hot-reload commit's `build_tree`/`build_node` —
//! it authors the exact same live tree, but from a compact, dependency-free package
//! rather than a metadata-rich in-memory `UiTree`, and with **no DSL compiler
//! present**. The builder-selection semantics, the folded-style-to-builder-style
//! lowering, and the "unset dimension is `Fit`" rule are kept in step with the
//! commit path so the three lowering targets (macro tokens, live commit, AOT
//! package) build structurally identical trees from the same frontend IR.
//!
//! Instantiation is two-phase, exactly like the commit: first the node table is
//! walked to author the retained tree and record each node's live [`NodeId`], then
//! the binding edges are wired by resolving each durable [`StateKey`] to a live
//! state cell and calling [`BindingTable::bind`]. The build phase borrows the state
//! store, so the binding phase runs after it, once the builder context is dropped —
//! the same phase split `commit.rs` uses.
//!
//! The whole module imports no `viso-dsl` type — that is the load-bearing exit
//! criterion of Slice P at the type-system level: an AOT-packaged app links and
//! runs with the compiler absent from its dependency graph.

use viso_ende::DecodeError;

use crate::binding::BindingTable;
use crate::component::{
    AbsoluteStyle, BuildCx, FlexStyle, FlowStyle, Handle, LeafStyle, NodeStore, ScrollStyle,
    StackStyle,
};
use crate::dirty::DirtyClass;
use crate::grid::GridStyle;
use crate::layout::{Axis, LayoutInput, Length, Sizing};
use crate::layout::{BoxRules, Inset};
use crate::node::NodeId;
use crate::state::{StateKey, StateStore, StateValue};
use crate::virtual_list::VirtualLists;

use super::package::{AotAxis, AotLength, AotNodeKind, AotRecord, AotSheet, AotStyle, AotView};
use crate::sheet::SheetSpec;

/// A neutral value for a state cell the loader has to allocate because the package
/// referenced a key the app has not authored yet. Mirrors the commit's brand-new
/// source handling (`StateValue::Int(0)`): the cell only needs to exist so its
/// bindings resolve; the app authors the real initial value on its next build.
const NEUTRAL_STATE: StateValue = StateValue::Int(0);

/// Validate a package blob and instantiate it into the given runtime, the
/// runtime form of "app startup reads an embedded asset and instantiates it
/// directly". The blob is read where it lies — an embedded `static`, mapped
/// flash, an mmap — one node record at a time, so nothing of it is copied.
///
/// A malformed blob returns a [`DecodeError`] rather than panicking — the bounded
/// decoder is the safety precondition for loading an untrusted asset —
/// and it does so before anything is instantiated. A well-formed but empty
/// package instantiates nothing and returns `None`.
pub fn load_from_bytes(
    blob: &[u8],
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &mut BindingTable,
    lists: &mut VirtualLists,
) -> Result<Option<NodeId>, DecodeError> {
    let view = AotView::new(blob)?;
    Ok(instantiate(&view, store, states, bindings, lists))
}

/// Instantiate a validated package into a live tree, returning its root.
///
/// The node table is walked in pre-order: a shared cursor descends into each
/// container's `child_count` children exactly as the commit's builder closures
/// recurse, so a node's pre-order index is the identity the binding edges
/// reference. Each node's live [`NodeId`] is recorded in that same pre-order,
/// then — once the build context is dropped and the state store is free again —
/// each edge is wired by durable [`StateKey`] identity. The view validated the
/// whole package, so this cannot fail partway.
pub fn instantiate(
    view: &AotView<'_>,
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &mut BindingTable,
    lists: &mut VirtualLists,
) -> Option<NodeId> {
    let mut node_ids = Vec::new();
    instantiate_indexed(view, store, states, bindings, lists, &mut node_ids)
}

/// [`instantiate`], also leaving each package node's live [`NodeId`] in
/// `node_ids` at its pre-order index, for a caller that attaches more to the
/// nodes it built (a view's event handlers).
pub fn instantiate_indexed(
    view: &AotView<'_>,
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &mut BindingTable,
    lists: &mut VirtualLists,
    node_ids: &mut Vec<Option<NodeId>>,
) -> Option<NodeId> {
    // Phase one: author the retained tree, recording node_index -> live NodeId by
    // pre-order index. The `BuildCx` borrows the state store, so binding is deferred
    // to phase two. The map is pre-sized and each slot is filled at its own index,
    // so a container's id lands before its children's regardless of author order.
    // The AOT package has no editable-text node yet (the compiler does not lower a
    // `text_input`), so a throwaway registry satisfies the reactive-cx contract
    // without threading an edit registry through the public loader API.
    let mut text_edits = crate::text_edit::TextEdits::new();
    // Likewise the AOT package lowers no semantic-state projection yet, so a
    // throwaway projector registry satisfies the reactive-cx contract without
    // threading one through the public loader API.
    let mut projectors = crate::reactive::SemanticProjector::new();
    let root = {
        let mut cx = BuildCx::with_reactive(
            store,
            states,
            bindings,
            lists,
            &mut text_edits,
            &mut projectors,
        );
        build_nodes(&mut cx, view.nodes(), node_ids);
        cx.root()
    };

    // Phase two: wire each binding edge by durable identity, mirroring the commit's
    // `rebind_static`. A key already registered (a state the app authored, or a
    // prior load) resolves to its live cell; an unknown key gets a neutral cell
    // allocated and registered, so the binding always resolves — the app authors
    // the real initial value on its own build. The view checked every edge names
    // a node of the table.
    for edge in view.edges() {
        let Some(Some(node)) = node_ids.get(edge.node as usize).copied() else {
            continue;
        };
        let state = resolve_state(states, edge.state);
        bindings.bind(state, node, DirtyClass::from_bits(edge.class));
    }

    root
}

/// Resolve a durable [`StateKey`] to its live state cell, allocating and registering
/// a neutral cell for a key the runtime has not seen yet. The cold registration path
/// reuses Slice O's `id_for_key`/`alloc`/`bind_key` so the AOT loader and the
/// hot-reload commit agree on state identity.
fn resolve_state(states: &mut StateStore, key: StateKey) -> crate::state::StateId {
    if let Some(id) = states.id_for_key(key) {
        return id;
    }
    let id = states.alloc(NEUTRAL_STATE);
    states.bind_key(id, key);
    id
}

/// Authors the pre-order node table `nodes` through `cx`, leaving each
/// node's live [`NodeId`] in `node_ids` at its pre-order index. The table
/// is a forest that closes, as [`AotView`] checks: a container's subtrees
/// are all in it.
pub fn build_nodes<'a>(
    cx: &mut BuildCx<'_>,
    nodes: impl ExactSizeIterator<Item = AotRecord<'a>>,
    node_ids: &mut Vec<Option<NodeId>>,
) {
    node_ids.clear();
    node_ids.resize(nodes.len(), None);
    cx.reserve_nodes(nodes.len());
    let mut nodes = nodes.enumerate();
    while build_node(cx, &mut nodes, node_ids) {}
}

/// Re-applies packaged node `node`'s style to `live`, a node built from an
/// earlier version of it that keeps its runtime state: the built size, gap
/// and grid template, its cell in a grid, and the bound environment lengths.
/// Only a request that moved is rewritten and dirtied. An axis or a gap bound
/// to environment lengths keeps its folded value until its terms change, and
/// the node is an adaptive scope or an avoiding region exactly when `node` is
/// one.
pub fn restyle_aot_node(
    store: &mut NodeStore,
    states: &mut StateStore,
    node: &AotRecord,
    live: NodeId,
) {
    let style = &node.style;
    let lengths = node.lengths();
    let lengths = lengths.as_ref();
    let (mut size, gap, padding) = match node.kind {
        AotNodeKind::Flex => {
            let built = flex_style(node);
            store.set_flex_arrangement(live, (built.axis, built.justify, built.align));
            (built.size, Some(built.gap), Some(built.padding))
        }
        AotNodeKind::Grid => {
            let mut built = grid_style(node);
            if let LayoutInput::Grid {
                column_gap,
                row_gap,
                ..
            } = store.layout_input(live)
            {
                if lengths.is_some_and(|lengths| lengths.gap.is_some()) {
                    built.column_gap = column_gap;
                }
                if lengths.is_some_and(|lengths| lengths.row_gap.is_some()) {
                    built.row_gap = row_gap;
                }
            }
            let (size, padding) = (built.size, built.padding);
            store.set_grid_template(live, built);
            (size, None, Some(padding))
        }
        AotNodeKind::Stack => {
            let built = stack_style(node);
            store.set_stack_align(live, built.align);
            if let Some(sheet) = style.sheet {
                store.set_layer(live, true);
                store.set_hittable(live, true);
                store.set_sheet(live, Some(sheet_spec(sheet)));
            } else if store.is_sheet(live) {
                store.set_sheet(live, None);
            }
            (built.size, None, Some(built.padding))
        }
        AotNodeKind::Absolute => {
            let built = absolute_style(node);
            (built.size, None, Some(built.padding))
        }
        AotNodeKind::Flow => {
            let built = flow_style(node);
            store.set_flex_arrangement(live, (built.axis, built.justify, built.align));
            if lengths.is_none_or(|lengths| lengths.row_gap.is_none()) {
                store.set_line_gap(live, built.line_gap);
            }
            (built.size, Some(built.gap), Some(built.padding))
        }
        AotNodeKind::Scroll => (scroll_style(node).size, None, None),
        AotNodeKind::Leaf => (leaf_style(node).size, None, None),
    };
    // Insets bound to environment lengths are theirs to fold.
    if lengths.is_none_or(|lengths| lengths.insets.is_none()) {
        match style.place {
            Some(place) => store.set_placement(live, place),
            None => store.clear_placement(live),
        }
    }
    if let Some(current) = store.size_request(live) {
        if lengths.is_some_and(|lengths| lengths.width.is_some()) {
            size.width = current.width;
        }
        if lengths.is_some_and(|lengths| lengths.height.is_some()) {
            size.height = current.height;
        }
    }
    store.set_fixed_size(live, size);
    if let Some(gap) = gap
        && lengths.is_none_or(|lengths| lengths.gap.is_none())
    {
        store.set_gap(live, gap);
    }
    // Lengths that carry the box rules own them; otherwise they are the
    // node's constant ones, or none.
    if lengths.is_none_or(|lengths| !lengths.has_rules()) {
        store.set_box_rules(live, node.rules().unwrap_or(BoxRules::NONE));
    }
    match lengths {
        Some(lengths) => store.bind_lengths(live, *lengths),
        None => store.unbind_lengths(live),
    }
    match style.scope {
        Some(scope) => states.mark_adaptive_scope(live, scope.basis),
        None => {
            states.unmark_adaptive_scope(live);
        }
    }
    store.set_layout_direction(live, style.scope.and_then(|scope| scope.direction));
    match style.avoid {
        Some(avoid) => states.mark_avoiding(live, avoid, Inset::default()),
        None => {
            states.unmark_avoiding(live);
            // An avoiding region's padding is the runtime's; any other's is
            // authored, folded here or, reading the environment, at layout.
            if let Some(padding) = padding
                && lengths.is_none_or(|lengths| lengths.padding.is_none())
            {
                store.set_padding(live, padding);
            }
        }
    }
}

/// Author the next node of `nodes` and its subtree, recording each node's live
/// [`NodeId`] at its own pre-order index; `false` once the table is done.
/// Mirrors the commit's `build_node`: the builder call is selected by
/// [`AotNodeKind`], and a container recurses into exactly its `child_count`
/// immediate children (each of which recurses over its own subtree), so the flat
/// table reconstructs the tree in one forward pass with `node_ids` staying
/// index-aligned.
fn build_node<'a>(
    cx: &mut BuildCx<'_>,
    nodes: &mut impl Iterator<Item = (usize, AotRecord<'a>)>,
    node_ids: &mut [Option<NodeId>],
) -> bool {
    let Some((index, node)) = nodes.next() else {
        return false;
    };
    // The handle is recorded at this node's own pre-order `index`, so ordering is
    // independent of when the closure authors children (which fill higher indices).
    let handle = build_aot_node(cx, &node, |cx| {
        for _ in 0..node.child_count {
            build_node(cx, nodes, node_ids);
        }
    });
    node_ids[index] = Some(handle.id());
    true
}

/// Authors one packaged node through the builder its [`AotNodeKind`] selects,
/// with `children` run inside a container (a leaf never runs it). The one
/// builder selection every path that authors from packaged node data shares.
pub fn build_aot_node(
    cx: &mut BuildCx<'_>,
    node: &AotRecord,
    children: impl FnOnce(&mut BuildCx<'_>),
) -> Handle {
    let style = &node.style;
    if let Some(place) = style.place {
        cx.place(place);
    }
    let handle = match node.kind {
        AotNodeKind::Flex => match (style.scope, style.avoid) {
            (Some(scope), _) => {
                cx.adaptive_scope(scope.basis, scope.direction, flex_style(node), children)
            }
            (None, Some(avoid)) => cx.avoiding(avoid, flex_style(node), children),
            (None, None) => cx.flex(flex_style(node), children),
        },
        AotNodeKind::Grid => cx.grid(grid_style(node), children),
        AotNodeKind::Stack => cx.stack(stack_style(node), children),
        AotNodeKind::Absolute => cx.absolute(absolute_style(node), children),
        AotNodeKind::Flow => cx.flow(flow_style(node), children),
        AotNodeKind::Scroll => cx.scroll(scroll_style(node), children),
        // A leaf hosts no child region; the view guarantees `child_count == 0`
        // for a leaf.
        AotNodeKind::Leaf => cx.leaf(leaf_style(node)),
    };
    if let Some(rules) = node.rules() {
        cx.set_box_rules(handle, rules);
    }
    if let Some(lengths) = node.lengths() {
        cx.bind_lengths(handle, lengths);
    }
    if let Some(sheet) = style.sheet.filter(|_| node.kind == AotNodeKind::Stack) {
        cx.sheet(handle, sheet_spec(sheet), None);
    }
    handle
}

/// The sheet a packaged sheet stack is.
fn sheet_spec(sheet: AotSheet) -> SheetSpec {
    SheetSpec {
        edge: sheet.edge,
        detents: sheet.detents().to_vec(),
        dismissible: sheet.dismissible,
    }
}

/// The flex builder style for a packaged container, mirroring the commit's
/// `flex_style`: axis and gap when set, an explicit size only when the node authored
/// a width or height, else the builder default.
fn flex_style(node: &AotRecord) -> FlexStyle {
    let style = &node.style;
    let mut out = FlexStyle::default();
    if let Some(axis) = style.axis {
        out.axis = axis_of(axis);
    }
    if let Some(justify) = style.justify {
        out.justify = justify;
    }
    if let Some(align) = style.align {
        out.align = align;
    }
    if let Some(gap) = style.gap {
        out.gap = gap;
    }
    if let Some(padding) = style.padding {
        out.padding = padding;
    }
    if has_size(node) {
        out.size = size_of(style);
    }
    out
}

/// The flow builder style for a packaged flow: its gaps, placements and
/// padding and, when the node authored one, its size; what it left out is
/// the builder default.
fn flow_style(node: &AotRecord) -> FlowStyle {
    let style = &node.style;
    let mut out = FlowStyle {
        gap: style.gap.unwrap_or_default(),
        line_gap: style.line_gap.unwrap_or_default(),
        padding: style.padding.unwrap_or_default(),
        ..FlowStyle::default()
    };
    if let Some(justify) = style.justify {
        out.justify = justify;
    }
    if let Some(align) = style.align {
        out.align = align;
    }
    if has_size(node) {
        out.size = size_of(style);
    }
    out
}

/// The grid builder style for a packaged grid: its tracks, gaps, cell
/// alignment and adaptive columns, its padding and, when the node authored
/// one, its size; what it left out is the builder default.
fn grid_style(node: &AotRecord) -> GridStyle {
    let style = &node.style;
    let mut out = GridStyle {
        padding: style.padding.unwrap_or_default(),
        ..GridStyle::default()
    };
    if let Some(grid) = node.grid() {
        out.columns = grid.columns;
        out.rows = grid.rows;
        out.auto_rows = grid.auto_rows;
        out.column_gap = grid.column_gap;
        out.row_gap = grid.row_gap;
        out.align_items = grid.align_items;
        out.adaptive_columns = grid.adaptive_columns;
    }
    if has_size(node) {
        out.size = size_of(style);
    }
    out
}

/// The stack builder style for a packaged stack: its alignment, its padding
/// and, when the node authored one, its size.
fn stack_style(node: &AotRecord) -> StackStyle {
    let style = &node.style;
    let mut out = StackStyle {
        padding: style.padding.unwrap_or_default(),
        align: style.content_align.unwrap_or_default(),
        ..StackStyle::default()
    };
    if has_size(node) {
        out.size = size_of(style);
    }
    out
}

/// The absolute builder style for a packaged absolute container: its
/// padding and, when the node authored one, its size.
fn absolute_style(node: &AotRecord) -> AbsoluteStyle {
    let style = &node.style;
    let mut out = AbsoluteStyle {
        padding: style.padding.unwrap_or_default(),
        ..AbsoluteStyle::default()
    };
    if has_size(node) {
        out.size = size_of(style);
    }
    out
}

/// The scroll builder style, mirroring the commit's `scroll_style`: the authored
/// axis (default column) and a size only when authored.
fn scroll_style(node: &AotRecord) -> ScrollStyle {
    let style = &node.style;
    let mut out = ScrollStyle::default();
    if let Some(axis) = style.axis {
        out.axis = axis_of(axis);
    }
    if has_size(node) {
        out.size = size_of(style);
    }
    if let Some(scroll) = style.scroll {
        out.bars = if scroll.bars {
            crate::scroll_bar::ScrollBars::Auto
        } else {
            crate::scroll_bar::ScrollBars::Hidden
        };
        out.across = scroll.across;
        out.tail = scroll.tail;
        out.snap = scroll.snap.map(|snap| snap_of(snap, scroll.snap_inset));
        out.dots = scroll.dots.then(crate::page_dots::PageDotsStyle::default);
    }
    out
}

/// A package's snap, `inset` dp in from the viewport's edges.
fn snap_of(snap: viso_schema::aot::AotSnap, inset: f32) -> crate::snap::ScrollSnap {
    use crate::snap::{ScrollSnap, SnapAlign};
    use viso_schema::aot::AotSnap;
    let base = match snap {
        AotSnap::Start => ScrollSnap::START,
        AotSnap::Center => ScrollSnap::CENTER,
        AotSnap::End => ScrollSnap {
            align: SnapAlign::End,
            ..ScrollSnap::START
        },
        AotSnap::Pages => ScrollSnap::PAGES,
    };
    ScrollSnap { inset, ..base }
}

/// The leaf builder style, mirroring the macro's: the size the node authored,
/// `Fit` on an axis it did not, so a leaf measures to its content.
fn leaf_style(node: &AotRecord) -> LeafStyle {
    LeafStyle {
        size: size_of(&node.style),
        ..LeafStyle::default()
    }
}

/// Whether the node authored a width or a height, folded or bound to the
/// environment, mirroring the commit's `has_size`.
fn has_size(node: &AotRecord) -> bool {
    let style = &node.style;
    style.width.is_some()
        || style.height.is_some()
        || node
            .lengths()
            .is_some_and(|lengths| lengths.width.is_some() || lengths.height.is_some())
}

/// The runtime [`Sizing`] for a packaged style, mirroring the commit's `size_of`: an
/// unset dimension lowers to `Fit`.
fn size_of(style: &AotStyle) -> Sizing {
    Sizing {
        width: length_of(style.width),
        height: length_of(style.height),
    }
}

/// One packaged length to a runtime [`Length`]; `None` is `Fit`, matching the
/// commit's `length_of`.
fn length_of(len: Option<AotLength>) -> Length {
    match len {
        Some(AotLength::Fixed(dp)) => Length::Fixed(dp),
        Some(AotLength::Relative { fixed, pct }) => Length::Relative { fixed, pct },
        Some(AotLength::Fill { weight }) => Length::Fill { weight },
        Some(AotLength::Fit) | None => Length::Fit,
        Some(AotLength::MinContent) => Length::MinContent,
        Some(AotLength::MaxContent) => Length::MaxContent,
    }
}

/// One packaged axis to the runtime [`Axis`].
fn axis_of(axis: AotAxis) -> Axis {
    match axis {
        AotAxis::Row => Axis::Row,
        AotAxis::Column => Axis::Column,
    }
}

#[cfg(test)]
mod tests {
    use super::super::package::{AotEdge, AotNode, AotPackage};
    use super::*;
    use crate::node::NodeArena;
    use crate::state::StateId;
    use viso_ende::Encode;

    /// A fresh, empty runtime — the four stores a release app instantiates into.
    struct Runtime {
        store: NodeStore,
        states: StateStore,
        bindings: BindingTable,
        lists: VirtualLists,
    }

    impl Runtime {
        fn new() -> Self {
            Runtime {
                store: NodeStore::new(),
                states: StateStore::new(),
                bindings: BindingTable::new(),
                lists: VirtualLists::new(),
            }
        }
    }

    /// The live pre-order node count reachable from `root`, walking arena ancestry.
    fn live_node_count(arena: &NodeArena, root: NodeId) -> usize {
        let mut count = 1;
        if let Some(child) = arena.first_child(root) {
            let mut next = Some(child);
            while let Some(id) = next {
                count += live_node_count(arena, id);
                next = arena.next_sibling(id);
            }
        }
        count
    }

    /// A Row (flex) with two leaf children, one of which a state drives.
    fn sample_package(label: StateKey) -> AotPackage {
        AotPackage {
            nodes: vec![
                AotNode {
                    kind: AotNodeKind::Flex,
                    style: AotStyle {
                        axis: Some(AotAxis::Row),
                        gap: Some(4.0),
                        ..AotStyle::default()
                    },
                    lengths: None,
                    rules: None,
                    grid: None,
                    child_count: 2,
                },
                AotNode {
                    kind: AotNodeKind::Leaf,
                    style: AotStyle::default(),
                    lengths: None,
                    rules: None,
                    grid: None,
                    child_count: 0,
                },
                AotNode {
                    kind: AotNodeKind::Leaf,
                    style: AotStyle::default(),
                    lengths: None,
                    rules: None,
                    grid: None,
                    child_count: 0,
                },
            ],
            edges: vec![AotEdge {
                state: label,
                node: 2,
                class: DirtyClass::MEASURE.bits()
                    | DirtyClass::LAYOUT.bits()
                    | DirtyClass::PAINT.bits(),
            }],
        }
    }

    #[test]
    fn instantiates_the_pre_order_tree() {
        let mut rt = Runtime::new();
        let label = StateKey::from_parts(7, 11);
        let blob = sample_package(label).encode_to_vec();
        let root = instantiate(
            &AotView::new(&blob).unwrap(),
            &mut rt.store,
            &mut rt.states,
            &mut rt.bindings,
            &mut rt.lists,
        )
        .expect("a non-empty package has a root");
        // Root + two leaves, all reachable through arena ancestry.
        assert_eq!(live_node_count(rt.store.arena(), root), 3);
    }

    #[test]
    fn wires_the_binding_to_the_right_node_and_class() {
        let mut rt = Runtime::new();
        let label = StateKey::from_parts(7, 11);
        let blob = sample_package(label).encode_to_vec();
        let root = instantiate(
            &AotView::new(&blob).unwrap(),
            &mut rt.store,
            &mut rt.states,
            &mut rt.bindings,
            &mut rt.lists,
        )
        .expect("root");

        // The edge referenced node index 2: the root's second child.
        let first = rt.store.arena().links(root).unwrap().first_child.unwrap();
        let second = rt.store.arena().links(first).unwrap().next_sibling.unwrap();

        // The unknown key was allocated and registered, and its binding reaches the
        // second leaf with exactly the edge's classes.
        let state = rt.states.id_for_key(label).expect("key was registered");
        let bindings: Vec<_> = rt.bindings.for_state(state).collect();
        assert_eq!(bindings.len(), 1, "one edge for the driven state");
        assert_eq!(bindings[0].node, second);
        assert_eq!(
            bindings[0].class,
            DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT
        );
    }

    #[test]
    fn reuses_an_already_registered_state_cell() {
        let mut rt = Runtime::new();
        let label = StateKey::from_parts(42, 42);
        // Pre-register the key with a real value, as the app's own build would.
        let existing: StateId = rt.states.alloc(StateValue::Int(99));
        assert!(rt.states.bind_key(existing, label));

        let blob = sample_package(label).encode_to_vec();
        instantiate(
            &AotView::new(&blob).unwrap(),
            &mut rt.store,
            &mut rt.states,
            &mut rt.bindings,
            &mut rt.lists,
        )
        .expect("root");

        // The loader bound to the existing cell rather than allocating a neutral one,
        // so the app's value survives and the binding keys against the same id.
        assert_eq!(rt.states.id_for_key(label), Some(existing));
        assert_eq!(rt.states.get(existing), Some(StateValue::Int(99)));
        assert_eq!(rt.bindings.for_state(existing).count(), 1);
    }

    #[test]
    fn empty_package_instantiates_nothing() {
        let mut rt = Runtime::new();
        let blob = AotPackage::default().encode_to_vec();
        let root = instantiate(
            &AotView::new(&blob).unwrap(),
            &mut rt.store,
            &mut rt.states,
            &mut rt.bindings,
            &mut rt.lists,
        );
        assert_eq!(root, None);
    }

    #[test]
    fn load_from_bytes_round_trips_through_the_blob() {
        let mut rt = Runtime::new();
        let label = StateKey::from_parts(1, 2);
        let blob = sample_package(label).encode_to_vec();
        let root = load_from_bytes(
            &blob,
            &mut rt.store,
            &mut rt.states,
            &mut rt.bindings,
            &mut rt.lists,
        )
        .expect("well-formed blob loads")
        .expect("non-empty package has a root");
        assert_eq!(live_node_count(rt.store.arena(), root), 3);
    }

    #[test]
    fn a_malformed_blob_is_an_error_not_a_panic() {
        let mut rt = Runtime::new();
        let mut blob = sample_package(StateKey::from_parts(1, 2)).encode_to_vec();
        blob[0] ^= 0xff; // clobber the magic
        let res = load_from_bytes(
            &blob,
            &mut rt.store,
            &mut rt.states,
            &mut rt.bindings,
            &mut rt.lists,
        );
        assert!(res.is_err(), "a corrupt blob must be a DecodeError");
    }
}
