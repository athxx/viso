//! F4.3 fence-recycle contract (§9.2): the only device buffers that ever churn
//! are an [`InstancePool`]'s backing buffer when it grows, and those are freed
//! through F1's deferred-destruction path — never in the frame that still reads
//! them, never leaked afterwards.
//!
//! There is no separate transient upload ring in this architecture: the five
//! persistent pools are the sole `write_buffer` consumers (an unchanged frame
//! uploads nothing, which a ring could not match), uniforms ride inline by value
//! in the draw command, and offscreen layers reuse a texture pool. So §9.2's
//! intent — no per-frame buffer realloc, fence-safe reclaim of what does churn —
//! reduces to the grow path, which these tests exercise against a real
//! [`HeadlessRaster`] through its observable surface:
//!
//! - `retired_count()` — how many slots are parked awaiting reclamation;
//! - `buffer_count()` — cumulative buffer creates (never decreases on reclaim);
//! - `begin_frame`/`present` — the epoch/fence cycle that gates reclamation.
//!
//! The headless backend completes a frame the instant it is presented, so a
//! buffer retired while building frame N is reclaimed by the `begin_frame` that
//! opens frame N+1 (a genuine one-frame deferral), and not before.

use viso_gpu::{BufferUsage, GpuBackend, HeadlessRaster, RawWindowHandle};
use viso_render::QuadInstance;
use viso_render::pool::InstancePool;

/// A quad instance whose bytes are fully determined by `tag`.
fn quad(tag: f32) -> QuadInstance {
    QuadInstance {
        rect_pos: [tag, tag],
        rect_size: [10.0, 10.0],
        color: [tag, 0.0, 0.0, 1.0],
        radius: 0.0,
        border_width: 0.0,
        border_color: [0.0, 0.0, 0.0, 0.0],
    }
}

/// A frame of `n` distinct quads.
fn frame(n: usize) -> Vec<QuadInstance> {
    (0..n).map(|i| quad(i as f32)).collect()
}

/// Growing the pool retires the old buffer into the deferred-destruction queue
/// rather than freeing it immediately: within the frame the grow happened in,
/// the old buffer is still in flight (the fence has not reached that epoch), so
/// it stays parked and is not reclaimed.
#[test]
fn grow_parks_old_buffer_without_freeing_it_in_flight() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, 16, 16);
    let mut pool: InstancePool<QuadInstance> = InstancePool::new(BufferUsage::INSTANCE, "quads");

    // Frame 0: establish a buffer.
    gpu.begin_frame(surface).expect("acquire frame 0");
    pool.sync(&mut gpu, &frame(3));
    let cap0 = pool.capacity();
    assert_eq!(gpu.buffer_count(), 1);
    assert_eq!(gpu.retired_count(), 0);

    // Frame 1: exceed capacity -> grow. The old buffer is retired (parked), but
    // this frame is still in flight, so it must NOT be reclaimed yet.
    gpu.begin_frame(surface).expect("acquire frame 1");
    pool.sync(&mut gpu, &frame(cap0 + 1));
    assert_eq!(gpu.buffer_count(), 2, "grow created a new buffer");
    assert_eq!(
        gpu.retired_count(),
        1,
        "old buffer parked for deferred destruction, not freed in-flight"
    );
}

/// The parked buffer is reclaimed by the first `begin_frame` after the frame it
/// was retired in is presented — exactly once, never earlier, never leaked.
#[test]
fn presented_frame_reclaims_the_retired_buffer_next_begin() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, 16, 16);
    let mut pool: InstancePool<QuadInstance> = InstancePool::new(BufferUsage::INSTANCE, "quads");

    // Frame 0: buffer created and presented.
    let f0 = gpu.begin_frame(surface).expect("acquire frame 0");
    pool.sync(&mut gpu, &frame(3));
    let cap0 = pool.capacity();
    gpu.present(f0);

    // Frame 1: grow (retires the old buffer), then present so its epoch completes.
    let f1 = gpu.begin_frame(surface).expect("acquire frame 1");
    pool.sync(&mut gpu, &frame(cap0 + 1));
    assert_eq!(gpu.retired_count(), 1, "retired during frame 1");
    gpu.present(f1);
    // Still parked: reclamation happens at the *next* begin_frame, not at present.
    assert_eq!(
        gpu.retired_count(),
        1,
        "not reclaimed before the fence is checked"
    );

    // Frame 2: opening it reclaims everything the fence has now passed — the old
    // buffer retired in frame 1, since frame 1 was presented.
    gpu.begin_frame(surface).expect("acquire frame 2");
    assert_eq!(
        gpu.retired_count(),
        0,
        "old buffer reclaimed once frame 1 completed"
    );
    // Reclamation returns the slot to its store; it does not create anything, so
    // the cumulative create count is unmoved.
    assert_eq!(
        gpu.buffer_count(),
        2,
        "reclaim frees a slot, never creates one"
    );
}

/// Several grows within one frame all park separately and all reclaim together
/// one frame later — the queue drains to empty, so nothing leaks even when a
/// single frame churns the buffer more than once.
#[test]
fn multiple_grows_in_one_frame_all_reclaim_one_frame_later() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, 16, 16);
    let mut pool: InstancePool<QuadInstance> = InstancePool::new(BufferUsage::INSTANCE, "quads");

    // Frame 0: four buffer creates in the same frame — the first fill plus three
    // grows past it — so three predecessors are parked (each grow retires the
    // buffer it replaced; the very first create had no predecessor).
    let f0 = gpu.begin_frame(surface).expect("acquire frame 0");
    pool.sync(&mut gpu, &frame(1));
    let c1 = pool.capacity();
    pool.sync(&mut gpu, &frame(c1 + 1));
    let c2 = pool.capacity();
    pool.sync(&mut gpu, &frame(c2 + 1));
    let c3 = pool.capacity();
    pool.sync(&mut gpu, &frame(c3 + 1));
    assert_eq!(gpu.buffer_count(), 4, "one initial create + three grows");
    assert_eq!(gpu.retired_count(), 3, "each grow parked its predecessor");
    gpu.present(f0);

    // Frame 1: opening it drains the whole batch retired in frame 0 at once.
    gpu.begin_frame(surface).expect("acquire frame 1");
    assert_eq!(
        gpu.retired_count(),
        0,
        "the full retired batch drained together"
    );
    assert_eq!(
        gpu.buffer_count(),
        4,
        "reclaim frees slots, creates nothing"
    );
}

/// Steady state never touches the retire queue: once the high-water mark is
/// reached, repainting (even a full re-fill) grows nothing, retires nothing, and
/// leaves the queue empty frame after frame.
#[test]
fn steady_state_retires_nothing() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, 16, 16);
    let mut pool: InstancePool<QuadInstance> = InstancePool::new(BufferUsage::INSTANCE, "quads");

    // Warm to the high-water mark.
    let f = gpu.begin_frame(surface).expect("warm frame");
    pool.sync(&mut gpu, &frame(8));
    gpu.present(f);
    let created_after_warm = gpu.buffer_count();

    // Many steady frames within capacity: full re-fills and one-slot repaints.
    for i in 0..16 {
        let f = gpu.begin_frame(surface).expect("steady frame");
        let mut live = frame(8);
        // Repaint one slot so the pool does real work but never grows.
        live[i % 8] = quad(100.0 + i as f32);
        pool.sync(&mut gpu, &live);
        gpu.present(f);
        assert_eq!(gpu.retired_count(), 0, "steady frame parks nothing");
    }

    // No buffer was ever recreated after warm-up: the grow/retire path is cold.
    assert_eq!(gpu.buffer_count(), created_after_warm);
    assert_eq!(gpu.retired_count(), 0);
}

/// `device_lost` must not strand a parked buffer: a backend that loses its device
/// mid-frame signals the current epoch and drains, so a buffer retired in an
/// in-flight frame is freed rather than leaked when no further `begin_frame` comes.
#[test]
fn device_loss_drains_parked_buffers() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, 16, 16);
    let mut pool: InstancePool<QuadInstance> = InstancePool::new(BufferUsage::INSTANCE, "quads");

    gpu.begin_frame(surface).expect("acquire frame 0");
    pool.sync(&mut gpu, &frame(3));
    let cap0 = pool.capacity();

    // Grow in an in-flight frame, then lose the device before presenting.
    gpu.begin_frame(surface).expect("acquire frame 1");
    pool.sync(&mut gpu, &frame(cap0 + 1));
    assert_eq!(gpu.retired_count(), 1);

    gpu.device_lost(surface);
    assert_eq!(
        gpu.retired_count(),
        0,
        "device loss drains the stalled queue rather than leaking it"
    );
}
