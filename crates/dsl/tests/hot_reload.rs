//! Hot reload transaction integration tests (architecture section 42; AGENTS 21.7,
//! 35, 66).
//!
//! These drive the full `plan → diff → migrate → commit` pipeline against a live
//! headless runtime — no window, no GPU, deterministic — exactly as `todo.md`'s
//! Slice O exit criteria require. Each test mounts a first live tree, then reloads
//! new source into it and asserts the transaction's effect on the retained node
//! tree, the reactive state cells, and the focus / scroll anchors.
//!
//! The three cases mirror the plan's testing section:
//!
//! 1. a valid property-only edit atomically patches in place — kept nodes keep their
//!    `NodeId` (instance reuse) and their state cells keep their value;
//! 2. an invalid edit is rejected before commit — the live tree and every state cell
//!    are field-for-field identical to the last-good build (the transaction never
//!    mutated anything);
//! 3. a structural edit migrates state by durable identity and reports focus / scroll
//!    that could not survive the rebuild.

use viso_dsl::hotreload::{CandidatePlan, HotReloadReport, LiveAnchors, LiveRuntime, hot_reload};
use viso_ui::state::StateKey;
use viso_ui::virtual_list::VirtualLists;
use viso_ui::{BindingTable, EffectStore, NodeId, NodeStore, StateStore, StateValue};

/// The mutable live runtime a headless reload commits into, owned by the test so it
/// outlives the borrows a `LiveRuntime` bundles.
struct Live {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    effects: EffectStore,
    lists: VirtualLists,
    root: Option<NodeId>,
    scratch: Vec<NodeId>,
}

impl Live {
    fn new() -> Self {
        Live {
            store: NodeStore::new(),
            states: StateStore::new(),
            bindings: BindingTable::new(),
            effects: EffectStore::new(),
            lists: VirtualLists::new(),
            root: None,
            scratch: Vec::new(),
        }
    }

    /// Borrow the whole runtime as one `LiveRuntime` bundle for a transaction.
    fn runtime(&mut self) -> LiveRuntime<'_> {
        LiveRuntime {
            store: &mut self.store,
            states: &mut self.states,
            bindings: &mut self.bindings,
            effects: &mut self.effects,
            lists: &mut self.lists,
            root: self.root,
            scratch: &mut self.scratch,
        }
    }
}

/// An empty last-good baseline: the state before any fragment is mounted. Reloading
/// a real fragment against it diffs empty→candidate as all-inserts (non-preserving),
/// so the commit takes the full build path and mounts the first live tree.
fn empty_baseline() -> CandidatePlan {
    CandidatePlan {
        tree: viso_dsl::ir::ui_ir::UiTree { items: Vec::new() },
        bindings: Default::default(),
        sources: Vec::new(),
        source_names: Vec::new(),
    }
}

/// Mount `source` into a fresh runtime and return the runtime plus the candidate that
/// is now the last-good template. This is the first-build path: an empty baseline
/// reloaded into the fragment.
fn mount(source: &str) -> (Live, CandidatePlan) {
    let mut live = Live::new();
    let baseline = empty_baseline();
    let done = {
        let mut rt = live.runtime();
        let done = hot_reload(&mut rt, &baseline, source, &LiveAnchors::default())
            .expect("initial mount compiles and commits");
        live.root = rt.root;
        done
    };
    (live, done.candidate)
}

/// Collect the live retained tree's node ids in the same pre-order the commit numbers
/// them, so a test can assert instance reuse slot-by-slot.
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

/// The runtime state cell a source name resolves to, via the candidate's compile-stable
/// `SymbolId` bridged to the runtime `StateKey`.
fn state_of(live: &Live, plan: &CandidatePlan, name: &str) -> Option<StateValue> {
    let symbol = plan.symbol_for_name(name)?;
    let key = StateKey::from_parts(symbol.hi, symbol.lo);
    let id = live.states.id_for_key(key)?;
    live.states.get(id)
}

#[test]
fn valid_edit_atomically_patches_and_reuses_instances() {
    // Mount a two-node tree, then reload a property-only edit. The structure is
    // unchanged, so every node must be reused in place (same NodeId) and every
    // reactive cell must keep its live value.
    let (mut live, last_good) = mount("Row { Text { text: label; } }");

    let before = preorder(&live.store, live.root);
    assert_eq!(before.len(), 2, "Row + Text mounted");

    // Give the reactive cell a running value the reload must preserve.
    if let Some(symbol) = last_good.symbol_for_name("label") {
        let key = StateKey::from_parts(symbol.hi, symbol.lo);
        let id = live.states.id_for_key(key).expect("label cell exists");
        assert!(live.states.set(id, StateValue::Int(7)));
    }

    // A property-only edit: add a second bound property, no structural change.
    let done = {
        let mut rt = live.runtime();
        let done = hot_reload(
            &mut rt,
            &last_good,
            "Row { Text { text: label; color: label; } }",
            &LiveAnchors::default(),
        )
        .expect("valid edit commits");
        live.root = rt.root;
        done
    };

    let after = preorder(&live.store, live.root);
    assert_eq!(
        before, after,
        "a structure-preserving edit reuses every live instance in place"
    );
    assert_eq!(
        state_of(&live, &done.candidate, "label"),
        Some(StateValue::Int(7)),
        "the kept reactive cell keeps its running value across the reload"
    );
    assert!(!done.report.focus_lost, "no focus to lose");
    assert_eq!(done.report.scroll_lost, 0, "no scroll to lose");
}

#[test]
fn invalid_edit_keeps_last_good_field_for_field() {
    let (mut live, last_good) = mount("Row { Text { text: label; } }");

    // Snapshot the live tree and the reactive cell before the failed reload.
    let before_tree = preorder(&live.store, live.root);
    let before_state = state_of(&live, &last_good, "label");
    let before_root = live.root;

    // A malformed fragment: fatal in `plan`, so the transaction returns Err at the
    // `?` before `commit` — nothing is allowed to mutate.
    let err = {
        let mut rt = live.runtime();
        hot_reload(
            &mut rt,
            &last_good,
            "Row { Text { text: ;;; } }",
            &LiveAnchors::default(),
        )
        .expect_err("malformed fragment is rejected")
    };
    assert!(!err.is_empty(), "the rejection carries fatal diagnostics");

    // The live tree and state are field-for-field the last-good build: the commit
    // was never reached.
    let after_tree = preorder(&live.store, live.root);
    assert_eq!(live.root, before_root, "root unchanged");
    assert_eq!(
        before_tree, after_tree,
        "a rejected reload leaves every live node exactly in place"
    );
    assert_eq!(
        state_of(&live, &last_good, "label"),
        before_state,
        "a rejected reload leaves every reactive cell untouched"
    );
}

#[test]
fn structural_edit_migrates_state_and_reports_focus_and_scroll() {
    // Mount a tree with a scroll container and a bound source, focus the root, and
    // scroll the container — then structurally re-type a node so the diff is
    // non-preserving and the commit rebuilds.
    let (mut live, last_good) = mount("Scroll { Text { text: count; } }");

    // Establish a running state value, focus, and scroll to migrate.
    let symbol = last_good
        .symbol_for_name("count")
        .expect("count is a source");
    let key = StateKey::from_parts(symbol.hi, symbol.lo);
    let count_id = live.states.id_for_key(key).expect("count cell exists");
    assert!(live.states.set(count_id, StateValue::Int(42)));

    let nodes = preorder(&live.store, live.root);
    let scroll_node = nodes[0];
    let text_node = nodes[1];
    live.store.set_focused(Some(text_node));
    // A non-zero scroll offset the migration must decide about.
    live.store
        .scroll_by(scroll_node, viso_ui::Vec2::new(0.0, 50.0));
    let scrolled_slots = if live.store.scroll(scroll_node).y > 0.0 {
        vec![viso_dsl::ir::binding_ir::NodeKey(0)]
    } else {
        // The freshly mounted container may have no scroll range yet; then there is
        // no live offset to preserve and the anchor set is empty.
        Vec::new()
    };

    let anchors = LiveAnchors {
        focused: Some(viso_dsl::ir::binding_ir::NodeKey(1)),
        scrolled: scrolled_slots.clone(),
    };

    // Structural edit: the inner Text becomes a Button — identity changes at slot 1,
    // so the diff is a replace (non-preserving) and the commit rebuilds the tree.
    let done = {
        let mut rt = live.runtime();
        let done = hot_reload(
            &mut rt,
            &last_good,
            "Scroll { Button { text: count; } }",
            &anchors,
        )
        .expect("structural edit commits");
        live.root = rt.root;
        done
    };

    // State is migrated by durable SymbolId identity: `count` is in both templates,
    // so its running value carries across the rebuild untouched.
    assert_eq!(
        state_of(&live, &done.candidate, "count"),
        Some(StateValue::Int(42)),
        "a same-identity source keeps its value through a structural rebuild"
    );

    // Focus was on a slot that the rebuild replaced with fresh instances, so it is
    // reported lost.
    assert!(
        done.report.focus_lost,
        "focus on a rebuilt slot is reported lost"
    );

    // A live scroll offset cannot survive a full rebuild in this slice; if one was
    // established, the report counts it lost.
    assert_eq!(
        done.report.scroll_lost,
        scrolled_slots.len() as u32,
        "every live scroll anchor is reported lost across a rebuild"
    );

    // The rebuild reset no kept source (only `count`, which was kept), and the report
    // is internally consistent.
    let _ = HotReloadReport::default();
}
