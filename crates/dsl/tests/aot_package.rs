//! Release AOT package end-to-end tests (architecture section 41; AGENTS 21.6, 60,
//! 66) — the executable form of Slice P's exit criterion.
//!
//! The non-negotiable exit criterion is: **an AOT-packaged app boots and renders with
//! the DSL compiler absent from the release graph.** These headless, deterministic
//! tests prove the closed loop `build → package → load → live tree` and, crucially,
//! that the load half touches *only* `viso-ui` runtime types — no `viso-dsl` symbol
//! enters the instantiation path. `viso-dsl` is used only on the build side to produce
//! the byte blob; everything after `build_package` returns is what a release binary
//! would run with the compiler stripped out.
//!
//! The comparison test closes the argument that the AOT package is the same lowering
//! the other two targets produce: the tree instantiated from the blob is structurally
//! identical to the one Slice O's live commit builds from the same source, so the
//! three targets (macro tokens, live commit, AOT package) are genuinely one frontend.

use viso_dsl::aot::build_package;
use viso_ui::aot::load_from_bytes;
use viso_ui::dirty::DirtyClass;
use viso_ui::state::StateKey;
use viso_ui::virtual_list::VirtualLists;
use viso_ui::{BindingTable, NodeId, NodeStore, StateStore, StateValue};

/// The four stores a release app instantiates a package into — the whole runtime the
/// load path needs, and nothing from the compiler.
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

/// The live retained tree's node ids in the pre-order the loader authors them — the
/// same pre-order the AOT node table and the binding `NodeKey`s are numbered in.
fn preorder(store: &NodeStore, root: Option<NodeId>) -> Vec<NodeId> {
    let mut out = Vec::new();
    if let Some(root) = root {
        walk(store, root, &mut out);
    }
    out
}

fn walk(store: &NodeStore, node: NodeId, out: &mut Vec<NodeId>) {
    out.push(node);
    let mut child = store.arena().links(node).and_then(|l| l.first_child);
    while let Some(c) = child {
        walk(store, c, out);
        child = store.arena().links(c).and_then(|l| l.next_sibling);
    }
}

#[test]
fn packaged_app_boots_and_renders_with_the_compiler_absent_from_the_load_path() {
    // Build side: the compiler lowers source to a compact asset blob. This is the only
    // place `viso-dsl` appears; a release binary runs this at build time, not startup.
    let blob = build_package("Row { Text { text: label; } }").expect("compiles to a package");

    // Load side: a release runtime instantiates the blob using only `viso-ui`. The
    // exit criterion holds at the type-system level — nothing below this line names a
    // `viso-dsl` type.
    let mut rt = Runtime::new();
    let root = load_from_bytes(
        &blob,
        &mut rt.store,
        &mut rt.states,
        &mut rt.bindings,
        &mut rt.lists,
    )
    .expect("a well-formed package loads")
    .expect("a non-empty package has a root");

    // The retained tree is the Row + Text the source declared, pre-order intact.
    let nodes = preorder(&rt.store, Some(root));
    assert_eq!(nodes.len(), 2, "Row (flex) + Text (leaf) mounted");
    let text = nodes[1];

    // The `text: label` binding survived the round trip by durable identity: the state
    // key the loader registered is the one the compiler assigned `label`, and its edge
    // reaches the Text node with the dirty classes a text write invalidates. This is
    // the "change the state → the right node goes dirty" bubble, checked at the binding
    // table (the loader's output), with the compiler out of the picture.
    let state = rt
        .states
        .id_for_key(text_label_key())
        .expect("the bound state was registered by the loader");
    let bindings = rt.bindings.for_state(state);
    assert_eq!(bindings.len(), 1, "one edge for the bound Text");
    assert_eq!(bindings[0].node, text, "the edge reaches the Text node");
    assert!(
        bindings[0]
            .class
            .contains(DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT),
        "a bound text content invalidates measure/layout/paint"
    );

    // And the cell is a live, writable runtime cell — the app can drive it, exactly as
    // a mounted tree expects, with no re-parse.
    assert!(rt.states.set(state, StateValue::Int(3)));
    assert_eq!(rt.states.get(state), Some(StateValue::Int(3)));
}

/// The durable state key `label` lowers to, recomputed from the compiler independently
/// of the load path (so the load path itself never touches the compiler). The blob
/// carries this key by identity; here we derive the same key to look the cell up.
fn text_label_key() -> StateKey {
    let plan = viso_dsl::hotreload::plan("Row { Text { text: label; } }").expect("compiles");
    let symbol = plan.symbol_for_name("label").expect("label is a source");
    StateKey::from_parts(symbol.hi, symbol.lo)
}

#[test]
fn a_corrupt_asset_is_rejected_not_a_panic() {
    // Loading an untrusted asset must never panic on a malformed blob (section 30);
    // the bounded decoder returns an error a release app can degrade on.
    let mut blob = build_package("Row { Text {} }").expect("compiles");
    blob[0] ^= 0xff; // clobber the wire magic
    let mut rt = Runtime::new();
    let res = load_from_bytes(
        &blob,
        &mut rt.store,
        &mut rt.states,
        &mut rt.bindings,
        &mut rt.lists,
    );
    assert!(
        res.is_err(),
        "a corrupt asset is a decode error, not a panic"
    );
}

#[test]
fn the_packaged_tree_matches_the_live_commit_tree_structurally() {
    // Both the live commit (Slice O) and the AOT package (Slice P) descend from the same
    // frontend, so from one source they must build the same-shape tree. We can't compare
    // NodeIds across two runtimes, but pre-order arity and per-node child fan-out are the
    // structure — and those must agree.
    let source = "Column { Row { Text {} Text {} } Text {} }";

    // AOT path: package → load.
    let blob = build_package(source).expect("compiles");
    let mut aot = Runtime::new();
    let aot_root = load_from_bytes(
        &blob,
        &mut aot.store,
        &mut aot.states,
        &mut aot.bindings,
        &mut aot.lists,
    )
    .expect("loads")
    .expect("root");
    let aot_shape = child_counts(&aot.store, aot_root);

    // Live commit path: the same source, mounted through the Slice O commit.
    let commit_shape = live_commit_child_counts(source);

    assert_eq!(
        aot_shape, commit_shape,
        "the AOT package and the live commit build the same-shape tree from one source"
    );
    // Sanity on the shape itself: Column(2 children), Row(2), Text(0), Text(0), Text(0).
    assert_eq!(aot_shape, vec![2, 2, 0, 0, 0]);
}

/// The per-node immediate-child count in pre-order — the structure of a tree, comparable
/// across two independent runtimes that assign different `NodeId`s.
fn child_counts(store: &NodeStore, root: NodeId) -> Vec<usize> {
    let mut out = Vec::new();
    child_counts_walk(store, root, &mut out);
    out
}

fn child_counts_walk(store: &NodeStore, node: NodeId, out: &mut Vec<usize>) {
    let mut count = 0;
    let mut child = store.arena().links(node).and_then(|l| l.first_child);
    let first = child;
    while let Some(c) = child {
        count += 1;
        child = store.arena().links(c).and_then(|l| l.next_sibling);
    }
    out.push(count);
    // Descend in the same pre-order the count was taken, so indices line up with the
    // AOT node table.
    let mut child = first;
    while let Some(c) = child {
        child_counts_walk(store, c, out);
        child = store.arena().links(c).and_then(|l| l.next_sibling);
    }
}

/// Mount `source` through the Slice O live commit and return its pre-order child-count
/// shape — the reference the AOT tree is compared against.
fn live_commit_child_counts(source: &str) -> Vec<usize> {
    use viso_dsl::hotreload::{CandidatePlan, LiveAnchors, LiveRuntime, hot_reload};
    use viso_ui::EffectStore;

    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut effects = EffectStore::new();
    let mut lists = VirtualLists::new();
    let mut scratch: Vec<NodeId> = Vec::new();

    // An empty last-good baseline: the empty→candidate diff is all-inserts, so the
    // commit takes the first-build path and mounts the whole tree.
    let baseline = CandidatePlan {
        tree: viso_dsl::ir::ui_ir::UiTree { items: Vec::new() },
        bindings: Default::default(),
        sources: Vec::new(),
        source_names: Vec::new(),
    };

    let mut rt = LiveRuntime {
        store: &mut store,
        states: &mut states,
        bindings: &mut bindings,
        effects: &mut effects,
        lists: &mut lists,
        root: None,
        scratch: &mut scratch,
    };
    hot_reload(&mut rt, &baseline, source, &LiveAnchors::default()).expect("commits");
    let root = rt.root.expect("mounted root");
    child_counts(&store, root)
}
