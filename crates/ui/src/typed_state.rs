//! Typed state cells: a [`StateId`] that knows its value's kind.
//!
//! A [`State<T>`] is `Copy` and holds the id alone. It reads and writes a `T`
//! through any context that reaches the store, so handlers and effects never
//! match on [`StateValue`]:
//!
//! ```
//! use viso_ui::{BuildCx, NodeStore, State, StateStore};
//!
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let count: State<i32> = BuildCx::new(&mut store).with_states(&mut states).state_of(0);
//! count.update(&mut states, |n| n + 1);
//! assert_eq!(count.get(&mut states), Some(1));
//! ```

use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;

use viso_render::Rgba;

use crate::state::{StateId, StateStore, StateValue};

/// A value kind a [`State`] cell holds: one of the [`StateValue`] variants.
pub trait StateKind: Copy + 'static {
    /// This value as the store keeps it.
    fn into_value(self) -> StateValue;

    /// The value `value` holds, if it is this kind.
    fn from_value(value: StateValue) -> Option<Self>;
}

impl StateKind for i32 {
    #[inline]
    fn into_value(self) -> StateValue {
        StateValue::Int(self)
    }

    #[inline]
    fn from_value(value: StateValue) -> Option<Self> {
        match value {
            StateValue::Int(n) => Some(n),
            _ => None,
        }
    }
}

impl StateKind for f32 {
    #[inline]
    fn into_value(self) -> StateValue {
        StateValue::Float(self)
    }

    #[inline]
    fn from_value(value: StateValue) -> Option<Self> {
        match value {
            StateValue::Float(x) => Some(x),
            _ => None,
        }
    }
}

impl StateKind for bool {
    #[inline]
    fn into_value(self) -> StateValue {
        StateValue::Bool(self)
    }

    #[inline]
    fn from_value(value: StateValue) -> Option<Self> {
        match value {
            StateValue::Bool(b) => Some(b),
            _ => None,
        }
    }
}

/// A straight-alpha color.
impl StateKind for Rgba {
    #[inline]
    fn into_value(self) -> StateValue {
        StateValue::Color(self.r, self.g, self.b, self.a)
    }

    #[inline]
    fn from_value(value: StateValue) -> Option<Self> {
        match value {
            StateValue::Color(r, g, b, a) => Some(Rgba { r, g, b, a }),
            _ => None,
        }
    }
}

/// The contexts a cell is read through. A context that tracks what its body
/// reads ([`EffectCx`](crate::EffectCx), [`ComputeCx`](crate::ComputeCx))
/// records the read as a dependency.
pub trait StateRead {
    /// The current value of cell `id`; `None` for a stale id.
    fn read(&mut self, id: StateId) -> Option<StateValue>;
}

impl StateRead for StateStore {
    #[inline]
    fn read(&mut self, id: StateId) -> Option<StateValue> {
        self.get(id)
    }
}

impl StateRead for crate::EventCx<'_> {
    #[inline]
    fn read(&mut self, id: StateId) -> Option<StateValue> {
        self.get(id)
    }
}

impl StateRead for crate::context::UpdateCx<'_> {
    #[inline]
    fn read(&mut self, id: StateId) -> Option<StateValue> {
        self.get(id)
    }
}

impl StateRead for crate::EffectCx<'_> {
    #[inline]
    fn read(&mut self, id: StateId) -> Option<StateValue> {
        self.get(id)
    }
}

impl StateRead for crate::ComputeCx<'_> {
    #[inline]
    fn read(&mut self, id: StateId) -> Option<StateValue> {
        self.get(id)
    }
}

impl StateRead for crate::structure::StructureCx<'_> {
    #[inline]
    fn read(&mut self, id: StateId) -> Option<StateValue> {
        self.states.get(id)
    }
}

/// A state cell holding a `T`: `Copy`, its [`StateId`] alone.
///
/// Mint one with [`BuildCx::state_of`](crate::BuildCx::state_of). Reads go
/// through any [`StateRead`] context and writes through any
/// [`StateWriter`](crate::StateWriter), both pending like any other state
/// write. A cell that was freed reads `None` and takes no write.
pub struct State<T> {
    id: StateId,
    kind: PhantomData<fn() -> T>,
}

impl<T: StateKind> State<T> {
    /// The cell `id` names, read as a `T`. A cell holding another kind reads
    /// `None` and is still written as a `T`.
    #[inline]
    pub fn from_id(id: StateId) -> Self {
        Self {
            id,
            kind: PhantomData,
        }
    }

    /// The untyped id, for bindings and the APIs that take one.
    #[inline]
    pub fn id(self) -> StateId {
        self.id
    }

    /// The cell's value; `None` once the cell is freed.
    #[inline]
    pub fn get(self, cx: &mut (impl StateRead + ?Sized)) -> Option<T> {
        cx.read(self.id).and_then(T::from_value)
    }

    /// Writes `value`; returns whether the cell was live.
    #[inline]
    pub fn set(self, cx: &mut (impl crate::StateWriter + ?Sized), value: T) -> bool {
        cx.set(self.id, value.into_value())
    }

    /// Writes `f` of the current value; returns whether it wrote, which it
    /// does not once the cell is freed.
    #[inline]
    pub fn update(
        self,
        cx: &mut (impl crate::StateWriter + ?Sized),
        f: impl FnOnce(T) -> T,
    ) -> bool {
        match self.get(cx) {
            Some(value) => self.set(cx, f(value)),
            None => false,
        }
    }
}

impl<T> From<State<T>> for StateId {
    #[inline]
    fn from(state: State<T>) -> StateId {
        state.id
    }
}

impl<T> Clone for State<T> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for State<T> {}

impl<T> PartialEq for State<T> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl<T> Eq for State<T> {}

impl<T> Hash for State<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl<T> fmt::Debug for State<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("State")
            .field(&std::any::type_name::<T>())
            .field(&self.id)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BuildCx, NodeStore};

    #[test]
    fn each_kind_round_trips_and_a_freed_cell_reads_none() {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut cx = BuildCx::new(&mut store).with_states(&mut states);
        let n = cx.state_of(3i32);
        let x = cx.state_of(0.5f32);
        let on = cx.state_of(false);
        let tint = cx.state_of(Rgba {
            r: 0.1,
            g: 0.2,
            b: 0.3,
            a: 1.0,
        });
        drop(cx);
        assert!(n.update(&mut states, |n| n * 2));
        assert!(on.set(&mut states, true));
        assert_eq!(n.get(&mut states), Some(6));
        assert_eq!(x.get(&mut states), Some(0.5));
        assert_eq!(on.get(&mut states), Some(true));
        assert_eq!(tint.get(&mut states).map(|c| c.g), Some(0.2));
        states.free(n.id());
        assert_eq!(n.get(&mut states), None);
        assert!(!n.update(&mut states, |n| n + 1));
    }

    #[test]
    fn another_kind_reads_none() {
        let mut states = StateStore::new();
        let id = states.alloc(StateValue::Bool(true));
        assert_eq!(State::<i32>::from_id(id).get(&mut states), None);
    }
}
