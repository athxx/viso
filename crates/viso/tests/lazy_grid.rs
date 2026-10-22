//! A `ui!` list's `columns` lay its rows in lanes, as a `.vs` file's do, and
//! the `LazyGrid` widget does from Rust.

use viso::ui::{
    BindingTable, BuildCx, Component, EffectStore, NodeStore, Rect, SemanticProjector, StateStore,
    TextEdits, VirtualLists, absorb_measurements, reconcile,
};

struct Page {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    lists: VirtualLists,
    effects: EffectStore,
}

impl Page {
    fn new() -> Self {
        Page {
            store: NodeStore::new(),
            states: StateStore::new(),
            bindings: BindingTable::new(),
            lists: VirtualLists::new(),
            effects: EffectStore::new(),
        }
    }

    fn build(&mut self, build: impl FnOnce(&mut BuildCx<'_>)) {
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();
        let mut cx = BuildCx::with_reactive(
            &mut self.store,
            &mut self.states,
            &mut self.bindings,
            &mut self.lists,
            &mut text_edits,
            &mut projectors,
        );
        build(&mut cx);
    }

    fn frames(&mut self, root: viso::ui::NodeId) {
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 200.0,
            h: 100.0,
        };
        self.store.layout(root, surface, &mut Vec::new());
        for _ in 0..3 {
            reconcile(
                &mut self.store,
                &mut self.lists,
                &mut self.states,
                &mut self.bindings,
                &mut self.effects,
            );
            self.store
                .relayout_dirty(root, surface, &mut Vec::new(), &mut Vec::new());
            absorb_measurements(&self.store, &mut self.lists);
            self.store.clear_dirty();
        }
    }

    /// The x and width of each mounted row, by row.
    fn lanes(&self, list: viso::ui::NodeId) -> Vec<(f32, f32)> {
        let canvas = self.lists.get(list).unwrap().canvas();
        let mut rows: Vec<Rect> = self
            .store
            .arena()
            .children(canvas)
            .map(|host| self.store.bounds(host))
            .collect();
        rows.sort_by(|a, b| a.y.total_cmp(&b.y).then(a.x.total_cmp(&b.x)));
        rows.iter().map(|r| (r.x, r.w)).collect()
    }
}

#[test]
fn a_fragment_s_list_lays_its_rows_in_columns() {
    let build = viso::ui! {
        VirtualList {
            columns: 2;
            column_gap: 10dp;
            for i in 0..6 key i {
                Text { height: 20dp; text: "cell"; }
            }
        }
    };
    let mut page = Page::new();
    let mut root = None;
    page.build(|cx| root = Some(build(cx).id()));
    let root = root.unwrap();
    page.frames(root);
    assert_eq!(page.lists.get(root).unwrap().lanes(), 2);
    assert_eq!(
        &page.lanes(root)[..4],
        [(0.0, 95.0), (105.0, 95.0), (0.0, 95.0), (105.0, 95.0)]
    );
}

#[test]
fn a_lazy_grid_counts_its_lanes_by_its_width() {
    use viso::ui::{Columns, LeafStyle, Length, Size};
    use viso::widgets::{LazyGridStyle, lazy_grid};
    let grid = lazy_grid(LazyGridStyle {
        columns: Columns::Adaptive { min: 60.0 },
        cross_gap: 10.0,
        ..Default::default()
    })
    .items(100, |_, cx| {
        cx.leaf(LeafStyle {
            size: Size {
                width: Length::fill(),
                height: Length::Fixed(40.0),
            },
            ..Default::default()
        });
    });
    let mut page = Page::new();
    let mut root = None;
    page.build(|cx| {
        grid.build(cx);
        root = cx.root();
    });
    let root = root.unwrap();
    page.frames(root);
    // floor((200 + 10) / 70) = 3 lanes, 60 wide.
    assert_eq!(page.lists.get(root).unwrap().lanes(), 3);
    assert_eq!(
        &page.lanes(root)[..3],
        [(0.0, 60.0), (70.0, 60.0), (140.0, 60.0)]
    );
}
