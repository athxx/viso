//! A `view!` or `component!` hands out a typed handle for each named node it
//! builds once: `nodes.save_button` is a `Handle<kind::Button>`, the node the
//! view built for it.

use viso::ui::{
    BindingTable, BuildCx, Handle, NodeId, NodeStore, Role, SemanticProjector, StateStore,
    TextEdits, VirtualLists, kind,
};

viso::component! {
    Toolbar {
        view {
            Row {
                node back: Button { text: "Back"; }
                Text { text: "Inbox"; }
                node compose: Button { text: "Compose"; }
            }
        }
    }
}

/// What `build` returns, built into a fresh store, and the store.
fn mount<T>(build: impl FnOnce(&mut BuildCx<'_>) -> T) -> (NodeStore, T) {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();
    let built = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        build(&mut cx)
    };
    (store, built)
}

/// The role and label node `id` says.
fn said(store: &NodeStore, id: NodeId) -> (Role, Option<String>) {
    let semantics = store.semantics(id).expect("the node speaks");
    (semantics.role, semantics.label.clone())
}

#[test]
fn a_components_named_nodes_are_its_built_nodes() {
    let (store, (_, view)) = mount(Toolbar::build);
    let back: Handle<kind::Button> = view.nodes.back;
    let kids: Vec<NodeId> = store.arena().children(view.id()).collect();
    assert_eq!(kids.len(), 3);
    assert_eq!(back.id(), kids[0]);
    assert_eq!(view.nodes.compose.id(), kids[2]);
    assert_eq!(said(&store, back.id()), (Role::Button, Some("Back".into())));
    assert_eq!(back.any().id(), back.id(), "it forgets its kind");
}

#[test]
fn a_views_named_nodes_are_typed_and_those_it_mounts_later_are_left_out() {
    let (store, view) = mount(viso::view!("fixtures/named_nodes.vs"));
    let title: Handle<kind::Text> = view.nodes.title;
    let save: Handle<kind::Button> = view.nodes.save_button;
    assert_eq!(store.parent(title.id()), Some(view.id()));
    assert_eq!(said(&store, save.id()), (Role::Button, Some("Save".into())));
    // `spinner` mounts with its `if`, so it has no field: naming it would not
    // compile.
}
