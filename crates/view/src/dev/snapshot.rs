//! A running app's development state, typed, for the process that replaces
//! it after a Rust rebuild: a [`DevSnapshot`].
//!
//! The snapshot holds only what Viso understands and the new process can
//! check against its own build: per mounted view its state cells by durable
//! key, its behavior's states by slot as plain data, and the focus, scroll
//! offset, text and selection, and active child of each node that holds one,
//! by its node in the view; per game its [`GameSnapshot`] bytes; per window
//! its geometry; and the bytes each extension the app registered captured.
//! Never a pointer, handle, descriptor, socket, task or platform object: a
//! closure or native handle a state holds is not captured, and that state
//! restarts from its initializer.
//!
//! A view restores through the host's plan from the view the old process ran
//! to the one the new build embeds, the plan a patch between the two would
//! carry: the same compatibility rules, conversions and `@migrate` functions,
//! applied by [`restore`](super::commit::restore) with the snapshot as the
//! last-good. The wire form is ende binary, bounded on decode.
//!
//! [`GameSnapshot`]: viso_behavior::game::GameSnapshot

use viso_behavior::Value;
use viso_behavior::native::MigratableState;
use viso_ende::{Decode, Decoder, Encode, Encoder};
use viso_ui::state::StateKey;
use viso_ui::{
    CaretAffinity, NodeId, NodeStore, Selection, StateId, StateStore, StateValue, TextEdits,
    TextOffset, TextPosition, Vec2,
};

use super::commit::{Carried, CarriedText, active_child};
use super::wire::{
    BuildId, NodeRef, ReloadPlan, Revision, SchemaFingerprint, WireError, malformed, read_list,
    read_option, read_string, read_u32, write_string,
};
use crate::host::ViewHost;
use crate::package::{read_state_value, write_state_value};
use crate::regions::ItemKey;

/// The version of the snapshot's wire form.
pub const SNAPSHOT_VERSION: u32 = 1;

const MAGIC: &[u8; 4] = b"VDS1";

/// The most views, nodes, states, games, windows or extensions a snapshot
/// lists, and the longest path or name it holds.
const MAX_ENTRIES: usize = 1 << 20;
const MAX_NAME: usize = 4096;

/// The longest text a field's snapshot holds, and the most bytes an
/// extension or a game's snapshot does.
const MAX_TEXT: usize = 16 << 20;
const MAX_BLOB: usize = 256 << 20;

/// A running app's development state.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DevSnapshot {
    /// The build the snapshot was taken from.
    pub source_build: BuildId,
    /// The app schema that build compiled against.
    pub app_schema: SchemaFingerprint,
    /// The revision the app matched: the views it ran.
    pub revision: Revision,
    pub views: Vec<ViewSnapshot>,
    pub games: Vec<GameEntry>,
    /// Each window, in the order the app opened them.
    pub windows: Vec<WindowGeometry>,
    pub extensions: Vec<Extension>,
}

/// One mount of a `.vs` file, as the processes before and after a restart
/// both name it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct MountRef {
    /// The canonical path of the file.
    pub path: String,
    /// Its window, by the order the app opened its windows in.
    pub window: u32,
    /// Its order among the mounts of the file in that window.
    pub mount: u32,
}

/// The development state of one view mount.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ViewSnapshot {
    pub mount: MountRef,
    /// Each state cell's value by its durable key, ascending by key.
    pub cells: Vec<(StateKey, StateValue)>,
    /// Each behavior state's value by its slot, `None` for one holding a
    /// closure or a native handle.
    pub states: Vec<Option<Value>>,
    /// Each node holding focus, a scroll offset, text or an active child.
    pub nodes: Vec<NodeState>,
}

/// What one node of a view holds.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeState {
    pub node: NodeRef,
    /// For a node a region mounts, the keys of the `for` items it sits in.
    pub path: Option<Vec<ItemKey>>,
    pub focus: bool,
    /// Its nonzero scroll offset.
    pub scroll: Option<Vec2>,
    /// Its edit text and selection.
    pub text: Option<(String, Selection)>,
    /// The ordinal of its one shown child among several.
    pub active_child: Option<u32>,
}

/// The snapshot of the game a view mount runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameEntry {
    pub mount: MountRef,
    /// Its [`GameSnapshot`](viso_behavior::game::GameSnapshot), encoded.
    pub snapshot: Vec<u8>,
}

/// Where a window was and how large, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowGeometry {
    pub size: (f64, f64),
    /// Its outer top-left corner on the desktop, where the platform reports
    /// one.
    pub position: Option<(f64, f64)>,
}

/// The bytes an extension the app registered captured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extension {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// Whether `value` is plain data: no closure or native handle anywhere in
/// it.
fn plain(value: &Value) -> bool {
    match value {
        Value::Nil | Value::Int(_) | Value::Float(_) | Value::Str(_) => true,
        Value::List(items) => items.iter().all(plain),
        Value::Agg(agg) => agg.fields.iter().all(plain),
        Value::Closure(_) | Value::Handle(_) => false,
    }
}

/// The development state of the view mount `mount`: `nodes` its live static
/// nodes by static index, `cells` its cells by durable key, `host` the
/// behavior its nodes dispatch into.
pub fn capture_view(
    store: &NodeStore,
    states: &StateStore,
    text_edits: &TextEdits,
    host: Option<&ViewHost>,
    nodes: &[Option<NodeId>],
    cells: &[(StateKey, StateId)],
    mount: MountRef,
) -> ViewSnapshot {
    let mut snapshot = ViewSnapshot {
        mount,
        ..ViewSnapshot::default()
    };
    snapshot.cells = cells
        .iter()
        .filter_map(|&(key, id)| Some((key, states.get(id)?)))
        .collect();
    snapshot
        .cells
        .sort_unstable_by_key(|&(key, _)| (key.hi, key.lo));
    if let Some(host) = host {
        let mut slot = 0;
        while host.state(slot).is_some() {
            let value = host.current(slot, states).filter(plain);
            snapshot.states.push(value);
            slot += 1;
        }
    }
    for (index, node) in nodes.iter().enumerate() {
        if let Some(node) = *node
            && let Some(state) = node_state(store, text_edits, node, NodeRef::Static(index as u32))
        {
            snapshot.nodes.push(state);
        }
    }
    if let Some(host) = host {
        for region in host.region_nodes(store) {
            let at = NodeRef::Region {
                region: region.region,
                arm: region.arm,
                item: region.item,
            };
            if let Some(mut state) = node_state(store, text_edits, region.node, at) {
                state.path = Some(region.path);
                snapshot.nodes.push(state);
            }
        }
    }
    snapshot
}

/// What live node `node`, `at` in its view, holds, `None` when it holds
/// nothing a snapshot keeps.
fn node_state(
    store: &NodeStore,
    text_edits: &TextEdits,
    node: NodeId,
    at: NodeRef,
) -> Option<NodeState> {
    let scroll = store.scroll(node);
    let children = {
        let mut count = 0;
        let mut child = store.arena().links(node).and_then(|l| l.first_child);
        while let Some(id) = child {
            count += 1;
            child = store.arena().links(id).and_then(|l| l.next_sibling);
        }
        count
    };
    let state = NodeState {
        node: at,
        path: None,
        focus: store.focused() == Some(node),
        scroll: (scroll != Vec2::ZERO).then_some(scroll),
        text: text_edits
            .get(node)
            .map(|buffer| (buffer.text.clone(), buffer.sel)),
        active_child: (children > 1).then(|| active_child(store, node)).flatten(),
    };
    let holds = state.focus
        || state.scroll.is_some()
        || state.text.is_some()
        || state.active_child.is_some();
    holds.then_some(state)
}

impl ViewSnapshot {
    /// The value the cell `key` held.
    pub fn cell(&self, key: StateKey) -> Option<StateValue> {
        self.cells
            .binary_search_by_key(&(key.hi, key.lo), |&(k, _)| (k.hi, k.lo))
            .ok()
            .map(|at| self.cells[at].1)
    }

    /// What each node `plan` keeps carries to the node of the view it plans
    /// for, as far as the plan carries each kind: the static nodes' and the
    /// region nodes'.
    pub(super) fn carried(&self, plan: &ReloadPlan) -> (Vec<Carried>, Vec<Carried>) {
        let (mut statics, mut regional) = (Vec::new(), Vec::new());
        for state in &self.nodes {
            let Some(carry) = plan.nodes.iter().find(|carry| carry.from == state.node) else {
                continue;
            };
            let carries = |kind| carry.carries.contains(kind);
            let carried = Carried {
                to: carry.to,
                path: state.path.clone(),
                focus: carries(MigratableState::FOCUS) && state.focus,
                scroll: state.scroll.filter(|_| carries(MigratableState::SCROLL)),
                buffer: state
                    .text
                    .clone()
                    .filter(|_| carries(MigratableState::SELECTION))
                    .map(|(text, selection)| CarriedText::Text(text, selection)),
                moving: Vec::new(),
                active_child: state
                    .active_child
                    .filter(|_| carries(MigratableState::ACTIVE_CHILD)),
            };
            let holds = carried.focus
                || carried.scroll.is_some()
                || carried.buffer.is_some()
                || carried.active_child.is_some();
            if !holds {
                continue;
            }
            match state.node {
                NodeRef::Static(_) => statics.push(carried),
                NodeRef::Region { .. } => regional.push(carried),
            }
        }
        (statics, regional)
    }

    /// Whether a node held the focus, and how many held a scroll offset.
    pub(super) fn census(&self) -> (bool, u32) {
        let focused = self.nodes.iter().any(|node| node.focus);
        let scrolled = self
            .nodes
            .iter()
            .filter(|node| node.scroll.is_some())
            .count();
        (focused, scrolled as u32)
    }
}

impl DevSnapshot {
    /// The snapshot as bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        enc.write_raw(MAGIC);
        enc.write_varint(u64::from(SNAPSHOT_VERSION));
        self.source_build.encode(&mut enc);
        self.app_schema.encode(&mut enc);
        enc.write_varint(self.revision.0);
        enc.write_varint(self.views.len() as u64);
        for view in &self.views {
            write_view(&mut enc, view);
        }
        enc.write_varint(self.games.len() as u64);
        for game in &self.games {
            write_mount(&mut enc, &game.mount);
            enc.write_bytes(&game.snapshot);
        }
        enc.write_varint(self.windows.len() as u64);
        for window in &self.windows {
            enc.write_f64(window.size.0);
            enc.write_f64(window.size.1);
            match window.position {
                Some((x, y)) => {
                    enc.write_u8(1);
                    enc.write_f64(x);
                    enc.write_f64(y);
                }
                None => enc.write_u8(0),
            }
        }
        enc.write_varint(self.extensions.len() as u64);
        for extension in &self.extensions {
            write_string(&mut enc, &extension.name);
            enc.write_bytes(&extension.bytes);
        }
        enc.into_bytes()
    }

    /// The snapshot `bytes` encode.
    ///
    /// # Errors
    ///
    /// Bytes that are not a snapshot of this version, or past a bound.
    pub fn decode(bytes: &[u8]) -> Result<DevSnapshot, WireError> {
        let mut dec = Decoder::new(bytes);
        if dec.read_raw(MAGIC.len())? != MAGIC {
            return Err(malformed(&dec));
        }
        if read_u32(&mut dec)? != SNAPSHOT_VERSION {
            return Err(WireError::Bound {
                what: "snapshot version",
            });
        }
        let source_build = BuildId::decode(&mut dec)?;
        let app_schema = SchemaFingerprint::decode(&mut dec)?;
        let revision = Revision(dec.read_varint()?);
        let views = read_list(&mut dec, MAX_ENTRIES, "snapshot views", read_view)?;
        let games = read_list(&mut dec, MAX_ENTRIES, "snapshot games", |dec| {
            Ok(GameEntry {
                mount: read_mount(dec)?,
                snapshot: read_blob(dec, "game snapshot")?,
            })
        })?;
        let windows = read_list(&mut dec, MAX_ENTRIES, "snapshot windows", |dec| {
            Ok(WindowGeometry {
                size: (dec.read_f64()?, dec.read_f64()?),
                position: read_option(dec, |dec| Ok((dec.read_f64()?, dec.read_f64()?)))?,
            })
        })?;
        let extensions = read_list(&mut dec, MAX_ENTRIES, "snapshot extensions", |dec| {
            Ok(Extension {
                name: read_string(dec, MAX_NAME, "extension name")?,
                bytes: read_blob(dec, "extension")?,
            })
        })?;
        dec.finish()?;
        Ok(DevSnapshot {
            source_build,
            app_schema,
            revision,
            views,
            games,
            windows,
            extensions,
        })
    }
}

fn read_blob(dec: &mut Decoder<'_>, what: &'static str) -> Result<Vec<u8>, WireError> {
    let len = dec.read_varint()?;
    if len > MAX_BLOB as u64 {
        return Err(WireError::Bound { what });
    }
    Ok(dec.read_raw(len as usize)?.to_vec())
}

fn write_mount(enc: &mut Encoder, mount: &MountRef) {
    write_string(enc, &mount.path);
    enc.write_varint(u64::from(mount.window));
    enc.write_varint(u64::from(mount.mount));
}

fn read_mount(dec: &mut Decoder<'_>) -> Result<MountRef, WireError> {
    Ok(MountRef {
        path: read_string(dec, MAX_NAME, "mount path")?,
        window: read_u32(dec)?,
        mount: read_u32(dec)?,
    })
}

fn write_view(enc: &mut Encoder, view: &ViewSnapshot) {
    write_mount(enc, &view.mount);
    enc.write_varint(view.cells.len() as u64);
    for &(key, value) in &view.cells {
        enc.write_u64(key.hi);
        enc.write_u64(key.lo);
        write_state_value(enc, value);
    }
    enc.write_varint(view.states.len() as u64);
    for state in &view.states {
        match state {
            Some(value) => {
                enc.write_u8(1);
                enc.write_bytes(&value.encode());
            }
            None => enc.write_u8(0),
        }
    }
    enc.write_varint(view.nodes.len() as u64);
    for node in &view.nodes {
        write_node_state(enc, node);
    }
}

fn read_view(dec: &mut Decoder<'_>) -> Result<ViewSnapshot, WireError> {
    let mount = read_mount(dec)?;
    let cells = read_list(dec, MAX_ENTRIES, "snapshot cells", |dec| {
        let key = StateKey::from_parts(dec.read_u64()?, dec.read_u64()?);
        Ok((key, read_state_value(dec)?))
    })?;
    if !cells.is_sorted_by_key(|&(key, _)| (key.hi, key.lo)) {
        return Err(malformed(dec));
    }
    let states = read_list(dec, MAX_ENTRIES, "snapshot states", |dec| {
        read_option(dec, |dec| {
            let bytes = read_blob(dec, "state value")?;
            Value::decode(&bytes).map_err(|_| malformed(dec))
        })
    })?;
    let nodes = read_list(dec, MAX_ENTRIES, "snapshot nodes", read_node_state)?;
    Ok(ViewSnapshot {
        mount,
        cells,
        states,
        nodes,
    })
}

fn write_position(enc: &mut Encoder, position: TextPosition) {
    enc.write_varint(position.offset.0 as u64);
    enc.write_bool(position.affinity == CaretAffinity::Downstream);
}

fn read_position(dec: &mut Decoder<'_>) -> Result<TextPosition, WireError> {
    let offset = usize::try_from(dec.read_varint()?).map_err(|_| malformed(dec))?;
    let affinity = if dec.read_bool()? {
        CaretAffinity::Downstream
    } else {
        CaretAffinity::Upstream
    };
    Ok(TextPosition {
        offset: TextOffset(offset),
        affinity,
    })
}

fn write_node_state(enc: &mut Encoder, state: &NodeState) {
    super::patch::write_node(enc, state.node);
    match &state.path {
        Some(path) => {
            enc.write_u8(1);
            enc.write_varint(path.len() as u64);
            for key in path {
                key.write(enc);
            }
        }
        None => enc.write_u8(0),
    }
    enc.write_bool(state.focus);
    match state.scroll {
        Some(offset) => {
            enc.write_u8(1);
            enc.write_f32(offset.x);
            enc.write_f32(offset.y);
        }
        None => enc.write_u8(0),
    }
    match &state.text {
        Some((text, selection)) => {
            enc.write_u8(1);
            write_string(enc, text);
            write_position(enc, selection.anchor);
            write_position(enc, selection.focus);
        }
        None => enc.write_u8(0),
    }
    match state.active_child {
        Some(index) => {
            enc.write_u8(1);
            enc.write_varint(u64::from(index));
        }
        None => enc.write_u8(0),
    }
}

fn read_node_state(dec: &mut Decoder<'_>) -> Result<NodeState, WireError> {
    let node = super::patch::read_node(dec)?;
    let path = read_option(dec, |dec| {
        read_list(dec, MAX_ENTRIES, "item keys", |dec| Ok(ItemKey::read(dec)?))
    })?;
    let focus = dec.read_bool()?;
    let scroll = read_option(dec, |dec| Ok(Vec2::new(dec.read_f32()?, dec.read_f32()?)))?;
    let text = read_option(dec, |dec| {
        let text = read_string(dec, MAX_TEXT, "field text")?;
        let anchor = read_position(dec)?;
        let focus = read_position(dec)?;
        Ok((text, Selection { anchor, focus }))
    })?;
    let active_child = read_option(dec, read_u32)?;
    Ok(NodeState {
        node,
        path,
        focus,
        scroll,
        text,
        active_child,
    })
}
