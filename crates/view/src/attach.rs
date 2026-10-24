//! Installing a view's handlers on its nodes.

use std::cell::RefCell;
use std::rc::Rc;

use viso_behavior::Value;
use viso_ui::{
    Access, BuildCx, DispatchPhase, EventCx, Handle, NodeId, NodeStore, Notice, NoticeHandler,
    NoticeKinds, PointerPhase, StateStore,
};

use crate::control::{Control, ControlKind, Drive};
use crate::host::ViewHost;
use crate::scope::Scope;

use viso_schema::view::route::EventRoute;
pub use viso_schema::view::route::Route;

/// Installs `routes` and the built-in response of `control` on the node `node`
/// names, dispatching into `host` in `scope`,
/// and returns the handle so authoring chains inline.
pub fn attach(
    cx: &mut BuildCx<'_>,
    host: &Rc<RefCell<ViewHost>>,
    node: Handle,
    routes: &[Route],
    control: Option<Control>,
    scope: &Scope,
) -> Handle {
    let routes = &sink(host, node.id(), routes, control.as_ref(), scope);
    let control_kind = control.as_ref().map(|c| c.kind);
    let (keys, pointers) = split(routes, control.as_ref());
    let wait = tap_wait(routes);
    if let Some(pointers) = pointers {
        let handler = handler(host, pointers, control.clone(), wait.clone(), scope);
        cx.on_pointer(node, handler);
    }
    if let Some(keys) = keys {
        // A refresh, a snap, a full swipe, a reorder, a drop and a waiting
        // tap come without the focus.
        let focus = keys.iter().any(|r| {
            !r.event.is_drop()
                && !matches!(
                    r.event,
                    EventRoute::Refresh
                        | EventRoute::SnapChanged
                        | EventRoute::FullSwiped
                        | EventRoute::Reordered
                        | EventRoute::Tap
                )
        }) || control.as_ref().is_some_and(|c| c.responds());
        cx.on_key(node, handler(host, keys, control, wait, scope));
        if focus {
            cx.focusable(node, true);
        }
    }
    if counts(routes) || viewer(control_kind) {
        cx.count_clicks(node);
    }
    if drags(routes) {
        cx.draggable(node, true);
    }
    if holds(routes) {
        cx.long_pressable(node, true);
    }
    if pinches(routes) {
        cx.pinchable(node, true);
    }
    if let Some((kinds, watcher)) = notices(host, routes, scope) {
        cx.watch(node, kinds, watcher);
    }
    if refreshes(routes) {
        cx.refreshable(node, true);
    }
    if snaps(routes) {
        cx.tell_snaps(node, true);
    }
    if reorders(routes) {
        cx.reorder_list(node, true);
    }
    node
}

/// Installs `routes` and the built-in response of `control` on node `id`,
/// replacing its prior handlers: one pointer handler for the pointer routes,
/// one key handler (and focusability) for the key routes, and both for a
/// control and the routes it reports. A node with nothing of a kind keeps no
/// handler of that kind.
pub fn attach_node(
    store: &mut NodeStore,
    host: &Rc<RefCell<ViewHost>>,
    id: NodeId,
    routes: &[Route],
    control: Option<Control>,
    scope: &Scope,
) {
    store.clear_event_handlers(id);
    let routes = &sink(host, id, routes, control.as_ref(), scope);
    match notices(host, routes, scope) {
        Some((kinds, watcher)) => store.watch(id, kinds, watcher),
        None => store.unwatch(id),
    }
    let control_kind = control.as_ref().map(|c| c.kind);
    let (keys, pointers) = split(routes, control.as_ref());
    let wait = tap_wait(routes);
    if let Some(pointers) = pointers {
        let handler = handler(host, pointers, control.clone(), wait.clone(), scope);
        store.set_pointer_handler(id, Box::new(handler));
    }
    if let Some(keys) = keys {
        let focus = takes_focus(&keys, control.as_ref());
        let handler = handler(host, keys, control, wait, scope);
        store.set_key_handler(id, Box::new(handler));
        if focus {
            store.set_focusable(id, true);
        }
    }
    if counts(routes) || viewer(control_kind) {
        store.count_clicks(id);
    } else {
        store.uncount_clicks(id);
    }
    // A viewer, a swipe row, a reorder handle and a sheet's panel take
    // their own drags.
    let own = store.is_viewer(id)
        || store.is_swipe_row(id)
        || store.is_reorder_handle(id)
        || store.is_sheet_panel(id);
    store.set_draggable(id, drags(routes) || own);
    store.set_long_pressable(id, holds(routes));
    store.set_pinchable(id, pinches(routes));
    if refreshes(routes) || store.is_refreshable(id) {
        store.set_refreshable(id, refreshes(routes));
    }
    store.tell_snaps(id, snaps(routes));
    if reorders(routes) || store.is_reorder_list(id) {
        store.set_reorder_list(id, reorders(routes));
    }
}

/// The routes of `routes` node `id` runs itself: those its parts report,
/// when the framework builds its parts, go to `host` for the parts to fire.
fn sink(
    host: &Rc<RefCell<ViewHost>>,
    id: NodeId,
    routes: &[Route],
    control: Option<&Control>,
    scope: &Scope,
) -> Vec<Route> {
    let Some(kind) = control.map(|c| c.kind).filter(|kind| kind.builds_parts()) else {
        return routes.to_vec();
    };
    let (reported, own): (Vec<Route>, Vec<Route>) = routes
        .iter()
        .copied()
        .partition(|route| kind.reports(route.event));
    if let Ok(mut host) = host.try_borrow_mut() {
        host.set_sink(id, reported, scope);
    }
    own
}

/// Whether a node whose key handler runs `keys` takes the focus for them as
/// it is re-attached: links take it as their text holds them, not as heard,
/// a text as it is selectable, and a refresh, a snap, a full swipe, a drop
/// and a waiting tap come without it.
fn takes_focus(keys: &[Route], control: Option<&Control>) -> bool {
    keys.iter().any(|r| {
        !r.event.is_drop()
            && !matches!(
                r.event,
                EventRoute::Link
                    | EventRoute::Refresh
                    | EventRoute::SnapChanged
                    | EventRoute::FullSwiped
                    | EventRoute::Reordered
                    | EventRoute::Tap
            )
    }) || control.is_some_and(|c| c.kind.responds())
}

/// Whether a control of `kind` is a viewer, which counts its clicks for its
/// double tap.
fn viewer(kind: Option<ControlKind>) -> bool {
    kind == Some(ControlKind::Viewer)
}

/// Whether `routes` hear double taps: the node counts its clicks.
fn counts(routes: &[Route]) -> bool {
    routes
        .iter()
        .any(|route| route.event == EventRoute::DoubleTap)
}

/// Whether `routes` hear both taps and double taps: a tap then waits out the
/// click interval, and runs only when no second press came.
fn waits(routes: &[Route]) -> bool {
    counts(routes) && routes.iter().any(|route| route.event == EventRoute::Tap)
}

/// The tap a node that [`waits`] holds back, shared by its pointer handler,
/// which hears the release, and its key handler, which hears the wake.
type TapWait = Rc<RefCell<Waiting>>;

/// A held-back tap and the wake that lets it run.
#[derive(Default)]
struct Waiting {
    /// The token of the wake the tap waits on; a later press moves it on, so
    /// a stale wake runs nothing.
    token: u32,
    tap: Option<Value>,
}

/// Wakes a waiting tap's carry this bit, kept off any token a control arms.
const TAP_WAKE: u32 = 1 << 31;

impl Waiting {
    /// Drops the waiting tap and moves the token on.
    fn cancel(&mut self) -> u32 {
        self.tap = None;
        self.token = self.token.wrapping_add(1) & !TAP_WAKE;
        self.token | TAP_WAKE
    }
}

fn tap_wait(routes: &[Route]) -> Option<TapWait> {
    waits(routes).then(TapWait::default)
}

/// Whether `routes` hear the item a snapping viewport settles on.
fn snaps(routes: &[Route]) -> bool {
    routes
        .iter()
        .any(|route| route.event == EventRoute::SnapChanged)
}

/// Whether `routes` take reorders: the list's rows lift from their handles.
fn reorders(routes: &[Route]) -> bool {
    routes
        .iter()
        .any(|route| route.event == EventRoute::Reordered)
}

/// Whether `routes` take refreshes: the viewport refreshes when pulled.
fn refreshes(routes: &[Route]) -> bool {
    routes
        .iter()
        .any(|route| route.event == EventRoute::Refresh)
}

/// Whether `routes` take drags.
fn drags(routes: &[Route]) -> bool {
    routes.iter().any(|route| route.event.is_drag())
}

/// Whether `routes` take pinches.
fn pinches(routes: &[Route]) -> bool {
    routes.iter().any(|route| route.event.is_pinch())
}

/// Whether `routes` take long presses.
fn holds(routes: &[Route]) -> bool {
    routes
        .iter()
        .any(|route| route.event == EventRoute::LongPress)
}

/// The routes a key handler runs and the routes a pointer handler runs, `None`
/// for a kind of handler the node needs none of. A control drives both, and the
/// routes it reports run from either — a button's `click` from a release and
/// from a key; a label responds to nothing.
fn split(routes: &[Route], control: Option<&Control>) -> (Option<Vec<Route>>, Option<Vec<Route>>) {
    let control = control.filter(|control| control.responds());
    let waits = waits(routes);
    let pick = |key: bool| {
        let picked: Vec<Route> = routes
            .iter()
            .copied()
            .filter(|route| {
                !route.event.is_notice()
                    && (route.event.is_control()
                        || control.is_some_and(|c| c.kind.reports(route.event))
                        || route.event.by_key_handler() == key
                        || (key && waits && route.event == EventRoute::Tap))
            })
            .collect();
        (control.is_some() || !picked.is_empty()).then_some(picked)
    };
    (pick(true), pick(false))
}

/// The node handler that runs `control`'s response to the sample under
/// dispatch, then every route of `routes` the sample fires, in declaration
/// order: a standard route on the target and bubble legs of the walk, a
/// control's route when the control reports it. With `wait`, a tap runs from
/// the wake its release arms, unless a second press came first.
///
/// A handler never runs inside another: the router takes a node's handler out
/// of the store before calling it and state writes are deferred to the flush, so
/// the host is never borrowed twice. The borrow is still checked, and a
/// re-entrant dispatch is skipped rather than panicking.
fn handler(
    host: &Rc<RefCell<ViewHost>>,
    routes: Vec<Route>,
    control: Option<Control>,
    wait: Option<TapWait>,
    scope: &Scope,
) -> impl FnMut(&mut EventCx<'_>) + 'static {
    let host = Rc::clone(host);
    let scope = scope.clone();
    move |cx: &mut EventCx<'_>| {
        let Ok(mut host) = host.try_borrow_mut() else {
            return;
        };
        // Its access key takes the focus to it; a button clicks instead.
        if cx.access() == Some(Access::Key)
            && control
                .as_ref()
                .is_none_or(|c| c.kind != ControlKind::Button)
            && let Some(node) = cx.node()
        {
            cx.request_focus(node);
        }
        let change = control
            .as_ref()
            .and_then(|control| control.drive(&mut host, &scope, cx));
        let phase = cx.phase();
        // A double tap's second press cancels the tap waiting on its first.
        if let Some(wait) = &wait
            && cx.pointer().is_some_and(|p| p.phase == PointerPhase::Down)
            && cx.clicks() > 1
        {
            wait.borrow_mut().cancel();
        }
        for &route in &routes {
            if route.event == EventRoute::Tap
                && let Some(wait) = &wait
            {
                if let Some(tap) = held_tap(wait, route, phase, cx) {
                    host.dispatch(route.handler, tap, &scope, cx);
                }
                continue;
            }
            let payload = match &change {
                // A control reports its change once, on its own node.
                Some((changed, value)) if *changed == route.event => value.clone(),
                // A release that removes a chip does not click it too.
                Some((EventRoute::Removed, _)) if route.event == EventRoute::Click => continue,
                _ if !runs_in(route, phase) => continue,
                _ => match crate::route::payload(route.event, cx) {
                    Some(payload) => payload,
                    None => continue,
                },
            };
            // A link an `on link` takes is the view's to follow, not the
            // system's to open.
            if route.event == EventRoute::Link {
                cx.prevent_default();
            }
            host.dispatch(route.handler, payload, &scope, cx);
        }
    }
}

/// The tap a node that waits runs on this dispatch: the one its wake lets
/// through. A single tap's release holds its tap back and arms the wake.
fn held_tap(
    wait: &TapWait,
    route: Route,
    phase: DispatchPhase,
    cx: &mut EventCx<'_>,
) -> Option<Value> {
    let mut wait = wait.borrow_mut();
    if let Some(token) = cx.woken() {
        return (token == wait.token | TAP_WAKE)
            .then(|| wait.tap.take())
            .flatten();
    }
    if !runs_in(route, phase) {
        return None;
    }
    let tap = crate::route::payload(route.event, cx)?;
    let token = wait.cancel();
    if cx.clicks() == 1
        && let Some(node) = cx.node()
    {
        wait.tap = Some(tap);
        cx.wake_after(node, cx.click_interval(), token);
    }
    None
}

/// The notices `routes` run on and the handler that runs them, dispatching
/// into `host` in `scope`; `None` when none runs on a notice.
fn notices(
    host: &Rc<RefCell<ViewHost>>,
    routes: &[Route],
    scope: &Scope,
) -> Option<(NoticeKinds, NoticeHandler)> {
    let routes: Vec<Route> = routes
        .iter()
        .copied()
        .filter(|route| route.event.is_notice())
        .collect();
    let kinds = routes
        .iter()
        .fold(NoticeKinds::default(), |kinds, route| match route.event {
            EventRoute::ScrollChanged => kinds | NoticeKinds::SCROLL,
            EventRoute::AnimationEnd => kinds | NoticeKinds::ANIMATION_END,
            _ => kinds | NoticeKinds::VISIBLE_RANGE,
        });
    if kinds.is_empty() {
        return None;
    }
    let host = Rc::clone(host);
    let scope = scope.clone();
    let watcher = move |notice: &Notice, states: &mut StateStore| {
        let Ok(mut host) = host.try_borrow_mut() else {
            return;
        };
        let (event, payload) = crate::route::notice_payload(notice);
        for route in routes.iter().filter(|route| route.event == event) {
            host.dispatch(route.handler, payload.clone(), &scope, states);
        }
    };
    Some((kinds, Box::new(watcher)))
}

/// Whether `route` runs in dispatch phase `phase`: a capture handler on the
/// capture leg, any other on the bubble leg, every one on the target.
fn runs_in(route: Route, phase: DispatchPhase) -> bool {
    match phase {
        DispatchPhase::Capture => route.capture,
        DispatchPhase::Target => true,
        DispatchPhase::Bubble => !route.capture,
    }
}
