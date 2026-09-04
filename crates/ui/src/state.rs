//! Centralized reactive state: a SoA store of scalar values keyed by a compact
//! generational [`StateId`], plus a per-frame pending write-set.
//!
//! State is the write end of the reactive link. A write updates the stored
//! value and records the id in a per-frame pending set; nothing recomputes at
//! write time. The frame's flush phase drains the pending set once, looks each
//! changed id up in the binding table, and marks the bound nodes dirty — so
//! many writes in one transaction collapse into a single flush and a single
//! targeted recompute.
//!
//! This slice stores a small set of scalar value kinds behind one type-erased
//! [`StateValue`] enum rather than a generic `State<T>`; that is enough to drive
//! the reactive link end to end. Full generic typing lands later.
//!
//! A [`StateKey`] side map gives each cell a durable, position-independent
//! identity, so a transactional hot reload can match a state cell in the old
//! build to the same cell in a recompiled build and carry its value across.
//! The key is a 128-bit value the compiler mints (it has the layout of the
//! compiler's stable symbol identity); the store treats it as an opaque pair of
//! `u64`s and never depends on the compiler crate.

use std::collections::HashMap;

/// A compact generational handle to a stored state value.
///
/// Like [`crate::node::NodeId`], the generation makes a handle to a freed slot
/// detectably stale, so a binding or a script can hold one safely across frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StateId {
    index: u32,
    generation: u32,
}

impl StateId {
    /// The dense slot index — the key into side-storage aligned to the store.
    #[inline]
    pub fn index(self) -> u32 {
        self.index
    }
    /// The generation this handle was minted at.
    #[inline]
    pub fn generation(self) -> u32 {
        self.generation
    }
}

/// A durable, position-independent identity for a state cell, used to match a
/// cell across a hot-reload recompile.
///
/// It is `#[repr(C)]` with the same `{ hi, lo }` layout the compiler's stable
/// symbol identity uses, so the hot-reload engine can hand its symbol identity
/// in without the UI crate depending on the compiler. The store only ever
/// compares and hashes it; it never interprets the bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct StateKey {
    /// High 64 bits of the identity.
    pub hi: u64,
    /// Low 64 bits of the identity.
    pub lo: u64,
}

impl StateKey {
    /// Construct a key from its two halves.
    #[inline]
    pub fn from_parts(hi: u64, lo: u64) -> Self {
        Self { hi, lo }
    }
}

/// The outcome of migrating one state cell across a hot reload, so the engine
/// can account for what was preserved versus reset (architecture section 42.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateMigration {
    /// The prior value survived unchanged (same key, compatible value).
    Kept,
    /// The prior value survived after a safe widening conversion.
    Widened,
    /// No prior cell matched the key, or the prior value was incompatible: the
    /// cell was (re)initialized and its runtime value reset.
    Reset,
}

/// A scalar state value. Type-erased for this slice's store; the variants cover
/// the scalars the reactive link needs to carry (numbers, a flag, a color).
///
/// A color is kept as four straight `f32` channels — the same shape the paint
/// tier already uses — so a bound color writes through without a pack/unpack.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StateValue {
    /// A signed integer (counters, indices).
    Int(i32),
    /// A floating-point number (progress, positions).
    Float(f32),
    /// A boolean flag (toggles, visibility).
    Bool(bool),
    /// A straight-alpha RGBA color, channels in `0.0..=1.0`.
    Color(f32, f32, f32, f32),
}

/// A slot in the state store: the current value plus the generation that
/// validates handles to it.
struct StateSlot {
    value: StateValue,
    generation: u32,
    occupied: bool,
}

/// The centralized SoA state store: value slots, a free list for slot reuse,
/// and the per-frame pending write-set.
///
/// It sits beside the [`crate::component::NodeStore`] in the app driver. Writes
/// are O(1) and allocation-free on the steady path (the pending set is a reused
/// buffer); the flush drains the pending set once per frame.
#[derive(Default)]
pub struct StateStore {
    slots: Vec<StateSlot>,
    free: Vec<u32>,
    /// Ids written since the last drain, deduplicated so one id appears once
    /// however many times it was set this frame.
    pending: Vec<StateId>,
    /// Durable identity → current cell, populated only for cells that carry a
    /// [`StateKey`] (compiler-declared state). This is a cold-path map touched
    /// at build/reload time, never on a frame path (section 45), so a hash map
    /// is fine.
    keys: HashMap<StateKey, StateId>,
}

impl StateStore {
    /// A fresh, empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate a state cell holding `initial`, reusing a free slot when
    /// possible. The returned [`StateId`] is the binding key.
    pub fn alloc(&mut self, initial: StateValue) -> StateId {
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            debug_assert!(!slot.occupied);
            slot.occupied = true;
            slot.value = initial;
            StateId {
                index,
                generation: slot.generation,
            }
        } else {
            let index = self.slots.len() as u32;
            self.slots.push(StateSlot {
                value: initial,
                generation: 0,
                occupied: true,
            });
            StateId {
                index,
                generation: 0,
            }
        }
    }

    /// Free a state cell, bumping its generation so surviving handles go stale.
    /// Returns whether the id was live.
    pub fn free(&mut self, id: StateId) -> bool {
        match self.slots.get_mut(id.index as usize) {
            Some(slot) if slot.occupied && slot.generation == id.generation => {
                slot.occupied = false;
                slot.generation = slot.generation.wrapping_add(1);
                self.free.push(id.index);
                // Drop any durable-key mapping to this exact cell, so a reused
                // slot never resurrects a stale identity. Cold path.
                if !self.keys.is_empty() {
                    self.keys.retain(|_, mapped| *mapped != id);
                }
                true
            }
            _ => false,
        }
    }

    /// Whether `id` refers to a currently-live cell.
    #[inline]
    pub fn is_live(&self, id: StateId) -> bool {
        matches!(
            self.slots.get(id.index as usize),
            Some(slot) if slot.occupied && slot.generation == id.generation
        )
    }

    /// The current value of a live cell. `None` for a stale/out-of-range handle.
    #[inline]
    pub fn get(&self, id: StateId) -> Option<StateValue> {
        let slot = self.slots.get(id.index as usize)?;
        (slot.occupied && slot.generation == id.generation).then_some(slot.value)
    }

    /// Write a new value and record the id in this frame's pending set.
    ///
    /// The recompute is deferred to the flush; only a genuine change (a value
    /// that differs from what is stored) is recorded, so re-setting the same
    /// value schedules no work. A live id already pending stays a single entry.
    /// Returns whether the write landed (the id was live).
    pub fn set(&mut self, id: StateId, value: StateValue) -> bool {
        let Some(slot) = self.slots.get_mut(id.index as usize) else {
            return false;
        };
        if !slot.occupied || slot.generation != id.generation {
            return false;
        }
        if slot.value == value {
            // No change: nothing to invalidate, nothing to flush.
            return true;
        }
        slot.value = value;
        if !self.pending.contains(&id) {
            self.pending.push(id);
        }
        true
    }

    /// Whether any write is waiting to be flushed this frame.
    #[inline]
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Move this frame's pending write-set into `out` (appended), leaving the
    /// store's pending set empty but with its capacity intact for next frame.
    /// The flush phase consumes it exactly once; `out` is caller-owned so the
    /// drain allocates nothing on the steady path.
    pub fn take_pending(&mut self, out: &mut Vec<StateId>) {
        out.append(&mut self.pending);
    }

    /// Attach a durable [`StateKey`] to a live cell, so a later hot reload can
    /// find it by identity. Cold path (build/reload only). A key already mapped
    /// is repointed to `id`; the id must be live for the mapping to land.
    /// Returns whether the mapping was recorded.
    pub fn bind_key(&mut self, id: StateId, key: StateKey) -> bool {
        if !self.is_live(id) {
            return false;
        }
        self.keys.insert(key, id);
        true
    }

    /// The live cell currently mapped to `key`, if any. Cold path.
    #[inline]
    pub fn id_for_key(&self, key: StateKey) -> Option<StateId> {
        self.keys.get(&key).copied().filter(|&id| self.is_live(id))
    }

    /// Migrate one state cell across a hot reload, keyed by durable identity
    /// (architecture section 42.1). Cold path — runs once per reload, never on a
    /// frame.
    ///
    /// `key` identifies the cell in both builds. `new_initial` is the recompiled
    /// initializer. `widen` is the compiler's safe-widening rule: given the prior
    /// value and the new initializer (which carries the new type via its
    /// variant), it returns `Some(v)` to keep the prior value — possibly widened
    /// into the new type — or `None` if the change is incompatible.
    ///
    /// - key hit + `widen` keeps the value unchanged → [`StateMigration::Kept`];
    /// - key hit + `widen` returns a converted value → [`StateMigration::Widened`];
    /// - no live cell for the key, or `widen` returns `None` → the cell is
    ///   (re)allocated with `new_initial` and [`StateMigration::Reset`].
    ///
    /// The returned id is always live and mapped to `key`. A reset of an existing
    /// keyed cell reuses that cell's slot (its value overwritten) so surviving
    /// bindings by [`StateId`] stay valid where the identity persisted.
    pub fn migrate_state(
        &mut self,
        key: StateKey,
        new_initial: StateValue,
        widen: impl FnOnce(StateValue, StateValue) -> Option<StateValue>,
    ) -> (StateId, StateMigration) {
        if let Some(id) = self.id_for_key(key) {
            // SAFETY of unwrap: id_for_key only returns live ids.
            let prior = self.get(id).expect("keyed cell is live");
            match widen(prior, new_initial) {
                Some(v) if v == prior => (id, StateMigration::Kept),
                Some(v) => {
                    // A widening conversion: overwrite in place, no pending
                    // write (the reload flush marks bound nodes wholesale).
                    if let Some(slot) = self.slots.get_mut(id.index as usize) {
                        slot.value = v;
                    }
                    (id, StateMigration::Widened)
                }
                None => {
                    // Incompatible: reset the value in the same slot, keeping
                    // identity so bindings remain valid.
                    if let Some(slot) = self.slots.get_mut(id.index as usize) {
                        slot.value = new_initial;
                    }
                    (id, StateMigration::Reset)
                }
            }
        } else {
            // New state this build: allocate and map.
            let id = self.alloc(new_initial);
            self.keys.insert(key, id);
            (id, StateMigration::Reset)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_get_set_roundtrips() {
        let mut store = StateStore::new();
        let id = store.alloc(StateValue::Int(1));
        assert_eq!(store.get(id), Some(StateValue::Int(1)));
        assert!(store.set(id, StateValue::Int(42)));
        assert_eq!(store.get(id), Some(StateValue::Int(42)));
    }

    #[test]
    fn set_records_pending_and_dedupes() {
        let mut store = StateStore::new();
        let a = store.alloc(StateValue::Int(0));
        let b = store.alloc(StateValue::Bool(false));
        assert!(!store.has_pending());

        store.set(a, StateValue::Int(1));
        store.set(a, StateValue::Int(2)); // same id again -> still one entry
        store.set(b, StateValue::Bool(true));
        assert!(store.has_pending());

        let mut out = Vec::new();
        store.take_pending(&mut out);
        assert_eq!(out, vec![a, b], "each changed id once, insertion order");
        assert!(!store.has_pending(), "drain empties the pending set");
    }

    #[test]
    fn setting_same_value_schedules_nothing() {
        let mut store = StateStore::new();
        let id = store.alloc(StateValue::Float(1.5));
        assert!(store.set(id, StateValue::Float(1.5)));
        assert!(
            !store.has_pending(),
            "a no-op write must not schedule a flush"
        );
    }

    #[test]
    fn freed_handle_is_stale() {
        let mut store = StateStore::new();
        let id = store.alloc(StateValue::Int(7));
        assert!(store.free(id));
        assert!(!store.is_live(id));
        assert_eq!(store.get(id), None);
        assert!(
            !store.set(id, StateValue::Int(9)),
            "stale write is rejected"
        );
    }

    #[test]
    fn migrate_keeps_value_on_identical_key() {
        let mut store = StateStore::new();
        let key = StateKey::from_parts(1, 2);
        let id = store.alloc(StateValue::Int(7));
        assert!(store.bind_key(id, key));
        store.set(id, StateValue::Int(41));

        // Same key, same type: prior value 41 kept, identity preserved.
        let (new_id, outcome) =
            store.migrate_state(key, StateValue::Int(0), |prior, _new| Some(prior));
        assert_eq!(new_id, id, "identity preserved across migration");
        assert_eq!(outcome, StateMigration::Kept);
        assert_eq!(store.get(new_id), Some(StateValue::Int(41)));
    }

    #[test]
    fn migrate_widens_value() {
        let mut store = StateStore::new();
        let key = StateKey::from_parts(9, 9);
        let id = store.alloc(StateValue::Int(3));
        store.bind_key(id, key);

        // Int -> Float safe widening: engine's closure converts the prior value.
        let (new_id, outcome) =
            store.migrate_state(key, StateValue::Float(0.0), |prior, _new| match prior {
                StateValue::Int(n) => Some(StateValue::Float(n as f32)),
                other => Some(other),
            });
        assert_eq!(new_id, id);
        assert_eq!(outcome, StateMigration::Widened);
        assert_eq!(store.get(new_id), Some(StateValue::Float(3.0)));
    }

    #[test]
    fn migrate_resets_on_incompatible_or_new() {
        let mut store = StateStore::new();
        let key = StateKey::from_parts(4, 4);

        // New key: allocated fresh, reported Reset.
        let (id, outcome) = store.migrate_state(key, StateValue::Bool(true), |_p, _n| None);
        assert_eq!(outcome, StateMigration::Reset);
        assert_eq!(store.get(id), Some(StateValue::Bool(true)));

        // Existing key but incompatible change: value reset, identity kept.
        store.set(id, StateValue::Bool(false));
        let (again, outcome) = store.migrate_state(key, StateValue::Bool(true), |_p, _n| None);
        assert_eq!(again, id, "reset reuses the same slot");
        assert_eq!(outcome, StateMigration::Reset);
        assert_eq!(store.get(again), Some(StateValue::Bool(true)));
    }

    #[test]
    fn freeing_a_keyed_cell_drops_the_mapping() {
        let mut store = StateStore::new();
        let key = StateKey::from_parts(5, 6);
        let id = store.alloc(StateValue::Int(1));
        store.bind_key(id, key);
        assert_eq!(store.id_for_key(key), Some(id));
        store.free(id);
        assert_eq!(store.id_for_key(key), None, "freed cell drops its key");
    }

    #[test]
    fn reused_slot_bumps_generation() {
        let mut store = StateStore::new();
        let a = store.alloc(StateValue::Int(1));
        store.free(a);
        let b = store.alloc(StateValue::Int(2));
        assert_eq!(a.index(), b.index());
        assert_ne!(a.generation(), b.generation());
        assert!(!store.is_live(a));
        assert!(store.is_live(b));
    }
}
