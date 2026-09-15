//! F4 frozen-contract pin: the persistent data path's packed batch key, render
//! chunk projection, dirty-range coalescer, and instance-pool upload discipline
//! (§9.1, §9.3, §9.6). D/C/E/M/A build their draw submission and incremental
//! rebuild on exactly these shapes — a field widened in the key, a chunk stage
//! dropped, the coalescer's bridge threshold moved, or the pool re-uploading a
//! clean frame would ripple invisibly into every consumer.
//!
//! The per-module unit tests already guard each piece in isolation; this
//! integration test is the single consolidated gate that fails loudly the moment
//! the *contract* — not one module's internals — moves.

use std::mem::size_of;

use viso_gpu::{BindGroupId, BufferUsage, GpuBackend, HeadlessRaster, RawWindowHandle};
use viso_render::batch::planner::{BatchItem, joins};
use viso_render::pool::{GAP_THRESHOLD, InstancePool, Range, coalesce};
use viso_render::{BatchFamily, BatchKey, BatchTarget, Rect, RenderChunk, RenderChunkId};

/// The four pipeline families and their frozen low-three-bit tags (§9.6). The
/// tag lives in `BatchKey` bits `0..3`; Quad and Mesh are the mergeable pair.
#[test]
fn batch_family_tags_and_mergeability_are_frozen() {
    for (family, tag, mergeable) in [
        (BatchFamily::Quad, 0u64, true),
        (BatchFamily::Image, 1, false),
        (BatchFamily::GlyphRun, 2, false),
        (BatchFamily::Mesh, 3, true),
    ] {
        let key = BatchKey::pack(family, BatchTarget::Main, None);
        assert_eq!(key.family(), family, "family round-trips through the key");
        assert_eq!(key.bits() & 0b111, tag, "family tag in the low three bits");
        assert_eq!(
            family.mergeable(),
            mergeable,
            "only Quad and Mesh instances merge into one draw"
        );
    }
}

/// The render target packs `0` for the surface and `i + 1` for offscreen pass
/// `i`, into `BatchKey` bits `14..24`.
#[test]
fn batch_target_field_encoding_is_frozen() {
    let main = BatchKey::pack(BatchFamily::Quad, BatchTarget::Main, None);
    assert_eq!(main.target_field(), 0, "Main is render target 0");

    for i in [0usize, 1, 3, 42] {
        let off = BatchKey::pack(BatchFamily::Quad, BatchTarget::Offscreen(i), None);
        assert_eq!(off.target_field(), i as u64 + 1, "Offscreen(i) packs i + 1");
    }
}

/// The three live dimensions — family (bits `0..3`), render target (`14..24`),
/// and bound resource (`24..48`) — occupy disjoint bit ranges: no two of them
/// alias, so a key uniquely identifies its `(family, target, resource)` triple.
/// This is the property that lets adjacency merge on key equality alone.
#[test]
fn batch_key_dimensions_do_not_alias() {
    let base = BatchKey::pack(BatchFamily::Quad, BatchTarget::Main, None);
    assert_eq!(base.bits(), 0, "the all-default key is zero");

    // Each dimension moved alone changes the key, and changes only its own field.
    let family = BatchKey::pack(BatchFamily::Mesh, BatchTarget::Main, None);
    assert_ne!(family, base);
    assert_eq!(family.target_field(), 0);
    assert_eq!(family.resource_field(), 0);

    let target = BatchKey::pack(BatchFamily::Quad, BatchTarget::Offscreen(0), None);
    assert_ne!(target, base);
    assert_eq!(target.family(), BatchFamily::Quad);
    assert_eq!(target.resource_field(), 0);

    let resource = BatchKey::pack(
        BatchFamily::Quad,
        BatchTarget::Main,
        Some(BindGroupId::new(9)),
    );
    assert_ne!(resource, base);
    assert_eq!(resource.family(), BatchFamily::Quad);
    assert_eq!(resource.target_field(), 0);
    assert_eq!(
        resource.resource_field(),
        9,
        "resource packs the bind-group index"
    );

    // The largest value each live field admits round-trips without spilling into
    // its neighbour: target is 10 bits, resource is 24 bits.
    let max_target = BatchKey::pack(BatchFamily::Quad, BatchTarget::Offscreen(0x3fe), None);
    assert_eq!(max_target.target_field(), 0x3ff);
    assert_eq!(
        max_target.resource_field(),
        0,
        "a full target field leaves resource clear"
    );

    let max_resource = BatchKey::pack(
        BatchFamily::Quad,
        BatchTarget::Main,
        Some(BindGroupId::new(0xff_ffff)),
    );
    assert_eq!(max_resource.resource_field(), 0xff_ffff);
    assert_eq!(
        max_resource.target_field(),
        0,
        "a full resource field leaves target clear"
    );
}

/// `joins` is the single adjacency predicate every merge site routes through:
/// two items share a draw iff both are mergeable, their keys are equal, and
/// their clips match structurally. Any one differing is a hard barrier.
#[test]
fn joins_adjacency_predicate_is_frozen() {
    let key = BatchKey::pack(BatchFamily::Quad, BatchTarget::Main, None);
    let clip = Some(Rect {
        x: 0.0,
        y: 0.0,
        w: 4.0,
        h: 4.0,
    });
    let item = |key, clip, mergeable| BatchItem {
        key,
        clip,
        mergeable,
    };

    let a = item(key, clip, true);
    assert!(joins(&a, &a), "identical mergeable items join");

    // Unmergeable on either side is a barrier.
    assert!(!joins(&a, &item(key, clip, false)));
    assert!(!joins(&item(key, clip, false), &a));

    // A different key or clip splits.
    let other_key = BatchKey::pack(BatchFamily::Quad, BatchTarget::Offscreen(0), None);
    assert!(!joins(&a, &item(other_key, clip, true)));
    assert!(!joins(&a, &item(key, None, true)));
}

/// A render chunk carries exactly the frozen field set `{ key, family, clip,
/// geometry, order }` and grows by `open` + `absorb`: `open` starts a one-wide
/// order span, each `absorb` extends the geometry range and the order span by
/// one. `RenderChunkId` is a transparent `u32` handle.
#[test]
fn render_chunk_shape_and_growth_are_frozen() {
    assert_eq!(size_of::<RenderChunkId>(), size_of::<u32>());
    assert_eq!(RenderChunkId(7).0, 7);

    let key = BatchKey::pack(BatchFamily::Quad, BatchTarget::Main, None);
    let mut chunk = RenderChunk::open(key, BatchFamily::Quad, None, 10, 1, 3);
    assert_eq!(chunk.key, key);
    assert_eq!(chunk.family, BatchFamily::Quad);
    assert_eq!(chunk.clip, None);
    assert_eq!(chunk.geometry, (10, 1), "open starts (geom_start, count)");
    assert_eq!(chunk.order, (3, 4), "open covers one paint-order position");

    // Two absorbs of two more units each: geometry count grows by the units, the
    // order span grows by one position per absorbed primitive.
    chunk.absorb(2);
    chunk.absorb(2);
    assert_eq!(chunk.geometry, (10, 5), "geometry count accumulates");
    assert_eq!(chunk.order, (3, 6), "order span extends one per absorb");
}

/// The dirty-range coalescer's contract (§9.3): a sorted, strictly-increasing
/// list of changed slots becomes contiguous `Range { start, len }` upload spans,
/// bridging clean gaps of at most `GAP_THRESHOLD` and splitting past it. One
/// changed slot is one minimal range (the hover case); no changes are no ranges.
#[test]
fn coalescer_ranges_and_thresholds_are_frozen() {
    assert_eq!(
        GAP_THRESHOLD, 4,
        "the bridge/split break-even point is frozen"
    );
    assert_eq!(
        size_of::<Range>(),
        2 * size_of::<usize>(),
        "Range is {{ start, len }}"
    );

    let ranges = |dirty: &[usize]| {
        let mut out = Vec::new();
        coalesce(dirty, &mut out);
        out
    };

    // No changes → no uploads. One change → one minimal one-slot range.
    assert!(ranges(&[]).is_empty());
    assert_eq!(ranges(&[7]), vec![Range { start: 7, len: 1 }]);

    // A gap of exactly GAP_THRESHOLD clean slots bridges into one range; one more
    // splits into two.
    let bridged = ranges(&[2, 2 + 1 + GAP_THRESHOLD]);
    assert_eq!(
        bridged,
        vec![Range {
            start: 2,
            len: 2 + GAP_THRESHOLD
        }]
    );
    let split = ranges(&[2, 2 + 2 + GAP_THRESHOLD]);
    assert_eq!(split.len(), 2, "a wider gap splits");
    assert_eq!(split[0], Range { start: 2, len: 1 });
}

/// The instance pool's upload discipline (§9.1): a pool is a grow-only device
/// buffer diffed against a CPU shadow. An unchanged frame uploads zero bytes and
/// issues zero `write_buffer` calls; a one-slot change uploads exactly one slot's
/// bytes; the buffer grows (never shrinks) and its capacity is a high-water mark.
#[test]
fn instance_pool_upload_discipline_is_frozen() {
    let mut gpu = HeadlessRaster::new();
    // A surface is created so the headless backend is fully live, matching how a
    // renderer drives its pools.
    let surface = gpu.create_surface(RawWindowHandle::Headless, 16, 16);
    let _ = gpu.surface_format(surface);

    let mut pool: InstancePool<u32> = InstancePool::new(BufferUsage::INSTANCE, "frozen-pool");
    assert!(pool.is_empty());
    assert_eq!(pool.capacity(), 0, "no device buffer until the first sync");

    // First non-empty sync: one full upload of four slots.
    let writes = pool.sync(&mut gpu, &[10, 20, 30, 40]);
    assert_eq!(writes, 1, "the initial fill is one contiguous upload");
    assert_eq!(pool.len(), 4);
    assert_eq!(pool.last_upload_bytes(), 4 * size_of::<u32>());
    let cap = pool.capacity();
    assert!(cap >= 4);

    // An identical frame diffs to nothing: zero uploads, zero bytes.
    let writes = pool.sync(&mut gpu, &[10, 20, 30, 40]);
    assert_eq!(writes, 0, "an unchanged frame uploads nothing");
    assert_eq!(pool.last_upload_bytes(), 0);

    // One changed slot: one minimal upload of exactly that slot's bytes.
    let writes = pool.sync(&mut gpu, &[10, 20, 99, 40]);
    assert_eq!(writes, 1, "a one-slot change is one upload");
    assert_eq!(
        pool.last_upload_bytes(),
        size_of::<u32>(),
        "exactly one slot's bytes"
    );

    // A shorter frame does not shrink capacity — the buffer is a high-water mark.
    pool.sync(&mut gpu, &[1, 2]);
    assert_eq!(pool.capacity(), cap, "capacity never shrinks");
    assert_eq!(pool.len(), 2);
}
