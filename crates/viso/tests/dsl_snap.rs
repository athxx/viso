//! A `ui!` viewport's `snap`, `snap_inset` and `page_dots` build the
//! snapping viewport and its dots, as a `.vs` file's do.

use viso::ui::{
    BindingTable, BuildCx, NodeStore, ScrollSnap, SemanticProjector, SnapAlign, StateStore,
    TextEdits, VirtualLists,
};

#[test]
fn a_fragment_s_viewport_snaps_and_shows_its_dots() {
    let build = viso::ui! {
        Scroll {
            axis: Axis::row;
            width: 200dp;
            height: 50dp;
            snap: Snap::center;
            snap_inset: 8dp;
            page_dots: true;
            Row {
                Text("a") { width: 120dp }
                Text("b") { width: 120dp }
            }
        }
    };
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
    assert_eq!(
        store.scroll_snap(root),
        Some(ScrollSnap {
            align: SnapAlign::Center,
            inset: 8.0,
            ..ScrollSnap::START
        })
    );
    let dots = store
        .arena()
        .children(root)
        .filter(|&c| store.is_chrome(c))
        .find_map(|layer| {
            let mut kids = store.arena().children(layer);
            kids.find(|&d| store.page_dots(d).is_some())
        });
    assert!(dots.is_some(), "the dots over its end edge");
}
