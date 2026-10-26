//! A file tree's directory rows draw a chevron that turns with their expansion:
//! an expand or collapse reconciles the surviving rows' chevrons in place.

use viso_ui::virtual_list;
use viso_ui::{
    BindingTable, BuildCx, CheckMark, CheckShape, EffectStore, Modifiers, NodeId, NodeStore,
    PointerButtons, PointerEvent, PointerPhase, Rect, SemanticProjector, StateStore, TextEdits,
    VirtualLists,
};
use viso_widgets::{FileTreeHandle, NodeKey, TreeNode, file_tree};

/// The marks drawn under `node`, in tree order.
fn marks(store: &NodeStore, node: NodeId, out: &mut Vec<CheckMark>) {
    out.extend(store.check_mark(node));
    for child in store.arena().children(node).collect::<Vec<_>>() {
        marks(store, child, out);
    }
}

#[test]
fn a_directory_row_s_chevron_turns_as_it_expands() {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();
    let mut effects = EffectStore::new();
    let slot: FileTreeHandle = states.handle();
    let a = NodeKey(1);
    let roots = vec![TreeNode::dir(
        NodeKey(0),
        "root",
        vec![
            TreeNode::dir(a, "a", vec![TreeNode::file(NodeKey(11), "a1")]),
            TreeNode::file(NodeKey(2), "b"),
        ],
    )];
    let root = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        file_tree(roots)
            .open(NodeKey(0))
            .handle(slot)
            .build(&mut cx);
        cx.root().expect("a root")
    };
    let handle = slot;
    let surface = Rect::new(0.0, 0.0, 300.0, 400.0);
    let frame = |store: &mut NodeStore,
                 lists: &mut VirtualLists,
                 states: &mut StateStore,
                 bindings: &mut BindingTable,
                 effects: &mut EffectStore| {
        handle.reconcile(states, store, lists);
        store.layout(root, surface, &mut Vec::new());
        virtual_list::reconcile(store, lists, states, bindings, effects);
        store.layout(root, surface, &mut Vec::new());
    };
    frame(
        &mut store,
        &mut lists,
        &mut states,
        &mut bindings,
        &mut effects,
    );
    let chevron = |checked| CheckMark {
        shape: CheckShape::Chevron,
        checked,
    };
    let mut drawn = Vec::new();
    marks(&store, root, &mut drawn);
    assert_eq!(drawn, [chevron(true), chevron(false)], "root open, a shut");

    let still = PointerEvent {
        x: 0.0,
        y: 0.0,
        phase: PointerPhase::Move,
        buttons: PointerButtons::NONE,
        modifiers: Modifiers::default(),
    };
    {
        let mut cx = viso_ui::EventCx::__new_pointer(&mut states, &bindings, &still);
        let handle = slot;
        handle.toggle(&mut cx, a);
    }
    frame(
        &mut store,
        &mut lists,
        &mut states,
        &mut bindings,
        &mut effects,
    );
    drawn.clear();
    marks(&store, root, &mut drawn);
    assert_eq!(drawn, [chevron(true), chevron(true)], "a turns down");
    assert!(store.is_transitioning(), "and eases over");
}
