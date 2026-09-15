//! `SlotMap<T>` — dense generational resource storage for the backends.
//!
//! Each backend keeps one `SlotMap` per resource kind (buffers, textures,
//! samplers, pipelines, bind groups, surfaces). A slot holds a value and a
//! generation; a handle is the `{index, generation}` pair returned at insert.
//! A lookup succeeds only when the handle's generation still matches the slot's,
//! so a handle left over from a reclaimed resource resolves to `None` rather
//! than silently hitting whatever value later took the slot.
//!
//! Storage is dense: values live in one `Vec` indexed directly by slot index —
//! no per-lookup indirection or hashing on the hot path. A free-list threads
//! reclaimed slots for reuse; reclaiming a slot bumps its generation so old
//! handles go stale. (The free-list and reclamation land with deferred
//! destruction; until then every insert appends and `len == created`.)

/// A resource handle: which slot, and which generation of that slot. Every
/// backend id newtype is a thin wrapper over this pair, so `SlotMap` returns it
/// raw and each backend stamps its own type on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RawId {
    /// Storage-slot index.
    pub index: u32,
    /// Slot generation at the time the handle was issued.
    pub generation: u32,
}

/// One storage slot: its current value (`None` once reclaimed) and the
/// generation a handle must match to resolve here.
struct Slot<T> {
    value: Option<T>,
    generation: u32,
}

/// Dense generational storage for one resource kind.
pub struct SlotMap<T> {
    slots: Vec<Slot<T>>,
    /// Indices of reclaimed slots available for reuse (LIFO).
    free: Vec<u32>,
    /// Cumulative count of live values — insertions minus removals.
    live: usize,
    /// Cumulative count of values ever inserted; never decreases.
    created: usize,
}

impl<T> Default for SlotMap<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> SlotMap<T> {
    /// An empty map.
    pub const fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            live: 0,
            created: 0,
        }
    }

    /// Insert a value, returning its handle. Reuses a reclaimed slot (with its
    /// bumped generation) when one is free, otherwise appends a fresh slot.
    pub fn insert(&mut self, value: T) -> RawId {
        self.live += 1;
        self.created += 1;
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            slot.value = Some(value);
            RawId {
                index,
                generation: slot.generation,
            }
        } else {
            let index = self.slots.len() as u32;
            self.slots.push(Slot {
                value: Some(value),
                generation: 0,
            });
            RawId {
                index,
                generation: 0,
            }
        }
    }

    /// Resolve a handle to a shared reference, or `None` if the slot is empty or
    /// the generation no longer matches (a stale handle).
    #[inline]
    pub fn get(&self, id: RawId) -> Option<&T> {
        let slot = self.slots.get(id.index as usize)?;
        if slot.generation != id.generation {
            return None;
        }
        slot.value.as_ref()
    }

    /// Resolve a handle to a mutable reference, or `None` (see [`get`](Self::get)).
    #[inline]
    pub fn get_mut(&mut self, id: RawId) -> Option<&mut T> {
        let slot = self.slots.get_mut(id.index as usize)?;
        if slot.generation != id.generation {
            return None;
        }
        slot.value.as_mut()
    }

    /// Remove the value a handle points at, bumping the slot's generation and
    /// threading it onto the free-list so a later insert reuses it. Returns the
    /// removed value, or `None` for a stale/empty handle.
    pub fn remove(&mut self, id: RawId) -> Option<T> {
        let slot = self.slots.get_mut(id.index as usize)?;
        if slot.generation != id.generation {
            return None;
        }
        let value = slot.value.take()?;
        slot.generation = slot.generation.wrapping_add(1);
        self.free.push(id.index);
        self.live -= 1;
        Some(value)
    }

    /// The number of live values currently stored.
    pub fn len(&self) -> usize {
        self.live
    }

    /// Whether the map holds no live values.
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// The cumulative number of values ever inserted. Unlike [`len`](Self::len)
    /// this never decreases, so it measures create traffic across the map's
    /// lifetime — the steady-state allocation contract the renderer bench guards.
    pub fn created(&self) -> usize {
        self.created
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_then_get() {
        let mut m = SlotMap::new();
        let a = m.insert(10u32);
        let b = m.insert(20u32);
        assert_eq!(m.get(a), Some(&10));
        assert_eq!(m.get(b), Some(&20));
        assert_eq!(a.index, 0);
        assert_eq!(b.index, 1);
        assert_eq!(a.generation, 0);
        assert_eq!(m.len(), 2);
        assert_eq!(m.created(), 2);
    }

    #[test]
    fn remove_frees_slot_and_bumps_generation() {
        let mut m = SlotMap::new();
        let a = m.insert("x");
        assert_eq!(m.remove(a), Some("x"));
        assert_eq!(m.len(), 0);
        // The freed slot is reused, at a higher generation.
        let b = m.insert("y");
        assert_eq!(b.index, a.index);
        assert_eq!(b.generation, a.generation + 1);
        assert_eq!(m.created(), 2);
    }

    #[test]
    fn stale_handle_resolves_to_none_not_a_wrong_object() {
        let mut m = SlotMap::new();
        let a = m.insert("first");
        m.remove(a);
        let b = m.insert("second");
        // `a` and `b` share an index but not a generation.
        assert_eq!(a.index, b.index);
        assert_ne!(a.generation, b.generation);
        assert_eq!(m.get(a), None);
        assert_eq!(m.get(b), Some(&"second"));
    }

    #[test]
    fn get_mut_mutates_live_slot_only() {
        let mut m = SlotMap::new();
        let a = m.insert(1i32);
        *m.get_mut(a).unwrap() += 41;
        assert_eq!(m.get(a), Some(&42));
        m.remove(a);
        assert_eq!(m.get_mut(a), None);
    }

    #[test]
    fn out_of_range_handle_is_none() {
        let m: SlotMap<u8> = SlotMap::new();
        assert_eq!(
            m.get(RawId {
                index: 5,
                generation: 0
            }),
            None
        );
    }
}
