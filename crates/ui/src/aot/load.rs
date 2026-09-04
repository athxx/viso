//! The release AOT loader: instantiate a compact [`AotPackage`] into a live tree
//! (architecture section 41; AGENTS 21.6, 60).
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

use viso_ende::{Decode, DecodeError};

use crate::binding::BindingTable;
use crate::component::{BuildCx, FlexStyle, LeafStyle, NodeStore, ScrollStyle};
use crate::dirty::DirtyClass;
use crate::layout::{Axis, Length, Size};
use crate::node::NodeId;
use crate::state::{StateKey, StateStore, StateValue};
use crate::virtual_list::VirtualLists;

use super::package::{AotAxis, AotLength, AotNode, AotNodeKind, AotPackage, AotStyle};

/// A neutral value for a state cell the loader has to allocate because the package
/// referenced a key the app has not authored yet. Mirrors the commit's brand-new
/// source handling (`StateValue::Int(0)`): the cell only needs to exist so its
/// bindings resolve; the app authors the real initial value on its next build.
const NEUTRAL_STATE: StateValue = StateValue::Int(0);

/// Decode a package blob and instantiate it into the given runtime, the runtime
/// form of "app startup reads an embedded asset and instantiates it directly".
///
/// A malformed blob returns a [`DecodeError`] rather than panicking — the bounded
/// decoder is the safety precondition for loading an untrusted asset (section 30).
/// A well-formed but empty package instantiates nothing and returns `None`.
pub fn load_from_bytes(
    blob: &[u8],
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &mut BindingTable,
    lists: &mut VirtualLists,
) -> Result<Option<NodeId>, DecodeError> {
    let pkg = AotPackage::decode_from_slice(blob)?;
    Ok(instantiate(&pkg, store, states, bindings, lists))
}

/// Instantiate an already-decoded package into a live tree, returning its root.
///
/// The node table is walked in pre-order: a shared cursor descends into each
/// container's `child_count` children exactly as the commit's builder closures
/// recurse, so a node's pre-order index (its position in [`AotPackage::nodes`]) is
/// the identity the binding edges reference. Each node's live [`NodeId`] is recorded
/// in that same pre-order, then — once the build context is dropped and the state
/// store is free again — each edge is wired by durable [`StateKey`] identity.
pub fn instantiate(
    pkg: &AotPackage,
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &mut BindingTable,
    lists: &mut VirtualLists,
) -> Option<NodeId> {
    // Phase one: author the retained tree, recording node_index -> live NodeId by
    // pre-order index. The `BuildCx` borrows the state store, so binding is deferred
    // to phase two. The map is pre-sized and each slot is filled at its own index,
    // so a container's id lands before its children's regardless of author order.
    let mut node_ids: Vec<Option<NodeId>> = vec![None; pkg.nodes.len()];
    let root = {
        let mut cx = BuildCx::with_reactive(store, states, bindings, lists);
        let mut cursor = 0usize;
        while cursor < pkg.nodes.len() {
            build_node(&mut cx, &pkg.nodes, &mut cursor, &mut node_ids);
        }
        cx.root()
    };

    // Phase two: wire each binding edge by durable identity, mirroring the commit's
    // `rebind_static`. A key already registered (a state the app authored, or a
    // prior load) resolves to its live cell; an unknown key gets a neutral cell
    // allocated and registered, so the binding always resolves — the app authors
    // the real initial value on its own build. An edge whose node index is out of
    // range is skipped defensively (a corrupt-but-well-typed package).
    for edge in &pkg.edges {
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

/// Author one node and its subtree, advancing the shared pre-order `cursor` past the
/// whole subtree and recording this node's live [`NodeId`] at its own pre-order
/// index. Mirrors the commit's `build_node`: the builder call is selected by
/// [`AotNodeKind`], and a container recurses into exactly its `child_count` immediate
/// children (each of which recurses over its own subtree), so the flat table
/// reconstructs the tree in one forward pass with `node_ids` staying index-aligned.
fn build_node(
    cx: &mut BuildCx<'_>,
    nodes: &[AotNode],
    cursor: &mut usize,
    node_ids: &mut [Option<NodeId>],
) {
    let index = *cursor;
    let node = &nodes[index];
    *cursor += 1;
    let child_count = node.child_count as usize;

    // The handle is recorded at this node's own pre-order `index`, so ordering is
    // independent of when the closure authors children (which fill higher indices).
    let handle = match node.kind {
        AotNodeKind::Flex => cx.flex(flex_style(&node.style), |cx| {
            for _ in 0..child_count {
                build_node(cx, nodes, cursor, node_ids);
            }
        }),
        AotNodeKind::Grid => cx.grid(Default::default(), |cx| {
            for _ in 0..child_count {
                build_node(cx, nodes, cursor, node_ids);
            }
        }),
        AotNodeKind::Scroll => cx.scroll(scroll_style(&node.style), |cx| {
            for _ in 0..child_count {
                build_node(cx, nodes, cursor, node_ids);
            }
        }),
        // A leaf hosts no child region; the emitter guarantees `child_count == 0` for
        // a leaf, so nothing is descended into here (a `VirtualList` collapses to a
        // leaf on the package side, exactly as the commit twin treats it).
        AotNodeKind::Leaf => cx.leaf(leaf_style(&node.style)),
    };
    node_ids[index] = Some(handle.id());
}

/// The flex builder style for a packaged container, mirroring the commit's
/// `flex_style`: axis and gap when set, an explicit size only when the node authored
/// a width or height, else the builder default.
fn flex_style(style: &AotStyle) -> FlexStyle {
    let mut out = FlexStyle::default();
    if let Some(axis) = style.axis {
        out.axis = axis_of(axis);
    }
    if let Some(gap) = style.gap {
        out.gap = gap;
    }
    if style.width.is_some() || style.height.is_some() {
        out.size = size_of(style);
    }
    out
}

/// The scroll builder style, mirroring the commit's `scroll_style`: the authored
/// axis (default column) and a size only when authored.
fn scroll_style(style: &AotStyle) -> ScrollStyle {
    let mut out = ScrollStyle::default();
    if let Some(axis) = style.axis {
        out.axis = axis_of(axis);
    }
    if style.width.is_some() || style.height.is_some() {
        out.size = size_of(style);
    }
    out
}

/// The leaf builder style, mirroring the commit's `leaf_style`: a size only when the
/// node authored a width or height, else the default.
fn leaf_style(style: &AotStyle) -> LeafStyle {
    let mut out = LeafStyle::default();
    if style.width.is_some() || style.height.is_some() {
        out.size = size_of(style);
    }
    out
}

/// The runtime [`Size`] for a packaged style, mirroring the commit's `size_of`: an
/// unset dimension lowers to `Fit`.
fn size_of(style: &AotStyle) -> Size {
    Size {
        width: length_of(style.width),
        height: length_of(style.height),
    }
}

/// One packaged length to a runtime [`Length`]; `None` is `Fit`, matching the
/// commit's `length_of`.
fn length_of(len: Option<AotLength>) -> Length {
    match len {
        Some(AotLength::Fixed(px)) => Length::Fixed(px),
        Some(AotLength::Fill { weight }) => Length::Fill { weight },
        Some(AotLength::Fit) | None => Length::Fit,
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
    use super::super::package::AotEdge;
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
        if let Some(child) = arena.links(root).and_then(|l| l.first_child) {
            let mut next = Some(child);
            while let Some(id) = next {
                count += live_node_count(arena, id);
                next = arena.links(id).and_then(|l| l.next_sibling);
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
                    child_count: 2,
                },
                AotNode {
                    kind: AotNodeKind::Leaf,
                    style: AotStyle::default(),
                    child_count: 0,
                },
                AotNode {
                    kind: AotNodeKind::Leaf,
                    style: AotStyle::default(),
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
        let pkg = sample_package(label);
        let root = instantiate(
            &pkg,
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
        let pkg = sample_package(label);
        let root = instantiate(
            &pkg,
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
        let bindings = rt.bindings.for_state(state);
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

        let pkg = sample_package(label);
        instantiate(
            &pkg,
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
        assert_eq!(rt.bindings.for_state(existing).len(), 1);
    }

    #[test]
    fn empty_package_instantiates_nothing() {
        let mut rt = Runtime::new();
        let root = instantiate(
            &AotPackage::default(),
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
