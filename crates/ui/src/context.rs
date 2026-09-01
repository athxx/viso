//! Phase-specific contexts.
//!
//! Instead of one omnipotent context, each frame phase gets a purpose-specific
//! one. This is both an architectural constraint and a performance / reasoning
//! tool: a `LayoutCx` cannot launch a network request, create a window, or
//! submit arbitrary GPU work.
//!
//! Phase 0 defines these as marker types so signatures like
//! `fn layout(&mut self, cx: &mut LayoutCx<'_>, ..)` can be written against a
//! stable contract before the real capabilities are filled in.

use core::marker::PhantomData;

use crate::binding::BindingTable;
use crate::component::NodeStore;
use crate::state::{StateId, StateStore, StateValue};

macro_rules! phase_cx {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        pub struct $name<'a> {
            _life: PhantomData<&'a mut ()>,
        }
        impl<'a> $name<'a> {
            #[doc(hidden)]
            pub fn __new() -> Self {
                Self { _life: PhantomData }
            }
        }
    };
}

phase_cx!(/// Application-scope context: create windows, register services.
    AppCx);
phase_cx!(/// Input dispatch context.
    EventCx);
phase_cx!(/// Layout context. No network, no window creation, no GPU submit.
    LayoutCx);
phase_cx!(/// Paint-data production context.
    PaintCx);
phase_cx!(/// Render/submit context (internal).
    RenderCx);
phase_cx!(/// Async task context.
    TaskCx);

/// State-flush / reactive update context.
///
/// This is the write end handed to component update / action handlers. Unlike
/// the marker contexts, it holds live borrows of the reactive stores so a
/// handler can read and write state. Writes are deferred: [`UpdateCx::set`]
/// records the change in the state store's per-frame pending set, and the
/// frame's flush phase turns each changed state into targeted node dirtying via
/// the [`BindingTable`]. Nothing recomputes at write time, so many writes in
/// one action collapse into one flush.
///
/// The context deliberately does *not* expose `mark_dirty` or layout entry
/// points: a handler declares intent by writing state, and invalidation follows
/// from the compiled bindings. It holds `nodes` for reads and for the flush's
/// own use, not as a manual-invalidation escape hatch.
pub struct UpdateCx<'a> {
    states: &'a mut StateStore,
    bindings: &'a BindingTable,
    #[allow(dead_code)]
    nodes: &'a mut NodeStore,
}

impl<'a> UpdateCx<'a> {
    /// Assemble the update context from the frame's reactive stores.
    #[doc(hidden)]
    pub fn __new(
        states: &'a mut StateStore,
        bindings: &'a BindingTable,
        nodes: &'a mut NodeStore,
    ) -> Self {
        Self {
            states,
            bindings,
            nodes,
        }
    }

    /// Read the current value of a state cell. `None` for a stale handle.
    #[inline]
    pub fn get(&self, id: StateId) -> Option<StateValue> {
        self.states.get(id)
    }

    /// Write a state value. The change is recorded in this frame's pending set
    /// and applied to bound nodes by the flush phase, not here. Returns whether
    /// the write landed (the handle was live). Setting the current value is a
    /// no-op that schedules no work.
    #[inline]
    pub fn set(&mut self, id: StateId, value: StateValue) -> bool {
        self.states.set(id, value)
    }

    /// The binding table backing this frame — the flush reads it to turn
    /// changed states into node dirtying.
    #[inline]
    pub fn bindings(&self) -> &BindingTable {
        self.bindings
    }
}
