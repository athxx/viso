//! Under the `hot-reload` feature a `view!` mount records the identities the
//! development session needs to reload it: the file and the hash of its
//! source, the module identity, the static shape, the root, every state cell
//! by its durable key, the behavior host and, for a view with regions, the
//! live node of each static node. It embeds no source text.
#![cfg(feature = "hot-reload")]

use viso::ui::{
    BindingTable, BuildCx, Handle, NodeId, NodeStore, SemanticProjector, StateStore, TextEdits,
    VirtualLists,
};
use viso_view::{MountRecord, take_mounts};

/// Builds `build` into a fresh store and returns the store, the states, the
/// root and the mounts recorded meanwhile.
fn mount(
    build: impl FnOnce(&mut BuildCx<'_>) -> Handle,
) -> (NodeStore, StateStore, NodeId, Vec<MountRecord>) {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();
    let root = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        build(&mut cx).id()
    };
    let mut mounts = Vec::new();
    take_mounts(&mut mounts);
    (store, states, root, mounts)
}

#[test]
fn a_view_records_its_mount() {
    let (_, states, root, mounts) = mount(viso::view!("fixtures/counter.vs"));
    let [record] = &mounts[..] else {
        panic!("one mount is recorded, found {}", mounts.len());
    };
    assert_eq!(record.root, root);
    assert!(record.file.ends_with("fixtures/counter.vs"));
    assert_eq!(
        record.source_hash,
        viso_view::dev::wire::source_hash(include_str!("fixtures/counter.vs"))
    );
    assert_eq!(record.statics, [2, 0, 0], "a column of two leaves");
    assert_eq!(record.package, "viso");
    assert_eq!(record.cells.len(), 2, "count and enabled");
    assert!(record.cells.iter().all(|&(_, id)| states.is_live(id)));
    assert!(
        record.host.is_some(),
        "the label's text runs on the behavior"
    );
    assert!(
        record.nodes.is_empty(),
        "a view without regions names no nodes"
    );
    let mut again = Vec::new();
    take_mounts(&mut again);
    assert!(again.is_empty(), "taking the mounts drains them");
}

#[test]
fn a_view_with_regions_records_its_static_nodes() {
    let (store, _, root, mounts) = mount(viso::view!("fixtures/regions.vs"));
    let [record] = &mounts[..] else {
        panic!("one mount is recorded, found {}", mounts.len());
    };
    assert!(
        record.host.is_some(),
        "the regions run on the view's behavior"
    );
    assert_eq!(record.nodes.len(), 9, "every node outside a region");
    assert_eq!(record.nodes[0], Some(root));
    assert_eq!(record.statics.len(), 9);
    assert!(
        record
            .nodes
            .iter()
            .all(|node| node.is_some_and(|node| store.arena().is_live(node)))
    );
}
