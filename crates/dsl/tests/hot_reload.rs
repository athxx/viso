//! Hot reload transaction integration tests (architecture section 42; AGENTS 21.7,
//! 35, 66).
//!
//! These drive the full `plan → diff → migrate → commit` pipeline against a live
//! headless runtime — no window, no GPU, deterministic — exactly as `todo.md`'s
//! Slice O exit criteria require. Each test mounts a first live tree, then reloads
//! new source into it and asserts the transaction's effect on the retained node
//! tree, the reactive state cells, and the focus, scroll and edit state of
//! kept nodes.
//!
//! The three cases mirror the plan's testing section:
//!
//! 1. a valid property-only edit atomically patches in place — kept nodes keep their
//!    `NodeId` (instance reuse) and their state cells keep their value;
//! 2. an invalid edit is rejected before commit — the live tree and every state cell
//!    are field-for-field identical to the last-good build (the transaction never
//!    mutated anything);
//! 3. a structural edit migrates state by durable identity, carries a kept
//!    node's focus, scroll and edit buffer to its rebuilt node, and reports the
//!    focus and scroll that could not survive the rebuild.

use std::cell::RefCell;
use std::rc::Rc;

use viso_dsl::hotreload::{CandidatePlan, LiveRuntime, hot_reload};
use viso_ui::state::StateKey;
use viso_ui::virtual_list::VirtualLists;
use viso_ui::{
    BindingTable, EffectStore, NodeId, NodeStore, SemanticProjector, StateStore, StateValue,
    TextEdits,
};
use viso_view::ViewHost;

/// The mutable live runtime a headless reload commits into, owned by the test so it
/// outlives the borrows a `LiveRuntime` bundles.
struct Live {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    effects: EffectStore,
    lists: VirtualLists,
    text_edits: TextEdits,
    projectors: SemanticProjector,
    root: Option<NodeId>,
    scratch: Vec<NodeId>,
    nodes: Vec<(viso_dsl::ir::binding_ir::NodeKey, NodeId)>,
    view: Option<Rc<RefCell<ViewHost>>>,
}

impl Live {
    fn new() -> Self {
        Live {
            store: NodeStore::new(),
            states: StateStore::new(),
            bindings: BindingTable::new(),
            effects: EffectStore::new(),
            lists: VirtualLists::new(),
            text_edits: TextEdits::new(),
            projectors: SemanticProjector::new(),
            root: None,
            scratch: Vec::new(),
            nodes: Vec::new(),
            view: None,
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
            text_edits: &mut self.text_edits,
            projectors: &mut self.projectors,
            root: self.root,
            scratch: &mut self.scratch,
            nodes: &mut self.nodes,
            view: &mut self.view,
        }
    }
}

/// An empty last-good baseline: the state before any fragment is mounted. Reloading
/// a real fragment against it diffs empty→candidate as all-inserts (non-preserving),
/// so the commit takes the full build path and mounts the first live tree.
fn empty_baseline() -> CandidatePlan {
    CandidatePlan::default()
}

/// Mount `source` into a fresh runtime and return the runtime plus the candidate that
/// is now the last-good template. This is the first-build path: an empty baseline
/// reloaded into the fragment.
fn mount(source: &str) -> (Live, CandidatePlan) {
    let mut live = Live::new();
    let baseline = empty_baseline();
    let done = {
        let mut rt = live.runtime();
        let done =
            hot_reload(&mut rt, &baseline, source).expect("initial mount compiles and commits");
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
        hot_reload(&mut rt, &last_good, "Row { Text { text: ;;; } }")
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

/// Lay the live tree out on a fixed surface, settling any carried scroll.
fn lay_out(live: &mut Live) {
    let root = live.root.expect("mounted");
    let surface = viso_ui::Rect {
        x: 0.0,
        y: 0.0,
        w: 400.0,
        h: 300.0,
    };
    live.store.layout(root, surface, &mut Vec::new());
}

/// Reload `source` into `live` over `last_good`, returning the commit.
fn reload(
    live: &mut Live,
    last_good: &CandidatePlan,
    source: &str,
) -> viso_dsl::hotreload::HotReload {
    let mut rt = live.runtime();
    let done = hot_reload(&mut rt, last_good, source).expect("the edit commits");
    live.root = rt.root;
    lay_out(live);
    done
}

const SCROLLED: &str = "width: 100dp; height: 50dp;";
const TALL: &str = "width: 100dp; height: 400dp;";

#[test]
fn structural_edit_migrates_state_and_reports_focus_and_scroll() {
    // Re-typing the inner node rebuilds the tree: the kept scroll container
    // carries its offset, and the focus on the replaced node is lost.
    let (mut live, last_good) = mount(&format!(
        "Scroll {{ {SCROLLED} Text {{ {TALL} text: count; }} }}"
    ));
    lay_out(&mut live);

    let symbol = last_good
        .symbol_for_name("count")
        .expect("count is a source");
    let key = StateKey::from_parts(symbol.hi, symbol.lo);
    let count_id = live.states.id_for_key(key).expect("count cell exists");
    assert!(live.states.set(count_id, StateValue::Int(42)));

    let [scroll_node, text_node] = preorder(&live.store, live.root)[..] else {
        panic!("Scroll + Text mounted");
    };
    live.store.set_focused(Some(text_node));
    live.store
        .scroll_by(scroll_node, viso_ui::Vec2::new(0.0, 50.0));
    assert_eq!(
        live.store.scroll(scroll_node).y,
        50.0,
        "the container scrolls"
    );

    let done = reload(
        &mut live,
        &last_good,
        &format!("Scroll {{ {SCROLLED} Button {{ {TALL} text: count; }} }}"),
    );

    assert_eq!(
        state_of(&live, &done.candidate, "count"),
        Some(StateValue::Int(42)),
        "a same-identity source keeps its value through a structural rebuild"
    );
    let [scroll_node, button] = preorder(&live.store, live.root)[..] else {
        panic!("Scroll + Button mounted");
    };
    assert_ne!(button, text_node, "the replaced node is rebuilt");
    assert!(done.report.focus_lost, "focus on a replaced node is lost");
    assert_eq!(live.store.focused(), None);
    assert_eq!(
        done.report.scroll_lost, 0,
        "the kept container's offset carries"
    );
    assert_eq!(live.store.scroll(scroll_node).y, 50.0);
}

#[test]
fn focus_and_scroll_follow_their_nodes_past_an_inserted_sibling() {
    let (mut live, last_good) = mount(&format!(
        "Column {{ Scroll {{ {SCROLLED} Text {{ {TALL} text: count; }} }} }}"
    ));
    lay_out(&mut live);
    let [_, scroll_node, text_node] = preorder(&live.store, live.root)[..] else {
        panic!("Column + Scroll + Text mounted");
    };
    live.store.set_focused(Some(text_node));
    live.store
        .scroll_by(scroll_node, viso_ui::Vec2::new(0.0, 30.0));

    // A sibling inserted before the scroll container shifts every later key.
    let done = reload(
        &mut live,
        &last_good,
        &format!(
            "Column {{ Text {{ text: count; }} Scroll {{ {SCROLLED} Text {{ {TALL} text: count; }} }} }}"
        ),
    );
    let [_, _, scroll_node, text_node] = preorder(&live.store, live.root)[..] else {
        panic!("Column + Text + Scroll + Text mounted");
    };
    assert!(!done.report.focus_lost);
    assert_eq!(done.report.scroll_lost, 0);
    assert_eq!(
        live.store.focused(),
        Some(text_node),
        "focus follows its node"
    );
    assert_eq!(
        live.store.scroll(scroll_node).y,
        30.0,
        "scroll follows its node"
    );

    // Removing the scroll container loses its offset, and the focus inside it.
    let done = reload(
        &mut live,
        &done.candidate,
        "Column { Text { text: count; } }",
    );
    assert!(done.report.focus_lost);
    assert_eq!(done.report.scroll_lost, 1);
    assert_eq!(live.store.focused(), None);
}

#[test]
fn a_text_input_keeps_its_text_and_selection_across_a_rebuild() {
    let (mut live, last_good) = mount("Column { TextInput { width: 100dp; height: 20dp; } }");
    lay_out(&mut live);
    let [_, input] = preorder(&live.store, live.root)[..] else {
        panic!("Column + TextInput mounted");
    };
    let mut buffer = viso_ui::Buffer::with_text("hello");
    buffer.sel = viso_ui::Selection {
        anchor: viso_ui::TextPosition::upstream(viso_ui::TextOffset(1)),
        focus: viso_ui::TextPosition::upstream(viso_ui::TextOffset(4)),
    };
    live.text_edits.register(input, Box::new(buffer.clone()));

    let done = reload(
        &mut live,
        &last_good,
        "Column { Text { text: label; } TextInput { width: 100dp; height: 20dp; } }",
    );
    assert_eq!(done.report.scroll_lost, 0);
    let [_, _, rebuilt] = preorder(&live.store, live.root)[..] else {
        panic!("Column + Text + TextInput mounted");
    };
    assert_ne!(rebuilt, input, "the tree is rebuilt");
    let carried = live.text_edits.get(rebuilt).expect("the buffer carries");
    assert_eq!((&carried.text, carried.sel), (&buffer.text, buffer.sel));
    assert!(
        live.text_edits.get(input).is_none(),
        "the old node holds none"
    );
}

/// The children of `parent`, in order.
fn children(store: &NodeStore, parent: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut child = store.arena().links(parent).and_then(|l| l.first_child);
    while let Some(c) = child {
        out.push(c);
        child = store.arena().links(c).and_then(|l| l.next_sibling);
    }
    out
}

#[test]
fn a_rebuild_replaces_only_the_views_subtree_in_place() {
    // Content the view is mounted into, built and owned outside the view.
    let mut live = Live::new();
    let (outer, outer_root) = {
        let mut rt = live.runtime();
        let done = hot_reload(
            &mut rt,
            &empty_baseline(),
            "Column { Text { text: first; } Text { text: last; } }",
        )
        .expect("the outer content mounts");
        (done, rt.root.expect("the outer content has a root"))
    };
    live.nodes.clear();
    let [first, last] = children(&live.store, outer_root)[..] else {
        panic!("the outer column has two children");
    };
    let first_state = {
        let symbol = outer.candidate.symbol_for_name("first").unwrap();
        live.states
            .id_for_key(StateKey::from_parts(symbol.hi, symbol.lo))
            .unwrap()
    };

    // Mount the view between the outer column's children.
    let last_good = {
        let mut rt = live.runtime();
        let done = hot_reload(&mut rt, &empty_baseline(), "Row { Text { text: label; } }")
            .expect("the view mounts");
        live.root = rt.root;
        done.candidate
    };
    let view_root = live.root.unwrap();
    live.store
        .arena_insert_before(outer_root, view_root, Some(last));

    // A structural edit rebuilds the view.
    {
        let mut rt = live.runtime();
        hot_reload(&mut rt, &last_good, "Row { Button { text: label; } }")
            .expect("the structural edit commits");
        live.root = rt.root;
    }
    let rebuilt = live.root.unwrap();
    assert_ne!(rebuilt, view_root, "the view's root was rebuilt");
    assert!(
        !live.store.arena().is_live(view_root),
        "the old root is freed"
    );
    assert_eq!(
        children(&live.store, outer_root),
        vec![first, rebuilt, last],
        "the rebuilt view takes the old one's place among its siblings"
    );
    assert!(
        live.bindings
            .for_state(first_state)
            .iter()
            .any(|edge| edge.node == first),
        "a sibling's edges survive the view's rebind"
    );
    assert_eq!(
        live.nodes.len(),
        2,
        "the view's slots are its own two nodes"
    );
    assert_eq!(live.nodes[0].1, rebuilt);
}

/// Lay the live tree out in a 400×300 surface and return each node's width and
/// height in pre-order.
fn boxes(live: &mut Live) -> Vec<(f32, f32)> {
    let root = live.root.expect("a mounted root");
    let surface = viso_ui::Rect {
        x: 0.0,
        y: 0.0,
        w: 400.0,
        h: 300.0,
    };
    live.store.layout(root, surface, &mut Vec::new());
    preorder(&live.store, live.root)
        .into_iter()
        .map(|id| {
            let b = live.store.bounds(id);
            (b.w, b.h)
        })
        .collect()
}

#[test]
fn a_size_edit_patches_kept_nodes_in_place() {
    // A static size edit, a static-to-environment edit and an `em` reading the
    // column's font size each reach the reused instances.
    let (mut live, last_good) = mount(
        "Row { Column { font_size: 20dp; width: 100dp; Text { width: 10dp; height: 10dp; } } }",
    );
    let before = preorder(&live.store, live.root);
    assert_eq!(boxes(&mut live)[2], (10.0, 10.0));

    let edits = [
        (
            "Row { Column { font_size: 20dp; width: 100dp; Text { width: 30dp; height: 10dp; } } }",
            (30.0, 10.0),
        ),
        (
            "Row { Column { font_size: 20dp; width: 100dp; Text { width: 1.5em; height: 2sp; } } }",
            (30.0, 2.0),
        ),
        (
            "Row { Column { font_size: 10dp; width: 100dp; Text { width: 1.5em; height: 2sp; } } }",
            (15.0, 2.0),
        ),
        (
            "Row { Column { width: 100dp; Text { width: 12dp; height: 2sp; } } }",
            (12.0, 2.0),
        ),
    ];
    let mut last_good = last_good;
    for (source, expected) in edits {
        let done = {
            let mut rt = live.runtime();
            hot_reload(&mut rt, &last_good, source)
                .unwrap_or_else(|e| panic!("{source} reloads: {e:?}"))
        };
        assert_eq!(
            preorder(&live.store, live.root),
            before,
            "{source} reuses every node"
        );
        assert_eq!(boxes(&mut live)[2], expected, "{source}");
        last_good = done.candidate;
    }
    let text = before[2];
    assert_eq!(live.store.bound_lengths(text).map(|l| l.width), Some(None));
    assert_eq!(
        live.store.resolved_font_size(text),
        Some(14.0),
        "the font source is gone"
    );
}

#[test]
fn a_mounted_environment_length_follows_the_environment() {
    let (mut live, _) = mount("Row { Text { width: 8px + 2sp; height: 10dp; } }");
    assert_eq!(boxes(&mut live)[1], (10.0, 10.0));
    live.store.set_length_env(viso_ui::LengthEnv {
        scale_factor: 2.0,
        text_scale: 2.0,
        ..viso_ui::LengthEnv::default()
    });
    assert_eq!(boxes(&mut live)[1], (8.0, 10.0));
}
