//! A running app's development state, typed, for the process that replaces
//! it after a Rust rebuild: a [`DevSnapshot`].
//!
//! The snapshot holds only what Viso understands and the new process can
//! check against its own build: per mounted view its state cells by durable
//! key, its behavior's states by slot as plain data, and the focus, scroll
//! offset, text and selection, and active child of each node that holds one,
//! by its node in the view; per game its [`GameSnapshot`] bytes; per window
//! the state cells its build allocated outside any view, by the key of
//! where they were allocated; and the bytes each extension the app
//! registered captured. Where the windows were
//! travels beside it, for the host to launch the next process with.
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
use crate::regions::ItemKey;

/// The version of the snapshot's wire form.
pub const SNAPSHOT_VERSION: u32 = 2;

const MAGIC: &[u8; 4] = b"VDS1";

/// The most views, nodes, states, games or extensions a snapshot
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
    pub cells: Vec<WindowCells>,
    pub extensions: Vec<Extension>,
}

/// The state cells one window's build allocated outside any view mount: a
/// pure-Rust `BuildCx::state` or a `component!` state.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WindowCells {
    /// The window, by the order the app opened its windows in.
    pub window: u32,
    /// Each cell in allocation order.
    pub cells: Vec<RustCell>,
}

/// One state cell allocated in Rust.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RustCell {
    /// Where it was allocated: its call site, or its component state.
    pub key: StateKey,
    /// Its ordinal among the window's cells of the same key.
    pub nth: u32,
    pub value: StateValue,
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
    let children = { store.arena().children(node).count() };
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
        enc.write_varint(self.cells.len() as u64);
        for window in &self.cells {
            enc.write_varint(u64::from(window.window));
            enc.write_varint(window.cells.len() as u64);
            for cell in &window.cells {
                enc.write_u64(cell.key.hi);
                enc.write_u64(cell.key.lo);
                enc.write_varint(u64::from(cell.nth));
                cell.value.encode(&mut enc);
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
        let cells = read_list(&mut dec, MAX_ENTRIES, "snapshot windows", |dec| {
            Ok(WindowCells {
                window: read_u32(dec)?,
                cells: read_list(dec, MAX_ENTRIES, "snapshot rust cells", |dec| {
                    Ok(RustCell {
                        key: StateKey::from_parts(dec.read_u64()?, dec.read_u64()?),
                        nth: read_u32(dec)?,
                        value: StateValue::decode(dec)?,
                    })
                })?,
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
            cells,
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
        value.encode(enc);
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
        Ok((key, StateValue::decode(dec)?))
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
    viso_dev_protocol::patch::write_node(enc, state.node);
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
    let node = viso_dev_protocol::patch::read_node(dec)?;
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

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use viso_behavior::native::{NativeObject, NativeValue, Obj};

    use super::*;

    /// A native object no snapshot may hold.
    struct Socket;

    impl NativeObject for Socket {
        const PATH: &'static str = "test::Socket";
    }

    fn snapshot() -> DevSnapshot {
        let mount = MountRef {
            path: "/p/src/view.vs".into(),
            window: 0,
            mount: 1,
        };
        DevSnapshot {
            source_build: BuildId(7),
            app_schema: SchemaFingerprint(8),
            revision: Revision(3),
            views: vec![ViewSnapshot {
                mount: mount.clone(),
                cells: vec![
                    (StateKey::from_parts(1, 2), StateValue::Int(5)),
                    (StateKey::from_parts(1, 3), StateValue::Bool(true)),
                ],
                states: vec![
                    Some(Value::Int(4)),
                    None,
                    Some(Value::Str(Rc::new("note".into()))),
                ],
                nodes: vec![NodeState {
                    node: NodeRef::Static(2),
                    path: None,
                    focus: true,
                    scroll: Some(Vec2 { x: 0.0, y: 50.0 }),
                    text: Some((
                        "draft".into(),
                        Selection::caret(TextPosition {
                            offset: TextOffset(3),
                            affinity: CaretAffinity::Downstream,
                        }),
                    )),
                    active_child: Some(1),
                }],
            }],
            games: vec![GameEntry {
                mount,
                snapshot: vec![1, 2, 3],
            }],
            cells: vec![WindowCells {
                window: 0,
                cells: vec![RustCell {
                    key: StateKey::from_parts(9, 9),
                    nth: 0,
                    value: StateValue::Float(1.5),
                }],
            }],
            extensions: vec![Extension {
                name: "route".into(),
                bytes: b"settings".to_vec(),
            }],
        }
    }

    /// Writes the sample snapshot as the seed of the `dev_snapshot` fuzz
    /// target's corpus.
    #[test]
    #[ignore = "writes the fuzz corpus's seeds"]
    fn fuzz_seeds() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/corpus/dev_snapshot");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("seed-000"), snapshot().encode()).unwrap();
    }

    #[test]
    fn a_snapshot_round_trips_and_names_its_build_and_compiler() {
        let snapshot = snapshot();
        let bytes = snapshot.encode();
        assert_eq!(&bytes[..4], b"VDS1");
        assert_eq!(DevSnapshot::decode(&bytes), Ok(snapshot));
    }

    #[test]
    fn another_magic_version_or_a_cut_is_refused() {
        let bytes = snapshot().encode();
        let mut magic = bytes.clone();
        magic[0] = b'X';
        assert!(DevSnapshot::decode(&magic).is_err());
        let mut version = bytes.clone();
        version[4] = SNAPSHOT_VERSION as u8 + 1;
        assert_eq!(
            DevSnapshot::decode(&version),
            Err(WireError::Bound {
                what: "snapshot version"
            })
        );
        for cut in 0..bytes.len() {
            assert!(DevSnapshot::decode(&bytes[..cut]).is_err(), "at {cut}");
        }
        let mut long = bytes;
        long.push(0);
        assert!(DevSnapshot::decode(&long).is_err(), "trailing bytes");
    }

    #[test]
    fn cells_out_of_key_order_are_refused() {
        let mut snapshot = snapshot();
        snapshot.views[0].cells.reverse();
        assert!(DevSnapshot::decode(&snapshot.encode()).is_err());
    }

    #[test]
    fn mutated_snapshots_never_panic_and_what_decodes_is_whole() {
        let bytes = snapshot().encode();
        let mut rng: u64 = 0xD1B5_4A32_D192_ED03;
        let mut next = |n: usize| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng % n.max(1) as u64) as usize
        };
        for _ in 0..20_000 {
            let mut mutated = bytes.clone();
            for _ in 0..1 + next(3) {
                if mutated.is_empty() {
                    break;
                }
                let at = next(mutated.len());
                match next(4) {
                    0 => mutated[at] ^= 1 << next(8),
                    1 => mutated[at] = [0x00, 0x7F, 0x80, 0xFF][next(4)],
                    2 => mutated.insert(at, next(256) as u8),
                    _ => mutated.truncate(at),
                }
            }
            if let Ok(decoded) = DevSnapshot::decode(&mutated) {
                let encoded = decoded.encode();
                let again = DevSnapshot::decode(&encoded).expect("its own encoding decodes");
                assert_eq!(again.encode(), encoded);
            }
        }
    }

    #[test]
    fn a_closure_or_native_handle_is_never_plain_data() {
        let handle = Obj::new(Socket).into_value();
        assert!(matches!(handle, Value::Handle(_)));
        assert!(!plain(&handle));
        assert!(!plain(&Value::List(Rc::new(vec![
            Value::Int(1),
            handle.clone()
        ]))));
        assert!(!plain(&Value::Agg(Rc::new(viso_behavior::Aggregate {
            tag: 0,
            fields: vec![Value::Nil, handle].into_boxed_slice(),
        }))));
        assert!(plain(&Value::List(Rc::new(vec![
            Value::Int(1),
            Value::Float(2.0),
            Value::Str(Rc::new("s".into())),
        ]))));
    }
}
