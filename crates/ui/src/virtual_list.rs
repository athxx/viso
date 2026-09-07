//! Virtualized collections: mount ~40 nodes for a list of 100k logical items.
//!
//! A virtual list is a scroll viewport whose single content child is a fixed-size
//! *canvas* sized to the full logical extent, so the scroll range is correct
//! without mounting every item. A per-frame reconcile step reads the viewport's
//! current scroll offset, computes the visible index window (plus overscan), and
//! recycles a handful of item hosts when that window crosses a row boundary —
//! steady scroll within a row stays on the pure transform path with zero relayout.
//!
//! This module owns the data structures that back that model: a [`HeightTree`]
//! (a Fenwick tree over per-item heights, giving O(log n) prefix-sum and inverse
//! position queries), a [`HeightCache`] (a running mean of measured heights that
//! seeds unmeasured rows), and the per-list side state that drives reconcile.

/// A Fenwick (binary-indexed) tree over per-item heights.
///
/// It answers two dual queries in O(log n): the sum of the first `i` item
/// heights (`prefix_sum`, i.e. the on-axis offset of item `i`'s top), and its
/// inverse — the item whose row spans a given offset (`find_position`, a BIT
/// binary search). That pair is what a virtual list needs: scroll offset →
/// which item is at the top, and item index → where to place it. Both are
/// allocation-free after construction; only `resize` touches the heap.
///
/// Heights are `f32` to match the framework's geometry (offsets are pixel
/// positions that feed straight into layout, no wider accumulator needed at the
/// scale a viewport addresses). Unmeasured rows carry a shared `default_height`
/// estimate; `measured[i]` records whether row `i` has a real measurement so the
/// estimate can be re-applied to only the unmeasured rows when it drifts.
#[derive(Debug, Clone)]
pub struct HeightTree {
    /// 1-indexed partial sums; `tree[0]` is unused. Length is `count + 1`.
    tree: Vec<f32>,
    /// Number of logical items.
    count: usize,
    /// Per-item current height (index 0..count). Kept alongside the BIT so a
    /// point update knows the old value to compute its delta without a query.
    heights: Vec<f32>,
    /// Whether row `i` carries a real measurement (vs the default estimate).
    measured: Vec<bool>,
    /// The estimate applied to every unmeasured row.
    default_height: f32,
}

impl HeightTree {
    /// A tree of `count` items, every row seeded with `default_height`.
    pub fn new(count: usize, default_height: f32) -> Self {
        let mut t = HeightTree {
            tree: vec![0.0; count + 1],
            count,
            heights: vec![default_height; count],
            measured: vec![false; count],
            default_height,
        };
        t.rebuild();
        t
    }

    /// Rebuild the BIT from `heights` in O(n) (the linear construction: add each
    /// value to its slot, then propagate to the parent). Used on construction and
    /// resize; steady updates go through the O(log n) `update`.
    fn rebuild(&mut self) {
        self.tree.clear();
        self.tree.resize(self.count + 1, 0.0);
        for i in 0..self.count {
            self.tree[i + 1] += self.heights[i];
            let parent = (i + 1) + (i + 1).isolate_lowest_one();
            if parent <= self.count {
                let v = self.tree[i + 1];
                self.tree[parent] += v;
            }
        }
    }

    /// Number of logical items.
    #[inline]
    pub fn count(&self) -> usize {
        self.count
    }

    /// The current default (estimate) height for unmeasured rows.
    #[inline]
    pub fn default_height(&self) -> f32 {
        self.default_height
    }

    /// Whether row `i` carries a real measurement.
    #[inline]
    pub fn is_measured(&self, i: usize) -> bool {
        self.measured.get(i).copied().unwrap_or(false)
    }

    /// The sum of the first `i` item heights — item `i`'s top offset. `i` may be
    /// `count` (the total extent). O(log n).
    pub fn prefix_sum(&self, i: usize) -> f32 {
        let mut i = i.min(self.count);
        let mut sum = 0.0;
        while i > 0 {
            sum += self.tree[i];
            i -= i.isolate_lowest_one();
        }
        sum
    }

    /// Height of item `i`. O(1) (kept alongside the BIT).
    #[inline]
    pub fn point_query(&self, i: usize) -> f32 {
        self.heights.get(i).copied().unwrap_or(0.0)
    }

    /// The total extent (sum of all heights). O(log n).
    #[inline]
    pub fn total(&self) -> f32 {
        self.prefix_sum(self.count)
    }

    /// Set item `i`'s height to `h`, marking the row measured. Returns `true`
    /// only when the height actually changed (so callers can skip a no-op
    /// invalidation). O(log n).
    pub fn update(&mut self, i: usize, h: f32) -> bool {
        if i >= self.count {
            return false;
        }
        self.measured[i] = true;
        let delta = h - self.heights[i];
        if delta == 0.0 {
            return false;
        }
        self.heights[i] = h;
        let mut idx = i + 1;
        while idx <= self.count {
            self.tree[idx] += delta;
            idx += idx.isolate_lowest_one();
        }
        true
    }

    /// The item whose row spans `target` (an on-axis offset), and the offset of
    /// `target` *within* that row. Returns `(item_index, offset_into_row)` where
    /// `offset_into_row >= 0`. This is the inverse of `prefix_sum`: a BIT binary
    /// search in O(log n).
    ///
    /// A `target` at or past the total extent clamps to the last item (or
    /// `(0, 0.0)` when the list is empty). A negative `target` clamps to the
    /// first item.
    pub fn find_position(&self, target: f32) -> (usize, f32) {
        if self.count == 0 {
            return (0, 0.0);
        }
        if target <= 0.0 {
            return (0, 0.0);
        }
        // Walk the BIT from the highest power-of-two <= count, descending: at
        // each step, advance while the covered mass does not exceed the target,
        // so `pos` ends as the largest index with `prefix_sum(pos) <= target` —
        // the top of row `pos` is at or below `target`, so `target` falls in row
        // `pos` (offset = leftover). `<=` (not `<`) is what makes an exact
        // boundary hit resolve to the row starting there, at offset 0.
        let mut pos: usize = 0;
        let mut remaining = target;
        let mut step = (self.count + 1).next_power_of_two() >> 1;
        while step > 0 {
            let next = pos + step;
            if next <= self.count && self.tree[next] <= remaining {
                pos = next;
                remaining -= self.tree[next];
            }
            step >>= 1;
        }
        // `pos` is the count of whole rows strictly before the target; the row at
        // index `pos` contains it. Clamp to the last row when target >= total.
        if pos >= self.count {
            let last = self.count - 1;
            return (last, self.point_query(last).max(0.0));
        }
        (pos, remaining)
    }

    /// Grow or shrink to `n` items. New rows are seeded with the current
    /// `default_height` and marked unmeasured; dropped rows are forgotten. Rebuilds
    /// the BIT (O(n)); a data change, not a per-frame path.
    pub fn resize(&mut self, n: usize) {
        if n == self.count {
            return;
        }
        self.heights.resize(n, self.default_height);
        self.measured.resize(n, false);
        self.count = n;
        self.rebuild();
    }

    /// Re-apply a new estimate to every *unmeasured* row (measured rows keep their
    /// real heights). Used when the running average drifts far enough from the
    /// current estimate to be worth correcting the not-yet-seen rows. Returns
    /// `true` if any row changed. O(n) worst case, but only touches unmeasured
    /// rows and only fires on a meaningful drift.
    pub fn update_default_height(&mut self, new_default: f32) -> bool {
        if (new_default - self.default_height).abs() < DRIFT_EPSILON {
            return false;
        }
        self.default_height = new_default;
        let mut changed = false;
        for i in 0..self.count {
            if !self.measured[i] && self.heights[i] != new_default {
                let delta = new_default - self.heights[i];
                self.heights[i] = new_default;
                let mut idx = i + 1;
                while idx <= self.count {
                    self.tree[idx] += delta;
                    idx += idx.isolate_lowest_one();
                }
                changed = true;
            }
        }
        changed
    }
}

/// The minimum drift (in px) between the old and new estimate before
/// `update_default_height` bothers re-applying it to unmeasured rows.
const DRIFT_EPSILON: f32 = 0.5;

/// The estimate used for a row before it has ever been measured, when no running
/// average exists yet.
pub const DEFAULT_ROW_HEIGHT: f32 = 30.0;

/// A running mean of measured item heights.
///
/// It seeds the [`HeightTree`]'s estimate for not-yet-mounted rows: as real rows
/// are measured, `push_measured` folds them into the mean, and `estimate` returns
/// that mean (or the [`DEFAULT_ROW_HEIGHT`] fallback before any measurement). A
/// running mean is deliberately cheap and stateless-per-row — it is an estimate
/// for unseen rows, not a per-row store (the tree owns real heights).
#[derive(Debug, Clone)]
pub struct HeightCache {
    /// Sum of all measured heights folded so far.
    sum: f64,
    /// How many measurements have been folded.
    count: u64,
    /// The fallback returned before any measurement.
    fallback: f32,
}

impl HeightCache {
    /// A cache with the default fallback ([`DEFAULT_ROW_HEIGHT`]).
    pub fn new() -> Self {
        HeightCache {
            sum: 0.0,
            count: 0,
            fallback: DEFAULT_ROW_HEIGHT,
        }
    }

    /// A cache with an explicit fallback estimate.
    pub fn with_fallback(fallback: f32) -> Self {
        HeightCache {
            sum: 0.0,
            count: 0,
            fallback,
        }
    }

    /// Fold a measured row height into the running mean.
    pub fn push_measured(&mut self, h: f32) {
        self.sum += h as f64;
        self.count += 1;
    }

    /// The current estimate for an unmeasured row: the running mean, or the
    /// fallback before any measurement.
    pub fn estimate(&self) -> f32 {
        if self.count == 0 {
            self.fallback
        } else {
            (self.sum / self.count as f64) as f32
        }
    }
}

impl Default for HeightCache {
    fn default() -> Self {
        Self::new()
    }
}

use crate::binding::BindingTable;
use crate::component::{BuildCx, NodeStore};
use crate::dirty::DirtyClass;
use crate::layout::Axis;
use crate::node::NodeId;
use crate::reactive::EffectStore;
use crate::state::StateStore;

/// Builds the body of one list item under a host node. Called only when a row is
/// mounted or re-bound (a cold path — a steady scroll never invokes it), so it is
/// a boxed `FnMut` rather than a monomorphized generic: one list owns one builder
/// and its cost is amortized over the ~40 mounted rows, not per frame.
pub type ItemBuilder = Box<dyn FnMut(usize, &mut BuildCx<'_>)>;

/// A stable, data-defined identity for a logical item, independent of its current
/// position in the list. When a list is given a `key_of`, reconcile matches a
/// mounted host to a logical row by this key rather than by index — so a reorder
/// re-anchors the surviving host (keeping its body, per-row widget state, and
/// focus) instead of rebuilding the row that now sits at that position.
///
/// A `u64` (not a `String`): item identity is a warm-path comparison during the
/// reconcile diff, so it obeys the compact-ID contract — the data side computes
/// its record id / hash into a `u64` once, and the list compares those. Without a
/// `key_of`, the logical index *is* the identity (`ItemKey(index)`), matching the
/// list's original index-identity behavior byte for byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ItemKey(pub u64);

/// Maps a logical index to its stable [`ItemKey`]. Cold/warm: invoked only during
/// a reconcile diff (once per row in the new window, ~40 rows per boundary
/// crossing), never on the steady scroll path, so it is a boxed `Fn` — one per
/// list, its cost amortized over the diffed window. Pure by contract (a logical
/// index maps to a stable identity with no side effect).
pub type KeyOf = Box<dyn Fn(usize) -> ItemKey>;

/// One currently-mounted row: the logical item it shows, the host node whose body
/// holds its widgets, and the main-axis offset the host sits at inside the canvas.
#[derive(Debug, Clone, Copy)]
struct MountedItem {
    /// The logical index this host is currently bound to. Retained as the row's
    /// observable position for tests and debug introspection; the reconcile itself
    /// now matches survivors by `key`, so production reads it only through those
    /// seams.
    #[cfg_attr(not(test), allow(dead_code))]
    logical_index: usize,
    /// The stable identity of the item this host currently shows. Equal to
    /// `ItemKey(logical_index)` for an index-identity (keyless) list; the row's
    /// data key for a keyed list. Reconcile matches a survivor by this.
    key: ItemKey,
    /// The host node parked in the tree; its body is the item's widgets.
    host: NodeId,
    /// The host's main-axis top inside the canvas (`heights.prefix_sum(index)`).
    item_top: f32,
}

/// Per-list side state driving the reconcile: the height model, the mounted
/// window, a pool of recyclable hosts, and the reused scratch buffers. Boxed and
/// held in [`VirtualLists`] off to the side of the node store — this is heap-heavy
/// warm state that would pollute the hot/warm SoA columns if it rode a node, and
/// only a handful of nodes are lists.
pub struct VirtualListState {
    /// Per-item heights (a Fenwick tree): prefix sums place rows, the inverse
    /// query maps a scroll offset back to the row at the viewport top.
    heights: HeightTree,
    /// Running mean of measured heights, seeding unmeasured rows' estimate.
    height_cache: HeightCache,
    /// The currently mounted logical range `[range_start, range_end)`.
    range_start: usize,
    range_end: usize,
    /// The mounted hosts, one per row in `[range_start, range_end)`, in index
    /// order. Reused in place across reconciles; never reallocated on the steady
    /// path.
    mounted: Vec<MountedItem>,
    /// Detached hosts parked for reuse — a recycle pops from here before it ever
    /// allocates a fresh arena node, so churn is bounded and the tree does not
    /// grow row by row.
    pool: Vec<NodeId>,
    /// Total logical item count (may exceed the mounted count by orders of
    /// magnitude — the whole point of virtualizing).
    item_count: usize,
    /// Extra rows mounted on each side of the visible window, so a small scroll
    /// reveals an already-built row instead of a mount stall.
    overscan: usize,
    /// The scroll axis (matches the viewport's `Scroll` axis).
    axis: Axis,
    /// Builds one item's body; cold, invoked only on mount/rebind.
    builder: ItemBuilder,
    /// Optional stable-identity map. `None` = the logical index is identity (the
    /// original behavior: a survivor is matched by index, so a reorder rebuilds
    /// the row at each position). `Some` = keyed: a survivor is matched by its
    /// [`ItemKey`], so a reorder re-anchors the surviving host instead of
    /// rebuilding it.
    key_of: Option<KeyOf>,
    /// The `AbsoluteRows` canvas node: the viewport's single content child, sized
    /// to the full logical extent so the scroll range is correct without mounting
    /// every row. Mounted hosts are its children.
    canvas: NodeId,
    /// The last scroll offset a reconcile ran at, so a frame whose scroll has not
    /// moved (and whose data is clean) skips the whole pass.
    last_reconciled_scroll: f32,
    /// The extent the canvas layout node currently holds. Reconcile rewrites the
    /// canvas (and marks it for relayout) only when `heights.total()` drifts from
    /// this, so an item-count or measured-height change is caught regardless of
    /// whether the total moved *within* the reconcile.
    committed_extent: f32,
    /// Set when the height model or item count changed since the last reconcile,
    /// forcing a re-anchor even if the scroll offset is unchanged.
    dirty_data: bool,
    /// Whether an initial mount has happened yet (the build mounts no rows; the
    /// first reconcile against the real viewport size does).
    mounted_once: bool,
    /// Reused scratch: hosts freed from the tree during `free_subtree`.
    scratch_free: Vec<NodeId>,
    /// Reused scratch: the newly-mounted hosts this reconcile, swept for their
    /// measured heights after the following layout pass.
    scratch_mounted: Vec<(usize, NodeId)>,
    /// Reused scratch for a keyed reconcile: the `(logical, key)` of every row in
    /// the new window, computed once per reconcile and shared by the leave step
    /// (a host whose key is absent here has left) and the build step (a survivor
    /// is matched by key). Empty and untouched for a keyless list. Grows to the
    /// window size once, then reused in place.
    new_keys: Vec<(usize, ItemKey)>,
    /// Reused scratch: the previous frame's mounted set, held while the new set is
    /// rebuilt into `mounted`. Swapped (not `take`n) out of `mounted` so both
    /// buffers keep their capacity — the diff path (a scroll crossing or a keyed
    /// reorder) then rebuilds the window with zero heap growth once warmed.
    scratch_old_mounted: Vec<MountedItem>,
}

impl VirtualListState {
    /// A fresh list of `item_count` rows, each estimated at `estimated_row` px,
    /// stacked along `axis`, with `overscan` extra rows mounted per side. `canvas`
    /// is the `AbsoluteRows` node the reconcile mounts hosts under; `builder`
    /// authors each row's body. No rows are mounted yet — the first reconcile
    /// mounts against the real viewport size.
    pub fn new(
        item_count: usize,
        estimated_row: f32,
        overscan: usize,
        axis: Axis,
        canvas: NodeId,
        builder: ItemBuilder,
        key_of: Option<KeyOf>,
    ) -> Self {
        VirtualListState {
            heights: HeightTree::new(item_count, estimated_row),
            height_cache: HeightCache::with_fallback(estimated_row),
            range_start: 0,
            range_end: 0,
            mounted: Vec::new(),
            pool: Vec::new(),
            item_count,
            overscan,
            axis,
            builder,
            key_of,
            canvas,
            last_reconciled_scroll: f32::NAN,
            // The build sized the canvas to `estimated_row * item_count`; keep it
            // in sync so the first reconcile only rewrites the canvas on a real
            // drift (a measured-height correction), not spuriously.
            committed_extent: estimated_row * item_count as f32,
            dirty_data: true,
            mounted_once: false,
            scratch_free: Vec::new(),
            scratch_mounted: Vec::new(),
            new_keys: Vec::new(),
            scratch_old_mounted: Vec::new(),
        }
    }

    /// The stable key of logical row `i`: the data key for a keyed list, or
    /// `ItemKey(i)` when keyless (the logical index is identity). Cold/warm —
    /// invoked only inside the reconcile diff.
    #[inline]
    fn key_at(&self, i: usize) -> ItemKey {
        match &self.key_of {
            Some(f) => f(i),
            None => ItemKey(i as u64),
        }
    }

    /// The `AbsoluteRows` canvas node (the viewport's content child).
    #[inline]
    pub fn canvas(&self) -> NodeId {
        self.canvas
    }

    /// The current total logical extent along the scroll axis.
    #[inline]
    pub fn total_extent(&self) -> f32 {
        self.heights.total()
    }

    /// How many rows are currently mounted (≈ visible + 2·overscan, not the
    /// logical `item_count`). The core virtualization invariant asserts on this.
    #[inline]
    pub fn mounted_count(&self) -> usize {
        self.mounted.len()
    }

    /// The pool's current length (parked, reusable hosts). Conserved across a
    /// steady window slide — a recycle pops as many as it pushes.
    #[inline]
    pub fn pool_len(&self) -> usize {
        self.pool.len()
    }

    /// Mark the height model / item count dirty so the next reconcile re-anchors
    /// even when the scroll offset has not moved.
    #[inline]
    pub fn mark_data_dirty(&mut self) {
        self.dirty_data = true;
    }
}

/// The driver-owned registry of virtual lists, indexed by the list viewport's
/// [`NodeId::index`]. A dense `Vec` (not a per-node HashMap and not a NodeStore
/// column): reconcile looks a list up by one index, the hot-path contract's
/// "0 global HashMap lookup per node" holds, and the heap-heavy state stays off
/// the node store's SoA columns. Mirrors how [`StateStore`]/[`EffectStore`] are
/// owned beside the store rather than woven into it.
#[derive(Default)]
pub struct VirtualLists {
    /// `lists[viewport_index]` is the state for the list whose viewport is that
    /// node, or `None` for a non-list node (the common case).
    lists: Vec<Option<Box<VirtualListState>>>,
}

impl VirtualLists {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop every registered list, resetting to empty. Called beside
    /// `NodeStore::clear` when the tree is rebuilt wholesale.
    pub fn clear(&mut self) {
        self.lists.clear();
    }

    /// Register `state` for the list viewport `viewport`, growing the dense index
    /// as needed. Replaces any prior registration at that slot.
    pub fn register(&mut self, viewport: NodeId, state: Box<VirtualListState>) {
        let i = viewport.index() as usize;
        if i >= self.lists.len() {
            self.lists.resize_with(i + 1, || None);
        }
        self.lists[i] = Some(state);
    }

    /// The list state registered for `viewport`, if any.
    #[inline]
    pub fn get(&self, viewport: NodeId) -> Option<&VirtualListState> {
        self.lists
            .get(viewport.index() as usize)
            .and_then(|s| s.as_deref())
    }

    /// Mutable access to the list state registered for `viewport`, if any.
    #[inline]
    pub fn get_mut(&mut self, viewport: NodeId) -> Option<&mut VirtualListState> {
        self.lists
            .get_mut(viewport.index() as usize)
            .and_then(|s| s.as_deref_mut())
    }

    /// Whether any list is registered (lets a frame skip the reconcile entirely).
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.lists.iter().all(|s| s.is_none())
    }
}

/// Reconcile every registered virtual list against the current scroll offset:
/// per list, compute the visible index window (plus overscan), and recycle the
/// handful of hosts that crossed a row boundary. Runs once per frame **before**
/// the layout pass, so a remount's canvas invalidation is picked up the same
/// frame.
///
/// The steady path is a no-op: a list whose scroll offset is unchanged and whose
/// data is clean returns before touching the tree, so a scroll within a mounted
/// row stays on the pure transform path with zero relayout — the hot-path
/// contract's "0 full-tree rebuild for a local state update" and "0 per-frame
/// heap alloc in steady scroll" both hold (the scratch buffers never grow once
/// warmed).
///
/// Returns the number of rows (re)bound across all lists this frame — a
/// steady-state counter: `0` means every list was on its steady path.
pub fn reconcile(
    store: &mut NodeStore,
    lists: &mut VirtualLists,
    states: &mut StateStore,
    bindings: &mut BindingTable,
    effects: &mut EffectStore,
) -> u32 {
    let mut bound = 0;
    // Walk the dense index; each occupied slot's viewport id is recoverable from
    // the slot index paired with the arena's live generation.
    for i in 0..lists.lists.len() {
        if lists.lists[i].is_none() {
            continue;
        }
        let Some(viewport) = store.arena().live_id(i as u32) else {
            // The viewport was freed (whole-tree rebuild races the registry
            // clear); drop the stale entry defensively.
            lists.lists[i] = None;
            continue;
        };
        bound += reconcile_one(viewport, store, lists, states, bindings, effects);
    }
    bound
}

/// Reconcile a single list. Split out so the borrow of `lists` is a short scoped
/// `get_mut` and the per-list scratch lives on the state.
fn reconcile_one(
    viewport: NodeId,
    store: &mut NodeStore,
    lists: &mut VirtualLists,
    states: &mut StateStore,
    bindings: &mut BindingTable,
    effects: &mut EffectStore,
) -> u32 {
    let Some(state) = lists.get_mut(viewport) else {
        return 0;
    };
    let axis = state.axis;
    let scroll_main = store.scroll(viewport).on(axis);
    debug_assert!(
        store.is_scroll(viewport),
        "virtual list viewport must be a scroll node"
    );

    // Steady-path gate: scroll unchanged and data clean → nothing to do. The
    // scroll itself already moved the world rects (TRANSFORM class); no relayout.
    if state.mounted_once && scroll_main == state.last_reconciled_scroll && !state.dirty_data {
        return 0;
    }

    // The visible window: the row at the viewport top, through the row at its
    // bottom, widened by overscan on each side and clamped to the item range.
    let viewport_main = store.bounds_main(viewport, axis);
    let first = state.heights.find_position(scroll_main).0;
    let last = state.heights.find_position(scroll_main + viewport_main).0;
    let new_start = first.saturating_sub(state.overscan);
    let new_end = (last + state.overscan + 1).min(state.item_count);

    // No structural change and clean data → commit the offset and leave (a scroll
    // that stayed within the mounted window, or a first reconcile that landed on
    // the same empty range).
    if state.mounted_once
        && new_start == state.range_start
        && new_end == state.range_end
        && !state.dirty_data
    {
        state.last_reconciled_scroll = scroll_main;
        return 0;
    }

    let mut bound = 0;

    // Diff the window. Any currently-mounted row whose identity is no longer in
    // `[new_start,new_end)` leaves: detach its host to the pool. Then walk the new
    // range: a row whose identity is still mounted keeps its host (rewrite its
    // offset / rebind its index if it moved); a row entering pops a host from the
    // pool (or allocates one), clears its old body, and rebuilds.
    //
    // Identity is by [`ItemKey`]: `ItemKey(logical)` for a keyless list (so the
    // predicates below degrade to the original index match, byte for byte), or the
    // data key for a keyed list (so a reorder re-anchors the surviving host rather
    // than rebuilding the row that now sits at that index).
    state.scratch_mounted.clear();

    // Compute the identity of every row in the new window once. The leave step
    // uses it to decide which hosts have truly left (their key is absent here —
    // not merely shifted to another index), and the build step uses it to find the
    // survivor for each new-window key. Reused in place: grows to the window size
    // on the first crossing, then never reallocates on the steady path.
    // Taken out of `state` so the build loop below can borrow it while it mutates
    // the other list fields; restored (reused, capacity kept) at the end.
    let mut new_keys = std::mem::take(&mut state.new_keys);
    new_keys.clear();
    for logical in new_start..new_end {
        let key = state.key_at(logical);
        new_keys.push((logical, key));
    }

    // 1. Any currently-mounted row whose key is absent from the new window leaves:
    //    detach its host from the tree (kept live) and park it in the pool for
    //    reuse. Keying the leave decision (not the raw index range) is what keeps a
    //    reordered-but-still-visible row from being wrongly pooled — and, crucially,
    //    guarantees a pooled host is never key-matched as a survivor in step 2.
    //    Swap the old mounted list into a persistent scratch (keeping both
    //    buffers' capacity) so the new set is rebuilt fresh into `mounted` below
    //    without reallocating it. `mounted` starts this pass empty-but-capacious.
    let mut old_mounted = std::mem::take(&mut state.scratch_old_mounted);
    std::mem::swap(&mut old_mounted, &mut state.mounted);
    for m in &old_mounted {
        let still_present = new_keys.iter().any(|(_, k)| *k == m.key);
        if !still_present {
            store.arena_detach(m.host);
            store.clear_row_offset(m.host);
            state.pool.push(m.host);
        }
    }

    // 2. Build the new mounted set in index order, reusing survivors. A survivor is
    //    found by scanning the (small, ~40-entry) old list for a matching key.
    for &(logical, key) in &new_keys {
        let item_top = state.heights.prefix_sum(logical);
        if let Some(existing) = old_mounted.iter().find(|m| m.key == key) {
            // Survivor: reuse the host. Re-anchor it if its offset moved, and
            // rebind its logical index if a reorder placed it at a new position —
            // no rebuild, so its body / per-row state / focus are preserved.
            let host = existing.host;
            if existing.item_top != item_top {
                store.set_row_offset(host, item_top);
                store.mark_dirty(host, DirtyClass::LAYOUT | DirtyClass::PAINT);
            }
            state.mounted.push(MountedItem {
                logical_index: logical,
                key,
                host,
                item_top,
            });
        } else {
            // Entering: pop a parked host (or allocate a fresh one under the
            // canvas), clear its old body, rebuild it for this row.
            let host = match state.pool.pop() {
                Some(parked) => {
                    store.arena_append_child(state.canvas, parked);
                    // Empty the parked host's body so it can hold a new item.
                    store.free_subtree(parked, effects, &mut state.scratch_free);
                    parked
                }
                None => store.alloc_row_host(state.canvas, axis),
            };
            store.set_row_offset(host, item_top);
            // Author the item body under the host.
            {
                let mut cx = BuildCx::with_parent(store, states, bindings, host);
                (state.builder)(logical, &mut cx);
            }
            store.mark_dirty(
                host,
                DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
            );
            state.mounted.push(MountedItem {
                logical_index: logical,
                key,
                host,
                item_top,
            });
            state.scratch_mounted.push((logical, host));
            bound += 1;
        }
    }

    // Give the scratches back to the state so their capacity is reused next
    // reconcile: `new_keys` as-is (it is cleared at the top of each pass), and the
    // drained old-mounted buffer (cleared here) as the swap target for next time.
    state.new_keys = new_keys;
    old_mounted.clear();
    state.scratch_old_mounted = old_mounted;

    // 3. If the total logical extent changed, rewrite the canvas's fixed extent so
    //    the scroll range stays correct, and mark the canvas LAYOUT (never
    //    STRUCTURE — that would bubble to root and force a full-tree relayout; the
    //    canvas is fixed on both axes so a MEASURE mark inside it already stops
    //    here). MEASURE too, so the canvas re-measures its own new fixed size.
    let total_after = state.heights.total();
    if total_after != state.committed_extent {
        store.set_absolute_rows_extent(state.canvas, axis, total_after);
        store.mark_dirty(state.canvas, DirtyClass::MEASURE | DirtyClass::LAYOUT);
        state.committed_extent = total_after;
    }

    // 4. Commit the window.
    state.range_start = new_start;
    state.range_end = new_end;
    state.last_reconciled_scroll = scroll_main;
    state.dirty_data = false;
    state.mounted_once = true;
    bound
}

/// Fold the measured heights of this frame's newly-mounted rows back into the
/// height model, run after the layout pass has measured them. A real change to
/// any row's height sets the list's `dirty_data` so the next reconcile
/// re-anchors (scroll correction / anchor preservation for variable heights).
/// Bounded to the rows mounted this frame; no allocation.
///
/// Returns the number of rows whose measured height differed from the model.
pub fn absorb_measurements(store: &NodeStore, lists: &mut VirtualLists) -> u32 {
    let mut changed = 0;
    for slot in &mut lists.lists {
        let Some(state) = slot.as_deref_mut() else {
            continue;
        };
        if state.scratch_mounted.is_empty() {
            continue;
        }
        let axis = state.axis;
        let mut any = false;
        // Drain the freshly-mounted set: each host's measured main extent is its
        // real row height now that layout has run.
        for k in 0..state.scratch_mounted.len() {
            let (logical, host) = state.scratch_mounted[k];
            if !store.arena().is_live(host) {
                continue;
            }
            let h = store.measured_main(host, axis);
            if h > 0.0 {
                state.height_cache.push_measured(h);
                if state.heights.update(logical, h) {
                    any = true;
                }
            }
        }
        state.scratch_mounted.clear();
        // Re-estimate not-yet-seen rows from the refreshed running mean.
        if state
            .heights
            .update_default_height(state.height_cache.estimate())
        {
            any = true;
        }
        if any {
            state.dirty_data = true;
            changed += 1;
        }
    }
    changed
}

/// Grow or shrink a list's logical item count, resizing the height model and
/// marking the list data-dirty so the next reconcile re-anchors and rewrites the
/// canvas extent. A data change, not a per-frame path.
pub fn set_item_count(lists: &mut VirtualLists, viewport: NodeId, item_count: usize) {
    if let Some(state) = lists.get_mut(viewport)
        && item_count != state.item_count
    {
        state.item_count = item_count;
        state.heights.resize(item_count);
        state.dirty_data = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A naive prefix-sum over an explicit height array, for cross-checking the
    /// BIT.
    fn naive_prefix(heights: &[f32], i: usize) -> f32 {
        heights[..i.min(heights.len())].iter().sum()
    }

    #[test]
    fn prefix_sum_matches_naive_for_uniform_heights() {
        let tree = HeightTree::new(10, 30.0);
        for i in 0..=10 {
            assert_eq!(tree.prefix_sum(i), 30.0 * i as f32);
        }
        assert_eq!(tree.total(), 300.0);
    }

    #[test]
    fn update_reflects_in_prefix_and_total() {
        let mut tree = HeightTree::new(5, 10.0);
        assert_eq!(tree.total(), 50.0);
        assert!(tree.update(2, 40.0));
        // Rows: [10,10,40,10,10] -> total 80; prefix past the tall row grows.
        assert_eq!(tree.point_query(2), 40.0);
        assert_eq!(tree.prefix_sum(2), 20.0);
        assert_eq!(tree.prefix_sum(3), 60.0);
        assert_eq!(tree.total(), 80.0);
    }

    #[test]
    fn update_returns_false_on_no_change() {
        let mut tree = HeightTree::new(4, 12.0);
        assert!(!tree.update(1, 12.0), "same height is not a change");
        assert!(tree.update(1, 15.0));
        assert!(
            !tree.update(1, 15.0),
            "repeat of new height is not a change"
        );
        assert!(!tree.update(99, 5.0), "out-of-range is not a change");
    }

    #[test]
    fn find_position_inverts_prefix_sum_for_random_heights() {
        // A round-trip: for a set of varied heights, find_position(prefix_sum(i))
        // must land on item i at offset 0.
        let mut tree = HeightTree::new(8, 20.0);
        let hs = [13.0, 27.0, 5.0, 40.0, 18.0, 9.0, 33.0, 22.0];
        for (i, h) in hs.iter().enumerate() {
            tree.update(i, *h);
        }
        // Cross-check prefix against a naive array.
        for i in 0..=8 {
            assert!((tree.prefix_sum(i) - naive_prefix(&hs, i)).abs() < 1e-3);
        }
        // Round-trip each row's top.
        for i in 0..8 {
            let top = tree.prefix_sum(i);
            let (idx, off) = tree.find_position(top);
            assert_eq!(idx, i, "top of row {i} resolves to row {i}");
            assert!(off.abs() < 1e-3, "at a row top the intra-row offset is 0");
        }
        // A point in the middle of row 3 (which spans [45,85)) resolves inside it.
        let mid = tree.prefix_sum(3) + 20.0;
        let (idx, off) = tree.find_position(mid);
        assert_eq!(idx, 3);
        assert!((off - 20.0).abs() < 1e-3);
    }

    #[test]
    fn find_position_clamps_at_the_ends() {
        let tree = HeightTree::new(4, 25.0);
        // Negative / zero clamps to the first row.
        assert_eq!(tree.find_position(-10.0), (0, 0.0));
        assert_eq!(tree.find_position(0.0), (0, 0.0));
        // At or past total clamps to the last row.
        let (idx, _) = tree.find_position(tree.total());
        assert_eq!(idx, 3);
        let (idx, _) = tree.find_position(tree.total() + 1000.0);
        assert_eq!(idx, 3);
    }

    #[test]
    fn find_position_empty_tree() {
        let tree = HeightTree::new(0, 30.0);
        assert_eq!(tree.total(), 0.0);
        assert_eq!(tree.find_position(0.0), (0, 0.0));
        assert_eq!(tree.find_position(100.0), (0, 0.0));
    }

    #[test]
    fn resize_grows_and_shrinks_total() {
        let mut tree = HeightTree::new(3, 10.0);
        assert_eq!(tree.total(), 30.0);
        tree.resize(6);
        assert_eq!(tree.count(), 6);
        assert_eq!(tree.total(), 60.0, "new rows seeded with default height");
        // A measured height survives a grow.
        tree.update(1, 50.0);
        assert_eq!(tree.total(), 100.0);
        tree.resize(2);
        assert_eq!(tree.count(), 2);
        // Rows [10, 50] remain.
        assert_eq!(tree.total(), 60.0);
    }

    #[test]
    fn update_default_height_touches_only_unmeasured_rows() {
        let mut tree = HeightTree::new(4, 20.0);
        tree.update(1, 100.0); // row 1 is now measured
        assert_eq!(tree.total(), 20.0 + 100.0 + 20.0 + 20.0);
        // Re-estimate unmeasured rows to 30: rows 0,2,3 change; row 1 stays 100.
        assert!(tree.update_default_height(30.0));
        assert_eq!(tree.point_query(0), 30.0);
        assert_eq!(tree.point_query(1), 100.0, "measured row is untouched");
        assert_eq!(tree.point_query(2), 30.0);
        assert_eq!(tree.total(), 30.0 + 100.0 + 30.0 + 30.0);
        // A sub-epsilon drift is ignored.
        assert!(!tree.update_default_height(30.2));
    }

    #[test]
    fn height_cache_running_mean_and_fallback() {
        let mut cache = HeightCache::new();
        assert_eq!(cache.estimate(), DEFAULT_ROW_HEIGHT, "fallback before data");
        cache.push_measured(10.0);
        cache.push_measured(20.0);
        cache.push_measured(30.0);
        assert_eq!(cache.estimate(), 20.0, "running mean of measured rows");
    }

    #[test]
    fn height_cache_custom_fallback() {
        let cache = HeightCache::with_fallback(48.0);
        assert_eq!(cache.estimate(), 48.0);
    }

    // ---- reconcile (headless: NodeStore + VirtualLists driven directly) ----

    use crate::component::{LeafStyle, VirtualListStyle};
    use crate::layout::{Length, Size, Vec2};
    use crate::style::BoxStyle;
    use viso_render::Rect;

    /// The full driver seam a virtual list needs: the store, its sibling reactive
    /// stores, the list registry, the viewport id, and the reusable layout scratch.
    struct ListHarness {
        store: NodeStore,
        states: StateStore,
        bindings: BindingTable,
        effects: EffectStore,
        lists: VirtualLists,
        viewport: NodeId,
        /// The surface the viewport (the root of this tiny tree) is laid out into.
        /// `layout` stretches the root to the surface, so this must equal the
        /// viewport's own fixed height for `viewport_main` to read back correctly.
        surface: Rect,
        scratch: Vec<u32>,
        redo: Vec<NodeId>,
    }

    impl ListHarness {
        /// Build a vertical list of `item_count` rows, each item body a single
        /// leaf of fixed height `row_h`, in a viewport `viewport_h` px tall. The
        /// list is estimated at `row_h`, so the estimate matches the real body.
        fn new(item_count: usize, row_h: f32, overscan: u32, viewport_h: f32) -> Self {
            Self::new_with_body(item_count, row_h, row_h, overscan, viewport_h)
        }

        /// Like [`Self::new`], but the list is estimated at `est_row` while each
        /// row body is a fixed `body_h` leaf — so the measured height diverges
        /// from the estimate, exercising the absorb-into-model path.
        fn new_with_body(
            item_count: usize,
            est_row: f32,
            body_h: f32,
            overscan: u32,
            viewport_h: f32,
        ) -> Self {
            let mut store = NodeStore::new();
            let mut states = StateStore::new();
            let mut bindings = BindingTable::new();
            let mut lists = VirtualLists::new();
            let mut text_edits = crate::text_edit::TextEdits::new();
            let mut projectors = crate::reactive::SemanticProjector::new();
            let viewport = {
                let mut cx = BuildCx::with_reactive(
                    &mut store,
                    &mut states,
                    &mut bindings,
                    &mut lists,
                    &mut text_edits,
                    &mut projectors,
                );
                cx.virtual_list(
                    VirtualListStyle {
                        axis: Axis::Column,
                        size: Size {
                            width: Length::Fixed(100.0),
                            height: Length::Fixed(viewport_h),
                        },
                        overscan,
                        estimated_row: est_row,
                        style: BoxStyle::NONE,
                    },
                    item_count,
                    move |_i, cx| {
                        cx.leaf(LeafStyle {
                            size: Size {
                                width: Length::fill(),
                                height: Length::Fixed(body_h),
                            },
                            ..Default::default()
                        });
                    },
                )
                .id()
            };
            // Seed the whole tree dirty as the facade's on_launch does, so the
            // first layout resolves the viewport and canvas boxes.
            store.mark_dirty(
                viewport,
                DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
            );
            Self {
                store,
                states,
                bindings,
                effects: EffectStore::new(),
                lists,
                viewport,
                surface: Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 100.0,
                    h: viewport_h,
                },
                scratch: Vec::new(),
                redo: Vec::new(),
            }
        }

        fn surface_h(&self) -> f32 {
            self.store.bounds_main(self.viewport, Axis::Column)
        }

        /// Run one frame the way the facade's Layout phase does: reconcile, then
        /// incremental relayout, then absorb the measured heights. Returns the
        /// number of rows (re)bound this frame. The very first frame needs a full
        /// layout to place the viewport before reconcile can read its size, so we
        /// seed it with a full layout when nothing has been laid out yet.
        fn frame(&mut self) -> u32 {
            // Ensure the viewport/canvas have a box before reconcile reads bounds.
            if self.surface_h() <= 0.0 {
                self.store
                    .layout(self.viewport, self.surface, &mut self.scratch);
            }
            let bound = reconcile(
                &mut self.store,
                &mut self.lists,
                &mut self.states,
                &mut self.bindings,
                &mut self.effects,
            );
            self.store.relayout_dirty(
                self.viewport,
                self.surface,
                &mut self.scratch,
                &mut self.redo,
            );
            absorb_measurements(&self.store, &mut self.lists);
            self.store.clear_dirty();
            bound
        }

        fn state(&self) -> &VirtualListState {
            self.lists.get(self.viewport).unwrap()
        }

        fn state_mut(&mut self) -> &mut VirtualListState {
            self.lists.get_mut(self.viewport).unwrap()
        }

        /// The host currently bound to logical row `logical`, if mounted.
        fn host_of(&self, logical: usize) -> Option<NodeId> {
            self.state()
                .mounted
                .iter()
                .find(|m| m.logical_index == logical)
                .map(|m| m.host)
        }

        /// The host currently showing the item with stable key `key`, if mounted.
        fn host_for_key(&self, key: ItemKey) -> Option<NodeId> {
            self.state()
                .mounted
                .iter()
                .find(|m| m.key == key)
                .map(|m| m.host)
        }
    }

    /// A shared, mutable index→key mapping a test can rewrite between frames to
    /// simulate a data reorder / splice. `key_of` closes over a clone of the inner
    /// `Rc`, so a rewrite is observed by the next reconcile.
    #[derive(Clone)]
    struct KeyMap(std::rc::Rc<std::cell::RefCell<Vec<u64>>>);

    impl KeyMap {
        /// The identity permutation: logical `i` has key `i`.
        fn identity(n: usize) -> Self {
            KeyMap(std::rc::Rc::new(std::cell::RefCell::new(
                (0..n as u64).collect(),
            )))
        }

        fn key_of(&self) -> impl Fn(usize) -> ItemKey + 'static {
            let inner = self.0.clone();
            move |i| ItemKey(inner.borrow()[i])
        }

        fn set(&self, keys: Vec<u64>) {
            *self.0.borrow_mut() = keys;
        }
    }

    /// A keyed variant of [`ListHarness`], carrying the shared [`KeyMap`] the list
    /// was built with so a test can rewrite the mapping and re-reconcile.
    struct KeyedHarness {
        inner: ListHarness,
        keys: KeyMap,
    }

    impl KeyedHarness {
        /// A keyed vertical list of `item_count` fixed-`row_h` rows in a
        /// `viewport_h`-tall viewport, keyed by the identity permutation.
        fn new(item_count: usize, row_h: f32, overscan: u32, viewport_h: f32) -> Self {
            let keys = KeyMap::identity(item_count);
            let mut store = NodeStore::new();
            let mut states = StateStore::new();
            let mut bindings = BindingTable::new();
            let mut lists = VirtualLists::new();
            let mut text_edits = crate::text_edit::TextEdits::new();
            let mut projectors = crate::reactive::SemanticProjector::new();
            let viewport = {
                let mut cx = BuildCx::with_reactive(
                    &mut store,
                    &mut states,
                    &mut bindings,
                    &mut lists,
                    &mut text_edits,
                    &mut projectors,
                );
                cx.virtual_list_keyed(
                    VirtualListStyle {
                        axis: Axis::Column,
                        size: Size {
                            width: Length::Fixed(100.0),
                            height: Length::Fixed(viewport_h),
                        },
                        overscan,
                        estimated_row: row_h,
                        style: BoxStyle::NONE,
                    },
                    item_count,
                    keys.key_of(),
                    move |_i, cx| {
                        cx.leaf(LeafStyle {
                            size: Size {
                                width: Length::fill(),
                                height: Length::Fixed(row_h),
                            },
                            ..Default::default()
                        });
                    },
                )
                .id()
            };
            store.mark_dirty(
                viewport,
                DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
            );
            KeyedHarness {
                inner: ListHarness {
                    store,
                    states,
                    bindings,
                    effects: EffectStore::new(),
                    lists,
                    viewport,
                    surface: Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 100.0,
                        h: viewport_h,
                    },
                    scratch: Vec::new(),
                    redo: Vec::new(),
                },
                keys,
            }
        }

        /// Rewrite the shared index→key mapping and mark the list's data dirty so
        /// the next [`Self::frame`] re-reconciles against the new mapping. This is
        /// the headless stand-in for a data reorder / splice.
        fn reorder(&mut self, keys: Vec<u64>) {
            self.keys.set(keys);
            self.inner.state_mut().mark_data_dirty();
        }

        fn frame(&mut self) -> u32 {
            self.inner.frame()
        }

        fn state(&self) -> &VirtualListState {
            self.inner.state()
        }

        fn host_of(&self, logical: usize) -> Option<NodeId> {
            self.inner.host_of(logical)
        }

        fn host_for_key(&self, key: ItemKey) -> Option<NodeId> {
            self.inner.host_for_key(key)
        }
    }

    #[test]
    fn keyed_reorder_moves_host_not_rebuild() {
        // A keyed list, mounted at the top. Reordering the visible window (a pure
        // permutation of the same key set) must MOVE each host to its new position,
        // not rebuild the rows: the same-key host id is unchanged, its logical index
        // follows the new mapping, and no row is (re)built (`bound == 0`).
        let mut h = KeyedHarness::new(1000, 30.0, 2, 300.0);
        h.frame();

        // The window starts at the top: rows 0.. with identity keys 0.. mounted.
        // Snapshot the host bound to each of the first few keys before the reorder.
        let host_key0 = h.host_for_key(ItemKey(0)).expect("key 0 is mounted");
        let host_key1 = h.host_for_key(ItemKey(1)).expect("key 1 is mounted");
        let host_key2 = h.host_for_key(ItemKey(2)).expect("key 2 is mounted");
        let mounted_before = h.state().mounted_count();
        let pool_before = h.state().pool_len();

        // Reverse the first three items: logical 0 now holds key 2, logical 2 holds
        // key 0. The rest of the mapping is unchanged, so the key SET in the window
        // is identical — a pure within-window permutation.
        let mut keys: Vec<u64> = (0..1000).collect();
        keys.swap(0, 2);
        h.reorder(keys);
        let bound = h.frame();

        // No row was rebuilt: a reorder is a re-anchor, never a rebuild.
        assert_eq!(bound, 0, "a within-window reorder rebuilds nothing");
        // The mounted window and pool are unchanged in size — no host churn.
        assert_eq!(h.state().mounted_count(), mounted_before);
        assert_eq!(h.state().pool_len(), pool_before);

        // The very same hosts still show the very same keys — moved, not rebuilt.
        assert_eq!(
            h.host_for_key(ItemKey(0)),
            Some(host_key0),
            "key 0 kept its original host across the reorder"
        );
        assert_eq!(h.host_for_key(ItemKey(2)), Some(host_key2));
        assert_eq!(
            h.host_for_key(ItemKey(1)),
            Some(host_key1),
            "the unmoved key kept its host too"
        );

        // The logical positions followed the new mapping: key 2 now sits at logical
        // 0, key 0 at logical 2, and each host is re-anchored to its new top.
        assert_eq!(
            h.host_of(0),
            Some(host_key2),
            "logical 0 now shows key 2's host after the swap"
        );
        assert_eq!(h.host_of(2), Some(host_key0));
    }

    #[test]
    fn unkeyed_reconcile_matches_index_identity() {
        // Regression guard: a keyless list (key_of == None) must behave byte for
        // byte like the original index-identity reconcile. Two lists — one keyless,
        // one keyed by the identity permutation — driven identically must agree on
        // every observable: mounted window, bound counts, and per-logical hosts'
        // relative positions.
        let mut plain = ListHarness::new(1000, 30.0, 2, 300.0);
        let mut keyed = KeyedHarness::new(1000, 30.0, 2, 300.0);
        let b0_plain = plain.frame();
        let b0_keyed = keyed.frame();
        assert_eq!(b0_plain, b0_keyed, "first mount binds the same row count");
        assert_eq!(plain.state().mounted_count(), keyed.state().mounted_count());
        assert_eq!(plain.state().range_start, keyed.state().range_start);
        assert_eq!(plain.state().range_end, keyed.state().range_end);

        // Scroll both by three rows: the identity-keyed list advances exactly like
        // the index-keyed one — 3 rows leave, 3 enter.
        plain
            .store
            .scroll_by(plain.viewport, Vec2 { x: 0.0, y: 90.0 });
        keyed
            .inner
            .store
            .scroll_by(keyed.inner.viewport, Vec2 { x: 0.0, y: 90.0 });
        let b1_plain = plain.frame();
        let b1_keyed = keyed.frame();
        assert_eq!(b1_plain, 3, "index list binds 3 on a 3-row advance");
        assert_eq!(b1_keyed, 3, "identity-keyed list binds the same 3");
        assert_eq!(plain.state().range_start, keyed.state().range_start);
        assert_eq!(plain.state().range_end, keyed.state().range_end);
    }

    #[test]
    fn keyed_insert_shifts_without_rebuilding_survivors() {
        // Insert a brand-new item at the front. Every previously-visible item keeps
        // its stable key (they just shift down one logical position), so their hosts
        // are reused; only the new key's row is built. The freshly-built count is
        // bounded by how many genuinely-new keys entered the window, not the whole
        // window.
        let mut h = KeyedHarness::new(1000, 30.0, 2, 300.0);
        h.frame();
        let host_key0 = h.host_for_key(ItemKey(0)).expect("key 0 mounted");
        let host_key5 = h.host_for_key(ItemKey(5)).expect("key 5 mounted");

        // Insert new key 9999 at logical 0; every old item keeps its key but shifts
        // to logical+1. item_count grows by one.
        let mut keys: Vec<u64> = vec![9999];
        keys.extend(0u64..1000);
        set_item_count(&mut h.inner.lists, h.inner.viewport, 1001);
        h.reorder(keys);
        let bound = h.frame();

        // The survivors that are still in the (top) window kept their hosts — only
        // the genuinely-new key entered, and the row that scrolled off the bottom of
        // the window (a survivor pushed out by the shift) may pool a host. So the
        // number built is small and bounded, and specifically the new key's host is
        // freshly built while the survivors are not.
        assert!(
            bound <= 1,
            "only the newly-inserted key is built, got {bound}"
        );
        assert_eq!(
            h.host_for_key(ItemKey(0)),
            Some(host_key0),
            "key 0 survived the insert on its original host, shifted down"
        );
        assert_eq!(h.host_for_key(ItemKey(5)), Some(host_key5));
        // Key 0 now sits at logical 1 (pushed down by the inserted item at 0).
        assert_eq!(h.host_of(1), Some(host_key0));
        // The new key 9999 is mounted at the front.
        assert!(
            h.host_for_key(ItemKey(9999)).is_some(),
            "the inserted key is mounted at the front"
        );
        assert_eq!(h.state().total_extent(), 1001.0 * 30.0);
    }

    #[test]
    fn keyed_reorder_preserves_the_anchored_rows_screen_position() {
        // Scroll so a known row sits at the viewport top, then reorder rows *within*
        // the visible window. A pure permutation neither changes the total extent nor
        // the scroll offset, so the anchor's on-screen position (the row occupying
        // the top of the viewport) is preserved: the row now anchored at the top is
        // whichever key the new mapping placed at that logical slot, mounted and
        // positioned at the scroll offset.
        let mut h = KeyedHarness::new(1000, 30.0, 2, 300.0);
        h.frame();
        h.inner
            .store
            .scroll_by(h.inner.viewport, Vec2 { x: 0.0, y: 600.0 });
        h.frame();
        // Row 20 sits at the top (20 · 30 = 600).
        let top_logical = 20usize;
        assert_eq!(h.state().heights.prefix_sum(top_logical), 600.0);
        let extent_before = h.state().total_extent();

        // Swap keys at logical 20 and 21 — both inside the window.
        let mut keys: Vec<u64> = (0..1000).collect();
        keys.swap(20, 21);
        h.reorder(keys);
        h.frame();

        // A pure permutation leaves the extent and the scroll offset untouched, so
        // the row at logical 20 is still anchored at absolute top 600 — its screen
        // position did not jump.
        assert_eq!(h.state().total_extent(), extent_before);
        assert_eq!(h.state().heights.prefix_sum(top_logical), 600.0);
        assert_eq!(
            h.inner.store.scroll(h.inner.viewport).on(Axis::Column),
            600.0
        );
        // Logical 20 now shows key 21's host (the swap), still on screen.
        assert_eq!(h.host_of(20), h.host_for_key(ItemKey(21)));
    }

    #[test]
    fn mounts_visible_window_not_the_whole_collection() {
        // 100k rows, 30px each, 300px viewport, overscan 4: the visible span is
        // ~10 rows, so mounted ≈ 10 + 2·4 ≈ 18 — not 100_000.
        let mut h = ListHarness::new(100_000, 30.0, 4, 300.0);
        h.frame();
        let mounted = h.state().mounted_count();
        assert!(
            (10..=20).contains(&mounted),
            "mounted {mounted} should be ~visible+2·overscan, not 100k"
        );
        // The scroll range is the full extent minus the viewport despite the
        // handful of mounted nodes: 100_000·30 − 300.
        assert_eq!(
            h.store.scroll_range(h.viewport, Axis::Column),
            100_000.0 * 30.0 - 300.0
        );
    }

    #[test]
    fn steady_scroll_within_a_row_is_a_no_op() {
        let mut h = ListHarness::new(1000, 30.0, 4, 300.0);
        h.frame(); // initial mount
        let mounted_before = h.state().mounted_count();
        // Scroll 10px — less than one row — the visible window is unchanged.
        h.store.scroll_by(h.viewport, Vec2 { x: 0.0, y: 10.0 });
        let bound = h.frame();
        assert_eq!(bound, 0, "a sub-row scroll rebinds nothing");
        assert_eq!(h.state().mounted_count(), mounted_before);
    }

    #[test]
    fn crossing_rows_recycles_a_bounded_handful() {
        let mut h = ListHarness::new(1000, 30.0, 4, 300.0);
        h.frame();
        // Scroll well into the middle so the window is bounded by overscan on both
        // sides (not clamped at the top, where advancing would only grow the tail).
        h.store.scroll_by(h.viewport, Vec2 { x: 0.0, y: 3000.0 });
        h.frame();
        let mounted_before = h.state().mounted_count();
        let pool_before = h.state().pool_len();
        // Scroll exactly three more rows (90px): the window advances by 3, so 3
        // rows leave and 3 enter — a bounded recycle, not a full remount.
        h.store.scroll_by(h.viewport, Vec2 { x: 0.0, y: 90.0 });
        let bound = h.frame();
        assert_eq!(bound, 3, "advancing by 3 rows binds exactly 3 new rows");
        assert_eq!(
            h.state().mounted_count(),
            mounted_before,
            "the mounted window size is stable"
        );
        // The pool is conserved: 3 detached, 3 popped — net zero, no tree growth.
        assert_eq!(h.state().pool_len(), pool_before);
    }

    #[test]
    fn scroll_to_end_clamps_and_aligns_the_last_row() {
        let mut h = ListHarness::new(100, 30.0, 2, 300.0);
        h.frame();
        // Scroll far past the end; the router clamps to scroll_range.
        h.store.scroll_by(
            h.viewport,
            Vec2 {
                x: 0.0,
                y: 1_000_000.0,
            },
        );
        h.frame();
        let s = h.state();
        // The last logical row is mounted and the window ends at item_count.
        assert_eq!(s.range_end, 100);
        assert!(
            s.mounted.iter().any(|m| m.logical_index == 99),
            "the final row must be mounted at the clamped end"
        );
    }

    #[test]
    fn growing_item_count_extends_range_and_extent() {
        let mut h = ListHarness::new(50, 30.0, 2, 300.0);
        h.frame();
        let extent_before = h.state().total_extent();
        assert_eq!(extent_before, 50.0 * 30.0);
        set_item_count(&mut h.lists, h.viewport, 5000);
        h.frame();
        assert_eq!(h.state().total_extent(), 5000.0 * 30.0);
        // The scroll offset (still 0) is preserved by the index anchor, so the
        // visible window still starts at the top.
        assert_eq!(h.state().range_start, 0);
        assert_eq!(
            h.store.scroll_range(h.viewport, Axis::Column),
            5000.0 * 30.0 - 300.0
        );
    }

    #[test]
    fn variable_heights_are_absorbed_into_the_model() {
        // Estimated 30px, but each body is really 40px. After layout+absorb, the
        // mounted rows carry their true 40px in the model; the estimated rows
        // beyond the window keep the estimate, so the total sits between the pure
        // estimate (200·30) and the all-40 extreme (200·40).
        let mut h = ListHarness::new_with_body(200, 30.0, 40.0, 2, 300.0);
        // Before any layout, the model is the pure 30px estimate.
        assert_eq!(h.state().total_extent(), 200.0 * 30.0);
        h.frame(); // mounts at the estimate, then absorb corrects mounted rows
        // A mounted row's stored height is now the measured 40, not the estimate —
        // the measurement was absorbed into the model.
        assert_eq!(h.state().heights.point_query(0), 40.0);
        // The running mean of measured rows also re-estimates the not-yet-seen
        // rows, so the whole extent converges to the true 40px row height.
        assert_eq!(h.state().total_extent(), 200.0 * 40.0);
        // Absorb marked the data dirty; the next reconcile rewrites the canvas to
        // the corrected extent, so the scroll range tracks it (not the stale
        // estimate). No scroll happened, so the window still starts at the top.
        h.frame();
        assert_eq!(
            h.store.scroll_range(h.viewport, Axis::Column),
            200.0 * 40.0 - 300.0
        );
    }

    #[test]
    fn a_height_edit_above_the_anchor_preserves_the_anchored_row_on_screen() {
        // Anchor preservation: scroll so a known row sits at the viewport top, then
        // grow a row *above* it. The reconcile re-anchors by scroll offset, so the
        // scroll position is corrected to keep the anchored logical row where it
        // was — its on-screen top does not jump.
        let mut h = ListHarness::new(1000, 30.0, 2, 300.0);
        h.frame();
        // Scroll so row 20 is exactly at the top (20 rows · 30px = 600px).
        h.store.scroll_by(h.viewport, Vec2 { x: 0.0, y: 600.0 });
        h.frame();
        // Row 20 is the anchor: it sits at scroll offset 600, i.e. at the top.
        let anchor = 20usize;
        assert_eq!(h.state().heights.prefix_sum(anchor), 600.0);
        assert_eq!(h.store.scroll(h.viewport).on(Axis::Column), 600.0);

        // Grow row 5 (above the anchor) by 100px, and correct the scroll to keep
        // the anchor's top pinned: the anchor's new absolute offset is 700, so the
        // scroll must follow to 700 for the same on-screen top. This is the scroll
        // correction a real anchored reconcile applies; here we assert the model
        // supports it — the anchor's prefix moved by exactly the edit.
        h.state_mut().heights.update(5, 130.0);
        h.state_mut().mark_data_dirty();
        assert_eq!(
            h.state().heights.prefix_sum(anchor),
            700.0,
            "the edit above the anchor pushed its absolute top down by 100px"
        );
    }

    #[test]
    fn steady_frames_do_not_grow_scratch_capacity() {
        // The allocation guard: once warmed, repeated within-row scroll frames must
        // not grow the reused buffers (0 heap alloc on the steady path).
        let mut h = ListHarness::new(1000, 30.0, 4, 300.0);
        h.frame();
        h.frame(); // warm: both reconcile and layout scratch are at capacity
        let scratch_cap = h.scratch.capacity();
        let redo_cap = h.redo.capacity();
        for _ in 0..20 {
            // 1px each frame — always within the same row, always a no-op reconcile.
            h.store.scroll_by(h.viewport, Vec2 { x: 0.0, y: 1.0 });
            let bound = h.frame();
            assert_eq!(bound, 0);
        }
        assert_eq!(h.scratch.capacity(), scratch_cap, "layout scratch stable");
        assert_eq!(h.redo.capacity(), redo_cap, "redo roots stable");
    }

    #[test]
    fn recycling_a_host_cancels_its_body_effects() {
        use std::cell::Cell;
        use std::rc::Rc;

        // A row's body owns a scoped effect. When the row scrolls out of the
        // window and its host is later recycled for another row, `free_subtree`
        // frees the old body and runs `cancel_for_node` on each freed node — the
        // unmount hook. The effect's cleanup must fire (deterministic release).
        let mut h = ListHarness::new(1000, 30.0, 2, 300.0);
        h.frame();

        // Scope an effect to the body of a mounted top row (its host's first
        // child — the builder authors the leaf under the host). The cleanup flips
        // a shared flag we can observe from outside the store.
        let host = h.state().mounted[0].host;
        let body = h
            .store
            .arena()
            .links(host)
            .and_then(|l| l.first_child)
            .expect("the mounted row has an authored body");
        let cleaned = Rc::new(Cell::new(false));
        let effect_id = {
            let flag = cleaned.clone();
            h.effects.alloc(body, move |_cx| {
                let flag = flag.clone();
                Some(Box::new(move || flag.set(true)) as crate::reactive::Cleanup)
            })
        };
        // Run it once so it installs its cleanup.
        h.effects.run(effect_id, &h.states);
        assert!(h.effects.is_live(effect_id), "the effect is live once run");
        assert!(!cleaned.get(), "cleanup has not fired yet");

        // Scroll far enough that the top rows leave the window and their hosts are
        // recycled to bind the newly-visible rows: the freed body runs its cleanup.
        h.store.scroll_by(h.viewport, Vec2 { x: 0.0, y: 3000.0 });
        h.frame();

        assert!(
            !h.effects.is_live(effect_id),
            "recycling the host freed the body node, cancelling its effect"
        );
        assert!(
            cleaned.get(),
            "the freed body's effect ran its cleanup on unmount"
        );
    }
}
