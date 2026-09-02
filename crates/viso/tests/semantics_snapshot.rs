//! The accessibility tree through the public facade: authored role/label folded
//! with live state (focus, bounds, handler-derived roles) into a flat, snapshot-
//! able `SemanticsTree`, re-derived only when a semantic invalidation is pending
//! — Slice G end to end.
//!
//! Like `keyboard_routing` and `style_token`, this drives the ui stores directly
//! (all reachable through `viso::ui`) rather than standing up a live scheduler
//! and a window: `derive_semantics` / `set_focused` / `set_semantics` are the
//! exact calls a facade frame makes. A focus move rides `focus_next`, which marks
//! PAINT + SEMANTICS just as it does inside a running app.

use std::cell::RefCell;
use std::rc::Rc;

use viso::render::Rect;
use viso::ui::{
    Axis, BuildCx, DirtyClass, EventCx, FlexStyle, LeafStyle, NodeId, NodeStore, Role, Semantics,
    Size, focus_next,
};

const SURFACE: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 100.0,
    h: 100.0,
};

/// A flex root (authored `Group`) holding an interactive leaf `a` (a pointer
/// handler + authored `Button`/"Add") and a plain leaf `b`, laid out on the
/// surface. Returns the store and the three ids in tree order.
fn demo_scene() -> (NodeStore, NodeId, NodeId, NodeId) {
    let mut store = NodeStore::new();
    let sink: Rc<RefCell<Vec<NodeId>>> = Rc::new(RefCell::new(Vec::new()));
    let root;
    {
        let capture = Rc::clone(&sink);
        let mut cx = BuildCx::new(&mut store);
        let h = cx.flex(
            FlexStyle {
                axis: Axis::Row,
                size: Size::fixed(100.0, 100.0),
                ..Default::default()
            },
            |cx| {
                let a = cx.leaf(LeafStyle {
                    size: Size::fixed(40.0, 40.0),
                    ..Default::default()
                });
                let a = cx.on_pointer(a, |_ev: &mut EventCx<'_>| {});
                let a = cx.semantics(a, Semantics::role(Role::Button).with_label("Add"));
                capture.borrow_mut().push(a.id());
                let b = cx.leaf(LeafStyle {
                    size: Size::fixed(40.0, 40.0),
                    ..Default::default()
                });
                capture.borrow_mut().push(b.id());
            },
        );
        root = cx.semantics(h, Semantics::role(Role::Group)).id();
    }
    let (a, b) = {
        let ids = sink.borrow();
        (ids[0], ids[1])
    };
    let mut scratch = Vec::new();
    store.layout(root, SURFACE, &mut scratch);
    (store, root, a, b)
}

#[test]
fn demo_scene_tree_shape_and_roles() {
    let (store, root, a, b) = demo_scene();
    let tree = store.derive_semantics(root);

    assert_eq!(tree.len(), 3, "root + two leaves");
    let r = tree.root().expect("non-empty tree");
    assert_eq!(r.role, Role::Group);
    assert_eq!(r.children, vec![1, 2], "the two leaves in tree order");

    let node_a = &tree.nodes[1];
    assert_eq!(node_a.id, a);
    assert_eq!(node_a.role, Role::Button, "authored role");
    assert_eq!(node_a.label.as_deref(), Some("Add"));
    assert!(node_a.bounds.w > 0.0 && node_a.bounds.h > 0.0, "laid out");

    let node_b = &tree.nodes[2];
    assert_eq!(node_b.id, b);
    assert_eq!(node_b.role, Role::Group, "a plain leaf is a Group");
    assert_eq!(node_b.label, None);
}

#[test]
fn focus_change_updates_only_that_nodes_semantics() {
    let (mut store, root, a, b) = demo_scene();
    store.set_focusable(a, true);
    store.set_focusable(b, true);

    // First Tab lands on `a`.
    assert_eq!(focus_next(&mut store, root, true), Some(a));
    let tree = store.derive_semantics(root);
    assert!(tree.get(a).unwrap().focused, "a holds focus");
    assert!(!tree.get(b).unwrap().focused, "b does not");

    // A focus move dirties SEMANTICS (and PAINT), so the incremental guard fires.
    store.clear_dirty();
    assert_eq!(focus_next(&mut store, root, true), Some(b));
    assert!(
        store
            .dirty(a)
            .contains(DirtyClass::PAINT | DirtyClass::SEMANTICS),
        "the focus move marked both PAINT and SEMANTICS on the old node"
    );
    assert!(
        store
            .dirty(b)
            .contains(DirtyClass::PAINT | DirtyClass::SEMANTICS),
        "and on the new node"
    );
    let tree = store
        .derive_semantics_dirty(root)
        .expect("a focus change is a semantic change");
    assert!(!tree.get(a).unwrap().focused, "focus left a");
    assert!(tree.get(b).unwrap().focused, "focus landed on b");
}

#[test]
fn label_change_re_derives_one_node() {
    let (mut store, root, a, b) = demo_scene();
    store.clear_dirty();

    store.set_semantics(a, Semantics::role(Role::Label).with_label("Count: 1"));
    let tree = store
        .derive_semantics_dirty(root)
        .expect("a label change is a semantic change");

    let node_a = tree.get(a).unwrap();
    assert_eq!(node_a.role, Role::Label);
    assert_eq!(node_a.label.as_deref(), Some("Count: 1"));

    let node_b = tree.get(b).unwrap();
    assert_eq!(node_b.role, Role::Group, "the sibling is untouched");
    assert_eq!(node_b.label, None);

    // No text node this slice: a label is SEMANTICS-only, never MEASURE/LAYOUT.
    let dirty_a = store.dirty(a);
    assert!(dirty_a.intersects(DirtyClass::SEMANTICS));
    assert!(
        !dirty_a.intersects(DirtyClass::MEASURE | DirtyClass::LAYOUT),
        "a label with no text node does not reflow"
    );
}

#[test]
fn clean_frame_derives_nothing() {
    let (mut store, root, _a, _b) = demo_scene();
    // Derive once, then settle: a frame with no semantic change does no work.
    let _ = store.derive_semantics(root);
    store.clear_dirty();

    assert!(
        store.derive_semantics_dirty(root).is_none(),
        "no SEMANTICS invalidation pending -> no re-derivation"
    );
}
