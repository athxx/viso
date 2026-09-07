//! The "state invalidation" benchmark category, specialized to the reactive
//! *accessibility-state* path section 8.1 adds: a control's live state (checked /
//! value / selection) reaching the derived semantics tree. Two per-frame costs
//! are measured, both incurred only when a bound cell actually changes:
//!
//! 1. `project_wake` — the flush-phase projection: one changed state fanning out
//!    to `FANOUT` bound nodes, each closure reading the cell and writing the
//!    node's `semantic_state` column (marking SEMANTICS). This is the new work a
//!    toggle/drag/selection triggers, run through the same `SemanticProjector::wake`
//!    seam the frame loop runs after `take_pending`.
//! 2. `derive_with_state` — `derive_semantics` over a tree whose `FANOUT` nodes
//!    each carry a live `SemanticState`, so the derive cost of reading the state
//!    column into the flat tree is visible (vs. an authored-only tree).
//!
//! A startup assertion pins the mechanism before timing: a projected wake writes
//! exactly `FANOUT` nodes' state columns, and the derived tree carries that state
//! — a regression that drops the projection (state stops reaching the tree) fails
//! the bench binary immediately rather than silently timing a no-op, mirroring
//! `reactive_binding.rs`'s strict-path guard.
//!
//! Run release (`cargo bench -p viso-ui`); criterion defaults to a release
//! profile. Debug timing is not a performance result.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{
    BindingTable, BuildCx, FlexStyle, LeafStyle, NodeId, NodeStore, Role, SemanticProjector,
    SemanticState, Semantics, StateId, StateStore, StateValue, TextEdits, VirtualLists,
};

/// The section 8.2 decision-gate baseline: a wide, labeled tree so the
/// full-rebuild cost center — a `String` label clone per labeled node in
/// `derive_semantics` — is real, not hidden in a 256-node flat fan-out. A single
/// leaf's label changes (marking SEMANTICS, which bubbles to the root), then the
/// *whole* tree is re-derived from the root, exactly as today's
/// `derive_semantics_dirty` does. This measures whether the full rebuild on a
/// one-node change is hot enough to justify per-subtree incremental derivation
/// before any incremental machinery is written (AGENTS section 7.3).
const CONTAINERS: usize = 100;

/// Labeled leaves per container. `CONTAINERS * LEAVES` labeled nodes carry a
/// non-trivial `String` each, so the per-node clone dominates the derive walk.
const LEAVES: usize = 60;

/// A labeled scene: one flex root over `CONTAINERS` flex containers, each holding
/// `LEAVES` labeled leaves — `CONTAINERS * LEAVES` labeled nodes, every label a
/// distinct non-trivial `String` so the clone cost in `derive_semantics` is
/// visible. Returns the store, the root, and one deep leaf whose label the bench
/// mutates.
fn labeled_scene() -> (NodeStore, NodeId, NodeId) {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();

    let mut probe = None;
    let root = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        let root = cx.flex(FlexStyle::default(), |cx| {
            for c in 0..CONTAINERS {
                cx.flex(FlexStyle::default(), |cx| {
                    for l in 0..LEAVES {
                        let leaf = cx.leaf(LeafStyle::default());
                        cx.semantics(
                            leaf,
                            Semantics::role(Role::Label)
                                .with_label(format!("item {c}-{l} accessible label")),
                        );
                        // Keep one deep leaf to mutate in the bench.
                        if c == CONTAINERS / 2 && l == LEAVES / 2 {
                            probe = Some(leaf.id());
                        }
                    }
                });
            }
        });
        root.id()
    };

    (store, root, probe.expect("a probe leaf was recorded"))
}

/// The startup guard for the baseline bench: the labeled scene has the expected
/// node count and the mutated probe leaf's label actually reaches the derived
/// tree — so a regression that drops the label (or the tree) fails loud rather
/// than timing a no-op walk. Mirrors `assert_projection_reaches_the_tree`.
fn assert_labeled_scene_derives() {
    let (mut store, root, probe) = labeled_scene();
    let tree = store.derive_semantics(root);
    // root + CONTAINERS containers + CONTAINERS*LEAVES leaves.
    let expected = 1 + CONTAINERS + CONTAINERS * LEAVES;
    assert_eq!(
        tree.len(),
        expected,
        "the labeled scene derives one node per live node"
    );

    store.clear_dirty();
    store.set_semantics(
        probe,
        Semantics::role(Role::Label).with_label("changed label"),
    );
    let tree = store
        .derive_semantics_dirty(root)
        .expect("a label change is a semantic change");
    assert_eq!(
        tree.get(probe).and_then(|n| n.label.as_deref()),
        Some("changed label"),
        "the changed label reaches the derived tree"
    );
}

/// One shared state fans out to this many bound option nodes — wide enough that
/// the per-node projection/derive walk dominates the per-state bookkeeping, so a
/// regression in either shows up in the timing rather than hiding in setup.
const FANOUT: usize = 256;

/// A harness holding everything a projection wake and a derive touch: the node
/// store (state columns + semantics), the projector registered by the build, the
/// state store the wake reads, the source cell, the scene root, and the changed
/// batch reused across iterations.
struct Harness {
    store: NodeStore,
    states: StateStore,
    projectors: SemanticProjector,
    source: StateId,
    root: NodeId,
    changed: Vec<StateId>,
}

/// Build a scene of `FANOUT` leaves under one flex root, each bound to a single
/// shared `Int` cell with a semantic-state projection (`checked = index == cell`)
/// — the radio-group shape, the widest fan-out of the four controls. The build
/// seeds each node's state column and registers each projection into the shared
/// `SemanticProjector`, exactly as a control's `build` does.
fn setup() -> Harness {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();

    let source = states.alloc(StateValue::Int(0));

    let root = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        let root = cx.flex(FlexStyle::default(), |cx| {
            for index in 0..FANOUT {
                let node = cx.leaf(LeafStyle::default());
                cx.bind_semantic_state(node, move |cx| {
                    let chosen =
                        matches!(cx.get(source), Some(StateValue::Int(i)) if i as usize == index);
                    SemanticState::checked(chosen)
                });
                cx.semantics(node, Semantics::role(Role::Radio));
            }
        });
        root.id()
    };

    Harness {
        store,
        states,
        projectors,
        source,
        root,
        changed: Vec::new(),
    }
}

/// One projection wake: flip the shared cell to a fresh index, drain the pending
/// batch, and run the projector — the flush-phase work a selection triggers.
/// Returns the number of nodes whose state column was rewritten.
fn project_wake(h: &mut Harness, next: i32) -> u32 {
    h.states.set(h.source, StateValue::Int(next));
    h.changed.clear();
    h.states.take_pending(&mut h.changed);
    h.projectors.wake(&h.changed, &h.states, &mut h.store)
}

/// The startup guard: a projected wake rewrites exactly `FANOUT` state columns
/// (every option's `checked` is re-derived against the new selection), and the
/// derived tree then carries that live state — the projection reaches the tree.
/// A regression that severs the projection makes one of these fail rather than
/// timing a silent no-op.
fn assert_projection_reaches_the_tree() {
    let mut h = setup();
    let woken = project_wake(&mut h, 1);
    assert_eq!(
        woken, FANOUT as u32,
        "a wake rewrites every bound option's semantic-state column"
    );

    let tree = h.store.derive_semantics(h.root);
    let checked_count = tree
        .nodes
        .iter()
        .filter(|n| n.state.and_then(|s| s.checked) == Some(true))
        .count();
    assert_eq!(
        checked_count, 1,
        "exactly one option is checked in the derived tree — the live selection \
         reached the semantics"
    );
}

fn bench_semantic_projection(c: &mut Criterion) {
    assert_projection_reaches_the_tree();

    // The flush-phase projection cost: one selection fanning out to FANOUT nodes.
    // Alternate the target index each iteration so every wake is a real change.
    c.bench_function("project_wake", |b| {
        let mut h = setup();
        let mut next = 0i32;
        b.iter(|| {
            next = (next + 1) % FANOUT as i32;
            black_box(project_wake(black_box(&mut h), next))
        });
    });

    // The derive cost over a tree carrying live state on every node.
    c.bench_function("derive_with_state", |b| {
        let mut h = setup();
        project_wake(&mut h, 1);
        b.iter(|| black_box(h.store.derive_semantics(black_box(h.root))));
    });
}

/// The section 8.2 decision-gate baseline: on a wide labeled tree, change a single
/// leaf's label (marking SEMANTICS, which bubbles to the root) and time the full
/// re-derive from the root — today's `derive_semantics_dirty` behavior. This is the
/// cost per-subtree incremental derivation would try to beat; the number decides
/// whether that machinery is worth writing (Gate A: not hot / no live consumer →
/// record and stop; Gate B: hot and consumed → build it).
fn bench_derive_full_single_label_change(c: &mut Criterion) {
    assert_labeled_scene_derives();

    c.bench_function("derive_full_single_label_change", |b| {
        let (mut store, root, probe) = labeled_scene();
        // Prime the derive path once so any first-touch growth is out of the timing.
        black_box(store.derive_semantics(root));
        let mut counter = 0u64;
        b.iter(|| {
            // A distinct label each iteration so every wake is a real change that
            // marks SEMANTICS and bubbles to the root.
            counter += 1;
            store.clear_dirty();
            store.set_semantics(
                probe,
                Semantics::role(Role::Label).with_label(format!("label {counter}")),
            );
            black_box(
                store
                    .derive_semantics_dirty(black_box(root))
                    .expect("a label change is a semantic change"),
            )
        });
    });
}

criterion_group!(
    benches,
    bench_semantic_projection,
    bench_derive_full_single_label_change
);
criterion_main!(benches);
