//! Installing a view's handlers on its nodes.

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::{
    Access, BuildCx, DispatchPhase, EventCx, Handle, NodeId, NodeStore, Notice, NoticeHandler,
    NoticeKinds, StateStore,
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
    let (keys, pointers) = split(routes, control.as_ref());
    if let Some(pointers) = pointers {
        cx.on_pointer(node, handler(host, pointers, control.clone(), scope));
    }
    if let Some(keys) = keys {
        // A refresh, and a snap, come without the focus.
        let focus = keys
            .iter()
            .any(|r| !matches!(r.event, EventRoute::Refresh | EventRoute::SnapChanged))
            || control.as_ref().is_some_and(|c| c.responds());
        cx.on_key(node, handler(host, keys, control, scope));
        if focus {
            cx.focusable(node, true);
        }
    }
    if drags(routes) {
        cx.draggable(node, true);
    }
    if holds(routes) {
        cx.long_pressable(node, true);
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
    match notices(host, routes, scope) {
        Some((kinds, watcher)) => store.watch(id, kinds, watcher),
        None => store.unwatch(id),
    }
    let (keys, pointers) = split(routes, control.as_ref());
    if let Some(pointers) = pointers {
        store.set_pointer_handler(
            id,
            Box::new(handler(host, pointers, control.clone(), scope)),
        );
    }
    if let Some(keys) = keys {
        let focus = takes_focus(&keys, control.as_ref());
        store.set_key_handler(id, Box::new(handler(host, keys, control, scope)));
        if focus {
            store.set_focusable(id, true);
        }
    }
    store.set_draggable(id, drags(routes));
    store.set_long_pressable(id, holds(routes));
    if refreshes(routes) || store.is_refreshable(id) {
        store.set_refreshable(id, refreshes(routes));
    }
    store.tell_snaps(id, snaps(routes));
}

/// Whether a node whose key handler runs `keys` takes the focus for them as
/// it is re-attached: links take it as their text holds them, not as heard,
/// a text as it is selectable, and a refresh and a snap come without it.
fn takes_focus(keys: &[Route], control: Option<&Control>) -> bool {
    keys.iter().any(|r| {
        !matches!(
            r.event,
            EventRoute::Link | EventRoute::Refresh | EventRoute::SnapChanged
        )
    }) || control.is_some_and(|c| c.kind.responds())
}

/// Whether `routes` hear the item a snapping viewport settles on.
fn snaps(routes: &[Route]) -> bool {
    routes
        .iter()
        .any(|route| route.event == EventRoute::SnapChanged)
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
    let pick = |key: bool| {
        let picked: Vec<Route> = routes
            .iter()
            .copied()
            .filter(|route| {
                !route.event.is_notice()
                    && (route.event.is_control()
                        || control.is_some_and(|c| c.kind.reports(route.event))
                        || route.event.by_key_handler() == key)
            })
            .collect();
        (control.is_some() || !picked.is_empty()).then_some(picked)
    };
    (pick(true), pick(false))
}

/// The node handler that runs `control`'s response to the sample under
/// dispatch, then every route of `routes` the sample fires, in declaration
/// order: a standard route on the target and bubble legs of the walk, a
/// control's route when the control reports it.
///
/// A handler never runs inside another: the router takes a node's handler out
/// of the store before calling it and state writes are deferred to the flush, so
/// the host is never borrowed twice. The borrow is still checked, and a
/// re-entrant dispatch is skipped rather than panicking.
fn handler(
    host: &Rc<RefCell<ViewHost>>,
    routes: Vec<Route>,
    control: Option<Control>,
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
        for &route in &routes {
            let payload = match &change {
                // A control reports its change once, on its own node.
                Some((changed, value)) if *changed == route.event => value.clone(),
                _ if !runs_in(route, phase) => continue,
                _ => match crate::route::payload(route.event, cx) {
                    Some(payload) => payload,
                    None => continue,
                },
            };
            host.dispatch(route.handler, payload, &scope, cx);
        }
    }
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
