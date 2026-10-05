//! Installing a view's handlers on its nodes.

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::{BuildCx, DispatchPhase, EventCx, Handle, NodeId, NodeStore};

use crate::control::Control;
use crate::host::ViewHost;
use crate::route::EventRoute;
use crate::scope::Scope;

/// One handler a node declares: the event it runs on and its index in the
/// view's handler table.
pub type Route = (EventRoute, u32);

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
        cx.on_key(node, handler(host, keys, control, scope));
        cx.focusable(node, true);
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
    let (keys, pointers) = split(routes, control.as_ref());
    if let Some(pointers) = pointers {
        store.set_pointer_handler(
            id,
            Box::new(handler(host, pointers, control.clone(), scope)),
        );
    }
    if let Some(keys) = keys {
        store.set_key_handler(id, Box::new(handler(host, keys, control, scope)));
        store.set_focusable(id, true);
    }
}

/// The routes a key handler runs and the routes a pointer handler runs, `None`
/// for a kind of handler the node needs none of. A control drives both, and the
/// routes it reports run from either; a label responds to nothing.
fn split(routes: &[Route], control: Option<&Control>) -> (Option<Vec<Route>>, Option<Vec<Route>>) {
    let control = control.filter(|control| control.kind.responds());
    let pick = |key: bool| {
        let picked: Vec<Route> = routes
            .iter()
            .copied()
            .filter(|(route, _)| route.is_control() || route.is_key() == key)
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
        let change = control
            .as_ref()
            .and_then(|control| control.drive(&mut host, &scope, cx));
        let bubbles = cx.phase() != DispatchPhase::Capture;
        for &(route, index) in &routes {
            let payload = match &change {
                Some((changed, value)) if *changed == route => value.clone(),
                _ if !bubbles => continue,
                _ => match route.payload(cx) {
                    Some(payload) => payload,
                    None => continue,
                },
            };
            host.dispatch(index, payload, &scope, cx);
        }
    }
}
