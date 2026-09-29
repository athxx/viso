//! The component instance a view's handlers run against.

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use viso_behavior::native::{Natives, SchemaConflict};
use viso_behavior::{
    Budget, ChunkKind, Event, Fault, FaultKind, Instance, LoadError, Module, Value, Vm,
};
use viso_ui::{EventCx, StateId, StateStore, StateValue};

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

    /// Copies each mirrored cell into the instance.
    fn sync(&mut self, cells: &dyn StateCells) {
        for (slot, link) in self.mirror.iter().enumerate() {
            let Some(Link::Mirror(id)) = *link else {
                continue;
            };
            if let Some(value) = cells.get(id).and_then(to_value) {
                self.instance.set_state(slot, value);
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

    /// Evaluates the pure entry `entry` of the handler table (a region's
    /// selector, iterable or key) with `args` against the current states, and
    /// returns its value. Nothing it does is kept: it writes no state and its
    /// events are dropped.
    pub fn evaluate(
        &mut self,
        entry: u32,
        args: &[Value],
        cells: &dyn StateCells,
    ) -> Result<Value, Fault> {
        let chunk = self.chunk(entry, ChunkKind::RegionEntry)?;
        self.sync(cells);
        let result = self.vm.call(&mut self.instance, chunk, args);
        self.instance.clear_dirty();
        result.map(|outcome| outcome.value)
    }

    /// The number of handlers the view declares.
    pub fn handler_count(&self) -> usize {
        self.handlers.len()
    }

    /// Runs handler `handler` with `payload` and the enclosing regions'
    /// bindings `scope`, as one transaction over the component's states.
    ///
    /// Returns whether it committed. A fault is kept as
    /// [`last_fault`](Self::last_fault); the transaction's writes and events are
    /// then discarded.
    pub fn dispatch(
        &mut self,
        handler: u32,
        payload: Value,
        scope: &[Value],
        cells: &mut dyn StateCells,
    ) -> bool {
        let chunk = match self.chunk(handler, ChunkKind::Handler) {
            Ok(chunk) => chunk,
            Err(fault) => {
                self.fault = Some(fault);
                return false;
            }
        };
        self.sync(cells);
        self.args.clear();
        self.args.push(payload);
        self.args.extend_from_slice(scope);
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
                        None => {}
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
    /// mirrors the new layout's slots again.
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
