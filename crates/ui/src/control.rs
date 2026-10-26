//! Control handles: the `Copy` ids an application keeps to drive a built
//! control.
//!
//! A control (a modal, a popup, a data grid, …) builds nodes and a block of
//! shared parts its handlers work on. Its handle is a [`ControlId`] alone, a
//! generational index into the [`ControlTable`] the [`StateStore`] keeps, so
//! it is `Copy`, can be minted before the control is built
//! ([`BuildCx::handle`](crate::BuildCx::handle)) and captured by any closure,
//! the control's own included. The table holds the parts weakly: the control's
//! handlers own them, so they go with its nodes, and a handle that outlives
//! them finds nothing and does nothing.
//!
//! Each control names its own handle type (`ModalHandle`, `PopupHandle`, …),
//! a [`ControlHandle`] over the id with the control's methods on it.

use std::any::Any;
use std::rc::{Rc, Weak};

use crate::state::StateStore;

/// A built control's identity: a slot of the [`ControlTable`] and the
/// generation that validates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ControlId {
    index: u32,
    generation: u32,
}

impl ControlId {
    /// The id of no control: every lookup misses and every bind is ignored.
    pub const DETACHED: ControlId = ControlId {
        index: u32::MAX,
        generation: u32::MAX,
    };
}

/// A control's typed handle: a [`ControlId`] and the methods of its control.
pub trait ControlHandle: Copy + 'static {
    /// The handle of the control `id` names.
    fn from_id(id: ControlId) -> Self;

    /// The control this handle names.
    fn id(self) -> ControlId;
}

/// Declares a control's handle type: a `Copy` [`ControlId`] newtype that is
/// a [`ControlHandle`]. The control adds its methods in an `impl` of its own.
///
/// ```
/// viso_ui::control_handle! {
///     /// Drives a built dialog.
///     pub struct DialogHandle;
/// }
/// ```
#[macro_export]
macro_rules! control_handle {
    ($(#[$meta:meta])* $vis:vis struct $name:ident;) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        $vis struct $name($crate::ControlId);

        impl $crate::ControlHandle for $name {
            fn from_id(id: $crate::ControlId) -> Self {
                $name(id)
            }

            fn id(self) -> $crate::ControlId {
                self.0
            }
        }
    };
}

/// One slot: the parts it holds weakly, and whether it waits for them.
struct Slot {
    parts: Option<Weak<dyn Any>>,
    generation: u32,
    /// Minted and not yet bound: kept, however long the build takes.
    reserved: bool,
}

impl Slot {
    /// Whether the slot's control is gone, or its id was never handed out.
    fn is_vacant(&self) -> bool {
        !self.reserved && self.parts.as_ref().is_none_or(|p| p.strong_count() == 0)
    }
}

/// The built controls' parts by [`ControlId`], held weakly. A cold-path
/// table: handles read it from event handlers, never per frame.
#[derive(Default)]
pub struct ControlTable {
    slots: Vec<Slot>,
    free: Vec<u32>,
    /// How many slots the next growth sweeps at: the vacant ones then go back
    /// to the free list, so a table with live controls doubles at most.
    sweep_at: usize,
}

impl ControlTable {
    /// Mints the id of a control about to be built.
    pub fn reserve(&mut self) -> ControlId {
        if self.free.is_empty() && self.slots.len() >= self.sweep_at {
            self.sweep();
        }
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            slot.reserved = true;
            return ControlId {
                index,
                generation: slot.generation,
            };
        }
        let index = self.slots.len() as u32;
        self.slots.push(Slot {
            parts: None,
            generation: 0,
            reserved: true,
        });
        ControlId {
            index,
            generation: 0,
        }
    }

    /// Binds `parts` to `id`, as its control builds; a stale id binds nothing.
    pub fn bind<P: 'static>(&mut self, id: ControlId, parts: &Rc<P>) {
        let Some(slot) = self.slot_mut(id) else {
            return;
        };
        let parts: Rc<dyn Any> = parts.clone();
        slot.parts = Some(Rc::downgrade(&parts));
        slot.reserved = false;
    }

    /// The parts bound to `id`, while its control lives and they are a `P`.
    pub fn get<P: 'static>(&self, id: ControlId) -> Option<Rc<P>> {
        let slot = self.slots.get(id.index as usize)?;
        if slot.generation != id.generation {
            return None;
        }
        slot.parts.as_ref()?.upgrade()?.downcast().ok()
    }

    /// How many slots the table holds, vacant ones included.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the table holds no slot.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    fn slot_mut(&mut self, id: ControlId) -> Option<&mut Slot> {
        self.slots
            .get_mut(id.index as usize)
            .filter(|slot| slot.generation == id.generation)
    }

    /// Returns the vacant slots to the free list, bumping their generations
    /// so the ids they gave out go stale.
    fn sweep(&mut self) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if slot.is_vacant() && slot.parts.is_some() {
                slot.parts = None;
                slot.generation = slot.generation.wrapping_add(1);
                self.free.push(index as u32);
            }
        }
        self.sweep_at = (self.slots.len() * 2).max(16);
    }
}

/// The contexts a handle reads its control's parts through.
pub trait Controls {
    /// The parts of control `id`, while it lives and they are a `P`.
    fn control<P: 'static>(&self, id: ControlId) -> Option<Rc<P>>;
}

impl Controls for StateStore {
    fn control<P: 'static>(&self, id: ControlId) -> Option<Rc<P>> {
        self.controls.get(id)
    }
}

impl Controls for ControlTable {
    fn control<P: 'static>(&self, id: ControlId) -> Option<Rc<P>> {
        self.get(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bound_control_is_found_until_its_parts_go() {
        let mut table = ControlTable::default();
        let id = table.reserve();
        assert!(table.get::<u32>(id).is_none(), "reserved, not yet bound");
        let parts = Rc::new(7u32);
        table.bind(id, &parts);
        assert_eq!(table.get::<u32>(id).as_deref(), Some(&7));
        assert!(table.get::<i64>(id).is_none(), "another control's parts");
        drop(parts);
        assert!(table.get::<u32>(id).is_none());
    }

    #[test]
    fn a_vacant_slot_is_reused_and_its_old_id_stays_stale() {
        let mut table = ControlTable::default();
        let mut live = Vec::new();
        let gone = table.reserve();
        table.bind(gone, &Rc::new(0u32));
        for i in 0..64u32 {
            let id = table.reserve();
            let parts = Rc::new(i);
            table.bind(id, &parts);
            live.push((id, parts));
        }
        assert!(table.len() < 66, "the gone slot came back: {}", table.len());
        assert!(table.get::<u32>(gone).is_none());
        for (id, parts) in &live {
            assert_eq!(table.get::<u32>(*id).as_deref(), Some(&**parts));
        }
    }

    #[test]
    fn a_reserved_slot_survives_a_sweep() {
        let mut table = ControlTable::default();
        let early = table.reserve();
        for _ in 0..64 {
            let id = table.reserve();
            table.bind(id, &Rc::new(()));
        }
        let parts = Rc::new(1u8);
        table.bind(early, &parts);
        assert_eq!(table.get::<u8>(early).as_deref(), Some(&1));
    }
}
