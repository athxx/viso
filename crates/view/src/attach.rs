//! Installing a view's handlers on its nodes.

use std::cell::RefCell;
use std::rc::Rc;

use viso_behavior::Value;
use viso_ui::{BuildCx, EventCx, Handle, NodeId, NodeStore};

use crate::host::ViewHost;
use crate::route::EventRoute;

/// One handler a node declares: the event it runs on and its index in the
/// view's handler table.
pub type Route = (EventRoute, u32);

/// Installs `routes` on the node `node` names, dispatching into `host` with the
/// enclosing regions' bindings `scope`, and returns the handle so authoring
/// chains inline.
pub fn attach(
    cx: &mut BuildCx<'_>,
    host: &Rc<RefCell<ViewHost>>,
    node: Handle,
    routes: &[Route],
    scope: &[Value],
) -> Handle {
    let (keys, pointers) = split(routes);
    if !pointers.is_empty() {
        cx.on_pointer(node, handler(host, pointers, scope));
    }
    if !keys.is_empty() {
        cx.on_key(node, handler(host, keys, scope));
        cx.focusable(node, true);
    }
    node
}

/// Installs `routes` on node `id`, replacing its prior handlers: one pointer
/// handler for the pointer routes, one key handler (and focusability) for the
/// key routes. A node with no route of a kind keeps no handler of that kind.
pub fn attach_node(
    store: &mut NodeStore,
    host: &Rc<RefCell<ViewHost>>,
    id: NodeId,
    routes: &[Route],
    scope: &[Value],
) {
    store.clear_event_handlers(id);
    let (keys, pointers) = split(routes);
    if !pointers.is_empty() {
        store.set_pointer_handler(id, Box::new(handler(host, pointers, scope)));
    }
    if !keys.is_empty() {
        store.set_key_handler(id, Box::new(handler(host, keys, scope)));
        store.set_focusable(id, true);
    }
}

/// `routes` split into its key routes and its pointer routes.
fn split(routes: &[Route]) -> (Vec<Route>, Vec<Route>) {
    routes
        .iter()
        .copied()
        .partition(|(route, _)| route.is_key())
}

/// The node handler that runs every route of `routes` the sample under dispatch
/// fires, in declaration order.
///
/// A handler never runs inside another: the router takes a node's handler out
/// of the store before calling it and state writes are deferred to the flush, so
/// the host is never borrowed twice. The borrow is still checked, and a
/// re-entrant dispatch is skipped rather than panicking.
fn handler(
    host: &Rc<RefCell<ViewHost>>,
    routes: Vec<Route>,
    scope: &[Value],
) -> impl FnMut(&mut EventCx<'_>) + 'static {
    let host = Rc::clone(host);
    let scope: Box<[Value]> = scope.into();
    move |cx: &mut EventCx<'_>| {
        for &(route, index) in &routes {
            let Some(payload) = route.payload(cx) else {
                continue;
            };
            let Ok(mut host) = host.try_borrow_mut() else {
                return;
            };
            host.dispatch(index, payload, &scope, cx);
        }
    }
}
