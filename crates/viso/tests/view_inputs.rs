//! `view!` passes the root component's inputs from Rust: each given value
//! is the input's, and an input it is not given holds its default.

use viso::ui::{
    BindingTable, BuildCx, NodeId, NodeStore, SemanticProjector, StateStore, TextEdits,
    VirtualLists,
};

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

fn label(store: &NodeStore, id: NodeId) -> Option<String> {
    store.semantics(id).and_then(|s| s.label.clone())
}

#[test]
fn an_input_not_given_holds_its_default() {
    let (store, view) = mount(viso::view!("fixtures/greeting.vs"));
    assert_eq!(
        label(&store, view.nodes.line.id()).as_deref(),
        Some("1 x hello, world")
    );
}

#[test]
fn given_inputs_are_the_rust_values() {
    let name = String::from("Ada");
    let (store, view) = mount(viso::view!("fixtures/greeting.vs", name: name, count: 3));
    assert_eq!(
        label(&store, view.nodes.line.id()).as_deref(),
        Some("3 x hello, Ada")
    );
}
