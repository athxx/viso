//! A `VirtualList`'s item template: the keyed `for` in its body mounts only
//! the rows its viewport and overscan show, follows its items as they change,
//! and its `NodeRef` scrolls to a row by index or key, mounted or not; a body
//! that is not one keyed `for` is `E3707`.

use std::collections::HashMap;

use viso_dsl::aot::build_view_package;
use viso_dsl::frontend::Origin;
use viso_ui::virtual_list::{VirtualLists, absorb_measurements, reconcile};
use viso_ui::{
    BindingTable, EffectStore, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase,
    PointerRouter, Rect, StateStore, Vec2, deliver_notices, run_structure_hooks,
};
use viso_view::load_view;

fn origin() -> Origin {
    Origin {
        package: "app".into(),
        module: vec!["feed".into()],
        language: None,
        ..Default::default()
    }
}

const FEED: &str = "component Feed {
    state count = 1000;
    view {
        Column {
            width: 200dp;
            height: 300dp;
            Button { width: 100dp; height: 20dp; text: \"middle\"; on click { list.scroll_to_index(500); } }
            Button { width: 100dp; height: 20dp; text: \"more\"; on click { count += 1000; } }
            Button { width: 100dp; height: 20dp; text: \"last\"; on click { list.scroll_to_key(count - 1); } }
            node list: VirtualList {
                width: 200dp;
                height: 200dp;
                estimated_extent: 20dp;
                overscan: 2;
                for i in 0..count key i {
                    Text { width: 200dp; height: 20dp; text: format(\"{}\", i); }
                }
            }
        }
    }
}";

struct Page {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    effects: EffectStore,
    lists: VirtualLists,
    root: NodeId,
    /// The text each node shows, as declared.
    texts: HashMap<NodeId, String>,
}

impl Page {
    fn load(source: &str) -> Self {
        let blob = build_view_package(source, &origin()).expect("packages");
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let view =
            load_view(&blob, &mut store, &mut states, &mut bindings, &mut lists).expect("loads");
        let mut page = Self {
            store,
            states,
            bindings,
            effects: EffectStore::default(),
            lists,
            root: view.root.expect("a root"),
            texts: HashMap::new(),
        };
        page.frame();
        page.frame();
        page
    }

    /// One frame's list reconcile, layout and measurement.
    fn frame(&mut self) {
        reconcile(
            &mut self.store,
            &mut self.lists,
            &mut self.states,
            &mut self.bindings,
            &mut self.effects,
        );
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 300.0,
        };
        self.store.layout(self.root, surface, &mut Vec::new());
        absorb_measurements(&self.store, &mut self.lists);
        deliver_notices(&mut self.store, &self.lists, &mut self.states);
        self.settle();
        let mut requests = Vec::new();
        self.store.take_text_requests(&mut requests);
        for (id, request) in requests {
            self.texts.insert(id, request.text);
        }
    }

    /// A click in the middle of `node`, the flush with its structure hooks,
    /// and a frame.
    fn click(&mut self, node: NodeId) {
        let rect = self.store.world(node);
        let mut chain = Vec::new();
        for phase in [PointerPhase::Down, PointerPhase::Up] {
            let event = PointerEvent {
                x: rect.x + rect.w * 0.5,
                y: rect.y + rect.h * 0.5,
                phase,
                buttons: PointerButtons::PRIMARY,
                modifiers: Default::default(),
            };
            PointerRouter::route(
                &mut self.store,
                &mut self.states,
                &self.bindings,
                self.root,
                event,
                &mut chain,
            );
        }
        self.settle();
        self.frame();
    }

    /// The flush with its structure hooks.
    fn settle(&mut self) {
        let mut changed = Vec::new();
        self.states.take_pending(&mut changed);
        self.store
            .flush_state_transactions(&changed, &self.bindings);
        run_structure_hooks(
            &mut self.store,
            &mut self.states,
            &mut self.bindings,
            &mut self.effects,
            &mut self.lists,
            &changed,
        );
    }

    fn child(&self, parent: NodeId, at: usize) -> NodeId {
        self.store
            .arena()
            .children(parent)
            .nth(at)
            .expect("a child")
    }

    /// The texts of the rows mounted on the list `list`, top to bottom.
    fn rows(&self, list: NodeId) -> Vec<String> {
        let canvas = self.child(list, 0);
        let mut rows: Vec<(f32, String)> = self
            .store
            .arena()
            .children(canvas)
            .filter_map(|host| {
                let text = self.store.arena().children(host).next()?;
                Some((self.store.world(host).y, self.texts.get(&text)?.clone()))
            })
            .collect();
        rows.sort_by(|a, b| a.0.total_cmp(&b.0));
        rows.into_iter().map(|(_, text)| text).collect()
    }
}

#[test]
fn a_list_mounts_the_rows_it_shows_and_follows_its_items() {
    let mut page = Page::load(FEED);
    let list = page.child(page.root, 3);
    let state = page.lists.get(list).expect("a list");
    assert_eq!(state.item_count(), 1000);
    let shown = page.rows(list);
    assert!(
        shown.len() <= 14,
        "a 200dp viewport of 20dp rows and 2 rows of overscan mounts {} rows",
        shown.len()
    );
    assert_eq!(shown[..3], ["0", "1", "2"]);
    assert_eq!(page.store.scroll(list).y, 0.0);

    let more = page.child(page.root, 1);
    page.click(more);
    assert_eq!(page.lists.get(list).expect("a list").item_count(), 2000);
    assert_eq!(page.rows(list)[..3], ["0", "1", "2"], "the rows that stay");
}

#[test]
fn a_node_ref_scrolls_its_list_to_a_row_by_index_or_key() {
    let mut page = Page::load(FEED);
    let list = page.child(page.root, 3);
    let middle = page.child(page.root, 0);
    page.click(middle);
    assert_eq!(page.store.scroll(list).y, 500.0 * 20.0);
    let shown = page.rows(list);
    assert!(
        shown.iter().any(|row| row == "500") && shown.len() <= 16,
        "the window around row 500 mounts in the same frame: {shown:?}"
    );

    let more = page.child(page.root, 1);
    page.click(more);
    let last = page.child(page.root, 2);
    page.click(last);
    assert_eq!(
        page.store.scroll(list).y,
        2000.0 * 20.0 - 200.0,
        "the last row's start, as near as the end of the list allows"
    );
    assert!(page.rows(list).iter().any(|row| row == "1999"));
}

#[test]
fn a_list_body_is_one_keyed_for() {
    for body in [
        "Text { text: \"head\"; } for i in 0..3 key i { Text { text: \"row\"; } }",
        "for i in 0..3 { Text { text: \"row\"; } }",
        "",
    ] {
        let source = format!("component Feed {{ view {{ VirtualList {{ {body} }} }} }}");
        let errors = build_view_package(&source, &origin()).expect_err("rejected");
        assert!(
            errors.iter().any(|d| d.code == "E3707"),
            "{body}: {:?}",
            errors.iter().map(|d| &d.code).collect::<Vec<_>>()
        );
    }
}

#[test]
fn a_row_shows_its_item_and_its_handlers_see_it_as_the_items_move() {
    let mut page = Page::load(
        "component Picker {
            state names = [\"a\", \"b\", \"c\"];
            state picked = \"\";
            view {
                Column {
                    width: 200dp;
                    height: 300dp;
                    Button {
                        width: 100dp; height: 20dp; text: \"flip\";
                        on click { names = [\"c\", \"b\", \"a\", \"d\"]; }
                    }
                    Text { width: 100dp; height: 20dp; text: picked; }
                    VirtualList {
                        width: 200dp;
                        height: 200dp;
                        estimated_extent: 20dp;
                        for name in names key name {
                            Button {
                                width: 200dp; height: 20dp; text: name;
                                on click { picked = name; }
                            }
                        }
                    }
                }
            }
        }",
    );
    let list = page.child(page.root, 2);
    let picked = page.child(page.root, 1);
    assert_eq!(page.rows(list), ["a", "b", "c"]);
    page.click(page.child(page.root, 0));
    assert_eq!(page.rows(list), ["c", "b", "a", "d"]);
    let canvas = page.child(list, 0);
    let first = page
        .store
        .arena()
        .children(canvas)
        .min_by(|a, b| page.store.world(*a).y.total_cmp(&page.store.world(*b).y))
        .expect("a row");
    page.click(page.child(first, 0));
    assert_eq!(page.texts.get(&picked).map(String::as_str), Some("c"));
}

#[test]
fn a_list_reports_the_rows_it_shows_and_a_scroll_its_offset() {
    let mut page = Page::load(
        "component Feed {
            state shown = \"\";
            state top = 0dp;
            view {
                Column {
                    width: 200dp;
                    height: 300dp;
                    Text { width: 200dp; height: 20dp; text: shown; }
                    Text { width: 200dp; height: 20dp; text: format(\"{}\", top); }
                    VirtualList {
                        width: 200dp;
                        height: 100dp;
                        estimated_extent: 20dp;
                        on visible_range_changed(range) {
                            shown = format(\"{}..{} of {}\", range.first, range.last, range.count);
                        }
                        for i in 0..50 key i { Text { width: 200dp; height: 20dp; text: \"row\"; } }
                    }
                    Scroll {
                        width: 200dp;
                        height: 50dp;
                        on scroll_changed(event) { top = event.content.height; }
                        Text { width: 200dp; height: 400dp; text: \"tall\"; }
                    }
                }
            }
        }",
    );
    page.frame();
    let shown = page.child(page.root, 0);
    let top = page.child(page.root, 1);
    let list = page.child(page.root, 2);
    let scroll = page.child(page.root, 3);
    assert_eq!(
        page.texts.get(&shown).map(String::as_str),
        Some("0..4 of 50")
    );
    page.store.scroll_by(list, Vec2 { x: 0.0, y: 210.0 });
    page.frame();
    page.frame();
    assert_eq!(
        page.texts.get(&shown).map(String::as_str),
        Some("10..15 of 50")
    );
    assert_eq!(page.texts.get(&top).map(String::as_str), Some("0dp"));
    page.store.scroll_by(scroll, Vec2 { x: 0.0, y: 5.0 });
    page.frame();
    page.frame();
    assert_eq!(page.texts.get(&top).map(String::as_str), Some("400dp"));
}

#[test]
fn a_row_can_hold_a_list_of_its_own() {
    let mut page = Page::load(
        "component Shelves {
            view {
                Column {
                    width: 200dp;
                    height: 300dp;
                    node shelves: VirtualList {
                        width: 200dp;
                        height: 100dp;
                        estimated_extent: 40dp;
                        for s in 0..100 key s {
                            VirtualList {
                                width: 200dp;
                                height: 40dp;
                                axis: Axis::row;
                                estimated_extent: 50dp;
                                for b in 0..1000 key b {
                                    Text { width: 50dp; height: 40dp; text: format(\"{}/{}\", s, b); }
                                }
                            }
                        }
                    }
                }
            }
        }",
    );
    let shelves = page.child(page.root, 0);
    let shelf = |page: &Page, at: usize| {
        let canvas = page.child(shelves, 0);
        let mut hosts: Vec<NodeId> = page.store.arena().children(canvas).collect();
        hosts.sort_by(|a, b| page.store.world(*a).y.total_cmp(&page.store.world(*b).y));
        page.child(hosts[at], 0)
    };
    let books = |page: &Page, list: NodeId| {
        let canvas = page.child(list, 0);
        let mut rows: Vec<(f32, String)> = page
            .store
            .arena()
            .children(canvas)
            .filter_map(|host| {
                let text = page.store.arena().children(host).next()?;
                Some((page.store.world(host).x, page.texts.get(&text)?.clone()))
            })
            .collect();
        rows.sort_by(|a, b| a.0.total_cmp(&b.0));
        rows.into_iter().map(|(_, text)| text).collect::<Vec<_>>()
    };
    let first = shelf(&page, 0);
    assert_eq!(page.lists.get(first).expect("a list").item_count(), 1000);
    let shown = books(&page, first);
    assert_eq!(shown[..2], ["0/0", "0/1"], "the row's list mounts with it");
    assert!(shown.len() <= 12, "{shown:?}");

    page.store.scroll_by(shelves, Vec2 { x: 0.0, y: 2000.0 });
    page.frame();
    page.frame();
    let at = shelf(&page, 0);
    let top = books(&page, at);
    assert!(
        top.first()
            .is_some_and(|t| t.starts_with("4") && t.ends_with("/0")),
        "a recycled row shows its new shelf: {top:?}"
    );
    let lists = page.lists.len();
    assert!(lists <= 16, "the lists of rows gone are dropped: {lists}");
}
