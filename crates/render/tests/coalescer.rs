//! F4.4 dirty-range coalescing (§9.3): the pool turns the slots that changed
//! this frame into a few contiguous upload ranges — bridging short gaps of clean
//! slots to avoid many tiny `write_buffer` calls, splitting on wide gaps — so a
//! local change stays a local upload and an unchanged frame uploads nothing.
//!
//! [`InstancePool::sync`] returns the number of `write_buffer` calls it issued,
//! which equals the number of coalesced ranges. These tests drive `sync`
//! against a real [`HeadlessRaster`] and assert that count for the shapes §9.3
//! cares about; the pure gap-threshold arithmetic is unit-tested inside the
//! coalescer module. The bytes reaching the GPU are unaffected by bridging —
//! a bridged clean slot re-uploads the value already there — which the golden
//! test pins separately.

use viso_gpu::{BufferUsage, HeadlessRaster};
use viso_render::QuadInstance;
use viso_render::pool::{GAP_THRESHOLD, InstancePool};

/// A quad instance whose bytes are fully determined by `tag`, so distinct tags
/// always compare unequal (a change) and equal tags never do (a stable slot).
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

/// A pool warmed to `n` live slots against a fresh backend.
fn warmed(
    n: usize,
) -> (
    HeadlessRaster,
    InstancePool<QuadInstance>,
    Vec<QuadInstance>,
) {
    let mut gpu = HeadlessRaster::new();
    let mut pool: InstancePool<QuadInstance> = InstancePool::new(BufferUsage::INSTANCE, "quads");
    let base = frame(n);
    pool.sync(&mut gpu, &base);
    (gpu, pool, base)
}

/// A single changed slot (the hover case) uploads exactly one minimal range.
#[test]
fn one_dirty_slot_is_one_range() {
    let (mut gpu, mut pool, base) = warmed(32);
    let mut next = base;
    next[10] = quad(910.0);
    assert_eq!(pool.sync(&mut gpu, &next), 1);
}

/// No changed slots upload nothing — zero `write_buffer` calls for the family.
#[test]
fn no_dirty_slots_upload_nothing() {
    let (mut gpu, mut pool, base) = warmed(32);
    assert_eq!(pool.sync(&mut gpu, &base), 0);
}

/// Two changes separated by fewer than the threshold's clean slots bridge into
/// one range rather than issuing two tiny copies.
#[test]
fn changes_within_gap_threshold_merge_into_one_range() {
    let (mut gpu, mut pool, base) = warmed(64);
    let mut next = base;
    // A clean gap strictly below the threshold between the two changed slots.
    let a = 5;
    let b = a + 1 + (GAP_THRESHOLD - 1);
    next[a] = quad(900.0 + a as f32);
    next[b] = quad(900.0 + b as f32);
    assert_eq!(pool.sync(&mut gpu, &next), 1);
}

/// Two changes separated by more than the threshold's clean slots split into
/// two ranges — shipping the wide clean gap would cost more than a second call.
#[test]
fn changes_past_gap_threshold_split_into_two_ranges() {
    let (mut gpu, mut pool, base) = warmed(64);
    let mut next = base;
    let a = 5;
    let b = a + 2 + GAP_THRESHOLD; // one clean slot past the threshold
    next[a] = quad(900.0 + a as f32);
    next[b] = quad(900.0 + b as f32);
    assert_eq!(pool.sync(&mut gpu, &next), 2);
}

/// Many scattered changes collapse to the number of clusters, not the number of
/// changed slots: three tight clusters far apart -> three ranges, not nine.
#[test]
fn scattered_clusters_collapse_to_cluster_count() {
    let (mut gpu, mut pool, base) = warmed(128);
    let mut next = base;
    // Three clusters of three tightly-packed changes each, clusters far apart.
    for &origin in &[0usize, 50, 100] {
        next[origin] = quad(1000.0 + origin as f32);
        next[origin + 1] = quad(1001.0 + origin as f32);
        next[origin + 2] = quad(1002.0 + origin as f32);
    }
    assert_eq!(pool.sync(&mut gpu, &next), 3);
}

/// The pool's coalescing scratch does not allocate across warmed frames: once a
/// frame's range count has been reached, later frames within it reuse capacity.
/// Two identical repaint workloads must allocate identically (the steady-state
/// bench pins this globally; here we assert the per-frame `sync` is stable).
#[test]
fn repeated_scattered_repaints_are_stable() {
    let (mut gpu, mut pool, base) = warmed(128);
    // Warm the scratch to a multi-range workload.
    let mut a = base.clone();
    for &o in &[0usize, 50, 100] {
        a[o] = quad(2000.0 + o as f32);
    }
    assert_eq!(pool.sync(&mut gpu, &a), 3);

    // A second, structurally identical repaint issues the same range count.
    let mut b = a.clone();
    for &o in &[0usize, 50, 100] {
        b[o] = quad(3000.0 + o as f32);
    }
    assert_eq!(pool.sync(&mut gpu, &b), 3);
}
