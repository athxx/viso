//! The [`NavigationStack`] control — a stack of pages, of which only the top is
//! visible.
//!
//! A `NavigationStack` lays every page in an overlapping stack (each filling the
//! control's region) but shows only the top one. Pushing a page reveals it and
//! hides the one below; popping does the reverse and fires `on_navigate` with the
//! new stack depth. Unlike [`Tabs`](crate::Tabs), a navigation stack has no
//! internal button strip — it is driven by application logic: an app captures a
//! [`NavHandle`] (see [`NavigationStack::handle`]) and calls
//! [`NavHandle::push`]/[`NavHandle::pop`] from a page's own controls, and the
//! stack also honors a keyboard back gesture (Escape or Backspace while focused).
//! Pointer/programmatic navigation and the keyboard back gesture are the *same*
//! action semantically, so an interactive control must have a keyboard equivalent
//! (AGENTS section 15).
//!
//! All pages build once, at build time. Pushing and popping do not rebuild them —
//! `DirtyClass::STRUCTURE` bubbles but no pass rebuilds a subtree, so pages are
//! shown and hidden through the retained [`hidden`](viso_ui::NodeStore::hidden)
//! flag: a hidden page folds out of layout and paint without a structural change
//! (architecture section 8.4 / section 11). A handler cannot flip the flag
//! directly (an [`EventCx`] holds no node store), so the flip is a deferred
//! request the router applies after the handler returns — the same discipline as
//! a state edit. Navigating writes a reactive `depth` cell bound to the stack's
//! `PAINT`; the pages' visibility is the layout/paint effect of the deferred
//! `hidden` flips. Lazy pages (build only the visible page, build the rest on
//! first reveal) are a later slice; this slice builds all pages and hides the
//! ones below the top.
//!
//! ```
//! use std::cell::RefCell;
//! use std::rc::Rc;
//! use viso_widgets::{NavHandleSlot, navigation_stack};
//! use viso_ui::{SemanticProjector, BuildCx, BindingTable, Component, LeafStyle, NodeStore, StateStore, TextEdits, VirtualLists};
//! use viso_ui::{BoxStyle, Size};
//!
//! let handle: NavHandleSlot = Rc::new(RefCell::new(None));
//! let control = navigation_stack()
//!     .page(|cx| { cx.leaf(LeafStyle { size: Size::fill(), style: BoxStyle::NONE }); })
//!     .page(|cx| { cx.leaf(LeafStyle { size: Size::fill(), style: BoxStyle::NONE }); })
//!     .on_navigate(|_ev, depth| {
//!         // handle the newly-visible page depth
//!         let _ = depth;
//!     })
//!     .handle(&handle);
//!
//! // NavigationStack authors reactive state, so it builds through a reactive cx.
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut text_edits = TextEdits::new();
//! let mut projectors = SemanticProjector::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits, &mut projectors);
//! control.build(&mut cx);
//! // `handle` is now filled; the app can `handle.borrow().clone().unwrap().push(ev)` from a page.
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::{
    Align, Axis, BoxStyle, BuildCx, Component, DirtyClass, EventCx, FlexStyle, Inset, Key, Length,
    NodeId, Role, Semantics, Size, StateId, StateValue,
};

/// A shared, mutable navigation callback carrying the new stack depth (the index
/// of the now-visible top page). It is cloned into the stack's key handler and the
/// [`NavHandle`] at build time so a keyboard back gesture and a programmatic
/// push/pop drive the same `on_navigate`. Navigation is never concurrent (pointer,
/// keyboard, and programmatic pushes are serial within one input transaction), so
/// the runtime never re-enters the `RefCell` borrow.
type SharedNavigate = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, usize)>>>>;

/// A page's build-time content builder. It authors the page's subtree into the
/// stack; boxed so a `NavigationStack` can hold a heterogeneous list of closures.
type PageBuilder = Box<dyn Fn(&mut BuildCx<'_>)>;

/// The shared list of built page ids, filled once during build and read only at
/// event time (a [`NavHandle`] or the back-gesture handler resolves a page id from
/// it to flip `hidden`). Written once, read only during dispatch — never
/// concurrently — so the borrow is uncontended.
type PagesCell = Rc<RefCell<Vec<NodeId>>>;

/// A shared slot an application creates, passes to
/// [`NavigationStack::handle`], and reads after `build` to obtain the control's
/// [`NavHandle`]. The `depth` cell and page ids are minted inside `build`, so the
/// handle cannot be returned by the builder chain; the app supplies this slot up
/// front and `build` fills it once the ids exist — the same deferred-fill idiom as
/// the internal pages slot. Written once during build, read once after.
pub type NavHandleSlot = Rc<RefCell<Option<NavHandle>>>;

/// A handle an application captures to drive a built [`NavigationStack`]
/// programmatically. Cheap to clone (it holds only ids and shared cells). Call
/// [`NavHandle::push`] to reveal the next page and [`NavHandle::pop`] to return to
/// the previous one — both from within an [`EventCx`] (an event handler), since a
/// navigation defers `hidden` flips the router applies. A push past the last page
/// or a pop below the first is a clamped no-op.
#[derive(Clone)]
pub struct NavHandle {
    /// The reactive cell holding the current top-page index.
    depth: StateId,
    /// The built page ids, in stack order.
    pages: PagesCell,
    /// The shared `on_navigate` callback.
    on_navigate: SharedNavigate,
}

impl NavHandle {
    /// Push to the next page: reveal page `depth + 1`, hide the current top, and
    /// fire `on_navigate` with the new depth. A push past the last page is a
    /// clamped no-op. Call from within an event handler.
    pub fn push(&self, ev: &mut EventCx<'_>) {
        let pages = self.pages.borrow();
        if let Some(StateValue::Int(cur)) = ev.get(self.depth) {
            navigate(&self.on_navigate, ev, self.depth, &pages, cur as usize + 1);
        }
    }

    /// Pop to the previous page: reveal page `depth - 1`, hide the current top, and
    /// fire `on_navigate` with the new depth. A pop below the first page is a
    /// clamped no-op. Call from within an event handler.
    pub fn pop(&self, ev: &mut EventCx<'_>) {
        let pages = self.pages.borrow();
        if let Some(StateValue::Int(cur)) = ev.get(self.depth)
            && cur > 0
        {
            navigate(&self.on_navigate, ev, self.depth, &pages, cur as usize - 1);
        }
    }
}

/// The visual and layout parameters of a [`NavigationStack`]: currently just the
/// control's own size request within its parent.
///
/// `size` defaults to [`Length::Fill`] on both axes so the control fills its
/// parent region (each page grows to hold its content); override it with a
/// [`Length::Fixed`] or [`Length::Fit`] axis. All fields are `Copy`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NavigationStackStyle {
    /// The control's own size request within its parent. Defaults to `Fill` on
    /// both axes.
    pub size: Size,
}

impl Default for NavigationStackStyle {
    fn default() -> Self {
        NavigationStackStyle {
            size: Size {
                width: Length::Fill { weight: 1.0 },
                height: Length::Fill { weight: 1.0 },
            },
        }
    }
}

/// A stack of pages, of which only the top is visible.
///
/// Construct one with [`navigation_stack`], add pages with [`NavigationStack::page`],
/// give it behavior with [`NavigationStack::on_navigate`], and capture a
/// [`NavHandle`] with [`NavigationStack::handle`] to push/pop from application
/// code. A keyboard back gesture (Escape or Backspace while the stack is focused)
/// pops.
///
/// See the [module docs](self) for a build example. Invalidation: navigating
/// writes a reactive `depth` cell bound to the stack's `PAINT`, and defers a pair
/// of `hidden` flips (hide the old top, show the new) the router applies — each
/// marking its page `LAYOUT | PAINT`. No rebuild: all pages are built once.
pub struct NavigationStack {
    /// The pages, in stack order (index `0` is the root page).
    pages: Vec<PageBuilder>,
    style: NavigationStackStyle,
    /// The shared navigation callback (see [`SharedNavigate`]). `None` until
    /// [`NavigationStack::on_navigate`] is called; a control with no handler still
    /// navigates, just without notifying anyone.
    on_navigate: SharedNavigate,
    /// The app-supplied slot `build` fills with the control's [`NavHandle`], or an
    /// unshared throwaway slot when the app did not ask for one.
    handle_slot: NavHandleSlot,
}

/// Construct an empty [`NavigationStack`] with no pages and no handler yet. Chain
/// [`NavigationStack::page`] to add pages, [`NavigationStack::on_navigate`] to give
/// it behavior, [`NavigationStack::handle`] to capture a [`NavHandle`], and
/// [`NavigationStack::style`]/[`NavigationStack::size`] to adjust its size.
pub fn navigation_stack() -> NavigationStack {
    NavigationStack {
        pages: Vec::new(),
        style: NavigationStackStyle::default(),
        on_navigate: Rc::new(RefCell::new(None)),
        handle_slot: Rc::new(RefCell::new(None)),
    }
}

impl NavigationStack {
    /// Add a page with a builder that authors its content into the stack at build
    /// time. Pages are pushed in stack order; the first page added is the root
    /// (initially-visible) page.
    pub fn page(mut self, page: impl Fn(&mut BuildCx<'_>) + 'static) -> Self {
        self.pages.push(Box::new(page));
        self
    }

    /// Set the navigation callback, fired with the new top-page depth on a push, a
    /// pop, or a keyboard back gesture. Replaces any previously set handler.
    pub fn on_navigate(self, handler: impl FnMut(&mut EventCx<'_>, usize) + 'static) -> Self {
        *self.on_navigate.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Replace the whole [`NavigationStackStyle`].
    pub fn style(mut self, style: NavigationStackStyle) -> Self {
        self.style = style;
        self
    }

    /// Set the control's own size request within its parent (defaults to `Fill`).
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }

    /// Register an app-supplied [`NavHandleSlot`] for programmatic push/pop. During
    /// [`build`](Component::build) the control fills the slot with a [`NavHandle`]
    /// bound to the just-minted `depth` cell and page ids; the app reads the slot
    /// after building and clones the handle into a page's controls. Storing the
    /// destination this way (rather than returning it) lets `build` fill it once
    /// the ids exist.
    pub fn handle(mut self, slot: &NavHandleSlot) -> Self {
        self.handle_slot = slot.clone();
        self
    }
}

/// Drive the shared callback with the new top-page depth if one is set; a
/// handler-less control is a no-op. Navigation is serial within one input
/// transaction, so the borrow is uncontended.
fn fire(cb: &SharedNavigate, ev: &mut EventCx<'_>, depth: usize) {
    if let Some(f) = cb.borrow_mut().as_mut() {
        f(ev, depth);
    }
}

/// Navigate the stack to top-page index `next` — but only when it is a genuine,
/// in-range change. Navigating to the current top, or to an out-of-range index, is
/// a no-op: no cell write, no `hidden` flip, no callback. This also coalesces the
/// capture/bubble double-dispatch a router performs when a back gesture lands on a
/// leaf inside the focused stack (the stack is then an ancestor, so its handler
/// runs on both passes): the first pass writes the new depth, flips the pages, and
/// fires; the second reads the just-written value and short-circuits. `EventCx::set`
/// writes the cell eagerly (the flush is deferred, but the stored value updates
/// now), so the guard sees the first pass's write within the same route.
///
/// The two `set_hidden` requests are deferred (an `EventCx` holds no node store);
/// the router applies them after the handler returns, hiding the old top page and
/// showing the new. `pages[i]` is page `i`'s node.
fn navigate(
    cb: &SharedNavigate,
    ev: &mut EventCx<'_>,
    depth: StateId,
    pages: &[NodeId],
    next: usize,
) {
    if next >= pages.len() {
        return;
    }
    let prev = match ev.get(depth) {
        Some(StateValue::Int(i)) => i as usize,
        _ => return,
    };
    if prev == next {
        return;
    }
    ev.set(depth, StateValue::Int(next as i32));
    if let Some(old) = pages.get(prev) {
        ev.set_hidden(*old, true);
    }
    if let Some(new) = pages.get(next) {
        ev.set_hidden(*new, false);
    }
    fire(cb, ev, next);
}

impl Component for NavigationStack {
    fn build(&self, cx: &mut BuildCx<'_>) {
        // One shared `depth` cell holding the visible top-page index. The stack
        // binds this cell to its PAINT so a navigation repaints the stack in one
        // targeted invalidation, not a rebuild. Pages switch through deferred
        // `hidden` flips, not this cell. The root page (index 0) is initially on
        // top.
        let depth = cx.state(StateValue::Int(0));

        // Pages are shown/hidden through their node ids, resolved only at event
        // time (a back gesture or a `NavHandle` flips `hidden`), long after the
        // whole tree is built. The page-build phase below pushes each page's id
        // into this shared, deferred-fill slot, and the handler and the handle read
        // it when they run. Written once during build, read only during dispatch —
        // never concurrently — so the borrow is uncontended.
        let pages_cell: PagesCell = Rc::new(RefCell::new(Vec::with_capacity(self.pages.len())));

        // The stack: every page overlaps in a stretched column, each filling the
        // region; all but the top are hidden. (There is no dedicated stack
        // container this slice — an aligned, stretched column overlaps its children
        // when each fills the region, the same shape the tabs panel area uses.)
        let key_cb = self.on_navigate.clone();
        let key_pages = pages_cell.clone();
        let root = cx.flex(
            FlexStyle {
                axis: Axis::Column,
                gap: 0.0,
                padding: Inset::all(0.0),
                align: Align::Stretch,
                size: self.style.size,
                style: BoxStyle::NONE,
            },
            |cx| {
                let mut pages = pages_cell.borrow_mut();
                for (index, page) in self.pages.iter().enumerate() {
                    let node = cx.flex(
                        FlexStyle {
                            axis: Axis::Column,
                            gap: 0.0,
                            padding: Inset::all(0.0),
                            align: Align::Stretch,
                            size: Size {
                                width: Length::Fill { weight: 1.0 },
                                height: Length::Fill { weight: 1.0 },
                            },
                            style: BoxStyle::NONE,
                        },
                        |cx| {
                            (page)(cx);
                        },
                    );
                    // Show only the root page; hide the rest at build time so the
                    // initial frame lays out one page.
                    cx.set_hidden(node, index != 0);
                    cx.semantics(node, Semantics::role(Role::Group));
                    pages.push(node.id());
                }
            },
        );

        // The stack is a focusable navigation container: a keyboard back gesture
        // (Escape or Backspace while it is focused) pops. Auto-repeat is ignored so
        // a held key pops once. The pages are resolved lazily inside the handler
        // from the shared slot filled above.
        cx.focusable(root, true);
        cx.on_key(root, move |ev| {
            let Some(k) = ev.key() else { return };
            if !k.pressed || k.repeat {
                return;
            }
            if matches!(k.key, Key::Escape | Key::Backspace) {
                let pages = key_pages.borrow();
                if let Some(StateValue::Int(cur)) = ev.get(depth)
                    && cur > 0
                {
                    navigate(&key_cb, ev, depth, &pages, cur as usize - 1);
                }
            }
        });
        cx.bind(depth, root, DirtyClass::PAINT);
        cx.semantics(root, Semantics::role(Role::Navigation));

        // Fill the app-supplied handle slot (if any) with a handle bound to the
        // just-minted cell and page ids, so the app can push/pop programmatically.
        *self.handle_slot.borrow_mut() = Some(NavHandle {
            depth,
            pages: pages_cell,
            on_navigate: self.on_navigate.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use viso_ui::{
        BindingTable, KeyEvent, LeafStyle, Modifiers, NodeStore, PointerButtons, PointerEvent,
        PointerPhase, SemanticProjector, StateStore, TextEdits, VirtualLists,
    };

    /// The reactive stores a navigation-stack build writes into, kept together so a
    /// test can build the control and then drive its handlers (and any captured
    /// [`NavHandle`]) against the same state.
    struct Reactive {
        store: NodeStore,
        states: StateStore,
        bindings: BindingTable,
        lists: VirtualLists,
        text_edits: TextEdits,
        projectors: SemanticProjector,
    }

    impl Reactive {
        fn new() -> Self {
            Reactive {
                store: NodeStore::new(),
                states: StateStore::new(),
                bindings: BindingTable::new(),
                lists: VirtualLists::new(),
                text_edits: TextEdits::new(),
                projectors: SemanticProjector::new(),
            }
        }

        /// Build a control through a reactive cx (it authors state, so a plain
        /// `BuildCx::new` would panic) and return its root (the stack container).
        fn build(&mut self, control: NavigationStack) -> NodeId {
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
                &mut self.text_edits,
                &mut self.projectors,
            );
            control.build(&mut cx);
            cx.root().expect("navigation stack declares a root node")
        }

        /// Feed a key sample to a node's key handler, restoring it after, then apply
        /// any deferred `hidden` flips it recorded — the router discipline (take,
        /// drive, restore, apply) reproduced for the test.
        fn key(&mut self, node: NodeId, ev: KeyEvent) {
            let mut handler = self.store.take_key_handler(node).expect("key handler");
            let hidden = {
                let mut cx = EventCx::__new_key(&mut self.states, &self.bindings, &ev);
                handler(&mut cx);
                cx.__take_hidden_requests()
            };
            self.store.restore_key_handler(node, handler);
            for (id, h) in hidden {
                self.store.set_hidden(id, h);
            }
        }

        /// Drive a `NavHandle` action (push/pop) as a router would: run it inside a
        /// throwaway `EventCx`, take the deferred `hidden` flips, and apply them.
        fn drive(&mut self, act: impl FnOnce(&mut EventCx<'_>)) {
            let ev = read_pointer();
            let hidden = {
                let mut cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
                act(&mut cx);
                cx.__take_hidden_requests()
            };
            for (id, h) in hidden {
                self.store.set_hidden(id, h);
            }
        }

        /// The current value of the shared depth cell (via a throwaway read cx).
        fn depth(&mut self, cell: StateId) -> Option<i32> {
            let ev = read_pointer();
            let cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
            match cx.get(cell) {
                Some(StateValue::Int(i)) => Some(i),
                _ => None,
            }
        }
    }

    /// A neutral pointer sample for read-only / handle-driven cx construction.
    fn read_pointer() -> PointerEvent {
        PointerEvent {
            x: 0.0,
            y: 0.0,
            phase: PointerPhase::Move,
            buttons: PointerButtons::NONE,
            modifiers: Modifiers::default(),
        }
    }

    /// A key press/release sample.
    fn key_ev(key: Key, pressed: bool, repeat: bool) -> KeyEvent {
        KeyEvent {
            key,
            pressed,
            repeat,
            modifiers: Modifiers::default(),
        }
    }

    /// The shared depth cell authored by the build. NavigationStack authors exactly
    /// one state cell (the shared `depth` Int), so a fresh `StateStore` allocating
    /// one `Int` cell yields the very handle the build produced (there is no public
    /// constructor for a bare `StateId`, and the build does not return it).
    fn depth_cell() -> StateId {
        StateStore::new().alloc(StateValue::Int(0))
    }

    /// Collect a node's direct children by walking the arena sibling chain.
    fn children(store: &NodeStore, parent: NodeId) -> Vec<NodeId> {
        let arena = store.arena();
        let mut out = Vec::new();
        let mut child = arena.links(parent).and_then(|l| l.first_child);
        while let Some(c) = child {
            out.push(c);
            child = arena.links(c).and_then(|l| l.next_sibling);
        }
        out
    }

    /// A page whose content is a single fixed leaf, so each page composes a real
    /// subtree rather than an empty node.
    fn leaf_page(cx: &mut BuildCx<'_>) {
        cx.leaf(LeafStyle {
            size: Size::fixed(10.0, 10.0),
            style: BoxStyle::NONE,
        });
    }

    /// A three-page stack with an app-captured handle, for the common test shape.
    fn three_pages(slot: &NavHandleSlot) -> NavigationStack {
        navigation_stack()
            .page(leaf_page)
            .page(leaf_page)
            .page(leaf_page)
            .handle(slot)
    }

    /// All pages build once; only the root page (index 0) is shown initially, the
    /// rest folded out through `hidden`.
    #[test]
    fn all_pages_build_and_only_the_root_is_shown() {
        let slot: NavHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(three_pages(&slot));

        let pages = children(&rx.store, root);
        assert_eq!(pages.len(), 3, "every page builds once");
        assert!(!rx.store.hidden(pages[0]), "the root page is shown");
        assert!(rx.store.hidden(pages[1]), "the deeper pages are hidden");
        assert!(rx.store.hidden(pages[2]), "the deeper pages are hidden");
    }

    /// The stack root is a focusable `Navigation` with a key handler; each page is a
    /// `Group`.
    #[test]
    fn stack_root_is_focusable_navigation_over_group_pages() {
        let slot: NavHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(three_pages(&slot));

        assert_eq!(
            rx.store.semantics(root).expect("root has semantics").role,
            Role::Navigation,
            "the control is a Navigation container"
        );
        assert!(rx.store.focusable(root), "the stack root is focusable");
        assert!(
            rx.store.has_key_handler(root),
            "the stack root attaches a key handler for the back gesture"
        );

        for page in children(&rx.store, root) {
            assert_eq!(
                rx.store.semantics(page).expect("page semantics").role,
                Role::Group,
                "each page is a Group"
            );
        }
    }

    /// A `NavHandle::push` reveals the next page, hides the current top, moves the
    /// depth cell, and fires `on_navigate` once; a push past the last page is a
    /// clamped no-op.
    #[test]
    fn handle_push_reveals_next_page_and_clamps_at_the_top() {
        let count = Rc::new(Cell::new(0u32));
        let last = Rc::new(Cell::new(None::<usize>));
        let (c, l) = (count.clone(), last.clone());

        let slot: NavHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(
            navigation_stack()
                .page(leaf_page)
                .page(leaf_page)
                .handle(&slot)
                .on_navigate(move |_ev, depth| {
                    l.set(Some(depth));
                    c.set(c.get() + 1);
                }),
        );
        let cell = depth_cell();
        let pages = children(&rx.store, root);
        let nav = slot.borrow().clone().expect("build fills the handle slot");

        // Push from the root reveals page 1.
        let n = nav.clone();
        rx.drive(|ev| n.push(ev));
        assert_eq!(rx.depth(cell), Some(1), "push moves the depth cell to 1");
        assert_eq!(count.get(), 1, "push fires on_navigate once");
        assert_eq!(last.get(), Some(1), "on_navigate carries the new depth");
        assert!(rx.store.hidden(pages[0]), "the old top is now hidden");
        assert!(!rx.store.hidden(pages[1]), "the new top is now shown");

        // A push past the last page is a clamped no-op.
        let n = nav.clone();
        rx.drive(|ev| n.push(ev));
        assert_eq!(rx.depth(cell), Some(1), "push past the top is a no-op");
        assert_eq!(count.get(), 1, "and does not fire again");
    }

    /// A `NavHandle::pop` returns to the previous page; a pop below the root is a
    /// clamped no-op.
    #[test]
    fn handle_pop_returns_and_clamps_at_the_root() {
        let slot: NavHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(three_pages(&slot));
        let cell = depth_cell();
        let pages = children(&rx.store, root);
        let nav = slot.borrow().clone().expect("build fills the handle slot");

        // Push twice to the top of the three-page stack.
        let n = nav.clone();
        rx.drive(|ev| n.push(ev));
        let n = nav.clone();
        rx.drive(|ev| n.push(ev));
        assert_eq!(rx.depth(cell), Some(2), "two pushes reach the top page");
        assert!(!rx.store.hidden(pages[2]), "the top page is shown");

        // Pop returns to page 1.
        let n = nav.clone();
        rx.drive(|ev| n.pop(ev));
        assert_eq!(rx.depth(cell), Some(1), "pop returns to page 1");
        assert!(rx.store.hidden(pages[2]), "the popped page is hidden");
        assert!(!rx.store.hidden(pages[1]), "the revealed page is shown");

        // Pop again to the root, then a pop below the root is a clamped no-op.
        let n = nav.clone();
        rx.drive(|ev| n.pop(ev));
        assert_eq!(rx.depth(cell), Some(0), "pop returns to the root");
        let n = nav.clone();
        rx.drive(|ev| n.pop(ev));
        assert_eq!(rx.depth(cell), Some(0), "pop below the root is a no-op");
    }

    /// A keyboard back gesture (Escape or Backspace while focused) pops; auto-repeat
    /// and key-up do not, and a back gesture at the root is a clamped no-op.
    #[test]
    fn keyboard_back_gesture_pops() {
        let count = Rc::new(Cell::new(0u32));
        let c = count.clone();

        let slot: NavHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(three_pages(&slot).on_navigate(move |_ev, _depth| {
            c.set(c.get() + 1);
        }));
        let cell = depth_cell();
        let pages = children(&rx.store, root);
        let nav = slot.borrow().clone().expect("build fills the handle slot");

        // Push to page 2, then Escape pops to page 1.
        let n = nav.clone();
        rx.drive(|ev| n.push(ev));
        let before = count.get();
        rx.key(root, key_ev(Key::Escape, true, false));
        assert_eq!(rx.depth(cell), Some(0), "Escape pops one page");
        assert_eq!(
            count.get(),
            before + 1,
            "the back gesture fires on_navigate"
        );
        assert!(!rx.store.hidden(pages[0]), "the root page is shown again");

        // Auto-repeat and key-up do not pop.
        let n = nav.clone();
        rx.drive(|ev| n.push(ev)); // back to page 1
        let before = count.get();
        rx.key(root, key_ev(Key::Escape, true, true)); // repeat: ignored
        rx.key(root, key_ev(Key::Escape, false, false)); // key-up: ignored
        assert_eq!(count.get(), before, "repeat and key-up do not pop");
        assert_eq!(rx.depth(cell), Some(1), "depth unchanged by repeat/key-up");

        // Backspace also pops.
        rx.key(root, key_ev(Key::Backspace, true, false));
        assert_eq!(rx.depth(cell), Some(0), "Backspace pops one page");

        // A back gesture at the root is a clamped no-op.
        let before = count.get();
        rx.key(root, key_ev(Key::Escape, true, false));
        assert_eq!(
            rx.depth(cell),
            Some(0),
            "a back gesture at the root is a no-op"
        );
        assert_eq!(count.get(), before, "and does not fire");
    }

    /// A handler-less stack still navigates through the shared cell — `build` does
    /// not panic and the back gesture is a no-op on the callback.
    #[test]
    fn handlerless_stack_still_navigates() {
        let slot: NavHandleSlot = Rc::new(RefCell::new(None));
        let mut rx = Reactive::new();
        let root = rx.build(three_pages(&slot));
        let cell = depth_cell();
        let pages = children(&rx.store, root);
        let nav = slot.borrow().clone().expect("build fills the handle slot");

        let n = nav.clone();
        rx.drive(|ev| n.push(ev));
        assert_eq!(
            rx.depth(cell),
            Some(1),
            "a handler-less control still moves the shared cell"
        );
        assert!(!rx.store.hidden(pages[1]), "and shows the new page");
    }
}
