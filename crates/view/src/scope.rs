//! What region content runs with: the bindings of the enclosing `for` items and
//! `match` scrutinees, and the states of the component instances it mounts.

use std::cell::RefCell;
use std::rc::Rc;

use viso_behavior::Value;
use viso_ui::{StateId, StateStore, StateValue};

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
#[derive(Debug)]
pub(crate) struct Locals {
    /// The component state slots, ascending.
    pub(crate) slots: Box<[u32]>,
    /// The value of each slot.
    pub(crate) values: RefCell<Box<[Value]>>,
    /// The revision cell of each slot.
    pub(crate) cells: Box<[StateId]>,
    /// The cell raised with any of [`cells`](Self::cells).
    pub(crate) pulse: StateId,
}

impl Locals {
    fn at(&self, slot: u32) -> Option<usize> {
        self.slots.binary_search(&slot).ok()
    }

    /// The revision cell of `slot`, `None` for a slot this mount does not keep.
    pub(crate) fn cell(&self, slot: u32) -> Option<StateId> {
        self.at(slot).map(|at| self.cells[at])
    }

    /// Keeps `value` as the value of `slot` and raises its revision; returns
    /// `false`, keeping nothing, for a slot this mount does not keep.
    pub(crate) fn store(&self, slot: u32, value: Value, cells: &mut dyn StateCells) -> bool {
        let Some(at) = self.at(slot) else {
            return false;
        };
        self.values.borrow_mut()[at] = value;
        for id in [self.cells[at], self.pulse] {
            if let Some(StateValue::Int(revision)) = cells.get(id) {
                cells.set(id, StateValue::Int(revision.wrapping_add(1)));
            }
        }
        true
    }

    /// Frees the revision cells.
    pub(crate) fn release(&self, states: &mut StateStore) {
        for &id in &self.cells {
            states.free(id);
        }
    }
}
