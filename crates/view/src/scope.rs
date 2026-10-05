//! What region content runs with: the bindings of the enclosing `for` items and
//! `match` scrutinees, and the states of the component instances it mounts.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use viso_behavior::Value;
use viso_ui::adaptive::{AnchorId, EnvField};
use viso_ui::{NodeId, StateId, StateStore, StateValue};

use crate::env::env_value;
use crate::host::StateCells;

/// The bindings and instance states a handler, a control or a region entry
/// inside region content runs with. Static content runs with the empty scope.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    /// The enclosing regions' bindings, outermost first.
    pub(crate) values: Vec<Value>,
    /// The states of every enclosing mount of region content that mounts a
    /// component instance, outermost first.
    pub(crate) locals: Vec<Rc<Locals>>,
}

impl Scope {
    /// The scope of static content.
    pub const EMPTY: Scope = Scope {
        values: Vec::new(),
        locals: Vec::new(),
    };

    /// The local cell of state slot `slot`, `None` for a slot no enclosing
    /// mount keeps.
    pub(crate) fn cell(&self, slot: u32) -> Option<StateId> {
        self.locals
            .iter()
            .rev()
            .find_map(|locals| locals.cell(slot))
    }
}

/// The states one mount of region content keeps for the component instances
/// it mounts. The component instance on the VM has one slot per state however
/// many times region content mounts it; each mount keeps its own values here
/// and loads them into those slots before any call in its scope.
///
/// A UI cell per state holds a revision the host raises after each committed
/// write, so the bindings and regions that read the state see the change, and
/// the view's pulse cell is raised with it to wake the view's structure hook.
///
/// An `env` slot's cell is the field's revision cell: the window's for a
/// window-wide field, shared with every reader, or the mount's own anchor's,
/// which wakes the pulse when it moves. Its value is rebuilt from the
/// environment when that revision moved since the last build.
#[derive(Debug)]
pub(crate) struct Locals {
    /// The component state slots, ascending.
    pub(crate) slots: Box<[u32]>,
    /// The value of each slot.
    pub(crate) values: RefCell<Box<[Value]>>,
    /// The revision cell of each slot.
    pub(crate) cells: Box<[StateId]>,
    /// The `env` slots among [`slots`](Self::slots), ascending.
    pub(crate) env: Box<[LocalEnv]>,
    /// The cell raised with any of [`cells`](Self::cells) a write raises.
    pub(crate) pulse: StateId,
    /// The root node of each instance this mount mounts that starts tasks,
    /// which owns them, by instance.
    pub(crate) owners: Box<[(u32, NodeId)]>,
}

/// An `env` slot a mount of region content keeps.
#[derive(Debug)]
pub(crate) struct LocalEnv {
    /// The slot's index in [`Locals::slots`].
    pub(crate) at: u32,
    pub(crate) field: EnvField,
    /// The anchor its anchored field resolves at; `None` for a window-wide
    /// field.
    pub(crate) anchor: Option<AnchorId>,
    /// The revision of the slot's cell its value was built at; `None` before
    /// the first build.
    pub(crate) seen: Cell<Option<i32>>,
}

impl Locals {
    /// The node owning the tasks instance `instance` starts, when this mount
    /// mounts it.
    pub(crate) fn owner_of(&self, instance: u32) -> Option<NodeId> {
        self.owners
            .iter()
            .find(|&&(i, _)| i == instance)
            .map(|&(_, node)| node)
    }

    fn at(&self, slot: u32) -> Option<usize> {
        self.slots.binary_search(&slot).ok()
    }

    /// The revision cell of `slot`, `None` for a slot this mount does not keep.
    pub(crate) fn cell(&self, slot: u32) -> Option<StateId> {
        self.at(slot).map(|at| self.cells[at])
    }

    fn env_at(&self, at: usize) -> Option<&LocalEnv> {
        self.env
            .binary_search_by_key(&at, |env| env.at as usize)
            .ok()
            .map(|index| &self.env[index])
    }

    /// Keeps `value` as the value of `slot` and raises its revision; returns
    /// `false`, keeping nothing, for a slot this mount does not keep. An `env`
    /// slot is read-only and keeps its value.
    pub(crate) fn store(&self, slot: u32, value: Value, cells: &mut dyn StateCells) -> bool {
        let Some(at) = self.at(slot) else {
            return false;
        };
        if self.env_at(at).is_some() {
            return true;
        }
        self.values.borrow_mut()[at] = value;
        for id in [self.cells[at], self.pulse] {
            if let Some(StateValue::Int(revision)) = cells.get(id) {
                cells.set(id, StateValue::Int(revision.wrapping_add(1)));
            }
        }
        true
    }

    /// Rebuilds each `env` slot whose cell's revision moved since its last
    /// build.
    pub(crate) fn refresh(&self, cells: &dyn StateCells) {
        if self.env.is_empty() {
            return;
        }
        let mut values = self.values.borrow_mut();
        for env in &self.env {
            let at = env.at as usize;
            let revision = match cells.get(self.cells[at]) {
                Some(StateValue::Int(revision)) => Some(revision),
                _ => None,
            };
            if revision.is_some() && revision == env.seen.get() {
                continue;
            }
            env.seen.set(revision);
            values[at] = env_value(env.field, cells.env(), env.anchor);
        }
    }

    /// Frees the revision cells and releases the anchors; a window-wide `env`
    /// cell is the window's and stays.
    pub(crate) fn release(&self, states: &mut StateStore) {
        for (at, &id) in self.cells.iter().enumerate() {
            match self.env_at(at) {
                None => {
                    states.free(id);
                }
                Some(env) => {
                    if let Some(anchor) = env.anchor {
                        states.release_anchor(anchor);
                    }
                }
            }
        }
    }
}
