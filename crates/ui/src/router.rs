//! Typed routes: an app's own route type, the [`Navigator`] that holds its
//! back stack, and the [`RouterView`] that shows it.
//!
//! A route is any `Clone + PartialEq` value — usually an enum whose variants
//! carry the page's parameters. The navigator is a `Copy` handle any handler
//! drives (`push`, `replace`, `pop`, `reset`), a deep link included; reads
//! (`current`, `can_pop`) record a dependency where the context tracks one.
//! Navigations a transaction makes land together: the router view reconciles
//! once, at the frame's flush.
//!
//! The router view mounts one page per route of the stack, keyed by the route
//! value, and shows the top: a page still on the stack keeps its nodes and
//! states while covered, and is revealed as it was; a page popped off goes,
//! after its leave. A system back action (Android's back, Escape, a mouse's
//! back button) asks the innermost router view showing more than one page: its
//! `on_back_requested` may refuse, else it pops; at the root page it is the
//! host's.
//!
//! ```
//! use viso_ui::{BuildCx, LeafStyle, NodeStore, StateStore, BindingTable, VirtualLists,
//!     TextEdits, SemanticProjector, router_view, routes};
//!
//! #[derive(Clone, PartialEq)]
//! enum Route {
//!     Inbox,
//!     Message { id: u32 },
//! }
//!
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let (mut bindings, mut lists, mut edits, mut projectors) = (
//!     BindingTable::new(), VirtualLists::new(), TextEdits::new(), SemanticProjector::new(),
//! );
//! let mut cx = BuildCx::with_reactive(
//!     &mut store, &mut states, &mut bindings, &mut lists, &mut edits, &mut projectors,
//! );
//! let nav = router_view(
//!     Route::Inbox,
//!     routes! {
//!         Route::Inbox => |cx: &mut BuildCx<'_>| { cx.leaf(LeafStyle::default()); },
//!         Route::Message { id } => |cx: &mut BuildCx<'_>| {
//!             let _ = id;
//!             cx.leaf(LeafStyle::default());
//!         },
//!     },
//! )
//! .build(&mut cx);
//! // A handler navigates: `nav.push(ev, Route::Message { id: 7 })`.
//! # let _ = nav;
//! ```

use std::cell::RefCell;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::mem::Discriminant;
use std::rc::Rc;

use crate::binding::Template;
use crate::component::{BuildCx, Component, FlexStyle, NodeStore, StackStyle};
use crate::context::{EventCx, StateWriter};
use crate::control::{ControlHandle, ControlId, Controls};
use crate::input::{DispatchPhase, Key};
use crate::navigation::PageTransition;
use crate::node::NodeId;
use crate::semantics::{Role, Semantics};
use crate::state::{StateId, StateValue};
use crate::structure::StructureCx;
use crate::typed_state::StateRead;
use crate::{Align, Axis, BoxStyle, DirtyClass, Inset, Justify, Length, Sizing};

/// The back stack of a [`RouterView`]: a `Copy` handle over the routes of
/// type `R` it shows, the top last.
///
/// Mint one up front with [`BuildCx::handle`] for handlers declared before
/// the router view, or take the one [`RouterView::build`] returns. Once the
/// router view is gone, reads answer `None` and navigations do nothing.
pub struct Navigator<R> {
    id: ControlId,
    route: PhantomData<fn() -> R>,
}

/// What a navigator and its router view share.
struct NavigatorParts<R> {
    /// The routes, the root first.
    stack: RefCell<Vec<R>>,
    /// Raised at each navigation: the router view and every read of the
    /// navigator depend on it.
    revision: StateId,
}

impl<R: Clone + PartialEq + 'static> Navigator<R> {
    /// The route on top; `None` once the router view is gone.
    pub fn current(self, cx: &mut (impl StateRead + Controls + ?Sized)) -> Option<R> {
        let parts = cx.control::<NavigatorParts<R>>(self.id)?;
        cx.read(parts.revision);
        parts.stack.borrow().last().cloned()
    }

    /// Whether a [`pop`](Self::pop) would reveal a page: more than one
    /// route is on the stack.
    pub fn can_pop(self, cx: &mut (impl StateRead + Controls + ?Sized)) -> bool {
        self.depth(cx) > 1
    }

    /// How many routes the stack holds; 0 once the router view is gone.
    pub fn depth(self, cx: &mut (impl StateRead + Controls + ?Sized)) -> usize {
        let Some(parts) = cx.control::<NavigatorParts<R>>(self.id) else {
            return 0;
        };
        cx.read(parts.revision);
        parts.stack.borrow().len()
    }

    /// The routes on the stack, the root first.
    pub fn routes(self, cx: &mut (impl StateRead + Controls + ?Sized)) -> Vec<R> {
        let Some(parts) = cx.control::<NavigatorParts<R>>(self.id) else {
            return Vec::new();
        };
        cx.read(parts.revision);
        parts.stack.borrow().clone()
    }

    /// Shows `route` over the current page, which keeps its state below it.
    pub fn push(self, cx: &mut (impl StateWriter + ?Sized), route: R) {
        self.navigate(cx, |stack| {
            stack.push(route);
            true
        });
    }

    /// Shows `route` in place of the current page, which goes.
    pub fn replace(self, cx: &mut (impl StateWriter + ?Sized), route: R) {
        self.navigate(cx, |stack| {
            if stack.last() == Some(&route) {
                return false;
            }
            stack.pop();
            stack.push(route);
            true
        });
    }

    /// Reveals the page below the current one, which goes; whether there was
    /// one. The root page stays.
    pub fn pop(self, cx: &mut (impl StateWriter + ?Sized)) -> bool {
        self.navigate(cx, |stack| stack.len() > 1 && stack.pop().is_some())
    }

    /// Makes `route` the only page, as a deep link does: a page of the stack
    /// with that route is kept, every other goes.
    pub fn reset(self, cx: &mut (impl StateWriter + ?Sized), route: R) {
        self.navigate(cx, |stack| {
            if stack.len() == 1 && stack[0] == route {
                return false;
            }
            stack.clear();
            stack.push(route);
            true
        });
    }

    /// Edits the stack with `edit` and, when it changed it, raises the
    /// revision; whether it did.
    fn navigate(
        self,
        cx: &mut (impl StateWriter + ?Sized),
        edit: impl FnOnce(&mut Vec<R>) -> bool,
    ) -> bool {
        let Some(parts) = cx.controls().get::<NavigatorParts<R>>(self.id) else {
            return false;
        };
        if !edit(&mut parts.stack.borrow_mut()) {
            return false;
        }
        let revision = match cx.read(parts.revision) {
            Some(StateValue::Int(n)) => n.wrapping_add(1),
            _ => 0,
        };
        cx.set(parts.revision, StateValue::Int(revision));
        true
    }
}

impl<R: 'static> ControlHandle for Navigator<R> {
    fn from_id(id: ControlId) -> Self {
        Navigator {
            id,
            route: PhantomData,
        }
    }

    fn id(self) -> ControlId {
        self.id
    }
}

impl<R> Clone for Navigator<R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R> Copy for Navigator<R> {}

impl<R> PartialEq for Navigator<R> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl<R> Eq for Navigator<R> {}

impl<R> Hash for Navigator<R> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl<R> fmt::Debug for Navigator<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Navigator").field(&self.id).finish()
    }
}

/// A system back action a [`RouterView`] is about to take: its
/// `on_back_requested` handler may [`prevent_default`](Self::prevent_default)
/// it, keeping the page — to ask about unsaved changes, say.
#[derive(Debug, Default)]
pub struct BackRequest {
    prevented: bool,
}

impl BackRequest {
    /// Keeps the current page: the router view does not pop.
    pub fn prevent_default(&mut self) {
        self.prevented = true;
    }

    /// Whether a handler kept the page.
    pub fn is_prevented(&self) -> bool {
        self.prevented
    }
}

/// How a route builds its page.
type PageFn<R> = Rc<dyn Fn(&mut BuildCx<'_>, &R)>;

/// The back-request handler.
type BackFn = Rc<RefCell<dyn FnMut(&mut EventCx<'_>, &mut BackRequest)>>;

/// The navigation outlet: shows the page of the top route of its
/// [`Navigator`], keeping the pages below it.
///
/// Construct one with [`router_view`]. Invalidation: a navigation raises one
/// revision cell; at the flush the view mounts the new pages, frees those
/// popped off and turns to the top page — the pages it trades mark
/// `MEASURE | LAYOUT | PAINT | SEMANTICS`. A covered page is folded out of
/// layout and paint, not rebuilt.
pub struct RouterView<R> {
    initial: R,
    page: PageFn<R>,
    on_back: Option<BackFn>,
    size: Sizing,
    transition: PageTransition,
    navigator: Option<Navigator<R>>,
}

/// A [`RouterView`] starting at `initial`, building each route's page with
/// `page` — usually a [`routes!`](crate::routes) table, whose `match` makes
/// a route without a page a compile error.
pub fn router_view<R: Clone + PartialEq + 'static>(
    initial: R,
    page: impl Fn(&mut BuildCx<'_>, &R) + 'static,
) -> RouterView<R> {
    RouterView {
        initial,
        page: Rc::new(page),
        on_back: None,
        size: Sizing {
            width: Length::Fill { weight: 1.0 },
            height: Length::Fill { weight: 1.0 },
        },
        transition: PageTransition::Native,
        navigator: None,
    }
}

impl<R: Clone + PartialEq + 'static> RouterView<R> {
    /// Asks `handler` before a system back action pops; it keeps the page
    /// with [`BackRequest::prevent_default`].
    pub fn on_back_requested(
        mut self,
        handler: impl FnMut(&mut EventCx<'_>, &mut BackRequest) + 'static,
    ) -> Self {
        self.on_back = Some(Rc::new(RefCell::new(handler)));
        self
    }

    /// Sets the view's own size request (defaults to `Fill`).
    pub fn size(mut self, size: Sizing) -> Self {
        self.size = size;
        self
    }

    /// Sets how its pages trade places (defaults to the platform's own way).
    pub fn transition(mut self, transition: PageTransition) -> Self {
        self.transition = transition;
        self
    }

    /// Binds `navigator`, minted with [`BuildCx::handle`] before the view is
    /// built, so handlers declared earlier can navigate.
    pub fn navigator(mut self, navigator: Navigator<R>) -> Self {
        self.navigator = Some(navigator);
        self
    }

    /// Builds the view into `cx` and returns its navigator.
    pub fn build(mut self, cx: &mut BuildCx<'_>) -> Navigator<R> {
        let navigator = *self.navigator.get_or_insert_with(|| cx.handle());
        Component::build(&self, cx);
        navigator
    }
}

/// Maps each route of an enum to the builder of its page: a `match` whose
/// arms are `pattern => page`, `page` a function or closure taking the
/// [`BuildCx`]; the pattern's bindings are in scope for it. A route without
/// an arm is a compile error.
///
/// ```
/// # use viso_ui::{BuildCx, LeafStyle, router_view, routes};
/// # #[derive(Clone, PartialEq)]
/// # enum Route { Inbox, Message { id: u32 } }
/// # fn inbox(cx: &mut BuildCx<'_>) { cx.leaf(LeafStyle::default()); }
/// # fn message(cx: &mut BuildCx<'_>, id: u32) { let _ = id; cx.leaf(LeafStyle::default()); }
/// let view = router_view(Route::Inbox, routes! {
///     Route::Inbox => inbox,
///     Route::Message { id } => |cx| message(cx, *id),
/// });
/// # let _ = view;
/// ```
#[macro_export]
macro_rules! routes {
    ($($route:pat => $page:expr),+ $(,)?) => {
        move |cx: &mut $crate::BuildCx<'_>, route: &_| match route {
            $($route => ($page)(cx),)+
        }
    };
}

/// The pages a router view keeps, and how it builds more.
struct Router<R> {
    /// The paged stack the pages are children of.
    stack: NodeId,
    page: PageFn<R>,
    /// The mounted pages, in child order: one per route of the stack, the
    /// root first.
    pages: Vec<(R, NodeId)>,
    /// Pages popped off that show on as they leave: freed once they have.
    leaving: Vec<NodeId>,
    /// One template per kind of route, so pages of one kind share their
    /// recorded structure.
    templates: Vec<(Discriminant<R>, Template)>,
    transition: PageTransition,
}

impl<R: Clone + PartialEq + 'static> Router<R> {
    /// Builds `route`'s page as the last child of the stack `cx` builds
    /// under.
    fn mount(&mut self, cx: &mut BuildCx<'_>, route: &R) -> NodeId {
        let kind = std::mem::discriminant(route);
        let at = match self.templates.iter().position(|(k, _)| *k == kind) {
            Some(at) => at,
            None => {
                self.templates.push((kind, Template::default()));
                self.templates.len() - 1
            }
        };
        let page = Rc::clone(&self.page);
        let mut template = self.templates[at].1;
        let node = cx.instance(&mut template, |cx| {
            let node = cx.flex(page_style(), |cx| page(cx, route));
            cx.semantics(node, Semantics::role(Role::Group));
            node.id()
        });
        self.templates[at].1 = template;
        node
    }

    /// Shows `routes`: keeps the pages of the routes still on it, mounts
    /// the new, lets go of the rest, and turns to the top.
    fn reconcile(&mut self, cx: &mut StructureCx<'_>, routes: &[R]) {
        let store = &mut *cx.store;
        let shown = store
            .shown_page(self.stack)
            .and_then(|i| store.arena().children(self.stack).nth(i));
        let mut scratch = Vec::new();
        self.free_left(cx, &mut scratch);

        let mut old = std::mem::take(&mut self.pages);
        let mut next: Vec<Option<(R, NodeId)>> = Vec::with_capacity(routes.len());
        for route in routes {
            next.push(
                old.iter()
                    .position(|(r, _)| r == route)
                    .map(|at| old.remove(at)),
            );
        }
        let fresh_top = next.last().is_some_and(Option::is_none);
        for (_, page) in old {
            if Some(page) == shown {
                self.leaving.push(page);
            } else {
                cx.store.free_tree(page, &mut *cx.effects, &mut scratch);
            }
        }
        for (slot, route) in next.iter_mut().zip(routes) {
            if slot.is_none() {
                let mut build = BuildCx::with_parent(cx.store, cx.states, cx.bindings, self.stack)
                    .with_lists(cx.lists);
                let node = self.mount(&mut build, route);
                *slot = Some((route.clone(), node));
            }
        }
        self.pages = next.into_iter().flatten().collect();

        // Children in stack order, the leaving pages above.
        let order: Vec<NodeId> = self
            .pages
            .iter()
            .map(|&(_, page)| page)
            .chain(self.leaving.iter().copied())
            .collect();
        for &page in &order {
            cx.store.arena_detach(page);
        }
        for &page in &order {
            cx.store.arena_append_child(self.stack, page);
        }
        cx.store.mark_dirty(
            self.stack,
            DirtyClass::MEASURE
                | DirtyClass::LAYOUT
                | DirtyClass::PAINT
                | DirtyClass::HIT_TEST
                | DirtyClass::SEMANTICS,
        );

        // Revealing a kept page turns back; a new page arrives forward.
        let top = self.pages.len().checked_sub(1);
        let arriving = top.map(|i| self.pages[i].1);
        cx.store
            .turn_page_from(self.stack, shown, top, !fresh_top, self.transition);
        self.free_left(cx, &mut scratch);
        if arriving != shown
            && let Some(page) = arriving
        {
            focus_page(cx.store, page);
        }
    }

    /// Frees the popped pages done leaving.
    fn free_left(&mut self, cx: &mut StructureCx<'_>, scratch: &mut Vec<NodeId>) {
        self.leaving.retain(|&page| {
            if cx.store.is_leaving(page) {
                return true;
            }
            cx.store.free_tree(page, &mut *cx.effects, scratch);
            false
        });
    }
}

/// Moves the focus to the first heading of `page`, else to the page.
fn focus_page(store: &mut NodeStore, page: NodeId) {
    let mut todo = vec![page];
    let mut heading = None;
    while let Some(node) = todo.pop() {
        if store
            .semantics(node)
            .is_some_and(|s| s.role == Role::Heading)
        {
            heading = Some(node);
            break;
        }
        let at = todo.len();
        todo.extend(store.arena().children(node));
        todo[at..].reverse();
    }
    store.set_focused(Some(heading.unwrap_or(page)));
}

/// A page: a column filling the stack.
fn page_style() -> FlexStyle {
    FlexStyle {
        axis: Axis::Column,
        gap: 0.0,
        padding: Inset::all(0.0),
        align: Align::Stretch,
        justify: Justify::Start,
        size: Sizing {
            width: Length::Fill { weight: 1.0 },
            height: Length::Fill { weight: 1.0 },
        },
        style: BoxStyle::NONE,
    }
}

impl<R: Clone + PartialEq + 'static> Component for RouterView<R> {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let root = cx.stack(
            StackStyle {
                size: self.size,
                ..Default::default()
            },
            |_| {},
        );
        let stack = root.id();
        let mut router = Router {
            stack,
            page: Rc::clone(&self.page),
            pages: Vec::new(),
            leaving: Vec::new(),
            templates: Vec::new(),
            transition: self.transition,
        };
        let first = cx.inside(root, |cx| router.mount(cx, &self.initial));
        router.pages.push((self.initial.clone(), first));
        cx.page(root, Some(0));
        cx.clip(root, true);
        cx.semantics(root, Semantics::role(Role::Navigation));

        let Some(navigator) = self.navigator else {
            return;
        };
        if !cx.is_reactive() {
            return;
        }
        let revision = cx.state(StateValue::Int(0));
        let parts = Rc::new(NavigatorParts {
            stack: RefCell::new(vec![self.initial.clone()]),
            revision,
        });
        cx.bind_control(navigator.id, stack, Rc::clone(&parts));

        // The flush shows what the transaction's navigations left.
        let router = Rc::new(RefCell::new(router));
        let watched = Rc::downgrade(&parts);
        cx.structure(|s| {
            let trim = s.store.memory_trim_cell(s.states);
            s.store.add_structure_hook([revision, trim], move |s, _| {
                let Some(parts) = watched.upgrade() else {
                    return;
                };
                let routes = parts.stack.borrow().clone();
                router.borrow_mut().reconcile(s, &routes);
            });
        });

        // A back action pops unless the handler keeps the page; at the root
        // page it goes on to the host.
        let on_back = self.on_back.clone();
        cx.focusable(root, true);
        cx.on_key(root, move |ev| {
            let Some(key) = ev.key() else { return };
            if !key.pressed
                || key.repeat
                || !matches!(key.key, Key::Escape | Key::Backspace | Key::Back)
                || ev.phase() == DispatchPhase::Capture
                || !navigator.can_pop(ev)
            {
                return;
            }
            let mut request = BackRequest::default();
            if let Some(on_back) = &on_back {
                (on_back.borrow_mut())(ev, &mut request);
            }
            if !request.prevented {
                navigator.pop(ev);
            }
            ev.stop_propagation();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{KeyRouter, Modifiers};
    use crate::reactive::EffectStore;
    use crate::structure::run_structure_hooks;
    use crate::{
        BindingTable, KeyEvent, LeafStyle, SemanticProjector, StateStore, TextEdits, VirtualLists,
    };

    #[derive(Debug, Clone, PartialEq)]
    enum Route {
        Inbox,
        Message(u32),
        Settings,
    }

    /// A router over three kinds of page, each a leaf with one state cell
    /// counting what it was shown, and the stores it runs over.
    struct App {
        store: NodeStore,
        states: StateStore,
        bindings: BindingTable,
        effects: EffectStore,
        lists: VirtualLists,
        edits: TextEdits,
        nav: Navigator<Route>,
        root: NodeId,
        /// The cell each page allocated, by route, as they were built.
        cells: Rc<RefCell<Vec<(Route, StateId)>>>,
        /// Whether the back handler keeps the page.
        keep: Rc<std::cell::Cell<bool>>,
        asked: Rc<std::cell::Cell<u32>>,
    }

    impl App {
        fn new() -> App {
            let mut store = NodeStore::new();
            let mut states = StateStore::new();
            let mut bindings = BindingTable::new();
            let mut lists = VirtualLists::new();
            let mut edits = TextEdits::new();
            let mut projectors = SemanticProjector::new();
            let cells: Rc<RefCell<Vec<(Route, StateId)>>> = Rc::default();
            let keep = Rc::new(std::cell::Cell::new(false));
            let asked = Rc::new(std::cell::Cell::new(0));
            let (nav, root) = {
                let mut cx = BuildCx::with_reactive(
                    &mut store,
                    &mut states,
                    &mut bindings,
                    &mut lists,
                    &mut edits,
                    &mut projectors,
                );
                let built = Rc::clone(&cells);
                let (keep, asked) = (Rc::clone(&keep), Rc::clone(&asked));
                let page = move |cx: &mut BuildCx<'_>, route: &Route| {
                    let cell = cx.state(StateValue::Int(0));
                    let leaf = cx.leaf(LeafStyle::default());
                    if *route == Route::Settings {
                        cx.semantics(leaf, Semantics::role(Role::Heading));
                    }
                    built.borrow_mut().push((route.clone(), cell));
                };
                let nav = router_view(Route::Inbox, page)
                    .transition(PageTransition::None)
                    .on_back_requested(move |_, request| {
                        asked.set(asked.get() + 1);
                        if keep.get() {
                            request.prevent_default();
                        }
                    })
                    .build(&mut cx);
                (nav, cx.root().expect("a root"))
            };
            App {
                store,
                states,
                bindings,
                effects: EffectStore::default(),
                lists,
                edits,
                nav,
                root,
                cells,
                keep,
                asked,
            }
        }

        /// Runs `f` as a handler at the root, then the flush.
        fn act(&mut self, f: impl FnOnce(&mut EventCx<'_>, Navigator<Route>)) {
            let nav = self.nav;
            KeyRouter::run_at(
                &mut self.store,
                &mut self.states,
                &self.bindings,
                &mut self.edits,
                self.root,
                |ev| f(ev, nav),
            );
            self.flush();
        }

        fn flush(&mut self) {
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
            self.bindings.collect(&mut self.store, &mut self.states);
        }

        /// A system back action, as the facade delivers it.
        fn back(&mut self) -> bool {
            let Some(stack) = self.store.back_target() else {
                return false;
            };
            let ev = KeyEvent {
                key: Key::Back,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::default(),
            };
            let took = KeyRouter::route_back(
                &mut self.store,
                &mut self.states,
                &self.bindings,
                &mut self.edits,
                self.root,
                stack,
                ev,
                &mut Vec::new(),
            );
            self.flush();
            took
        }

        /// The routes of the pages mounted, in child order.
        fn pages(&self) -> usize {
            self.store.arena().children(self.root).count()
        }

        /// The cell the page last built for `route` holds.
        fn cell(&self, route: &Route) -> StateId {
            self.cells
                .borrow()
                .iter()
                .rev()
                .find(|(r, _)| r == route)
                .map(|&(_, c)| c)
                .expect("built")
        }

        fn current(&mut self) -> Option<Route> {
            let nav = self.nav;
            nav.current(&mut self.states)
        }
    }

    #[test]
    fn a_covered_page_keeps_its_state_and_a_popped_one_goes() {
        let mut app = App::new();
        let inbox = app.cell(&Route::Inbox);
        app.states.set(inbox, StateValue::Int(7));
        app.flush();

        app.act(|ev, nav| nav.push(ev, Route::Message(3)));
        assert_eq!(app.pages(), 2);
        assert_eq!(app.store.shown_page(app.root), Some(1));
        assert_eq!(app.current(), Some(Route::Message(3)));
        let message = app.cell(&Route::Message(3));

        app.act(|ev, nav| {
            assert!(nav.pop(ev));
        });
        assert_eq!(app.pages(), 1, "the popped page went");
        assert_eq!(app.store.shown_page(app.root), Some(0));
        assert_eq!(app.states.get(inbox), Some(StateValue::Int(7)), "kept");
        assert!(!app.states.is_live(message), "its state went with it");
        assert_eq!(app.cells.borrow().len(), 2, "the inbox was not rebuilt");
    }

    #[test]
    fn navigations_in_one_transaction_land_together() {
        let mut app = App::new();
        app.act(|ev, nav| {
            nav.push(ev, Route::Message(1));
            nav.push(ev, Route::Message(2));
            nav.replace(ev, Route::Settings);
        });
        assert_eq!(
            app.nav.routes(&mut app.states),
            [Route::Inbox, Route::Message(1), Route::Settings]
        );
        assert_eq!(app.pages(), 3);
        let built: Vec<Route> = app.cells.borrow().iter().map(|(r, _)| r.clone()).collect();
        assert_eq!(
            built,
            [Route::Inbox, Route::Message(1), Route::Settings],
            "Message(2) never mounted"
        );
        // The focus lands on the new page's heading.
        let heading = app.store.focused().expect("focused");
        assert_eq!(
            app.store.semantics(heading).map(|s| s.role),
            Some(Role::Heading)
        );

        // A reset keeps the page of its route and lets the rest go.
        let inbox = app.cell(&Route::Inbox);
        app.act(|ev, nav| nav.reset(ev, Route::Inbox));
        assert_eq!(app.pages(), 1);
        assert!(app.states.is_live(inbox));
        assert!(!app.nav.can_pop(&mut app.states));
    }

    #[test]
    fn a_back_action_pops_unless_the_page_is_kept_and_passes_at_the_root() {
        let mut app = App::new();
        assert!(!app.back(), "at the root the host takes it");
        assert_eq!(app.asked.get(), 0);

        app.act(|ev, nav| nav.push(ev, Route::Settings));
        app.keep.set(true);
        assert!(app.back());
        assert_eq!(app.asked.get(), 1);
        assert_eq!(app.current(), Some(Route::Settings), "kept");

        app.keep.set(false);
        assert!(app.back());
        assert_eq!(app.current(), Some(Route::Inbox));
        assert_eq!(app.pages(), 1);
    }
}
