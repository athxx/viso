//! The adaptive layouts across size classes: an adaptive split stands its
//! panes side by side or stacks them, and an adaptive navigation lays its
//! destinations out as a bar, a rail or a sidebar, each re-arranging the same
//! nodes as the window resizes, the focus kept or moved where it stays
//! reachable.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use viso_ui::{
    Axis, BindingTable, BoxStyle, BuildCx, EffectStore, Handle, Key, KeyEvent, KeyRouter,
    LeafStyle, Modifiers, NodeId, NodeStore, Rect, Role, SemanticProjector, Sizing, StateStore,
    TextEdits, VirtualLists, run_structure_hooks,
};
use viso_widgets::{NavigationLayout, SplitHandle, adaptive_navigation, adaptive_split};

const HEIGHT: f32 = 600.0;

/// A control mounted in a window, run frame by frame.
struct Window {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    effects: EffectStore,
    edits: TextEdits,
    root: NodeId,
    width: f32,
}

impl Window {
    fn mount(width: f32, build: impl FnOnce(&mut BuildCx<'_>)) -> Self {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        states.update_env(|env| {
            env.window.width = width;
            env.window.height = HEIGHT;
        });
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();
        let root = {
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut states,
                &mut bindings,
                &mut lists,
                &mut edits,
                &mut projectors,
            );
            build(&mut cx);
            cx.root().expect("a root")
        };
        let mut window = Window {
            store,
            states,
            bindings,
            effects: EffectStore::new(),
            edits,
            root,
            width,
        };
        window.frame();
        window
    }

    fn layout(&mut self) {
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: self.width,
            h: HEIGHT,
        };
        self.store.layout(self.root, surface, &mut Vec::new());
    }

    /// One frame: lay out, settle the environment against the layout, run
    /// what changed, and lay out what that changed.
    fn frame(&mut self) {
        self.layout();
        self.states.settle_env(&self.store);
        let mut changed = Vec::new();
        self.states.take_pending(&mut changed);
        self.store
            .flush_state_transactions(&changed, &self.bindings);
        run_structure_hooks(
            &mut self.store,
            &mut self.states,
            &mut self.bindings,
            &mut self.effects,
            &mut VirtualLists::new(),
            &changed,
        );
        self.layout();
        // Pages trading places run to where they rest.
        for _ in 0..200 {
            if !self.store.is_transitioning() {
                break;
            }
            self.store.tick_transitions(Duration::from_millis(50));
            self.layout();
        }
        assert!(!self.store.is_transitioning(), "the pages come to rest");
    }

    fn resize(&mut self, width: f32) {
        self.width = width;
        self.states.update_env(|env| env.window.width = width);
        self.frame();
    }

    /// A press of `key` at the focus, then a frame.
    fn key(&mut self, key: Key) {
        for pressed in [true, false] {
            let ev = KeyEvent {
                key,
                pressed,
                repeat: false,
                modifiers: Modifiers::default(),
            };
            KeyRouter::route_key(
                &mut self.store,
                &mut self.states,
                &self.bindings,
                &mut self.edits,
                self.root,
                ev,
                &mut Vec::new(),
            );
        }
        self.frame();
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.store.arena().children(node).collect()
    }

    fn role(&self, node: NodeId) -> Option<Role> {
        self.store.semantics(node).map(|s| s.role)
    }

    fn shows(&self, node: NodeId) -> bool {
        self.store.shows(node)
    }

    fn bounds(&self, node: NodeId) -> Rect {
        self.store.world(node)
    }
}

/// A focusable leaf filling its row, whose node lands in `out`.
fn focusable(cx: &mut BuildCx<'_>, out: &Cell<Option<NodeId>>) -> Handle {
    let leaf = cx.leaf(LeafStyle {
        size: Sizing::fixed(40.0, 40.0),
        style: BoxStyle::NONE,
    });
    cx.focusable(leaf, true);
    out.set(Some(leaf.id()));
    leaf
}

/// The nodes of a built split: the first list row, the second, the detail's
/// field, and the handle.
struct SplitNodes {
    rows: [Rc<Cell<Option<NodeId>>>; 2],
    field: Rc<Cell<Option<NodeId>>>,
    handle: Rc<Cell<Option<SplitHandle>>>,
}

impl SplitNodes {
    fn row(&self, i: usize) -> NodeId {
        self.rows[i].get().expect("built")
    }

    fn field(&self) -> NodeId {
        self.field.get().expect("built")
    }
}

/// A split whose list rows show the detail on Enter.
fn split(width: f32) -> (Window, SplitNodes) {
    let nodes = SplitNodes {
        rows: [Rc::default(), Rc::default()],
        field: Rc::default(),
        handle: Rc::default(),
    };
    let (rows, field, slot) = (
        nodes.rows.clone(),
        Rc::clone(&nodes.field),
        Rc::clone(&nodes.handle),
    );
    let window = Window::mount(width, move |cx| {
        let handle: SplitHandle = cx.handle();
        slot.set(Some(handle));
        adaptive_split()
            .handle(handle)
            .list(move |cx| {
                for row in &rows {
                    let leaf = focusable(cx, row);
                    cx.on_key(leaf, move |ev| {
                        if ev.key().is_some_and(|k| k.pressed && k.key == Key::Enter) {
                            handle.show_detail(ev);
                        }
                    });
                }
            })
            .detail(move |cx| {
                focusable(cx, &field);
            })
            .build(cx);
    });
    (window, nodes)
}

/// The pane holding `node`: the root's descendant two levels down.
fn pane_of(w: &Window, node: NodeId) -> NodeId {
    w.store.parent(node).expect("in a pane")
}

#[test]
fn a_split_stacks_below_expanded_and_keeps_the_focus_reachable() {
    let (mut w, n) = split(1000.0);
    let (list, detail) = (pane_of(&w, n.row(0)), pane_of(&w, n.field()));
    // Expanded: side by side, the list at its width, the detail beside it.
    assert!(w.shows(list) && w.shows(detail));
    assert_eq!(w.bounds(list).w, 320.0);
    assert!(w.bounds(detail).x > w.bounds(list).x + 320.0 - 1.0);
    assert_eq!(w.store.parent(list), w.store.parent(detail));

    // The second row shows the detail; side by side, nothing folds.
    w.store.set_focused(Some(n.row(1)));
    w.key(Key::Enter);
    assert!(w.shows(list) && w.shows(detail));
    assert_eq!(w.store.focused(), Some(n.row(1)));

    // Narrowed to compact, the panes stack with the detail in front, and the
    // focus moves out of the folded list into it.
    w.resize(400.0);
    assert!(!w.shows(list) && w.shows(detail));
    assert_eq!(w.bounds(detail).w, 400.0);
    assert_eq!(w.store.focused(), Some(n.field()));
    let stack = w.store.parent(detail).expect("stacked");
    assert_eq!(w.children(stack), [list, detail]);

    // Back returns to the list and the row that had the focus.
    w.key(Key::Escape);
    assert!(w.shows(list) && !w.shows(detail));
    assert_eq!(w.store.focused(), Some(n.row(1)));
    assert_eq!(w.bounds(list).w, 400.0);

    // Medium still stacks; widened to expanded, both stand again, the focus
    // where it was.
    w.resize(700.0);
    assert!(w.shows(list) && !w.shows(detail));
    w.resize(1000.0);
    assert!(w.shows(list) && w.shows(detail));
    assert_eq!(w.bounds(list).w, 320.0);
    assert_eq!(w.store.focused(), Some(n.row(1)));
    let row = w.store.parent(list).expect("side by side");
    assert_eq!(w.children(row).len(), 3, "list, divider, detail");
    assert!(n.handle.get().is_some());
}

#[test]
fn a_split_narrowed_with_the_list_in_front_keeps_the_list() {
    let (mut w, n) = split(1000.0);
    let (list, detail) = (pane_of(&w, n.row(0)), pane_of(&w, n.field()));
    w.store.set_focused(Some(n.field()));
    w.resize(400.0);
    assert!(w.shows(list) && !w.shows(detail));
    assert_eq!(
        w.store.focused(),
        Some(n.row(0)),
        "the focus leaves the folded detail for the list's first row"
    );
    // Back with the list in front is the app's, not the split's.
    w.key(Key::Escape);
    assert!(w.shows(list) && !w.shows(detail));
}

/// A navigation of three destinations, each page holding a focusable leaf.
fn navigation(width: f32) -> Window {
    Window::mount(width, |cx| {
        let page = |cx: &mut BuildCx<'_>| {
            focusable(cx, &Cell::new(None));
        };
        adaptive_navigation()
            .destination("Home", page)
            .destination("Search", page)
            .destination("Profile", page)
            .build(cx);
    })
}

/// The strip of destinations and the paged content, in the root's order.
fn parts(w: &Window) -> (NodeId, NodeId, Vec<NodeId>) {
    let children = w.children(w.root);
    let nav = *children
        .iter()
        .find(|&&c| w.role(c) == Some(Role::TabList))
        .expect("a tablist");
    let content = *children.iter().find(|&&c| c != nav).expect("content");
    (nav, content, w.children(nav))
}

fn layout_of(w: &Window) -> NavigationLayout {
    let (nav, content, _) = parts(w);
    let order = w.children(w.root);
    let (axis, ..) = w.store.flex_arrangement(nav).expect("a flex strip");
    match (order[0] == content, axis) {
        (true, Axis::Row) => NavigationLayout::Bar,
        (false, Axis::Column) if w.bounds(nav).w <= 80.0 => NavigationLayout::Rail,
        (false, Axis::Column) => NavigationLayout::Sidebar,
        other => panic!("no layout: {other:?}"),
    }
}

#[test]
fn a_navigation_is_a_bar_a_rail_and_a_sidebar_by_size_class() {
    let mut w = navigation(400.0);
    assert_eq!(layout_of(&w), NavigationLayout::Bar);
    let (nav, content, items) = parts(&w);
    assert!(w.bounds(nav).y >= w.bounds(content).y + w.bounds(content).h - 1.0);
    let widths: Vec<f32> = items.iter().map(|&i| w.bounds(i).w).collect();
    assert!(
        widths.windows(2).all(|p| (p[0] - p[1]).abs() < 0.5),
        "{widths:?}"
    );
    let pages = w.children(content);
    assert!(w.shows(pages[0]) && !w.shows(pages[1]));

    // Along the bar the arrows move the selection and the focus.
    w.store.set_focused(Some(items[0]));
    w.key(Key::Right);
    assert_eq!(w.store.focused(), Some(items[1]));
    assert!(!w.shows(pages[0]) && w.shows(pages[1]));

    // Medium: a rail before the content, the focus kept.
    w.resize(700.0);
    assert_eq!(layout_of(&w), NavigationLayout::Rail);
    let (nav, content, _) = parts(&w);
    assert_eq!(w.bounds(nav).w, 80.0);
    assert!(w.bounds(content).x >= 80.0);
    assert_eq!(w.store.focused(), Some(items[1]));
    assert_eq!(
        w.store.flex_arrangement(items[1]).map(|a| a.0),
        Some(Axis::Column),
        "icon above caption"
    );
    // Down the rail; across it the arrows do nothing.
    w.key(Key::Down);
    assert_eq!(w.store.focused(), Some(items[2]));
    w.key(Key::Right);
    assert_eq!(w.store.focused(), Some(items[2]));
    assert!(w.shows(pages[2]) && !w.shows(pages[1]));

    // Expanded: a sidebar, each icon beside its caption.
    w.resize(1000.0);
    assert_eq!(layout_of(&w), NavigationLayout::Sidebar);
    let (nav, ..) = parts(&w);
    assert_eq!(w.bounds(nav).w, 240.0);
    assert_eq!(
        w.store.flex_arrangement(items[2]).map(|a| a.0),
        Some(Axis::Row)
    );
    assert_eq!(w.store.focused(), Some(items[2]));
    let state = w.store.semantic_state(items[2]).expect("announced");
    assert_eq!(state.selected, Some(true));
    assert_eq!(state.position, Some((3, 3)));

    // Back to compact: the bar below the content again, the same nodes.
    w.resize(400.0);
    assert_eq!(layout_of(&w), NavigationLayout::Bar);
    assert_eq!(parts(&w).2, items);
    assert_eq!(w.store.focused(), Some(items[2]));
}
