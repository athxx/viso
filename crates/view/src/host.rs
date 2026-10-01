//! The component instance a view's handlers run against.

use std::cell::RefCell;
use std::fmt;
use std::rc::{Rc, Weak};

use viso_behavior::native::{Natives, SchemaConflict};
use viso_behavior::{
    Budget, ChunkKind, Event, Fault, FaultKind, Instance, LoadError, Module, Value, Vm,
};
use viso_ui::{EventCx, NodeStore, StateId, StateStore, StateValue, StructureHookId};

use crate::regions::LocalTemplate;
use crate::scope::{Locals, Scope};

/// Where a view's state cells are read and written: the [`StateStore`] outside a
/// dispatch, the [`EventCx`] during one (whose writes are deferred to the flush).
pub trait StateCells {
    /// The current value of cell `id`, `None` for a stale id.
    fn get(&self, id: StateId) -> Option<StateValue>;
    /// Writes cell `id`, returning whether the id was live.
    fn set(&mut self, id: StateId, value: StateValue) -> bool;
}

impl StateCells for StateStore {
    fn get(&self, id: StateId) -> Option<StateValue> {
        StateStore::get(self, id)
    }

    fn set(&mut self, id: StateId, value: StateValue) -> bool {
        StateStore::set(self, id, value)
    }
}

impl StateCells for EventCx<'_> {
    fn get(&self, id: StateId) -> Option<StateValue> {
        EventCx::get(self, id)
    }

    fn set(&mut self, id: StateId, value: StateValue) -> bool {
        EventCx::set(self, id, value)
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
}

/// The UI cells a view's regions allocate while it runs, released with the
/// view.
#[derive(Debug, Default)]
struct RegionCells {
    /// Whether regions are mounted under the view's nodes.
    mounted: bool,
    /// The structure hook of each region mount, removed with the view.
    hooks: Vec<StructureHookId>,
    pulses: Vec<StateId>,
    locals: Vec<Weak<Locals>>,
    /// The length at which [`locals`](Self::locals) next drops its dead
    /// entries.
    prune: usize,
}

impl ViewHost {
    /// A host for component `component` of `module`, linked against the
    /// standard natives with no capability granted.
    ///
    /// A fault while running the state initializers does not fail creation: the
    /// host keeps it and reports it from every dispatch.
    pub fn new(module: Rc<Module>, component: &str) -> Result<ViewHost, HostError> {
        let index = module
            .component(component)
            .ok_or_else(|| HostError::NoComponent(component.to_owned()))?;
        let layout = module.layout(index);
        let handlers = layout.handlers.clone();
        let states = layout.states.len();
        let mut vm = Vm::new(Rc::clone(&module), Budget::default());
        vm.link(&Natives::standard(), &[])
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

    /// The component's state slot named `name`.
    pub fn state_slot(&self, name: &str) -> Option<usize> {
        let index = self.instance.component()?;
        self.module().layout(index).state(name)
    }

    /// The instance's current value of state slot `slot`.
    pub fn state(&self, slot: usize) -> Option<&Value> {
        self.instance.states().get(slot)
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

    fn link(&mut self, slot: usize, link: Link) -> bool {
        match self.mirror.get_mut(slot) {
            Some(cell) => {
                *cell = Some(link);
                true
            }
            None => false,
        }
    }

    /// Copies each mirrored cell, and each state `scope` keeps, into the
    /// instance.
    fn sync(&mut self, scope: &Scope, cells: &dyn StateCells) {
        for (slot, link) in self.mirror.iter().enumerate() {
            let Some(Link::Mirror(id)) = *link else {
                continue;
            };
            if let Some(value) = cells.get(id).and_then(to_value) {
                self.instance.set_state(slot, value);
            }
        }
        for locals in &scope.locals {
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
            Some(Some(Link::Mirror(id) | Link::Track(id))) => Some(*id),
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
    pub(crate) fn mark_regions(&mut self, hook: StructureHookId) {
        self.regions.mounted = true;
        self.regions.hooks.push(hook);
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

    /// Removes the view's region hooks and frees the cells its regions
    /// allocated, as unmounting or rebuilding the view's tree does.
    pub fn release_regions(&mut self, store: &mut NodeStore, states: &mut StateStore) {
        for hook in self.regions.hooks.drain(..) {
            store.remove_structure_hook(hook);
        }
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
                self.written.clear();
                self.written.extend(self.instance.dirty());
                self.instance.clear_dirty();
                for &slot in &self.written {
                    match self.mirror[slot] {
                        Some(Link::Mirror(id)) => {
                            let Some(witness) = cells.get(id) else {
                                continue;
                            };
                            if let Some(value) = to_state(&self.instance.states()[slot], witness) {
                                cells.set(id, value);
                            }
                        }
                        Some(Link::Track(id)) => {
                            if let Some(StateValue::Int(revision)) = cells.get(id) {
                                cells.set(id, StateValue::Int(revision.wrapping_add(1)));
                            }
                        }
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
                true
            }
            Err(fault) => {
                self.fault = Some(fault);
                false
            }
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

    /// Replaces the module with a recompiled one, as a hot reload does. Each
    /// state the new component still declares keeps its value by name; the
    /// others start from their new initializers. Mirrors are cleared: the caller
    /// mirrors the new layout's slots again. The cells and hooks the view's
    /// mounts registered stay the host's, for the caller to release or replace.
    pub fn reload(&mut self, module: Rc<Module>, component: &str) -> Result<(), HostError> {
        let mut next = ViewHost::new(module, component)?;
        if let (Some(old), Some(new)) = (self.instance.component(), next.instance.component()) {
            let old_layout = self.module().layout(old);
            let new_layout = Rc::clone(next.module());
            let new_layout = new_layout.layout(new);
            for (slot, name) in new_layout.states.iter().enumerate() {
                if let Some(prior) = old_layout.state(name) {
                    next.instance
                        .set_state(slot, self.instance.states()[prior].clone());
                }
            }
        }
        next.events = std::mem::take(&mut self.events);
        next.regions = std::mem::take(&mut self.regions);
        next.values = self.values.take();
        *self = next;
        Ok(())
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
fn to_value(value: StateValue) -> Option<Value> {
    match value {
        StateValue::Int(n) => Some(Value::Int(i64::from(n))),
        StateValue::Float(x) => Some(Value::Float(f64::from(x))),
        StateValue::Bool(b) => Some(Value::bool(b)),
        StateValue::Color(..) => None,
    }
}

/// A VM value as the UI cell whose current value `witness` fixes its kind.
/// `None` when the value does not fit the cell.
fn to_state(value: &Value, witness: StateValue) -> Option<StateValue> {
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
