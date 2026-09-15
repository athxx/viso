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
    let frame = gpu
        .begin_frame(surface)
        .expect("headless acquire never fails");
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
    gpu.begin_frame(surface)
        .expect("headless acquire never fails");
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
        let frame = gpu
            .begin_frame(surface)
            .expect("headless acquire never fails");

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
    let frame = gpu
        .begin_frame(surface)
        .expect("headless acquire never fails");
    gpu.present(frame);
    gpu.begin_frame(surface)
        .expect("headless acquire never fails");
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
    let frame = gpu
        .begin_frame(surface)
        .expect("headless acquire never fails");
    gpu.present(frame);
    gpu.begin_frame(surface)
        .expect("headless acquire never fails");
    gpu.write_buffer(live, 0, &[2u8; 64]);
}

// --- Surface lifecycle: device loss, resize, out-of-date acquire -------------
//
// `begin_frame` is fallible: a `None` acquire is a transient skip that must not
// advance the epoch, or it would park a frame no `present` ever completes and
// stall the retire queue behind it. `device_lost` exists to break exactly that
// stall — dropping the held drawable and treating the current epoch as finished
// so parked slots reclaim. `resize_surface` re-lays-out the surface without
// leaking the drawable it may have been holding. Driven through the public API
// on the headless backend, where a presented frame completes immediately.

/// A frame opened but abandoned — presented via neither `present` nor a clean
/// second `begin_frame` — would leave the fence one epoch behind forever, so a
/// slot retired in that frame could never reclaim. `device_lost` treats the
/// current epoch as finished and drains the stalled queue.
#[test]
fn device_lost_unblocks_a_stalled_retire_queue() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, 8, 8);

    let victim = gpu.create_buffer(&buf());

    // Open a frame and retire a buffer into it, then abandon the frame: no
    // `present` runs, so the fence never advances past the previous epoch and
    // the parked slot is stuck.
    gpu.begin_frame(surface)
        .expect("headless acquire never fails");
    gpu.destroy_buffer(victim);
    assert_eq!(gpu.retired_count(), 1, "the slot is parked in this frame");

    // A plain next frame cannot save it: its reclaim runs against a fence that
    // still trails the parked epoch, so the slot stays parked.
    gpu.begin_frame(surface)
        .expect("headless acquire never fails");
    assert_eq!(
        gpu.retired_count(),
        1,
        "without completion the parked slot cannot reclaim"
    );

    // Recover: `device_lost` signals the current epoch complete and drains.
    gpu.device_lost(surface);
    assert_eq!(
        gpu.retired_count(),
        0,
        "device_lost treats the current epoch as finished and reclaims"
    );

    // The slot is back on the free-list: the next create reuses it at a bumped
    // generation, so the abandoned frame left no leak behind.
    let reused = gpu.create_buffer(&buf());
    assert_eq!(reused.index, victim.index);
    assert_ne!(reused.generation, victim.generation);

    // And the backend keeps drawing after recovery.
    let frame = gpu
        .begin_frame(surface)
        .expect("headless acquire succeeds again after recovery");
    gpu.present(frame);
}

/// A resize reallocates the surface's framebuffer to the new physical size and
/// drops any drawable held for the old geometry — the read-back buffer tracks
/// the new dimensions exactly, with no stale pixels left over.
#[test]
fn resize_reallocates_the_surface_without_leaking_the_old_drawable() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, 8, 8);

    // Four bytes (BGRA8) per pixel at the initial size.
    assert_eq!(gpu.read_pixels_bgra8(surface).len(), 8 * 8 * 4);

    // Open a frame so a drawable is held, then resize mid-frame: the held
    // drawable is for the old geometry and must be dropped, not presented.
    gpu.begin_frame(surface)
        .expect("headless acquire never fails");
    gpu.resize_surface(surface, 16, 4);

    // The framebuffer now tracks the new physical size exactly.
    assert_eq!(gpu.read_pixels_bgra8(surface).len(), 16 * 4 * 4);

    // The surface still acquires and presents cleanly at the new size — the
    // mid-frame resize left no half-opened frame wedged in the epoch.
    let frame = gpu
        .begin_frame(surface)
        .expect("headless acquire succeeds after resize");
    gpu.present(frame);
    gpu.begin_frame(surface)
        .expect("headless acquire never fails");
    assert_eq!(
        gpu.retired_count(),
        0,
        "a mid-frame resize parks no phantom epoch"
    );
}
