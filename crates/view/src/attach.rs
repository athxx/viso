//! Installing a view's handlers on its nodes.

use std::cell::RefCell;
use std::rc::Rc;

use viso_ende::{DecodeError, Decoder, Encoder};
use viso_ui::{BuildCx, DispatchPhase, EventCx, Handle, NodeId, NodeStore};

use crate::control::Control;
use crate::host::ViewHost;
use crate::route::EventRoute;
use crate::scope::Scope;

/// One handler a node declares: the event it runs on, its index in the view's
/// handler table, and whether it is an `on capture` handler.
///
/// A capture handler runs as the sample walks down to its target — on the
/// node's ancestors' capture leg, and on the target itself; any other handler
/// runs on the target and as the sample bubbles back up (§52).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    /// The event it runs on.
    pub event: EventRoute,
    /// Its index in the component's handler table.
    pub handler: u32,
    /// Whether it runs on the capture leg instead of the bubble leg.
    pub capture: bool,
}

/// The tag bit marking a capture route; event discriminants stay below it.
const CAPTURE: u8 = 0x80;

impl Route {
    /// Writes the route: its event's discriminant with the high bit set for a
    /// capture handler, then its handler index. A route written before
    /// capture handlers existed reads back as a bubble one.
    pub(crate) fn encode(self, enc: &mut Encoder) {
        enc.write_u8(self.event as u8 | if self.capture { CAPTURE } else { 0 });
        enc.write_varint(u64::from(self.handler));
    }

    /// Reads a route [`Route::encode`] wrote.
    pub(crate) fn decode(dec: &mut Decoder<'_>) -> Result<Route, DecodeError> {
        let offset = dec.position();
        let tag = dec.read_u8()?;
        let event = EventRoute::from_u8(tag & !CAPTURE).ok_or(DecodeError::Malformed { offset })?;
        let offset = dec.position();
        let handler =
            u32::try_from(dec.read_varint()?).map_err(|_| DecodeError::Malformed { offset })?;
        Ok(Route {
            event,
            handler,
            capture: tag & CAPTURE != 0,
        })
    }

    /// Whether it runs in dispatch phase `phase`.
    pub fn runs_in(self, phase: DispatchPhase) -> bool {
        match phase {
            DispatchPhase::Capture => self.capture,
            DispatchPhase::Target => true,
            DispatchPhase::Bubble => !self.capture,
        }
    }
}

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
            .filter(|route| route.event.is_control() || route.event.is_key() == key)
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
        let phase = cx.phase();
        for &route in &routes {
            let payload = match &change {
                // A control reports its change once, on its own node.
                Some((changed, value)) if *changed == route.event => value.clone(),
                _ if !route.runs_in(phase) => continue,
                _ => match route.event.payload(cx) {
                    Some(payload) => payload,
                    None => continue,
                },
            };
            host.dispatch(route.handler, payload, &scope, cx);
        }
    }
}
