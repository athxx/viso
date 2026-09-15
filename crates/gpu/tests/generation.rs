//! The generation-safety contract of the backends' resource storage.
//!
//! Every backend keeps one `SlotMap` per resource kind; a resource handle is
//! the `{index, generation}` pair the map hands back at insert. The guarantee
//! this test pins down: once a slot is reclaimed and reused, the old handle and
//! the new handle share an index but differ in generation, so a stale handle
//! resolves to `None` rather than to whatever value later took the slot. A
//! wrong-object hit — the failure this scheme exists to make impossible — would
//! surface here as `Some(<the new value>)` for the stale handle.

use viso_gpu::slots::{RawId, SlotMap};

/// Reusing a reclaimed slot bumps its generation, so successive occupants of
/// one index are told apart by generation.
#[test]
fn reused_slot_gets_a_distinct_generation() {
    let mut map = SlotMap::new();

    let first = map.insert("first");
    map.remove(first);
    let second = map.insert("second");

    // The free-list handed the slot back, so the index repeats.
    assert_eq!(first.index, second.index);
    // But the generation moved on, so the two handles are not equal.
    assert_ne!(first.generation, second.generation);
    assert_ne!(first, second);
}

/// A handle left over from a removed value never resolves to the value that
/// later reused its slot — it resolves to `None`.
#[test]
fn stale_handle_never_resolves_to_a_wrong_object() {
    let mut map = SlotMap::new();

    let stale = map.insert(10u32);
    map.remove(stale);
    let live = map.insert(20u32);

    // Same slot, different generation.
    assert_eq!(stale.index, live.index);

    // The live handle sees its own value.
    assert_eq!(map.get(live), Some(&20));
    // The stale handle is detectably dead — not a silent hit on `20`.
    assert_eq!(map.get(stale), None);
    assert_eq!(map.get_mut(stale), None);
}

/// Generations advance across repeated reuse of the same index, and no earlier
/// handle ever comes back to life.
#[test]
fn generations_advance_across_repeated_reuse() {
    let mut map = SlotMap::new();
    let mut stale = Vec::new();

    let mut handle = map.insert(0u32);
    for value in 1..=8u32 {
        stale.push(handle);
        map.remove(handle);
        handle = map.insert(value);
        // Always the same slot, always a fresh generation.
        assert_eq!(handle.index, stale[0].index);
        assert!(stale.iter().all(|old| old.generation != handle.generation));
    }

    // Only the current handle is live; every prior one is dead.
    assert_eq!(map.get(handle), Some(&8));
    for old in &stale {
        assert_eq!(map.get(*old), None);
    }
}

/// A handle for an index the map never issued resolves to `None`, not a panic
/// or a neighboring slot.
#[test]
fn out_of_range_handle_resolves_to_none() {
    let map: SlotMap<u8> = SlotMap::new();
    assert_eq!(
        map.get(RawId {
            index: 7,
            generation: 0
        }),
        None
    );
}
