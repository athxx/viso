//! A `VirtualList`'s item template: the keyed `for` in its body mounts only
//! the rows its viewport and overscan show, follows its items as they change,
//! and its `NodeRef` scrolls to a row by index or key, mounted or not; a body
//! that is not one keyed `for` is `E3707`. A list with `on refresh`
//! refreshes while its `refreshing` says so. A `Scroll` with `across`
//! scrolls both ways.

use std::collections::HashMap;

use viso_dsl::aot::build_view_package;
use viso_dsl::frontend::Origin;
use viso_ui::virtual_list::{VirtualLists, absorb_measurements, reconcile};
use viso_ui::{
    BindingTable, EffectStore, KeyRouter, NodeId, NodeStore, PointerButtons, PointerEvent,
    PointerPhase, PointerRouter, Rect, StateStore, Vec2, deliver_notices, run_structure_hooks,
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
                    Text { width: 200dp; height: 20dp; text: \"{i}\"; }
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

    /// The canvas of list `list`, past its scroll bar.
    fn canvas(&self, list: NodeId) -> NodeId {
        self.store
            .arena()
            .children(list)
            .find(|&c| !self.store.is_chrome(c))
            .expect("a canvas")
    }

    /// The texts of the rows mounted on the list `list`, top to bottom.
    fn rows(&self, list: NodeId) -> Vec<String> {
        let canvas = self.canvas(list);
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

    /// `n` frames 16 ms apart, the scroll motion advancing in each.
    fn glide(&mut self, n: usize) {
        for _ in 0..n {
            self.store
                .tick_scroll_motion(std::time::Duration::from_millis(16));
            self.frame();
        }
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

const GLIDING: &str = "component Feed {
    state count = 1000;
    view {
        Column {
            width: 200dp;
            height: 300dp;
            Button { width: 100dp; height: 20dp; text: \"middle\"; on click { list.scroll_to_index(500, smooth: true); } }
            Button { width: 100dp; height: 20dp; text: \"more\"; on click { count += 1000; } }
            node list: VirtualList {
                width: 200dp;
                height: 200dp;
                estimated_extent: 20dp;
                tail: true;
                scroll_bars: false;
                for i in 0..count key i {
                    Text { width: 200dp; height: 20dp; text: \"{i}\"; }
                }
            }
        }
    }
}";

#[test]
fn a_list_glides_to_a_row_and_a_tailing_one_follows_its_end() {
    let mut page = Page::load(GLIDING);
    let list = page.child(page.root, 2);
    assert_eq!(page.store.scroll_thumb(list), None, "no scroll bar");
    let end = |n: f32| n * 20.0 - 200.0;
    assert_eq!(
        page.store.scroll(list).y,
        end(1000.0),
        "it opens at the end"
    );

    let more = page.child(page.root, 1);
    page.click(more);
    page.glide(60);
    assert_eq!(
        page.store.scroll(list).y,
        end(2000.0),
        "it followed the end"
    );
    assert!(page.rows(list).iter().any(|row| row == "1999"));

    let middle = page.child(page.root, 0);
    page.click(middle);
    assert!(page.store.is_gliding(list));
    page.glide(90);
    assert_eq!(page.store.scroll(list).y, 500.0 * 20.0);
    assert!(page.rows(list).iter().any(|row| row == "500"));
    // Away from the end, more rows leave it be.
    page.click(more);
    page.glide(10);
    assert_eq!(page.store.scroll(list).y, 500.0 * 20.0);
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
    let canvas = page.canvas(list);
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
                    Text { width: 200dp; height: 20dp; text: \"{top}\"; }
                    VirtualList {
                        width: 200dp;
                        height: 100dp;
                        estimated_extent: 20dp;
                        on visible_range_changed(range) {
                            shown = \"{range.first}..{range.last} of {range.count}\";
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
                                    Text { width: 50dp; height: 40dp; text: \"{s}/{b}\"; }
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
        let canvas = page.canvas(shelves);
        let mut hosts: Vec<NodeId> = page.store.arena().children(canvas).collect();
        hosts.sort_by(|a, b| page.store.world(*a).y.total_cmp(&page.store.world(*b).y));
        page.child(hosts[at], 0)
    };
    let books = |page: &Page, list: NodeId| {
        let canvas = page.canvas(list);
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

const REFRESHING: &str = "component Feed {
    state loading = false;
    state asked = 0;
    view {
        Column {
            width: 200dp;
            height: 300dp;
            Button { width: 100dp; height: 20dp; text: \"done\"; on click { loading = false; } }
            VirtualList {
                width: 200dp;
                height: 120dp;
                estimated_extent: 20dp;
                refreshing: loading;
                on refresh { loading = true; asked += 1; }
                for i in 0..100 key i {
                    Text { width: 200dp; height: 20dp; text: \"{i}\"; }
                }
            }
            VirtualList {
                width: 200dp;
                height: 120dp;
                estimated_extent: 20dp;
                refreshing: false;
                on refresh { asked += 1; }
                for i in 0..100 key i {
                    Text { width: 200dp; height: 20dp; text: \"{i}\"; }
                }
            }
        }
    }
}";

impl Page {
    /// Tell the viewports a refresh was asked of, the flush, and a frame.
    fn refresh(&mut self) {
        let mut edits = viso_ui::TextEdits::new();
        KeyRouter::route_refreshes(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            &mut edits,
        );
        self.settle();
        self.frame();
    }
}

#[test]
fn a_list_refreshes_while_its_owner_says_so() {
    let mut page = Page::load(REFRESHING);
    let (done, owned, unowned) = (
        page.child(page.root, 0),
        page.child(page.root, 1),
        page.child(page.root, 2),
    );
    assert!(page.store.is_refreshable(owned) && page.store.is_refreshable(unowned));
    assert!(!page.store.focusable(owned), "a refresh takes no focus");

    assert!(page.store.begin_refresh(owned));
    page.refresh();
    assert!(page.store.is_refreshing(owned), "its handler said so");
    page.click(done);
    assert!(
        !page.store.is_refreshing(owned),
        "and then said it was done"
    );

    assert!(page.store.begin_refresh(unowned));
    page.refresh();
    assert!(!page.store.is_refreshing(unowned), "nobody said so");
}

const SECTIONS: &str = "component Feed {
    state grouped = true;
    view {
        Column {
            width: 200dp;
            height: 300dp;
            Button { width: 100dp; height: 20dp; text: \"flat\"; on click { grouped = false; } }
            VirtualList {
                width: 200dp;
                height: 100dp;
                estimated_extent: 20dp;
                overscan: 1;
                for i in 0..100 key i {
                    Text { width: 200dp; height: 20dp; sticky: grouped && i % 10 == 0; text: \"{i}\"; }
                }
            }
            Scroll {
                width: 200dp;
                height: 100dp;
                Column {
                    Column {
                        height: 200dp;
                        Text { width: 200dp; height: 20dp; sticky: true; text: \"a\"; }
                    }
                    Column {
                        height: 200dp;
                        Text { width: 200dp; height: 20dp; sticky: true; text: \"b\"; }
                    }
                }
            }
        }
    }
}";

impl Page {
    /// The host of the row of list `list` that shows `text`.
    fn row_showing(&self, list: NodeId, text: &str) -> Option<NodeId> {
        let canvas = self.canvas(list);
        self.store.arena().children(canvas).find(|&host| {
            self.store
                .arena()
                .children(host)
                .next()
                .and_then(|row| self.texts.get(&row))
                .is_some_and(|t| t == text)
        })
    }

    /// Scroll `viewport` to `y` and run a frame.
    fn scroll_to(&mut self, viewport: NodeId, y: f32) {
        self.store.set_scroll(viewport, Vec2 { x: 0.0, y });
        self.frame();
        self.frame();
    }
}

#[test]
fn a_rows_sticky_root_heads_its_section_and_a_sticky_node_its_parent() {
    let mut page = Page::load(SECTIONS);
    let (flat, list, scroll) = (
        page.child(page.root, 0),
        page.child(page.root, 1),
        page.child(page.root, 2),
    );
    let top = page.store.world(list).y;
    assert_eq!(
        page.lists.get(list).unwrap().headers(),
        (0..100).step_by(10).collect::<Vec<_>>()
    );
    // Far down section 0, its header stays at the top.
    page.scroll_to(list, 150.0);
    let header = page.row_showing(list, "0").expect("row 0 stays mounted");
    assert_eq!(page.store.world(header).y, top);
    // Section 1's header pushes it on as it comes.
    page.scroll_to(list, 190.0);
    assert_eq!(page.store.world(header).y, top - 10.0);
    let next = page.row_showing(list, "10").unwrap();
    assert_eq!(page.store.world(next).y, top + 10.0);
    // The row root itself stays put inside its host.
    let root = page.store.arena().children(next).next().unwrap();
    assert!(!page.store.is_sticky(root));

    // A sticky node holds within its parent.
    let column = page.child(scroll, 0);
    let a = page.child(page.child(column, 0), 0);
    let b = page.child(page.child(column, 1), 0);
    let seen = page.store.world(scroll).y;
    page.scroll_to(scroll, 50.0);
    assert_eq!(page.store.world(a).y, seen);
    page.scroll_to(scroll, 190.0);
    assert_eq!(
        page.store.world(a).y,
        seen - 10.0,
        "its section pushes it on"
    );
    assert_eq!(page.store.world(b).y, seen + 10.0);

    page.click(flat);
    assert!(page.lists.get(list).unwrap().headers().is_empty());
}

const PAGES: &str = "component Tour {
    state page: U32 = 0;
    view {
        Column {
            width: 200dp;
            height: 300dp;
            Text { width: 200dp; height: 20dp; text: \"{page}\"; }
            VirtualList {
                axis: Axis::row;
                width: 200dp;
                height: 100dp;
                pages: true;
                page_dots: true;
                on snap_changed(e) { page = e.value; }
                for i in 0..5 key i {
                    Text { text: \"{i}\"; }
                }
            }
            Scroll {
                axis: Axis::row;
                width: 200dp;
                height: 50dp;
                snap: Snap::start;
                snap_inset: 16dp;
                Row {
                    padding: 16dp;
                    gap: 8dp;
                    for i in 0..6 key i {
                        Text { width: 120dp; height: 20dp; text: \"{i}\"; }
                    }
                }
            }
        }
    }
}";

impl Page {
    /// Frames until the scroll motion stops, the snaps told after each.
    fn rest(&mut self) {
        let mut edits = viso_ui::TextEdits::new();
        for _ in 0..240 {
            if !self.store.is_scroll_moving() && !self.store.snaps_due() {
                return;
            }
            self.store
                .tick_scroll_motion(std::time::Duration::from_millis(16));
            self.frame();
            KeyRouter::route_snaps(
                &mut self.store,
                &mut self.states,
                &self.bindings,
                &mut edits,
            );
            self.settle();
            self.frame();
        }
    }
}

#[test]
fn a_list_of_pages_turns_a_page_a_flick_and_its_handler_hears_each() {
    let mut page = Page::load(PAGES);
    let (shown, pager, shelf) = (
        page.child(page.root, 0),
        page.child(page.root, 1),
        page.child(page.root, 2),
    );
    assert_eq!(
        page.store.scroll_snap(pager),
        Some(viso_ui::ScrollSnap::PAGES)
    );
    let host = page.child(page.canvas(pager), 0);
    assert_eq!(
        page.store.bounds(host).w,
        200.0,
        "a page is the viewport's width"
    );
    let dots = page
        .store
        .arena()
        .children(pager)
        .filter(|&c| page.store.is_chrome(c))
        .find_map(|layer| {
            let mut kids = page.store.arena().children(layer);
            kids.find(|&d| page.store.page_dots(d).is_some())
        })
        .expect("the dots over the pages");
    assert_eq!(page.store.page_dots(dots).map(|d| d.count), Some(5));

    page.store.fling(pager, 3000.0);
    page.rest();
    assert_eq!(page.store.scroll(pager).x, 200.0, "one page, however hard");
    assert_eq!(page.store.page_dots(dots).unwrap().current, 1);
    page.frame();
    assert_eq!(page.texts.get(&shown).map(String::as_str), Some("1"));

    let snap = page.store.scroll_snap(shelf).unwrap();
    assert_eq!((snap.align, snap.inset), (viso_ui::SnapAlign::Start, 16.0));
    page.store.set_scroll(shelf, Vec2 { x: 100.0, y: 0.0 });
    page.frame();
    page.store.fling(shelf, 0.0);
    page.rest();
    // Item 1 starts at 16 + 128: it rests 16 in.
    assert_eq!(page.store.scroll(shelf).x, 128.0);
}

const GRID: &str = "component Photos {
    view {
        VirtualList {
            columns: 3;
            gap: 10dp;
            column_gap: 5dp;
            for i in 0..30 key i {
                Text { height: 50dp; list.full_line: i == 3; text: \"{i}\"; }
            }
        }
    }
}";

#[test]
fn a_list_with_columns_lays_its_items_in_lanes_a_full_line_item_alone() {
    let mut page = Page::load(GRID);
    for _ in 0..3 {
        page.frame();
    }
    let list = page.root;
    assert_eq!(page.lists.get(list).unwrap().lanes(), 3);
    let canvas = page.canvas(list);
    let placed: Vec<(String, Rect)> = page
        .store
        .arena()
        .children(canvas)
        .map(|host| {
            let text = page.child(host, 0);
            (page.texts[&text].clone(), page.store.bounds(host))
        })
        .collect();
    let at = |name: &str| placed.iter().find(|(t, _)| t == name).unwrap().1;
    let origin = page.store.bounds(canvas);
    let local = |r: Rect| (r.x - origin.x, r.y - origin.y, r.w);
    // The root list fills the 400-wide page: lanes 130 wide, 5 apart.
    assert_eq!(local(at("0")), (0.0, 0.0, 130.0));
    assert_eq!(local(at("2")), (270.0, 0.0, 130.0));
    assert_eq!(local(at("3")), (0.0, 60.0, 400.0), "alone on its line");
    assert_eq!(local(at("4")), (0.0, 120.0, 130.0));
}

const ACROSS: &str = "component Sheet {
    view {
        Column {
            Scroll {
                width: 200dp;
                height: 200dp;
                across: true;
                scroll_bars: false;
                Column { width: 500dp; height: 600dp; }
            }
        }
    }
}";

#[test]
fn a_scroll_across_scrolls_both_ways() {
    let page = Page::load(ACROSS);
    let viewport = page.child(page.root, 0);
    assert!(page.store.scrolls_on(viewport, viso_ui::Axis::Row));
    assert_eq!(page.store.scroll_range(viewport, viso_ui::Axis::Row), 300.0);
    assert_eq!(
        page.store.scroll_range(viewport, viso_ui::Axis::Column),
        400.0
    );
}
