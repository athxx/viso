//! The component instance a view's handlers run against.

use std::cell::RefCell;
use std::fmt;
use std::rc::{Rc, Weak};

use viso_behavior::native::{Natives, SchemaConflict, Services};
use viso_behavior::{
    Budget, ChunkKind, ComponentEffect, Event, Fault, FaultKind, Instance, LoadError, Module,
    Value, Vm,
};
use viso_ui::adaptive::{AdaptiveEnv, AnchorId, EnvField};
use viso_ui::{EventCx, NodeId, NodeStore, StateId, StateStore, StateValue, StructureHookId};

use crate::env::env_value;
use crate::regions::{LocalTemplate, Mounted, RegionNode};
use crate::scope::{Locals, Scope};

/// Where a view's state cells are read and written: the [`StateStore`] outside a
/// dispatch, the [`EventCx`] during one (whose writes are deferred to the flush).
pub trait StateCells {
    /// The current value of cell `id`, `None` for a stale id.
    fn get(&self, id: StateId) -> Option<StateValue>;
    /// Writes cell `id`, returning whether the id was live.
    fn set(&mut self, id: StateId, value: StateValue) -> bool;
    /// The adaptive environment the view's `env` reads see.
    fn env(&self) -> &AdaptiveEnv;
}

impl StateCells for StateStore {
    fn get(&self, id: StateId) -> Option<StateValue> {
        StateStore::get(self, id)
    }

    fn set(&mut self, id: StateId, value: StateValue) -> bool {
        StateStore::set(self, id, value)
    }

    fn env(&self) -> &AdaptiveEnv {
        StateStore::env(self)
    }
}

impl StateCells for EventCx<'_> {
    fn get(&self, id: StateId) -> Option<StateValue> {
        EventCx::get(self, id)
    }

    fn set(&mut self, id: StateId, value: StateValue) -> bool {
        EventCx::set(self, id, value)
    }

    fn env(&self) -> &AdaptiveEnv {
        EventCx::env(self)
    }
}

/// Why a [`ViewHost`] could not be created.
#[derive(Debug)]
pub enum HostError {
    /// The module bytes did not decode or verify.
    Load(LoadError),
    /// The module has no component of this name.
    NoComponent(String),
    /// A native import did not link against the standard registry.
    Link(SchemaConflict),
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HostError::Load(error) => write!(f, "the behavior module does not load: {error}"),
            HostError::NoComponent(name) => {
                write!(f, "the behavior module has no component `{name}`")
            }
            HostError::Link(conflict) => write!(f, "the behavior module does not link: {conflict}"),
        }
    }
}

impl std::error::Error for HostError {}

/// How a state slot reaches its UI cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Link {
    /// The cell holds the state's value.
    Mirror(StateId),
    /// The cell holds a revision of a state only the instance holds.
    Track(StateId),
    /// The cell holds the revision of the `env` field the slot reads; see
    /// [`EnvLink`].
    Env(StateId),
}

/// A state slot holding an `env` field, filled from the environment before a
/// call whenever the field's revision cell has moved since the last fill.
#[derive(Debug, Clone, Copy)]
struct EnvLink {
    slot: u32,
    field: EnvField,
    /// Where an anchored field resolves.
    anchor: Option<AnchorId>,
    cell: StateId,
    /// The revision the slot was last filled at.
    seen: Option<StateValue>,
}

/// One mounted component's behavior: its instance on the VM, the UI state cell
/// each of its states is mirrored into, and its view's handler table.
///
/// The UI state store is the authoritative copy of every mirrored state. Before
/// a handler runs, the host copies each mirrored cell into the instance; after
/// it commits, the host writes back exactly the slots the handler wrote. A state
/// the store cannot hold (a string, a list) lives only in the instance and is
/// *tracked* instead: its UI cell holds a revision the host raises after each
/// committed write, so the bindings and regions that read it still see the
/// change.
///
/// The states of an instance a control-flow region mounts have no cell of the
/// view: each mount of the region content keeps its own values, which the host
/// loads into the instance before every call in that content's [`Scope`] and
/// stores back after a committed write.
///
/// A slot holding an `env` field is filled from the environment, its VM value
/// rebuilt only when the field's revision cell moves.
///
/// A handler fault rolls its transaction back, is kept as
/// [`last_fault`](Self::last_fault), and never unwinds into the event router.
pub struct ViewHost {
    vm: Vm,
    instance: Instance,
    /// The instance's creation fault, if any; every dispatch then reports it
    /// instead of running against a partial instance.
    broken: Option<Fault>,
    /// The handler chunks, by handler index.
    handlers: Box<[u32]>,
    /// The UI cell linked to each state slot.
    mirror: Vec<Option<Link>>,
    /// Reused call arguments: the payload, then the region bindings.
    args: Vec<Value>,
    /// Reused list of slots a dispatch wrote.
    written: Vec<usize>,
    fault: Option<Fault>,
    events: Vec<Event>,
    /// The cells the view's mounted regions allocated: each region mount's
    /// pulse cell, and the region content mounts keeping instance states.
    regions: RegionCells,
    /// The structure hook re-delivering the values the view's static nodes
    /// show, replaced when they mount again.
    values: Option<StructureHookId>,
    /// The slots holding `env` fields.
    env: Vec<EnvLink>,
    /// The environment anchor of each static component root that reads an
    /// anchored field.
    anchors: Vec<(NodeId, AnchorId)>,
    /// The nodes the view's effects are mounted on.
    effect_owners: Vec<NodeId>,
    /// The reload generation.
    epoch: u64,
}

/// The UI cells a view's regions allocate while it runs, released with the
/// view.
#[derive(Debug, Default)]
struct RegionCells {
    /// Whether regions are mounted under the view's nodes.
    mounted: bool,
    /// The structure hook of each region mount, removed with the view.
    hooks: Vec<StructureHookId>,
    /// The content each hook keeps mounted, read by [`ViewHost::region_nodes`].
    mounts: Vec<Weak<RefCell<Mounted>>>,
    pulses: Vec<StateId>,
    locals: Vec<Weak<Locals>>,
    /// The length at which [`locals`](Self::locals) next drops its dead
    /// entries.
    prune: usize,
}

impl ViewHost {
    /// A host for component `component` of `module`, linked against the
    /// standard natives with the capabilities the module's package is granted.
    ///
    /// A fault while running the state initializers does not fail creation: the
    /// host keeps it and reports it from every dispatch.
    pub fn new(module: Rc<Module>, component: &str) -> Result<ViewHost, HostError> {
        let grant: Vec<&str> = module.capabilities().iter().map(|c| &**c).collect();
        ViewHost::with_capabilities(Rc::clone(&module), component, &grant)
    }

    /// A host for component `component` of `module` granted exactly
    /// `capabilities`, whatever the package asks for: a preview of generated
    /// code runs with [`PREVIEW_CAPABILITIES`](viso_behavior::native::PREVIEW_CAPABILITIES)
    /// plus what its user grants. A handler calling a native outside the grant
    /// faults with `E6103` and rolls back; the host keeps running.
    pub fn with_capabilities(
        module: Rc<Module>,
        component: &str,
        capabilities: &[&str],
    ) -> Result<ViewHost, HostError> {
        let index = module
            .component(component)
            .ok_or_else(|| HostError::NoComponent(component.to_owned()))?;
        let layout = module.layout(index);
        let handlers = layout.handlers.clone();
        let states = layout.states.len();
        let mut vm = Vm::new(Rc::clone(&module), Budget::default());
        vm.link(&Natives::standard(), capabilities)
            .map_err(HostError::Link)?;
        let (instance, broken) = match vm.instantiate(index, []) {
            Ok(instance) => (instance, None),
            Err(fault) => (Instance::detached(), Some(fault)),
        };
        Ok(ViewHost {
            vm,
            instance,
            broken,
            handlers,
            mirror: vec![None; states],
            args: Vec::new(),
            written: Vec::new(),
            fault: None,
            events: Vec::new(),
            regions: RegionCells::default(),
            values: None,
            env: Vec::new(),
            anchors: Vec::new(),
            effect_owners: Vec::new(),
            epoch: 0,
        })
    }

    /// A host for component `component` of the module encoded in `bytes`.
    pub fn from_bytes(bytes: &[u8], component: &str) -> Result<ViewHost, HostError> {
        let module = Module::decode(bytes).map_err(HostError::Load)?;
        ViewHost::new(Rc::new(module), component)
    }

    /// The module the host runs.
    pub fn module(&self) -> &Rc<Module> {
        self.vm.module()
    }

    /// The capabilities the host's natives are linked with.
    pub fn capabilities(&self) -> &[Box<str>] {
        self.vm.grant()
    }

    /// The host services its natives use, such as the clipboard.
    pub fn services_mut(&mut self) -> &mut Services {
        self.vm.services_mut()
    }

    /// The component's state slot named `name`.
    pub fn state_slot(&self, name: &str) -> Option<usize> {
        let index = self.instance.component()?;
        self.module().layout(index).state(name)
    }

    /// The instance's current value of state slot `slot`.
    pub fn state(&self, slot: usize) -> Option<&Value> {
        self.instance.states().get(slot)
    }

    /// The authoritative value of state slot `slot`: its mirrored cell's in
    /// `cells`, or the instance's for a slot no cell mirrors.
    pub fn current(&self, slot: usize, cells: &dyn StateCells) -> Option<Value> {
        match self.mirror.get(slot)? {
            Some(Link::Mirror(id)) => cells.get(*id).and_then(vm_value),
            Some(Link::Env(_)) => {
                let link = self.env.iter().find(|link| link.slot as usize == slot)?;
                Some(env_value(link.field, cells.env(), link.anchor))
            }
            _ => self.instance.states().get(slot).cloned(),
        }
    }

    /// Sets the instance's state slot `slot`, as a hot reload carries a value
    /// into a new instance. Returns `false` for a slot out of range.
    pub fn set_state(&mut self, slot: usize, value: Value) -> bool {
        if slot >= self.instance.states().len() {
            return false;
        }
        self.instance.set_state(slot, value);
        self.instance.clear_dirty();
        true
    }

    /// Runs chunk `chunk`, a record field default or a `fn`, with `args`
    /// against the instance, and returns its value. Nothing it does is kept:
    /// it writes no state and its events are dropped.
    pub fn run(&mut self, chunk: u32, args: &[Value]) -> Result<Value, Fault> {
        if let Some(fault) = &self.broken {
            return Err(fault.clone());
        }
        let callable = self
            .module()
            .chunks()
            .get(chunk as usize)
            .is_some_and(|c| matches!(c.kind, ChunkKind::FieldDefault | ChunkKind::Fn));
        if !callable {
            return Err(Fault {
                kind: FaultKind::Internal,
                at: None,
                message: format!("chunk {chunk} is not a function"),
            });
        }
        let result = self.vm.call(&mut self.instance, chunk, args);
        self.instance.clear_dirty();
        result.map(|outcome| outcome.value)
    }

    /// Mirrors state slot `slot` into UI cell `id`. Returns `false`, mirroring
    /// nothing, for a slot out of range.
    pub fn mirror(&mut self, slot: usize, id: StateId) -> bool {
        self.link(slot, Link::Mirror(id))
    }

    /// Tracks state slot `slot`, a value no UI cell holds, through the integer
    /// revision cell `id`: the host raises it after every committed write of the
    /// slot. Returns `false`, tracking nothing, for a slot out of range.
    pub fn track(&mut self, slot: usize, id: StateId) -> bool {
        self.link(slot, Link::Track(id))
    }

    /// Fills state slot `slot` with `env` field `field`, resolved for an
    /// anchored field at `anchor`, the root of the component instance that
    /// reads it. Returns `false`, linking nothing, for a slot out of range or
    /// an anchored field without an anchor.
    pub fn link_env(
        &mut self,
        slot: usize,
        field: EnvField,
        anchor: Option<NodeId>,
        states: &mut StateStore,
    ) -> bool {
        if slot >= self.mirror.len() {
            return false;
        }
        let (cell, anchor) = if field.anchored() {
            let Some(node) = anchor else {
                return false;
            };
            let anchor = match self.anchors.iter().find(|(n, _)| *n == node) {
                Some(&(_, anchor)) => anchor,
                None => {
                    let anchor = states.anchor_env(node, None);
                    self.anchors.push((node, anchor));
                    anchor
                }
            };
            (states.anchor_cell(anchor, field), Some(anchor))
        } else {
            (states.env_cell(field), None)
        };
        let Some(cell) = cell else {
            return false;
        };
        self.env.retain(|link| link.slot as usize != slot);
        self.env.push(EnvLink {
            slot: slot as u32,
            field,
            anchor,
            cell,
            seen: None,
        });
        self.link(slot, Link::Env(cell))
    }

    /// Releases each environment anchor no `env` slot resolves at any more,
    /// as relinking a reloaded view leaves the anchors of its old nodes.
    pub fn prune_env(&mut self, states: &mut StateStore) {
        let env = &self.env;
        self.anchors.retain(|&(_, anchor)| {
            let used = env.iter().any(|link| link.anchor == Some(anchor));
            if !used {
                states.release_anchor(anchor);
            }
            used
        });
    }

    /// The revision cell of each linked `env` slot, by ascending slot.
    pub(crate) fn env_cells(&self) -> Vec<(u32, StateId)> {
        let mut cells: Vec<(u32, StateId)> =
            self.env.iter().map(|link| (link.slot, link.cell)).collect();
        cells.sort_unstable_by_key(|&(slot, _)| slot);
        cells
    }

    /// Unlinks every `env` slot and releases the view's environment anchors,
    /// as unmounting the view does.
    pub fn release_env(&mut self, states: &mut StateStore) {
        for link in self.env.drain(..) {
            if let Some(Some(Link::Env(_))) = self.mirror.get(link.slot as usize) {
                self.mirror[link.slot as usize] = None;
            }
        }
        for (_, anchor) in self.anchors.drain(..) {
            states.release_anchor(anchor);
        }
    }

    fn link(&mut self, slot: usize, link: Link) -> bool {
        match self.mirror.get_mut(slot) {
            Some(cell) => {
                *cell = Some(link);
                true
            }
            None => false,
        }
    }

    /// Copies each mirrored cell, each `env` field whose revision moved, and
    /// each state `scope` keeps, into the instance.
    fn sync(&mut self, scope: &Scope, cells: &dyn StateCells) {
        for (slot, link) in self.mirror.iter().enumerate() {
            let Some(Link::Mirror(id)) = *link else {
                continue;
            };
            if let Some(value) = cells.get(id).and_then(vm_value) {
                self.instance.set_state(slot, value);
            }
        }
        for link in &mut self.env {
            let revision = cells.get(link.cell);
            if revision.is_some() && revision == link.seen {
                continue;
            }
            link.seen = revision;
            if (link.slot as usize) < self.instance.states().len() {
                let value = env_value(link.field, cells.env(), link.anchor);
                self.instance.set_state(link.slot as usize, value);
            }
        }
        for locals in &scope.locals {
            locals.refresh(cells);
            let values = locals.values.borrow();
            for (&slot, value) in locals.slots.iter().zip(values.iter()) {
                if (slot as usize) < self.instance.states().len() {
                    self.instance.set_state(slot as usize, value.clone());
                }
            }
        }
    }

    /// The chunk of handler-table entry `entry`, which must be of `kind`, or
    /// the fault a call of it reports.
    fn chunk(&self, entry: u32, kind: ChunkKind) -> Result<u32, Fault> {
        if let Some(fault) = &self.broken {
            return Err(fault.clone());
        }
        self.handlers
            .get(entry as usize)
            .copied()
            .filter(|&chunk| {
                self.module()
                    .chunks()
                    .get(chunk as usize)
                    .is_some_and(|c| c.kind == kind)
            })
            .ok_or_else(|| Fault {
                kind: FaultKind::Internal,
                at: None,
                message: format!("the view has no {kind:?} entry {entry}"),
            })
    }

    /// Appends to `out` the cells whose change can change the value of pure
    /// entry `entry` in `scope`: the cell of each state it reads, following its
    /// calls, or every cell of the view and of `scope` when it calls a closure
    /// value, whose reads are not known.
    pub(crate) fn entry_cells(&self, entry: u32, scope: &Scope, out: &mut Vec<StateId>) {
        let Ok(chunk) = self.chunk(entry, ChunkKind::RegionEntry) else {
            return;
        };
        let reads = self.vm.reads(chunk);
        let linked = |slot: usize| match self.mirror.get(slot) {
            Some(Some(Link::Mirror(id) | Link::Track(id) | Link::Env(id))) => Some(*id),
            _ => None,
        };
        if reads.opaque {
            out.extend((0..self.mirror.len()).filter_map(linked));
            out.extend(scope.locals.iter().flat_map(|locals| locals.cells.iter()));
            return;
        }
        for slot in reads.states {
            out.extend(scope.cell(slot).or_else(|| linked(slot as usize)));
        }
    }

    /// Evaluates the pure entry `entry` of the handler table (a region's
    /// selector, iterable or key) with the bindings of `scope`, then `extra`,
    /// against the current states, and returns its value. Nothing it does is
    /// kept: it writes no state and its events are dropped.
    pub fn evaluate(
        &mut self,
        entry: u32,
        scope: &Scope,
        extra: Option<&Value>,
        cells: &dyn StateCells,
    ) -> Result<Value, Fault> {
        let chunk = self.chunk(entry, ChunkKind::RegionEntry)?;
        self.sync(scope, cells);
        self.args.clear();
        self.args.extend_from_slice(&scope.values);
        self.args.extend(extra.cloned());
        let result = self.vm.call(&mut self.instance, chunk, &self.args);
        self.args.clear();
        self.instance.clear_dirty();
        result.map(|outcome| outcome.value)
    }

    /// The initial values of the instance states `locals` a mount of region
    /// content keeps, each initializer run with the bindings of `scope` after
    /// the ones before it. A state without an initializer, and every state
    /// after a faulting one, starts `Nil`; the fault is kept as
    /// [`last_fault`](Self::last_fault).
    pub(crate) fn initialize(
        &mut self,
        locals: &[LocalTemplate],
        scope: &Scope,
        cells: &dyn StateCells,
    ) -> Box<[Value]> {
        self.sync(scope, cells);
        let mut values = vec![Value::Nil; locals.len()];
        for (value, local) in values.iter_mut().zip(locals) {
            let Some(entry) = local.init else {
                continue;
            };
            let result = self.chunk(entry, ChunkKind::StateInit).and_then(|chunk| {
                self.vm
                    .call(&mut self.instance, chunk, &scope.values)
                    .map(|outcome| outcome.value)
            });
            match result {
                Ok(initial) => {
                    if (local.slot as usize) < self.instance.states().len() {
                        self.instance
                            .set_state(local.slot as usize, initial.clone());
                    }
                    *value = initial;
                }
                Err(fault) => {
                    self.fault = Some(fault);
                    break;
                }
            }
        }
        self.instance.clear_dirty();
        values.into_boxed_slice()
    }

    /// Keeps `locals`, a mount of region content, so the view's release frees
    /// its cells if the mount is still alive then.
    pub(crate) fn adopt(&mut self, locals: &Rc<Locals>) {
        let regions = &mut self.regions;
        if regions.locals.len() >= regions.prune {
            regions.locals.retain(|weak| weak.strong_count() > 0);
            regions.prune = (regions.locals.len() * 2).max(16);
        }
        regions.locals.push(Rc::downgrade(locals));
    }

    /// Records that regions mount under the view's nodes, patched by `hook`.
    pub(crate) fn mark_regions(&mut self, hook: StructureHookId, mounted: &Rc<RefCell<Mounted>>) {
        self.regions.mounted = true;
        self.regions.hooks.push(hook);
        self.regions.mounts.push(Rc::downgrade(mounted));
    }

    /// Whether regions are mounted under the view's nodes, which only a
    /// rebuild of its tree unmounts.
    pub fn has_regions(&self) -> bool {
        self.regions.mounted
    }

    /// Drops the structure hook re-delivering the static nodes' values, as
    /// unmounting the view without clearing its tree does.
    pub fn release_values(&mut self, store: &mut NodeStore) {
        if let Some(hook) = self.values.take() {
            store.remove_structure_hook(hook);
        }
    }

    /// Swaps in `hook` as the one re-delivering the static nodes' values and
    /// returns the one it replaces.
    pub(crate) fn replace_values_hook(
        &mut self,
        hook: Option<StructureHookId>,
    ) -> Option<StructureHookId> {
        std::mem::replace(&mut self.values, hook)
    }

    /// Keeps a region mount's pulse cell, freed with the view.
    pub(crate) fn adopt_pulse(&mut self, pulse: StateId) {
        self.regions.pulses.push(pulse);
    }

    /// Every node the view's regions mount and show, with the arm item it was
    /// built from and the keys of the `for` items around it, so a reload can
    /// pair each with the node that replaces it.
    pub fn region_nodes(&self, store: &NodeStore) -> Vec<RegionNode> {
        let mut out = Vec::new();
        for mounted in self.regions.mounts.iter().filter_map(Weak::upgrade) {
            mounted.borrow().census(store, &mut out);
        }
        out
    }

    /// Removes the view's region hooks and frees the cells its regions
    /// allocated, as unmounting or rebuilding the view's tree does.
    pub fn release_regions(&mut self, store: &mut NodeStore, states: &mut StateStore) {
        for hook in self.regions.hooks.drain(..) {
            store.remove_structure_hook(hook);
        }
        self.regions.mounts.clear();
        for locals in self.regions.locals.drain(..) {
            if let Some(locals) = locals.upgrade() {
                locals.release(states);
            }
        }
        for pulse in self.regions.pulses.drain(..) {
            states.free(pulse);
        }
        self.regions.prune = 0;
        self.regions.mounted = false;
    }

    /// The number of handlers the view declares.
    pub fn handler_count(&self) -> usize {
        self.handlers.len()
    }

    /// Runs handler `handler` with `payload` in `scope`, as one transaction
    /// over the component's states.
    ///
    /// Returns whether it committed. A fault is kept as
    /// [`last_fault`](Self::last_fault); the transaction's writes and events are
    /// then discarded.
    pub fn dispatch(
        &mut self,
        handler: u32,
        payload: Value,
        scope: &Scope,
        cells: &mut dyn StateCells,
    ) -> bool {
        let chunk = match self.chunk(handler, ChunkKind::Handler) {
            Ok(chunk) => chunk,
            Err(fault) => {
                self.fault = Some(fault);
                return false;
            }
        };
        self.sync(scope, &*cells);
        self.args.clear();
        self.args.push(payload);
        self.args.extend_from_slice(&scope.values);
        let result = self.vm.call(&mut self.instance, chunk, &self.args);
        self.args.clear();
        match result {
            Ok(outcome) => {
                self.events.extend(outcome.events);
                self.write_back(scope, cells);
                true
            }
            Err(fault) => {
                self.fault = Some(fault);
                false
            }
        }
    }

    /// Writes each state slot the committed call wrote to the cell that holds
    /// it: a mirrored cell takes the value, a tracked one a new revision, and a
    /// slot `scope` keeps its mount's value.
    fn write_back(&mut self, scope: &Scope, cells: &mut dyn StateCells) {
        self.written.clear();
        self.written.extend(self.instance.dirty());
        self.instance.clear_dirty();
        for &slot in &self.written {
            match self.mirror[slot] {
                Some(Link::Mirror(id)) => {
                    let Some(witness) = cells.get(id) else {
                        continue;
                    };
                    if let Some(value) = cell_value(&self.instance.states()[slot], witness) {
                        cells.set(id, value);
                    }
                }
                Some(Link::Track(id)) => {
                    if let Some(StateValue::Int(revision)) = cells.get(id) {
                        cells.set(id, StateValue::Int(revision.wrapping_add(1)));
                    }
                }
                Some(Link::Env(_)) => {}
                None => {
                    let value = &self.instance.states()[slot];
                    for locals in scope.locals.iter().rev() {
                        if locals.store(slot as u32, value.clone(), cells) {
                            break;
                        }
                    }
                }
            }
        }
    }

    /// The effects that mount with the view, in source order.
    pub fn effects(&self) -> &[ComponentEffect] {
        match self.instance.component() {
            Some(index) => &self.module().layout(index).effects,
            None => &[],
        }
    }

    /// The reload generation: a [`reload`](Self::reload) raises it, so an
    /// effect mounted before it neither runs nor cleans up against the new
    /// module.
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Records that effects are mounted on `node`, which releasing them
    /// cancels.
    pub(crate) fn own_effects(&mut self, node: NodeId) {
        if !self.effect_owners.contains(&node) {
            self.effect_owners.push(node);
        }
    }

    /// Takes the nodes effects are mounted on.
    pub(crate) fn take_effect_owners(&mut self) -> Vec<NodeId> {
        std::mem::take(&mut self.effect_owners)
    }

    /// Runs the effect body at handler-table entry `entry` in `scope` as one
    /// transaction and returns its value, the cleanup closure or `Nil`. A
    /// fault is kept as [`last_fault`](Self::last_fault) and discards the
    /// transaction.
    pub(crate) fn run_effect(
        &mut self,
        entry: u32,
        scope: &Scope,
        cells: &mut dyn StateCells,
    ) -> Option<Value> {
        let chunk = match self.chunk(entry, ChunkKind::Effect) {
            Ok(chunk) => chunk,
            Err(fault) => {
                self.fault = Some(fault);
                return None;
            }
        };
        self.sync(scope, &*cells);
        match self.vm.call(&mut self.instance, chunk, &scope.values) {
            Ok(outcome) => {
                self.events.extend(outcome.events);
                self.write_back(scope, cells);
                Some(outcome.value)
            }
            Err(fault) => {
                self.fault = Some(fault);
                None
            }
        }
    }

    /// Runs an effect's cleanup closure with the states `scope` keeps. It
    /// writes no state; a fault is kept as [`last_fault`](Self::last_fault).
    pub(crate) fn run_cleanup(&mut self, cleanup: &Value, scope: &Scope) {
        for locals in &scope.locals {
            let values = locals.values.borrow();
            for (&slot, value) in locals.slots.iter().zip(values.iter()) {
                if (slot as usize) < self.instance.states().len() {
                    self.instance.set_state(slot as usize, value.clone());
                }
            }
        }
        let result = self.vm.call_value(&mut self.instance, cleanup, &[]);
        self.instance.clear_dirty();
        match result {
            Ok(outcome) => self.events.extend(outcome.events),
            Err(fault) => self.fault = Some(fault),
        }
    }

    /// Keeps `fault`, raised outside a dispatch (by a region's entry, or a value
    /// a region cannot mount), as [`last_fault`](Self::last_fault).
    pub fn record_fault(&mut self, fault: Fault) {
        self.fault = Some(fault);
    }

    /// The most recent dispatch fault, if any.
    pub fn last_fault(&self) -> Option<&Fault> {
        self.fault.as_ref()
    }

    /// Takes the most recent dispatch fault.
    pub fn take_fault(&mut self) -> Option<Fault> {
        self.fault.take()
    }

    /// Drains the component events the handlers emitted, in emit order.
    pub fn drain_events(&mut self) -> std::vec::Drain<'_, Event> {
        self.events.drain(..)
    }

    /// Replaces the instance with `next`, a host of the recompiled module into
    /// which the caller has [set](Self::set_state) each state it carries, as a
    /// hot reload does. Its mirrors start cleared: the caller mirrors the new
    /// layout's slots, and links its `env` slots, again. The undrained events,
    /// the environment anchors, and the cells and hooks the view's mounts
    /// registered, stay this host's, for the caller to reuse, release or
    /// replace; [`prune_env`](Self::prune_env) releases the anchors relinking
    /// left unused. The installed services move to `next`.
    pub fn reload(&mut self, mut next: ViewHost) {
        *next.vm.services_mut() = std::mem::take(self.vm.services_mut());
        next.events = std::mem::take(&mut self.events);
        next.regions = std::mem::take(&mut self.regions);
        next.values = self.values.take();
        next.anchors = std::mem::take(&mut self.anchors);
        next.epoch = self.epoch + 1;
        *self = next;
    }
}

impl fmt::Debug for ViewHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ViewHost")
            .field("states", &self.instance.states())
            .field("handlers", &self.handlers)
            .field("mirror", &self.mirror)
            .field("fault", &self.fault)
            .finish_non_exhaustive()
    }
}

/// A mirrored UI value as the VM represents it: `Bool` is an integer, `F32` a
/// float. A color has no VM form.
pub fn vm_value(value: StateValue) -> Option<Value> {
    match value {
        StateValue::Int(n) => Some(Value::Int(i64::from(n))),
        StateValue::Float(x) => Some(Value::Float(f64::from(x))),
        StateValue::Bool(b) => Some(Value::bool(b)),
        StateValue::Color(..) => None,
    }
}

/// A VM value as the UI cell whose current value `witness` fixes its kind.
/// `None` when the value does not fit the cell.
pub fn cell_value(value: &Value, witness: StateValue) -> Option<StateValue> {
    match witness {
        StateValue::Int(_) => Some(StateValue::Int(i32::try_from(value.as_int()?).ok()?)),
        StateValue::Float(_) => Some(StateValue::Float(value.as_float()? as f32)),
        StateValue::Bool(_) => Some(StateValue::Bool(value.as_int()? != 0)),
        StateValue::Color(..) => None,
    }
}

/// The host for an embedded, build-time-verified module, sharing one decoded
/// module per thread across every mount of the same view.
///
/// # Panics
///
/// If the module does not load or has no such component. The macro that
/// embeds it decodes, verifies and links it at build time, so this does not
/// happen for bytes it produced.
#[doc(hidden)]
pub fn __embedded(bytes: &'static [u8], component: &str) -> Rc<RefCell<ViewHost>> {
    thread_local! {
        static MODULES: RefCell<Vec<(usize, Rc<Module>)>> = const { RefCell::new(Vec::new()) };
    }
    let key = bytes.as_ptr().addr();
    let module = MODULES.with(|modules| {
        let mut modules = modules.borrow_mut();
        if let Some((_, module)) = modules.iter().find(|(k, _)| *k == key) {
            return Rc::clone(module);
        }
        let module =
            Rc::new(Module::decode(bytes).unwrap_or_else(|error| {
                panic!("an embedded behavior module does not load: {error}")
            }));
        modules.push((key, Rc::clone(&module)));
        module
    });
    match ViewHost::new(module, component) {
        Ok(host) => Rc::new(RefCell::new(host)),
        Err(error) => panic!("an embedded view does not mount: {error}"),
    }
}
