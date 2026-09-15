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
use viso_gpu::{BufferDesc, BufferUsage, GpuBackend, HeadlessRaster, RawWindowHandle};

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

// --- Deferred destruction, end-to-end through the backend --------------------
//
// The tests above pin the storage primitive; these pin the *reclamation timing*
// the backend layers on top of it: a destroyed resource's slot is parked, not
// freed, and becomes reusable only after the frame that could still be reading
// it has completed. Driven entirely through the public `GpuBackend` API on the
// headless backend (deterministic: a presented frame completes immediately, so
// the fence trails the current epoch by exactly the one in-flight frame).

fn buf() -> BufferDesc {
    BufferDesc {
        size: 64,
        usage: BufferUsage::INSTANCE | BufferUsage::CPU_WRITE,
        label: "generation-test",
    }
}

/// A slot freed by `destroy_*` is not reclaimed the instant it is destroyed: it
/// stays parked while its frame is in flight, so a buffer created in that window
/// takes a fresh slot rather than the parked one. Only after the frame is
/// presented and a later `begin_frame` drains the queue does the slot return to
/// the free-list — and the next create reuses it, at a bumped generation.
#[test]
fn destroyed_slot_is_reclaimed_only_after_its_frame_completes() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, 8, 8);

    let first = gpu.create_buffer(&buf());

    // Open a frame, then destroy `first` inside it: the slot is parked against
    // this frame's epoch, not freed.
    let frame = gpu.begin_frame(surface);
    gpu.destroy_buffer(first);
    assert_eq!(gpu.retired_count(), 1, "destroy parks the slot");

    // A create *while the parked slot's frame is still in flight* cannot reuse
    // it — the GPU might still be reading `first` this frame — so it appends.
    let during = gpu.create_buffer(&buf());
    assert_ne!(
        during.index, first.index,
        "a slot retired this frame must not be reused before the frame completes"
    );

    // Complete the frame. Now the parked slot's epoch is finished.
    gpu.present(frame);

    // The next frame drains the queue: `first`'s slot is freed (generation
    // bumped) and returns to the free-list.
    let _ = gpu.begin_frame(surface);
    assert_eq!(
        gpu.retired_count(),
        0,
        "the completed frame's slot is reclaimed"
    );

    // A create now reuses `first`'s slot — same index, higher generation.
    let reused = gpu.create_buffer(&buf());
    assert_eq!(
        reused.index, first.index,
        "the reclaimed slot returns to the free-list for reuse"
    );
    assert_ne!(
        reused.generation, first.generation,
        "reuse bumps the generation so the retired handle goes stale"
    );

    // The retired handle now aliases the reused slot's index but not its
    // generation — a write through it is a detectable miss, never a wrong-object
    // hit on `reused`'s buffer.
    assert_eq!(first.index, reused.index);
    assert_ne!(first, reused);
}

/// Repeatedly growing a buffer — destroy the old, create the new, every frame —
/// keeps the retire queue bounded by the in-flight window (never growing without
/// bound) and drains it to empty once frames stop. No slot is freed prematurely,
/// so each create is a genuine new allocation and the cumulative count advances
/// by exactly one per frame.
#[test]
fn repeated_growth_keeps_the_retire_queue_bounded() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, 8, 8);

    let mut current = gpu.create_buffer(&buf());
    let baseline = gpu.buffer_count();

    for frame_index in 0..64 {
        let frame = gpu.begin_frame(surface);

        // A growth: the old buffer is retired, a bigger one takes its place. The
        // new buffer is live this frame; the old one is parked for reclamation.
        gpu.destroy_buffer(current);
        current = gpu.create_buffer(&buf());

        // At most the one buffer retired this frame is parked: the previous
        // frame's retire drained at this `begin_frame`.
        assert!(
            gpu.retired_count() <= 1,
            "frame {frame_index}: retire queue grew past the in-flight window"
        );

        gpu.present(frame);

        // Each frame is a genuine create — no slot was freed early to corrupt
        // the count.
        assert_eq!(
            gpu.buffer_count(),
            baseline + frame_index + 1,
            "frame {frame_index}: buffer growth is a real allocation each frame"
        );
    }

    // Draining frame: the last retire completes and the queue empties.
    let frame = gpu.begin_frame(surface);
    gpu.present(frame);
    let _ = gpu.begin_frame(surface);
    assert_eq!(
        gpu.retired_count(),
        0,
        "the retire queue drains to empty once growth stops"
    );

    // The final buffer still resolves — the churn never touched the live slot.
    gpu.write_buffer(current, 0, &[1u8; 64]);
}

/// Retiring a handle that is already stale or was never issued is a harmless
/// no-op: it must not park a bogus entry that a later reclaim would use to free
/// an unrelated live slot.
#[test]
fn destroying_a_stale_handle_is_a_no_op() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, 8, 8);

    let live = gpu.create_buffer(&buf());
    let bogus = viso_gpu::BufferId {
        index: 999,
        generation: 7,
    };

    gpu.destroy_buffer(bogus);
    assert_eq!(gpu.retired_count(), 0, "an unknown handle parks nothing");

    // Cycle a frame; nothing was reclaimed, and the live buffer is untouched.
    let frame = gpu.begin_frame(surface);
    gpu.present(frame);
    let _ = gpu.begin_frame(surface);
    gpu.write_buffer(live, 0, &[2u8; 64]);
}
