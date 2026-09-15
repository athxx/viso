//! F4.2 persistent instance pool invariants (§9.1): a local change costs a
//! local upload, never a full-scene re-upload.
//!
//! [`InstancePool`] keeps a CPU shadow of what currently sits in its device
//! buffer and, on [`sync`](InstancePool::sync), diffs the freshly lowered
//! draw-order array against that shadow, uploading only the maximal runs of
//! changed slots. These tests drive `sync` against a real [`HeadlessRaster`]
//! and assert the §9.1 contract through its observable surface — the write
//! count `sync` returns, `buffer()`/`capacity()`/`len()`, and the backend's
//! `buffer_count()` (create/retire accounting) — to prove:
//!
//! - a first non-empty frame creates the buffer and uploads once;
//! - an unchanged frame issues **zero** uploads;
//! - a one-slot repaint issues **one** minimal upload, and a stable slot keeps
//!   its place across frames (never re-uploaded);
//! - two separated changed slots issue two runs, adjacent ones coalesce to one;
//! - emptying the frame draws nothing and leaves no live slots;
//! - exceeding capacity grows once (buffer_count +1, a new buffer identity,
//!   the old buffer retired) and forces a single full upload.

use viso_gpu::{BufferUsage, HeadlessRaster};
use viso_render::QuadInstance;
use viso_render::pool::InstancePool;

/// A quad instance whose bytes are fully determined by `tag`, so two instances
/// with different tags always compare unequal (a repaint) and equal tags never
/// do (a stable slot).
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

/// A new empty pool never touches the GPU.
#[test]
fn empty_pool_has_no_buffer() {
    let gpu = HeadlessRaster::new();
    let pool: InstancePool<QuadInstance> = InstancePool::new(BufferUsage::INSTANCE, "test-quads");
    assert_eq!(pool.buffer(), None);
    assert_eq!(pool.capacity(), 0);
    assert_eq!(pool.len(), 0);
    assert!(pool.is_empty());
    assert_eq!(gpu.buffer_count(), 0);
}

/// First non-empty sync creates one buffer and uploads the whole array once;
/// re-syncing the identical array uploads nothing.
#[test]
fn first_frame_uploads_once_then_unchanged_uploads_zero() {
    let mut gpu = HeadlessRaster::new();
    let mut pool: InstancePool<QuadInstance> =
        InstancePool::new(BufferUsage::INSTANCE, "test-quads");

    let frame = [quad(1.0), quad(2.0), quad(3.0)];

    // First frame: buffer created, one full upload.
    assert_eq!(pool.sync(&mut gpu, &frame), 1);
    assert_eq!(gpu.buffer_count(), 1);
    assert!(pool.buffer().is_some());
    assert_eq!(pool.len(), 3);
    assert!(pool.capacity() >= 3);

    // Identical frame: byte-for-byte match against the shadow, zero uploads,
    // same buffer, no new allocation.
    let buffer_before = pool.buffer();
    assert_eq!(pool.sync(&mut gpu, &frame), 0);
    assert_eq!(gpu.buffer_count(), 1);
    assert_eq!(pool.buffer(), buffer_before);
    assert_eq!(pool.len(), 3);
}

/// A single-slot repaint uploads exactly one run; the unchanged slots keep
/// their place and are not re-uploaded.
#[test]
fn one_slot_change_uploads_one_run() {
    let mut gpu = HeadlessRaster::new();
    let mut pool: InstancePool<QuadInstance> =
        InstancePool::new(BufferUsage::INSTANCE, "test-quads");

    let base = [quad(1.0), quad(2.0), quad(3.0)];
    pool.sync(&mut gpu, &base);
    let buffer = pool.buffer();

    // Repaint only the middle slot.
    let mut changed = base;
    changed[1] = quad(9.0);
    assert_eq!(pool.sync(&mut gpu, &changed), 1);
    // No grow: same buffer, no new create.
    assert_eq!(pool.buffer(), buffer);
    assert_eq!(gpu.buffer_count(), 1);

    // Settle: re-syncing the now-current array uploads nothing.
    assert_eq!(pool.sync(&mut gpu, &changed), 0);
}

/// Two non-adjacent changed slots upload as two separate runs; two adjacent
/// changed slots coalesce into one run.
#[test]
fn separated_changes_split_adjacent_changes_merge() {
    let mut gpu = HeadlessRaster::new();
    let mut pool: InstancePool<QuadInstance> =
        InstancePool::new(BufferUsage::INSTANCE, "test-quads");

    let base = [quad(1.0), quad(2.0), quad(3.0), quad(4.0), quad(5.0)];
    pool.sync(&mut gpu, &base);

    // Change slots 1 and 3 (a gap at slot 2 keeps them separate) -> two runs.
    let mut split = base;
    split[1] = quad(20.0);
    split[3] = quad(40.0);
    assert_eq!(pool.sync(&mut gpu, &split), 2);

    // Change slots 1 and 2 (adjacent) -> one coalesced run.
    let mut merged = split;
    merged[1] = quad(21.0);
    merged[2] = quad(31.0);
    assert_eq!(pool.sync(&mut gpu, &merged), 1);
}

/// An empty frame draws nothing and leaves no live slots, but keeps the buffer
/// (no shrink); re-adding slots re-uploads them.
#[test]
fn empty_frame_clears_live_slots_without_destroying_buffer() {
    let mut gpu = HeadlessRaster::new();
    let mut pool: InstancePool<QuadInstance> =
        InstancePool::new(BufferUsage::INSTANCE, "test-quads");

    let frame = [quad(1.0), quad(2.0)];
    pool.sync(&mut gpu, &frame);
    let buffer = pool.buffer();
    assert_eq!(gpu.buffer_count(), 1);

    // Empty frame: nothing drawn, no live slots, buffer retained.
    assert_eq!(pool.sync(&mut gpu, &[]), 0);
    assert_eq!(pool.len(), 0);
    assert!(pool.is_empty());
    assert_eq!(pool.buffer(), buffer);
    assert_eq!(gpu.buffer_count(), 1);

    // Re-adding slots re-uploads against the emptied shadow.
    assert_eq!(pool.sync(&mut gpu, &frame), 1);
    assert_eq!(pool.len(), 2);
}

/// Exceeding capacity grows once: a new buffer is created (buffer_count +1,
/// a fresh identity) and the old buffer is retired through the deferred path.
/// The grow forces a single full upload.
#[test]
fn growth_creates_new_buffer_and_retires_old() {
    let mut gpu = HeadlessRaster::new();
    let mut pool: InstancePool<QuadInstance> =
        InstancePool::new(BufferUsage::INSTANCE, "test-quads");

    // First frame establishes some capacity.
    let small = [quad(1.0), quad(2.0), quad(3.0)];
    pool.sync(&mut gpu, &small);
    let cap0 = pool.capacity();
    let buffer0 = pool.buffer();
    assert_eq!(gpu.buffer_count(), 1);

    // A frame larger than capacity forces exactly one grow.
    let big: Vec<QuadInstance> = (0..cap0 + 1).map(|i| quad(i as f32)).collect();
    assert_eq!(pool.sync(&mut gpu, &big), 1);

    // A new buffer was created (create count +1) and the identity changed; the
    // old buffer was handed to the retire path rather than leaked.
    assert_eq!(gpu.buffer_count(), 2);
    assert_ne!(pool.buffer(), buffer0);
    assert!(pool.capacity() > cap0);
    assert_eq!(pool.len(), big.len());

    // After the grow the shadow mirrors the new contents: a re-sync is silent.
    assert_eq!(pool.sync(&mut gpu, &big), 0);
    // No spurious second grow.
    assert_eq!(gpu.buffer_count(), 2);
}

/// The pool works with the `u32` index stream (a POD element type that is not a
/// `#[derive(GpuPod)]` instance struct): the same diff/upload contract holds.
#[test]
fn u32_index_stream_diffs_like_instances() {
    let mut gpu = HeadlessRaster::new();
    let mut pool: InstancePool<u32> = InstancePool::new(BufferUsage::INDEX, "test-indices");

    let base = [0u32, 1, 2, 0, 2, 3];
    assert_eq!(pool.sync(&mut gpu, &base), 1);
    assert_eq!(pool.sync(&mut gpu, &base), 0);

    let mut changed = base;
    changed[4] = 5;
    assert_eq!(pool.sync(&mut gpu, &changed), 1);
    assert_eq!(pool.len(), 6);
}
