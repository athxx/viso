//! The release form of a view with behavior: its UI package, its behavior
//! module, and the tables that join them, loaded with no compiler present.

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use viso_ende::{Decode, DecodeError, Decoder, Encode, Encoder, ProtocolTag};
use viso_ui::aot::{AotPackage, instantiate_indexed};
use viso_ui::state::StateKey;
use viso_ui::{BindingTable, NodeId, NodeStore, StateStore, StateValue, VirtualLists};

use crate::attach::{Route, attach_node};
use crate::host::{HostError, ViewHost};
use crate::route::EventRoute;

/// A compiled view with behavior, as a release build embeds it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ViewPackage {
    /// The retained tree and its binding edges.
    pub ui: AotPackage,
    /// The encoded behavior module, empty for a view without handlers.
    pub behavior: Vec<u8>,
    /// The component of [`behavior`](Self::behavior) the view mounts.
    pub component: String,
    /// The view's state cells, allocated with their initial values before the
    /// tree mounts.
    pub states: Vec<ViewState>,
    /// The node handlers, grouped by node in ascending node order.
    pub handlers: Vec<ViewHandler>,
}

/// A component state held in a UI cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewState {
    /// The cell's durable identity, the key the UI package's edges name.
    pub key: StateKey,
    /// The component's state slot the cell mirrors, `None` for a cell no
    /// handler writes.
    pub slot: Option<u32>,
    /// The cell's initial value.
    pub initial: StateValue,
}

/// One node handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewHandler {
    /// The pre-order index of the node in [`ViewPackage::ui`].
    pub node: u32,
    /// The event it runs on.
    pub route: EventRoute,
    /// Its index in the component's handler table.
    pub handler: u32,
}

/// A loaded view: its root and the host its handlers dispatch into.
#[derive(Debug)]
pub struct LoadedView {
    /// The root node, `None` for an empty tree.
    pub root: Option<NodeId>,
    /// The component instance, `None` for a view without handlers.
    pub host: Option<Rc<RefCell<ViewHost>>>,
}

/// Why a [`ViewPackage`] did not load.
#[derive(Debug)]
pub enum ViewLoadError {
    /// The blob is not a well-formed package.
    Decode(DecodeError),
    /// The behavior module does not mount.
    Host(HostError),
}

impl fmt::Display for ViewLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ViewLoadError::Decode(error) => write!(f, "the view package does not decode: {error}"),
            ViewLoadError::Host(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ViewLoadError {}

/// Decodes `blob` and instantiates the view into the runtime.
pub fn load_view(
    blob: &[u8],
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &mut BindingTable,
    lists: &mut VirtualLists,
) -> Result<LoadedView, ViewLoadError> {
    let package = ViewPackage::decode_from_slice(blob).map_err(ViewLoadError::Decode)?;
    instantiate_view(&package, store, states, bindings, lists)
}

/// Instantiates a decoded view into the runtime: its state cells first, with
/// their initial values, then the tree, then the host and its handlers.
pub fn instantiate_view(
    package: &ViewPackage,
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &mut BindingTable,
    lists: &mut VirtualLists,
) -> Result<LoadedView, ViewLoadError> {
    let mut host = if package.behavior.is_empty() {
        None
    } else {
        let host = ViewHost::from_bytes(&package.behavior, &package.component)
            .map_err(ViewLoadError::Host)?;
        Some(host)
    };
    for state in &package.states {
        let id = match states.id_for_key(state.key) {
            Some(id) => id,
            None => {
                let id = states.alloc(state.initial);
                states.bind_key(id, state.key);
                id
            }
        };
        if let (Some(host), Some(slot)) = (&mut host, state.slot) {
            host.mirror(slot as usize, id);
        }
    }
    let mut node_ids = Vec::new();
    let root = instantiate_indexed(&package.ui, store, states, bindings, lists, &mut node_ids);
    let Some(host) = host.map(|host| Rc::new(RefCell::new(host))) else {
        return Ok(LoadedView { root, host: None });
    };
    let mut routes: Vec<Route> = Vec::new();
    for group in package.handlers.chunk_by(|a, b| a.node == b.node) {
        let Some(Some(id)) = node_ids.get(group[0].node as usize).copied() else {
            continue;
        };
        routes.clear();
        routes.extend(group.iter().map(|h| (h.route, h.handler)));
        attach_node(store, &host, id, &routes, &[]);
    }
    Ok(LoadedView {
        root,
        host: Some(host),
    })
}

/// A pre-allocation hint that never trusts a length prefix past a sane ceiling.
fn bounded_capacity(count: u64) -> usize {
    const MAX_PREALLOC: u64 = 4096;
    count.min(MAX_PREALLOC) as usize
}

/// Reads a varint that must fit a `u32`.
fn read_u32(dec: &mut Decoder<'_>) -> Result<u32, DecodeError> {
    let offset = dec.position();
    u32::try_from(dec.read_varint()?).map_err(|_| DecodeError::Malformed { offset })
}

impl Encode for ViewPackage {
    fn encode(&self, enc: &mut Encoder) {
        ProtocolTag::current().encode(enc);
        self.ui.encode(enc);
        enc.write_bytes(&self.behavior);
        enc.write_str(&self.component);
        enc.write_varint(self.states.len() as u64);
        for state in &self.states {
            enc.write_u64(state.key.hi);
            enc.write_u64(state.key.lo);
            enc.write_varint(state.slot.map_or(0, |slot| u64::from(slot) + 1));
            match state.initial {
                StateValue::Int(n) => {
                    enc.write_u8(0);
                    enc.write_i32(n);
                }
                StateValue::Float(x) => {
                    enc.write_u8(1);
                    enc.write_f32(x);
                }
                StateValue::Bool(b) => {
                    enc.write_u8(2);
                    enc.write_bool(b);
                }
                StateValue::Color(r, g, b, a) => {
                    enc.write_u8(3);
                    for channel in [r, g, b, a] {
                        enc.write_f32(channel);
                    }
                }
            }
        }
        enc.write_varint(self.handlers.len() as u64);
        for handler in &self.handlers {
            enc.write_varint(u64::from(handler.node));
            enc.write_u8(handler.route as u8);
            enc.write_varint(u64::from(handler.handler));
        }
    }
}

impl Decode for ViewPackage {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let tag_offset = dec.position();
        if !ProtocolTag::decode(dec)?.is_compatible() {
            return Err(DecodeError::Malformed { offset: tag_offset });
        }
        let ui = AotPackage::decode(dec)?;
        let behavior = dec.read_bytes()?.to_vec();
        let component = dec.read_str()?.to_owned();
        let count = dec.read_varint()?;
        let mut states = Vec::with_capacity(bounded_capacity(count));
        for _ in 0..count {
            let hi = dec.read_u64()?;
            let lo = dec.read_u64()?;
            let slot = read_u32(dec)?.checked_sub(1);
            let offset = dec.position();
            let initial = match dec.read_u8()? {
                0 => StateValue::Int(dec.read_i32()?),
                1 => StateValue::Float(dec.read_f32()?),
                2 => StateValue::Bool(dec.read_bool()?),
                3 => StateValue::Color(
                    dec.read_f32()?,
                    dec.read_f32()?,
                    dec.read_f32()?,
                    dec.read_f32()?,
                ),
                _ => return Err(DecodeError::Malformed { offset }),
            };
            states.push(ViewState {
                key: StateKey::from_parts(hi, lo),
                slot,
                initial,
            });
        }
        let count = dec.read_varint()?;
        let mut handlers = Vec::with_capacity(bounded_capacity(count));
        for _ in 0..count {
            let node = read_u32(dec)?;
            let offset = dec.position();
            let route =
                EventRoute::from_u8(dec.read_u8()?).ok_or(DecodeError::Malformed { offset })?;
            let handler = read_u32(dec)?;
            handlers.push(ViewHandler {
                node,
                route,
                handler,
            });
        }
        Ok(ViewPackage {
            ui,
            behavior,
            component,
            states,
            handlers,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_package_round_trips_and_rejects_a_bad_route() {
        let package = ViewPackage {
            ui: AotPackage::default(),
            behavior: vec![1, 2, 3],
            component: "Counter".into(),
            states: vec![ViewState {
                key: StateKey::from_parts(7, 9),
                slot: Some(0),
                initial: StateValue::Int(3),
            }],
            handlers: vec![ViewHandler {
                node: 1,
                route: EventRoute::Click,
                handler: 0,
            }],
        };
        let bytes = package.encode_to_vec();
        assert_eq!(ViewPackage::decode_from_slice(&bytes), Ok(package));
        let mut bad = bytes.clone();
        let route = bytes.len() - 2;
        bad[route] = 99;
        assert!(ViewPackage::decode_from_slice(&bad).is_err());
    }
}
