//! The runtime's half of an inspection: each window's tree as it stands,
//! with where each node a view mounts comes from in that view.

use std::collections::HashMap;

use viso_ende::{Decode, DecodeError, Decoder, Encode, Encoder};
use viso_ui::{InspectKind, NodeId, NodeStore, Rect, Role, SemanticState};

use crate::ViewHost;

/// Every window of an app, in the order it opened them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Census {
    pub windows: Vec<WindowCensus>,
}

impl Census {
    /// Its wire form.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }

    /// The census `bytes` spell.
    ///
    /// # Errors
    ///
    /// Bytes that are not a census.
    pub fn from_bytes(bytes: &[u8]) -> Result<Census, DecodeError> {
        Census::decode_from_slice(bytes)
    }
}

/// One window's tree: its nodes in pre-order, the root first, and the view
/// mounts they come from.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WindowCensus {
    pub mounts: Vec<CensusMount>,
    pub nodes: Vec<CensusNode>,
}

/// A view mounted in the window: its `.vs` file, as the build names it, and
/// the hash of the source the build compiled it from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CensusMount {
    pub file: String,
    pub source_hash: u64,
}

/// A node as the window shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct CensusNode {
    /// Its identity in the window's store.
    pub index: u32,
    pub generation: u32,
    /// Its parent, by index into [`WindowCensus::nodes`].
    pub parent: Option<u32>,
    pub kind: InspectKind,
    /// Its box in window coordinates, scrolling included.
    pub bounds: Rect,
    /// Whether neither it nor an ancestor is hidden.
    pub shown: bool,
    pub focusable: bool,
    pub focused: bool,
    /// What it tells assistive technology, `None` for a node outside the
    /// semantics tree.
    pub semantics: Option<NodeSemantics>,
    /// Where in a mounted view it comes from, `None` for a node no view
    /// mounts.
    pub origin: Option<NodeOrigin>,
}

/// A node's semantics.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeSemantics {
    pub role: Role,
    pub label: Option<String>,
    pub hint: Option<String>,
    pub value: Option<String>,
    pub heading_level: Option<u8>,
    pub state: SemanticState,
}

/// Where a node comes from in the view of mount `mount`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeOrigin {
    /// The static node `index`, in the view's static pre-order.
    Static { mount: u32, index: u32 },
    /// The node item `item` of arm `arm` of region `region` mounted.
    Region {
        mount: u32,
        region: u32,
        arm: u32,
        item: u32,
    },
}

/// A view mounted in the window the census is taken of.
pub struct CensusView<'a> {
    pub file: &'a str,
    pub source_hash: u64,
    /// The live node of each static node, by static index.
    pub nodes: &'a [Option<NodeId>],
    /// Its behavior host, which knows the nodes its regions mount.
    pub host: Option<&'a ViewHost>,
}

/// The census of the tree under `root` in `store`, whose nodes `views`
/// mount.
pub fn census(store: &NodeStore, root: Option<NodeId>, views: &[CensusView<'_>]) -> WindowCensus {
    let mut out = WindowCensus {
        mounts: views
            .iter()
            .map(|view| CensusMount {
                file: view.file.to_owned(),
                source_hash: view.source_hash,
            })
            .collect(),
        nodes: Vec::new(),
    };
    let Some(root) = root else {
        return out;
    };
    let mut origins: HashMap<NodeId, NodeOrigin> = HashMap::new();
    for (mount, view) in views.iter().enumerate() {
        let mount = mount as u32;
        for (index, node) in view.nodes.iter().enumerate() {
            if let Some(node) = node {
                origins.entry(*node).or_insert(NodeOrigin::Static {
                    mount,
                    index: index as u32,
                });
            }
        }
        for node in view
            .host
            .map(|host| host.region_nodes(store))
            .unwrap_or_default()
        {
            origins.entry(node.node).or_insert(NodeOrigin::Region {
                mount,
                region: node.region,
                arm: node.arm,
                item: node.item,
            });
        }
    }
    let semantics: HashMap<NodeId, NodeSemantics> = store
        .derive_semantics(root)
        .nodes
        .into_iter()
        .map(|node| {
            (
                node.id,
                NodeSemantics {
                    role: node.role,
                    label: node.label,
                    hint: node.hint,
                    value: node.value,
                    heading_level: node.heading_level,
                    state: node.state.unwrap_or_default(),
                },
            )
        })
        .collect();
    let tree = store.inspect_tree(root);
    let mut at: HashMap<NodeId, u32> = HashMap::with_capacity(tree.nodes.len());
    out.nodes.reserve(tree.nodes.len());
    for node in &tree.nodes {
        let parent = node.parent.and_then(|parent| at.get(&parent).copied());
        let shown =
            !node.flags.hidden && parent.is_none_or(|parent| out.nodes[parent as usize].shown);
        at.insert(node.id, out.nodes.len() as u32);
        out.nodes.push(CensusNode {
            index: node.id.index(),
            generation: node.id.generation(),
            parent,
            kind: node.kind,
            bounds: node.world,
            shown,
            focusable: node.flags.focusable,
            focused: node.flags.focused,
            semantics: semantics.get(&node.id).cloned(),
            origin: origins.get(&node.id).copied(),
        });
    }
    out
}

/// The static nodes of a view without regions mounted at `root`, by static
/// index: the live tree walked in pre-order with `shape`, the number of
/// static children of each static node in pre-order. A view's static
/// children are exactly a container's live children; a node that authors no
/// children (a leaf, a `VirtualList` mounting its own items) has none.
pub fn static_nodes(store: &NodeStore, root: NodeId, shape: &[u32]) -> Vec<Option<NodeId>> {
    let mut out = vec![None; shape.len()];
    let mut cursor = 0;
    while cursor < shape.len() {
        let live = (cursor == 0).then_some(root);
        walk_static(store, shape, live, &mut cursor, &mut out);
    }
    out
}

fn walk_static(
    store: &NodeStore,
    shape: &[u32],
    live: Option<NodeId>,
    cursor: &mut usize,
    out: &mut [Option<NodeId>],
) {
    let index = *cursor;
    *cursor += 1;
    out[index] = live;
    let links = |id: NodeId| store.arena().links(id);
    let mut child = live.and_then(|id| links(id)?.first_child);
    for _ in 0..shape[index] {
        if *cursor >= shape.len() {
            return;
        }
        let at = child;
        child = at.and_then(|id| links(id)?.next_sibling);
        walk_static(store, shape, at, cursor, out);
    }
}

const KINDS: [InspectKind; 6] = [
    InspectKind::Container,
    InspectKind::Text,
    InspectKind::Image,
    InspectKind::Path,
    InspectKind::Shader,
    InspectKind::Asset,
];

pub(super) fn malformed(dec: &Decoder<'_>) -> DecodeError {
    DecodeError::Malformed {
        offset: dec.position(),
    }
}

/// A count of items each at least a byte long, bounded by what is left.
pub(super) fn count(dec: &mut Decoder<'_>) -> Result<usize, DecodeError> {
    let n = dec.read_varint()?;
    if n > dec.remaining() as u64 {
        return Err(malformed(dec));
    }
    Ok(n as usize)
}

pub(super) fn write_option_str(enc: &mut Encoder, value: Option<&str>) {
    match value {
        Some(value) => {
            enc.write_u8(1);
            enc.write_str(value);
        }
        None => enc.write_u8(0),
    }
}

pub(super) fn read_option_str(dec: &mut Decoder<'_>) -> Result<Option<String>, DecodeError> {
    Ok(match dec.read_u8()? {
        0 => None,
        1 => Some(dec.read_str()?.to_owned()),
        _ => return Err(malformed(dec)),
    })
}

fn write_option_bool(enc: &mut Encoder, value: Option<bool>) {
    enc.write_u8(match value {
        None => 0,
        Some(false) => 1,
        Some(true) => 2,
    });
}

fn read_option_bool(dec: &mut Decoder<'_>) -> Result<Option<bool>, DecodeError> {
    Ok(match dec.read_u8()? {
        0 => None,
        1 => Some(false),
        2 => Some(true),
        _ => return Err(malformed(dec)),
    })
}

pub(super) fn write_strings(enc: &mut Encoder, values: &[String]) {
    enc.write_varint(values.len() as u64);
    for value in values {
        enc.write_str(value);
    }
}

pub(super) fn read_strings(dec: &mut Decoder<'_>) -> Result<Vec<String>, DecodeError> {
    (0..count(dec)?)
        .map(|_| dec.read_str().map(str::to_owned))
        .collect()
}

impl Encode for Census {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_varint(self.windows.len() as u64);
        for window in &self.windows {
            window.encode(enc);
        }
    }
}

impl Decode for Census {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let windows = (0..count(dec)?)
            .map(|_| WindowCensus::decode(dec))
            .collect::<Result<_, _>>()?;
        Ok(Census { windows })
    }
}

impl Encode for WindowCensus {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_varint(self.mounts.len() as u64);
        for mount in &self.mounts {
            enc.write_str(&mount.file);
            enc.write_u64(mount.source_hash);
        }
        enc.write_varint(self.nodes.len() as u64);
        for node in &self.nodes {
            enc.write_varint(u64::from(node.index));
            enc.write_varint(u64::from(node.generation));
            // The parent precedes the node: 0 for none, else its index + 1.
            enc.write_varint(node.parent.map_or(0, |parent| u64::from(parent) + 1));
            enc.write_u8(KINDS.iter().position(|k| *k == node.kind).unwrap_or(0) as u8);
            for v in [node.bounds.x, node.bounds.y, node.bounds.w, node.bounds.h] {
                enc.write_f32(v);
            }
            enc.write_u8(
                u8::from(node.shown) | u8::from(node.focusable) << 1 | u8::from(node.focused) << 2,
            );
            match &node.semantics {
                None => enc.write_u8(0),
                Some(semantics) => {
                    enc.write_u8(1);
                    let role = Role::ALL.iter().position(|r| *r == semantics.role);
                    enc.write_u8(role.unwrap_or(0) as u8);
                    write_option_str(enc, semantics.label.as_deref());
                    write_option_str(enc, semantics.hint.as_deref());
                    write_option_str(enc, semantics.value.as_deref());
                    enc.write_u8(semantics.heading_level.unwrap_or(0));
                    let state = &semantics.state;
                    write_option_bool(enc, state.checked);
                    write_option_bool(enc, state.expanded);
                    write_option_bool(enc, state.selected);
                    match state.value {
                        Some(value) => {
                            enc.write_u8(1);
                            enc.write_f32(value);
                        }
                        None => enc.write_u8(0),
                    }
                    match state.range {
                        Some((min, max)) => {
                            enc.write_u8(1);
                            enc.write_f32(min);
                            enc.write_f32(max);
                        }
                        None => enc.write_u8(0),
                    }
                }
            }
            match node.origin {
                None => enc.write_u8(0),
                Some(NodeOrigin::Static { mount, index }) => {
                    enc.write_u8(1);
                    enc.write_varint(u64::from(mount));
                    enc.write_varint(u64::from(index));
                }
                Some(NodeOrigin::Region {
                    mount,
                    region,
                    arm,
                    item,
                }) => {
                    enc.write_u8(2);
                    for v in [mount, region, arm, item] {
                        enc.write_varint(u64::from(v));
                    }
                }
            }
        }
    }
}

impl Decode for WindowCensus {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let u32_of =
            |dec: &mut Decoder<'_>| u32::try_from(dec.read_varint()?).map_err(|_| malformed(dec));
        let mounts = (0..count(dec)?)
            .map(|_| {
                Ok(CensusMount {
                    file: dec.read_str()?.to_owned(),
                    source_hash: dec.read_u64()?,
                })
            })
            .collect::<Result<Vec<_>, DecodeError>>()?;
        let len = count(dec)?;
        let mut nodes: Vec<CensusNode> = Vec::with_capacity(len);
        for at in 0..len {
            let index = u32_of(dec)?;
            let generation = u32_of(dec)?;
            let parent = match dec.read_varint()? {
                0 => None,
                p if p <= at as u64 => Some(p as u32 - 1),
                _ => return Err(malformed(dec)),
            };
            let kind = *KINDS
                .get(usize::from(dec.read_u8()?))
                .ok_or_else(|| malformed(dec))?;
            let bounds = Rect {
                x: dec.read_f32()?,
                y: dec.read_f32()?,
                w: dec.read_f32()?,
                h: dec.read_f32()?,
            };
            let flags = dec.read_u8()?;
            if flags > 0b111 {
                return Err(malformed(dec));
            }
            let semantics = match dec.read_u8()? {
                0 => None,
                1 => {
                    let role = *Role::ALL
                        .get(usize::from(dec.read_u8()?))
                        .ok_or_else(|| malformed(dec))?;
                    let label = read_option_str(dec)?;
                    let hint = read_option_str(dec)?;
                    let value = read_option_str(dec)?;
                    let heading_level = Some(dec.read_u8()?).filter(|&level| level > 0);
                    let checked = read_option_bool(dec)?;
                    let expanded = read_option_bool(dec)?;
                    let selected = read_option_bool(dec)?;
                    let number = match dec.read_u8()? {
                        0 => None,
                        1 => Some(dec.read_f32()?),
                        _ => return Err(malformed(dec)),
                    };
                    let range = match dec.read_u8()? {
                        0 => None,
                        1 => Some((dec.read_f32()?, dec.read_f32()?)),
                        _ => return Err(malformed(dec)),
                    };
                    Some(NodeSemantics {
                        role,
                        label,
                        hint,
                        value,
                        heading_level,
                        state: SemanticState {
                            checked,
                            value: number,
                            range,
                            expanded,
                            selected,
                        },
                    })
                }
                _ => return Err(malformed(dec)),
            };
            let origin = match dec.read_u8()? {
                0 => None,
                1 => Some(NodeOrigin::Static {
                    mount: u32_of(dec)?,
                    index: u32_of(dec)?,
                }),
                2 => Some(NodeOrigin::Region {
                    mount: u32_of(dec)?,
                    region: u32_of(dec)?,
                    arm: u32_of(dec)?,
                    item: u32_of(dec)?,
                }),
                _ => return Err(malformed(dec)),
            };
            if let Some(NodeOrigin::Static { mount, .. } | NodeOrigin::Region { mount, .. }) =
                origin
                && mount as usize >= mounts.len()
            {
                return Err(malformed(dec));
            }
            nodes.push(CensusNode {
                index,
                generation,
                parent,
                kind,
                bounds,
                shown: flags & 1 != 0,
                focusable: flags & 2 != 0,
                focused: flags & 4 != 0,
                semantics,
                origin,
            });
        }
        Ok(WindowCensus { mounts, nodes })
    }
}
