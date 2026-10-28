//! The runtime of a `web-dom` page.
//!
//! The page is the view as HTML its reader's browser lays out; its script
//! keeps the DOM current. What the view computes and does — each value a
//! node shows, each handler, region entry, effect and task — runs here, on
//! the behavior VM, inside the app the page loads: the same module, the
//! same natives and, in a release build, the same compiled code as on every
//! other target. The script calls a handler-table entry with its arguments
//! and reads back what the calls changed, emitted, started and asked of
//! named nodes ([`Batch`]).
//!
//! The app hands the runtime its view as it builds: `view!` offers the
//! module it embeds, its compiled code and its native libraries
//! ([`offer`]), and the page opens the one whose component it mounts
//! ([`Page::open`]).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use viso_behavior::aot::{NativeCode, build_hash};
use viso_behavior::event::EventControl;
use viso_behavior::native::{NativeError, NativeLibrary, Natives};
use viso_behavior::{
    Budget, Event, Fault, FaultKind, Fiber, Instance, Module, NodeRequest, Outcome, Start,
    TaskStep, Value, Vm,
};

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod web;

/// A view the app offers a page: the module `view!` embeds, the Rust it
/// compiles to when the build carries it, and the libraries of the native
/// packages it calls.
#[derive(Clone, Copy)]
pub struct Boot {
    pub module: &'static [u8],
    pub component: &'static str,
    pub native: Option<&'static NativeCode>,
    pub libraries: &'static [&'static [&'static NativeLibrary]],
}

thread_local! {
    static OFFERED: RefCell<Vec<Boot>> = const { RefCell::new(Vec::new()) };
}

/// Offers `boot` to the page: the first offer of each component is the one
/// a page mounting it opens.
pub fn offer(boot: Boot) {
    OFFERED.with(|offered| {
        let mut offered = offered.borrow_mut();
        if !offered.iter().any(|b| b.component == boot.component) {
            offered.push(boot);
        }
    });
}

/// What the calls since the last [`Page::take`] did besides their values.
#[derive(Debug, Default)]
pub struct Batch {
    /// The state slots they wrote, ascending, each once.
    pub changed: Vec<u32>,
    /// The events they emitted, in order.
    pub events: Vec<Event>,
    /// The tasks they started, in order.
    pub starts: Vec<Start>,
    /// What they asked of named nodes, in order.
    pub nodes: Vec<NodeRequest>,
}

/// The view's component instance on the VM, which the page script calls.
pub struct Page {
    vm: Vm,
    instance: Instance,
    /// The chunk of each handler-table entry.
    handlers: Box<[u32]>,
    batch: Batch,
}

impl Page {
    /// Opens the view of component `component` the app offered: on `module`,
    /// the page's own build of it, when given — a dev page whose `.vs` edits
    /// the app was not built again for — else on the module the app embeds,
    /// which must be the build `build` the page was written from.
    ///
    /// # Errors
    ///
    /// What keeps the view from running, described.
    pub fn open(component: &str, module: Option<&[u8]>, build: u64) -> Result<Page, String> {
        let boot = OFFERED
            .with(|offered| {
                offered
                    .borrow()
                    .iter()
                    .find(|b| b.component == component)
                    .copied()
            })
            .ok_or_else(|| {
                format!(
                    "the app offers no view of `{component}`: its `build` mounts it with `view!`"
                )
            })?;
        let decoded = match module {
            Some(bytes) => Module::decode(bytes),
            None => Module::decode_static(boot.module),
        };
        let module = Rc::new(decoded.map_err(|e| format!("the view's module does not load: {e}"))?);
        if build_hash(&module) != build {
            return Err(format!(
                "the page and the app were built from different sources of `{component}`: build them again together"
            ));
        }
        let index = module
            .component(component)
            .ok_or_else(|| format!("the view's module has no component `{component}`"))?;
        let handlers = module.layout(index).handlers.clone();
        let natives =
            natives(boot.libraries).map_err(|c| format!("the view does not link: {c}"))?;
        let grant: Vec<&str> = module.capabilities().iter().map(|c| &**c).collect();
        let mut vm = Vm::new(Rc::clone(&module), Budget::default());
        vm.link(&natives, &grant)
            .map_err(|c| format!("the view does not link: {c}"))?;
        // The page's own build runs on the interpreter where the app's
        // compiled code is of another.
        if let Some(native) = boot.native
            && let Err(mismatch) = vm.install_native(native)
            && cfg!(not(feature = "vm"))
        {
            return Err(format!("the app's compiled view is stale: {mismatch}"));
        }
        let instance = vm
            .instantiate(index, [])
            .map_err(|f| format!("the view does not start: {}", f.message))?;
        vm.services_mut().insert(EventControl::new());
        Ok(Page {
            vm,
            instance,
            handlers,
            batch: Batch::default(),
        })
    }

    /// The value handler-table entry `entry` gives `args`, against the
    /// current states. Nothing it does is kept.
    ///
    /// # Errors
    ///
    /// Its fault.
    pub fn evaluate(&mut self, entry: u32, args: &[Value]) -> Result<Value, Fault> {
        let chunk = self.chunk(entry)?;
        let result = self.vm.call(&mut self.instance, chunk, args);
        self.instance.clear_dirty();
        result.map(|outcome| outcome.value)
    }

    /// Runs handler-table entry `entry` with `args` as one transaction and
    /// returns its value; what it did joins the batch.
    ///
    /// # Errors
    ///
    /// Its fault, which rolled it back.
    pub fn transact(&mut self, entry: u32, args: &[Value]) -> Result<Value, Fault> {
        let chunk = self.chunk(entry)?;
        let result = self.vm.call(&mut self.instance, chunk, args);
        self.land(result)
    }

    /// Runs the closure `callee` with `args` as one transaction, as
    /// [`transact`](Self::transact) does an entry.
    ///
    /// # Errors
    ///
    /// Its fault, which rolled it back.
    pub fn call(&mut self, callee: &Value, args: &[Value]) -> Result<Value, Fault> {
        let result = self.vm.call_value(&mut self.instance, callee, args);
        self.land(result)
    }

    fn land(&mut self, result: Result<Outcome, Fault>) -> Result<Value, Fault> {
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(fault) => {
                self.instance.clear_dirty();
                return Err(fault);
            }
        };
        for slot in self.instance.dirty() {
            mark(&mut self.batch.changed, slot as u32);
        }
        self.instance.clear_dirty();
        self.batch.events.extend(outcome.events);
        self.batch.starts.extend(outcome.starts);
        self.batch.nodes.extend(outcome.nodes);
        Ok(outcome.value)
    }

    /// Sets state slot `slot` outside a transaction, as the states a mount of
    /// region content keeps are put back before it runs; a slot out of range
    /// is ignored.
    pub fn put(&mut self, slot: u32, value: Value) {
        if (slot as usize) < self.instance.states().len() {
            self.instance.set_state(slot as usize, value);
        }
    }

    /// State slot `slot`, `Nil` out of range.
    pub fn state(&self, slot: u32) -> Value {
        self.instance
            .states()
            .get(slot as usize)
            .cloned()
            .unwrap_or_default()
    }

    /// Writes each slot of `changes` as a committed write, as the browser's
    /// environment fields arrive: a value that changes joins the batch.
    pub fn write(&mut self, changes: impl IntoIterator<Item = (u32, Value)>) {
        for (slot, value) in changes {
            let Some(held) = self.instance.states().get(slot as usize) else {
                continue;
            };
            if *held != value {
                self.instance.set_state(slot as usize, value);
                mark(&mut self.batch.changed, slot);
            }
        }
    }

    /// Puts the default theme in each of `slots`, as an export fixes it.
    pub fn theme(&mut self, slots: &[u32]) {
        let theme = viso_behavior::theme::default_theme();
        for &slot in slots {
            self.put(slot, theme.clone());
        }
    }

    /// Takes the batch.
    pub fn take(&mut self) -> Batch {
        std::mem::take(&mut self.batch)
    }

    /// Starts the task of chunk `task` with `args`.
    ///
    /// # Errors
    ///
    /// Its fault.
    pub fn start_task(&mut self, task: u32, args: &[Value]) -> Result<TaskStep, Fault> {
        let step = self.vm.start_task(&mut self.instance, task, args);
        self.instance.clear_dirty();
        step
    }

    /// Resumes the task `fiber` awaits with `result`.
    ///
    /// # Errors
    ///
    /// Its fault.
    pub fn resume_task(
        &mut self,
        fiber: Fiber,
        result: Result<Value, NativeError>,
    ) -> Result<TaskStep, Fault> {
        let step = self.vm.resume_task(&mut self.instance, fiber, result);
        self.instance.clear_dirty();
        step
    }

    /// Forgets the animation `id`, which the page ended.
    pub fn animation_ended(&mut self, id: u64) {
        self.vm.animation_ended(id);
    }

    /// The VM's services, where the page installs its timers.
    pub fn services_mut(&mut self) -> &mut viso_behavior::native::Services {
        self.vm.services_mut()
    }

    fn chunk(&self, entry: u32) -> Result<u32, Fault> {
        self.handlers
            .get(entry as usize)
            .copied()
            .ok_or_else(|| Fault {
                kind: FaultKind::Internal,
                at: None,
                message: format!("the view has no entry {entry}"),
            })
    }
}

/// Adds `slot` to the ascending `changed`, once.
fn mark(changed: &mut Vec<u32>, slot: u32) {
    if let Err(at) = changed.binary_search(&slot) {
        changed.insert(at, slot);
    }
}

/// The standard libraries and those of `packages`.
fn natives(
    packages: &[&'static [&'static NativeLibrary]],
) -> Result<Arc<Natives>, viso_behavior::native::SchemaConflict> {
    match packages {
        [] => Ok(Natives::standard()),
        [one] => Natives::shared(one),
        _ => Natives::shared(&packages.concat()),
    }
}

#[cfg(test)]
mod tests;
