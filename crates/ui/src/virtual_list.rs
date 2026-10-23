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

    /// A tree over `heights`, each measured.
    pub(crate) fn from_heights(heights: Vec<f32>) -> Self {
        let count = heights.len();
        let mut t = HeightTree {
            tree: Vec::new(),
            count,
            measured: vec![true; count],
            heights,
            default_height: 0.0,
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
pub(crate) const DRIFT_EPSILON: f32 = 0.5;

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
    /// The mean of the measured heights folded so far, kept incrementally
    /// in `f32` (no running sum to lose precision as it grows).
    mean: f32,
    /// How many measurements have been folded.
    count: u32,
    /// The fallback returned before any measurement.
    fallback: f32,
}

impl HeightCache {
    /// A cache with the default fallback ([`DEFAULT_ROW_HEIGHT`]).
    pub fn new() -> Self {
        HeightCache {
            mean: 0.0,
            count: 0,
            fallback: DEFAULT_ROW_HEIGHT,
        }
    }

    /// A cache with an explicit fallback estimate.
    pub fn with_fallback(fallback: f32) -> Self {
        HeightCache {
            mean: 0.0,
            count: 0,
            fallback,
        }
    }

    /// Fold a measured row height into the running mean.
    pub fn push_measured(&mut self, h: f32) {
        self.count = self.count.saturating_add(1);
        self.mean += (h - self.mean) / self.count as f32;
    }

    /// The current estimate for an unmeasured row: the running mean, or the
    /// fallback before any measurement.
    pub fn estimate(&self) -> f32 {
        if self.count == 0 {
            self.fallback
        } else {
            self.mean
        }
    }
}

impl Default for HeightCache {
    fn default() -> Self {
        Self::new()
    }
}

use std::collections::VecDeque;

use crate::binding::{BindingTable, Template};
use crate::component::{BuildCx, NodeStore};
use crate::dirty::DirtyClass;
use crate::layout::{Axis, LayoutInput, Length, Size};
use crate::node::NodeId;
use crate::reactive::EffectStore;
use crate::semantics::{Role, Semantics};
use crate::state::StateStore;
use crate::structure::StructureCx;
use crate::style::BoxStyle;
use crate::virtual_grid::{Flow, ListGrid};

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

/// Whether logical row `i` heads a section: a header row stays at the
/// viewport's start edge while its section passes under it, until the next
/// header pushes it on. Cold, like [`KeyOf`]: asked of every row once when
/// the data changes.
pub type StickyOf = Box<dyn Fn(usize) -> bool>;

/// One currently-mounted row: the logical item it shows, the host node whose body
/// holds its widgets, and the main-axis offset the host sits at inside the canvas.
#[derive(Debug, Clone, Copy)]
struct MountedItem {
    /// The logical index this host is currently bound to: the place its row
    /// announces. The reconcile matches survivors by `key`.
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
    /// Where the items sit: a list's heights (a Fenwick tree whose prefix
    /// sums place rows and whose inverse query maps a scroll offset back to
    /// the row at the viewport top), or a grid's lines or lanes.
    flow: Flow,
    /// How it lays its items in lanes, if it does.
    grid: Option<ListGrid>,
    /// Which items span a grid's line, besides its section headers.
    full_of: Option<StickyOf>,
    /// Running mean of measured heights, seeding unmeasured rows' estimate.
    height_cache: HeightCache,
    /// The currently mounted logical range `[range_start, range_end)`.
    range_start: usize,
    range_end: usize,
    /// The mounted hosts, one per row in `[range_start, range_end)`, in index
    /// order. A scroll that slides the window pops and pushes its ends; never
    /// reallocated on the steady path.
    mounted: VecDeque<MountedItem>,
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
    /// The rows' template: every row built the same way shares its binding
    /// edges and adds only its base (§18.1).
    template: Template,
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
    /// The row the next reconcile scrolls to the start edge, mounted or not,
    /// and whether it glides there.
    reveal: Option<(usize, bool)>,
    /// The row a glide is headed for: each reconcile aims it where the
    /// height model now puts the row.
    gliding_to: Option<usize>,
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
    scratch_old_mounted: VecDeque<MountedItem>,
    /// Reused scratch: the new window's keys, sorted, and the previous mounted
    /// set's keys with each one's index there, sorted — so the leave and build
    /// steps find a key by binary search instead of a scan per row.
    sorted_new: Vec<u64>,
    sorted_old: Vec<(u64, u32)>,
    /// Which rows head a section, if any do.
    sticky_of: Option<StickyOf>,
    /// The header rows, in index order, as `sticky_of` gave them when the
    /// data last changed.
    headers: Vec<usize>,
    /// The data changed since `headers` was taken.
    headers_stale: bool,
    /// The header row the viewport's start shows while the window no longer
    /// holds it: mounted past the window, pinned at the start edge.
    pinned: Option<MountedItem>,
    /// The row lifted to reorder, held mounted past the window while it is.
    held: Option<MountedItem>,
    /// The key of the header to keep when its row leaves the window.
    keep: Option<ItemKey>,
    /// The host showing the current header, raised above the rows.
    header_host: Option<NodeId>,
    /// Whether each row is exactly as long as the viewport.
    pages: bool,
    /// The viewport extent the pages were last made, `0` before.
    page_extent: f32,
    /// Reused: the snap points near a release.
    snap_points: Vec<(f32, usize)>,
    /// Reused: the stretches about a probe a snap may rest on.
    snap_spans: Vec<(f32, f32, usize)>,
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
            flow: Flow::new(item_count, estimated_row, None, 1),
            grid: None,
            full_of: None,
            height_cache: HeightCache::with_fallback(estimated_row),
            range_start: 0,
            range_end: 0,
            mounted: VecDeque::new(),
            pool: Vec::new(),
            item_count,
            overscan,
            axis,
            builder,
            template: Template::new(),
            key_of,
            canvas,
            last_reconciled_scroll: f32::NAN,
            // The build sized the canvas to `estimated_row * item_count`; keep it
            // in sync so the first reconcile only rewrites the canvas on a real
            // drift (a measured-height correction), not spuriously.
            committed_extent: estimated_row * item_count as f32,
            dirty_data: true,
            mounted_once: false,
            reveal: None,
            gliding_to: None,
            scratch_free: Vec::new(),
            scratch_mounted: Vec::new(),
            new_keys: Vec::new(),
            scratch_old_mounted: VecDeque::new(),
            sorted_new: Vec::new(),
            sorted_old: Vec::new(),
            sticky_of: None,
            headers: Vec::new(),
            headers_stale: false,
            pinned: None,
            held: None,
            keep: None,
            header_host: None,
            pages: false,
            page_extent: 0.0,
            snap_points: Vec::new(),
            snap_spans: Vec::new(),
        }
    }

    /// Make each row exactly as long as the viewport, or not.
    pub fn paged(mut self: Box<Self>, pages: bool) -> Box<Self> {
        self.pages = pages;
        self
    }

    /// Lay the items in lanes as `grid` says, or one a line without.
    pub fn gridded(mut self: Box<Self>, grid: Option<ListGrid>) -> Box<Self> {
        let estimate = self.flow.default_height();
        let lanes = grid.map_or(1, |g| g.lanes_in(0.0));
        self.flow = Flow::new(self.item_count, estimate, grid, lanes);
        self.grid = grid;
        self.headers_stale = true;
        self
    }

    /// How many lanes it lays its items on: one for a list.
    pub fn lanes(&self) -> u16 {
        self.flow.lanes()
    }

    /// Let `full_of` say which items span a grid's line (a section header
    /// always does).
    pub fn set_full_of(&mut self, full_of: Option<StickyOf>) {
        self.full_of = full_of;
        self.headers_stale = true;
        self.dirty_data = true;
    }

    /// Recount a grid's adaptive lanes for the cross extent its canvas has
    /// now, keeping the item the viewport starts at in view.
    fn fit_lanes(&mut self, viewport: NodeId, cx: &mut StructureCx<'_>) {
        let Some(grid) = self.grid else { return };
        let cross = match self.axis {
            Axis::Column => cx.store.bounds(self.canvas).w,
            Axis::Row => cx.store.bounds(self.canvas).h,
        };
        if cross <= 0.0 {
            return;
        }
        let lanes = grid.lanes_in(cross);
        if lanes == self.flow.lanes() {
            return;
        }
        let axis = self.axis;
        let scroll = cx.store.scroll_distance(viewport, axis);
        let extent = cx.store.bounds_main(viewport, axis);
        let anchor =
            (self.item_count > 0 && scroll > 0.0).then(|| self.flow.visible(scroll, extent).0);
        self.flow.set_lanes(lanes);
        self.flow.settle();
        cx.store
            .set_absolute_rows_lanes(self.canvas, lanes, grid.cross_gap);
        if let Some(anchor) = anchor {
            let room = (self.flow.total() - extent).max(0.0);
            cx.store
                .seek_scroll(viewport, axis, self.flow.top(anchor).min(room));
        }
        self.dirty_data = true;
    }

    /// Whether each row is exactly as long as the viewport.
    pub fn is_paged(&self) -> bool {
        self.pages
    }

    /// Make every page as long as the viewport now is, keeping the page the
    /// viewport shows.
    fn fit_pages(&mut self, viewport: NodeId, cx: &mut StructureCx<'_>) {
        let axis = self.axis;
        let extent = cx.store.bounds_main(viewport, axis);
        if extent <= 0.0 || extent == self.page_extent {
            return;
        }
        let page = if self.page_extent > 0.0 {
            (cx.store.scroll_distance(viewport, axis) / self.page_extent).round()
        } else {
            0.0
        };
        let was = self.page_extent;
        self.page_extent = extent;
        self.flow = Flow::List(HeightTree::new(self.item_count, extent));
        self.height_cache = HeightCache::with_fallback(extent);
        let size = self.page_size();
        for row in self
            .mounted
            .iter_mut()
            .chain(self.pinned.as_mut())
            .chain(self.held.as_mut())
        {
            row.item_top = self.flow.top(row.logical_index);
            cx.store.set_fixed_size(row.host, size);
            cx.store.set_row_offset(row.host, row.item_top);
        }
        if was > 0.0 {
            cx.store.seek_scroll(viewport, axis, page * extent);
        }
        self.dirty_data = true;
    }

    /// The size of a page's host.
    fn page_size(&self) -> Size {
        let main = Length::Fixed(self.page_extent);
        match self.axis {
            Axis::Row => Size {
                width: main,
                height: Length::fill(),
            },
            Axis::Column => Size {
                width: Length::fill(),
                height: main,
            },
        }
    }

    /// Push onto `out` the snap points of the rows about where a viewport
    /// `extent` long `at` from its start shows its row as `snap` says.
    fn snap_candidates(
        &mut self,
        at: f32,
        snap: crate::snap::ScrollSnap,
        extent: f32,
        out: &mut Vec<(f32, usize)>,
    ) {
        if self.item_count == 0 {
            return;
        }
        let range = (self.flow.total() - extent).max(0.0);
        let probe = match snap.align {
            crate::snap::SnapAlign::Start => at + snap.inset,
            crate::snap::SnapAlign::Center => at + extent * 0.5,
            crate::snap::SnapAlign::End => at + extent - snap.inset - 0.5,
        };
        let mut spans = std::mem::take(&mut self.snap_spans);
        spans.clear();
        self.flow.snap_spans(probe, &mut spans);
        for &(start, size, index) in &spans {
            out.push((
                crate::snap::snap_point(snap, start, size, extent, range),
                index,
            ));
        }
        self.snap_spans = spans;
    }

    /// The release of viewport `viewport` its store left the list: glide to
    /// the row it comes to rest on, or coast on past none near.
    fn aim_snap(&mut self, viewport: NodeId, aim: crate::snap::SnapAim, cx: &mut StructureCx<'_>) {
        let Some(snap) = cx.store.scroll_snap(viewport) else {
            return;
        };
        let extent = cx.store.bounds_main(viewport, self.axis);
        let (from, rest, velocity) = match aim {
            crate::snap::SnapAim::Release {
                from,
                rest,
                velocity,
            } => (from, rest, velocity),
            crate::snap::SnapAim::Item(index) => {
                let index = index.min(self.item_count.saturating_sub(1));
                let (start, size) = self.flow.snap_span(index);
                let to = crate::snap::snap_point(
                    snap,
                    start,
                    size,
                    extent,
                    (self.flow.total() - extent).max(0.0),
                );
                cx.store.snap_to(viewport, to, index, 0.0);
                return;
            }
        };
        let mut points = std::mem::take(&mut self.snap_points);
        points.clear();
        self.snap_candidates(from, snap, extent, &mut points);
        self.snap_candidates(rest, snap, extent, &mut points);
        points.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        points.dedup_by_key(|p| p.1);
        let flick = cx.store.scroll_physics().min_velocity;
        match crate::snap::choose(&points, snap, (from, rest, velocity), flick, extent) {
            Some((to, index)) => cx.store.snap_to(viewport, to, index, velocity),
            None => {
                let sign = if self.axis == Axis::Row
                    && cx.store.direction_at(viewport.index())
                        == crate::adaptive::LayoutDirection::Rtl
                {
                    -1.0
                } else {
                    1.0
                };
                cx.store.coast(viewport, velocity * sign);
            }
        }
        self.snap_points = points;
    }

    /// Tell viewport `viewport`'s store the row it shows nearest.
    fn note_snap(&mut self, viewport: NodeId, cx: &mut StructureCx<'_>) {
        let Some(snap) = cx.store.scroll_snap(viewport) else {
            return;
        };
        let extent = cx.store.bounds_main(viewport, self.axis);
        let at = cx.store.scroll_distance(viewport, self.axis);
        let mut points = std::mem::take(&mut self.snap_points);
        points.clear();
        self.snap_candidates(at, snap, extent, &mut points);
        if let Some(&(_, index)) = points
            .iter()
            .min_by(|a, b| (a.0 - at).abs().total_cmp(&(b.0 - at).abs()))
        {
            cx.store.note_snap_index(viewport, index);
        }
        self.snap_points = points;
    }

    /// Let `sticky_of` say which rows head a section (none without).
    pub fn set_sticky_of(&mut self, sticky_of: Option<StickyOf>) {
        self.sticky_of = sticky_of;
        self.headers_stale = true;
        self.dirty_data = true;
    }

    /// The header rows, in index order.
    pub fn headers(&self) -> &[usize] {
        &self.headers
    }

    /// Asks `sticky_of` of every row again when the data changed.
    fn take_headers(&mut self) {
        if !std::mem::take(&mut self.headers_stale) {
            return;
        }
        self.headers.clear();
        if let Some(sticky_of) = &self.sticky_of {
            self.headers
                .extend((0..self.item_count).filter(|&i| sticky_of(i)));
        }
        if self.grid.is_some() {
            let full = if self.headers.is_empty() && self.full_of.is_none() {
                Vec::new()
            } else {
                let mut full = vec![false; self.item_count];
                for &h in &self.headers {
                    full[h] = true;
                }
                if let Some(full_of) = &self.full_of {
                    for (i, f) in full.iter_mut().enumerate() {
                        *f |= full_of(i);
                    }
                }
                full
            };
            self.flow.set_full(full);
        }
    }

    /// The header of the section row `row` belongs to, with the next
    /// header, if any.
    fn header_of(&self, row: usize) -> Option<(usize, Option<usize>)> {
        let at = self.headers.partition_point(|&h| h <= row);
        let header = *self.headers.get(at.checked_sub(1)?)?;
        Some((header, self.headers.get(at).copied()))
    }

    /// The offset that shows row `index` at the viewport's start: under its
    /// section's pinned header, unless it heads the section itself.
    fn reveal_offset(&self, index: usize) -> f32 {
        let top = self.flow.top(index);
        match self.header_of(index) {
            Some((header, _)) if header != index => top - self.flow.extent(header),
            _ => top,
        }
    }

    /// A row leaving the window parks its host, but the current header's row
    /// stays mounted, pinned past the window, and a lifted row held.
    fn leave(&mut self, row: MountedItem, store: &mut NodeStore) {
        if store.is_lifted(row.host) {
            if let Some(old) = self.held.replace(row) {
                park(store, &mut self.pool, old.host);
            }
        } else if self.keep != Some(row.key) {
            park(store, &mut self.pool, row.host);
        } else if let Some(old) = self.pinned.replace(row) {
            park(store, &mut self.pool, old.host);
        }
    }

    /// Show the header of the section at the viewport's start `scroll` —
    /// the window's row of it, or one mounted past the window — pinned at
    /// the start edge until the next header pushes it on. Returns the rows
    /// bound.
    fn place_header(&mut self, first: usize, scroll: f32, cx: &mut StructureCx<'_>) -> u32 {
        let mut bound = 0;
        let header = self.header_of(first);
        let host = match header {
            None => None,
            Some((h, _)) if (self.range_start..self.range_end).contains(&h) => {
                Some(self.mounted[h - self.range_start].host)
            }
            Some((h, _)) => {
                let key = self.key_at(h);
                match self.pinned {
                    Some(row) if row.key == key => {
                        let item_top = self.flow.top(h);
                        if row.item_top != item_top {
                            cx.store.set_row_offset(row.host, item_top);
                            cx.store
                                .mark_dirty(row.host, DirtyClass::LAYOUT | DirtyClass::PAINT);
                        }
                        self.pinned = Some(MountedItem {
                            logical_index: h,
                            item_top,
                            ..row
                        });
                        Some(row.host)
                    }
                    _ => {
                        if let Some(row) = self.pinned.take() {
                            park(cx.store, &mut self.pool, row.host);
                        }
                        let row = self.mount(h, key, cx);
                        bound += 1;
                        self.pinned = Some(row);
                        Some(row.host)
                    }
                }
            }
        };
        if let Some(row) = self.pinned
            && Some(row.host) != host
        {
            self.pinned = None;
            park(cx.store, &mut self.pool, row.host);
        }
        if self.header_host != host {
            if let Some(old) = self.header_host {
                cx.store.unpin(old);
            }
            self.header_host = host;
        }
        if let (Some(host), Some((h, next))) = (host, header) {
            let top = self.flow.top(h);
            let room = next.map_or(f32::INFINITY, |n| self.flow.top(n)) - self.flow.extent(h);
            let shift = (scroll.min(room) - top).max(0.0);
            let pin = match self.axis {
                Axis::Column => crate::layout::Vec2 { x: 0.0, y: shift },
                Axis::Row
                    if cx.store.direction_at(self.canvas.index())
                        == crate::adaptive::LayoutDirection::Rtl =>
                {
                    crate::layout::Vec2 { x: -shift, y: 0.0 }
                }
                Axis::Row => crate::layout::Vec2 { x: shift, y: 0.0 },
            };
            cx.store.pin(host, pin);
        }
        bound
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
        self.flow.total()
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

    /// Rebuild the mounted set for `[new_start, new_end)` by key: survivors keep
    /// their host and move if their offset did, the rest enter. Returns the rows
    /// bound.
    fn rebind(&mut self, new_start: usize, new_end: usize, cx: &mut StructureCx<'_>) -> u32 {
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
        self.scratch_mounted.clear();

        // Compute the identity of every row in the new window once. The leave step
        // uses it to decide which hosts have truly left (their key is absent here —
        // not merely shifted to another index), and the build step uses it to find the
        // survivor for each new-window key. Reused in place: grows to the window size
        // on the first crossing, then never reallocates on the steady path.
        // Taken out of `self` so the build loop below can borrow it while it mutates
        // the other list fields; restored (reused, capacity kept) at the end.
        let mut new_keys = std::mem::take(&mut self.new_keys);
        new_keys.clear();
        for logical in new_start..new_end {
            let key = self.key_at(logical);
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
        let mut old_mounted = std::mem::take(&mut self.scratch_old_mounted);
        std::mem::swap(&mut old_mounted, &mut self.mounted);
        let mut sorted_new = std::mem::take(&mut self.sorted_new);
        sorted_new.clear();
        sorted_new.extend(new_keys.iter().map(|(_, k)| k.0));
        sorted_new.sort_unstable();
        let mut sorted_old = std::mem::take(&mut self.sorted_old);
        sorted_old.clear();
        sorted_old.extend(
            old_mounted
                .iter()
                .enumerate()
                .map(|(i, m)| (m.key.0, i as u32)),
        );
        sorted_old.sort_unstable();
        for m in &old_mounted {
            let still_present = sorted_new.binary_search(&m.key.0).is_ok();
            if !still_present {
                self.leave(*m, cx.store);
            }
        }

        // 2. Build the new mounted set in index order, reusing survivors. A survivor is
        //    found by a binary search over the old set's sorted keys.
        for &(logical, key) in &new_keys {
            let item_top = self.flow.top(logical);
            let at = sorted_old.partition_point(|&(k, _)| k < key.0);
            let survivor = sorted_old
                .get(at)
                .filter(|&&(k, _)| k == key.0)
                .map(|&(_, i)| &old_mounted[i as usize]);
            if let Some(existing) = survivor {
                // Survivor: reuse the host. Re-anchor it if its offset moved, and
                // rebind its logical index if a reorder placed it at a new position —
                // no rebuild, so its body / per-row state / focus are preserved.
                let host = existing.host;
                let lane = self.flow.lane(logical);
                if existing.item_top != item_top || cx.store.row_lane(host) != lane {
                    cx.store.set_row_offset(host, item_top);
                    cx.store.set_row_lane(host, lane);
                    cx.store
                        .mark_dirty(host, DirtyClass::LAYOUT | DirtyClass::PAINT);
                }
                self.mounted.push_back(MountedItem {
                    logical_index: logical,
                    key,
                    host,
                    item_top,
                });
            } else {
                let row = self.mount(logical, key, cx);
                self.mounted.push_back(row);
                bound += 1;
            }
        }

        // Give the scratches back to the state so their capacity is reused next
        // reconcile: `new_keys` as-is (it is cleared at the top of each pass), and the
        // drained old-mounted buffer (cleared here) as the swap target for next time.
        self.new_keys = new_keys;
        old_mounted.clear();
        self.scratch_old_mounted = old_mounted;
        self.sorted_new = sorted_new;
        self.sorted_old = sorted_old;
        bound
    }

    /// Slide the mounted window to `[new_start, new_end)`, which overlaps the
    /// current one and whose rows kept their keys and offsets: the rows past
    /// either end leave, the rows uncovered at either end enter. Returns the
    /// rows bound.
    fn slide(&mut self, new_start: usize, new_end: usize, cx: &mut StructureCx<'_>) -> u32 {
        self.scratch_mounted.clear();
        let (start, end) = (self.range_start, self.range_end);
        let front = new_start.saturating_sub(start);
        let back = end.saturating_sub(new_end);
        for _ in 0..front {
            if let Some(m) = self.mounted.pop_front() {
                self.leave(m, cx.store);
            }
        }
        for _ in 0..back {
            if let Some(m) = self.mounted.pop_back() {
                self.leave(m, cx.store);
            }
        }
        let mut entering = std::mem::take(&mut self.scratch_old_mounted);
        for logical in new_start..start {
            let key = self.key_at(logical);
            entering.push_back(self.mount(logical, key, cx));
        }
        while let Some(row) = entering.pop_back() {
            self.mounted.push_front(row);
        }
        self.scratch_old_mounted = entering;
        for logical in end..new_end {
            let key = self.key_at(logical);
            let row = self.mount(logical, key, cx);
            self.mounted.push_back(row);
        }
        self.scratch_mounted.len() as u32
    }

    /// Mount logical row `logical` at its offset: pop a parked host (or
    /// allocate one under the canvas), empty its old body and build the row's.
    fn mount(&mut self, logical: usize, key: ItemKey, cx: &mut StructureCx<'_>) -> MountedItem {
        let item_top = self.flow.top(logical);
        // The pinned header's row, or the held lifted one, comes back into
        // the window as it is.
        let back = if self.pinned.is_some_and(|row| row.key == key) {
            self.pinned.take()
        } else if self.held.is_some_and(|row| row.key == key) {
            self.held.take()
        } else {
            None
        };
        if let Some(row) = back {
            if row.item_top != item_top {
                cx.store.set_row_offset(row.host, item_top);
                cx.store
                    .mark_dirty(row.host, DirtyClass::LAYOUT | DirtyClass::PAINT);
            }
            return MountedItem {
                logical_index: logical,
                key,
                host: row.host,
                item_top,
            };
        }
        let host = match self.pool.pop() {
            Some(parked) => {
                cx.store.arena_append_child(self.canvas, parked);
                // Drop the row instance the parked host held, so the new row
                // takes over its blocks.
                cx.store
                    .free_subtree(parked, cx.effects, &mut self.scratch_free);
                cx.bindings.collect(cx.store, cx.states);
                parked
            }
            None => cx.store.alloc_row_host(self.canvas, self.axis),
        };
        if self.pages && self.page_extent > 0.0 {
            cx.store.set_fixed_size(host, self.page_size());
        }
        cx.store.set_row_offset(host, item_top);
        cx.store.set_row_lane(host, self.flow.lane(logical));
        {
            let mut build =
                BuildCx::with_parent(cx.store, cx.states, cx.bindings, host).with_lists(cx.lists);
            let builder = &mut self.builder;
            build.instance(&mut self.template, |build| builder(logical, build));
        }
        cx.store.mark_dirty(
            host,
            DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
        );
        self.scratch_mounted.push((logical, host));
        MountedItem {
            logical_index: logical,
            key,
            host,
            item_top,
        }
    }

    /// Gives each mounted row's host its 1-based place among the list's items,
    /// writing only the hosts whose place moved.
    fn announce_positions(&self, store: &mut NodeStore) {
        let count = u32::try_from(self.item_count).unwrap_or(u32::MAX);
        for row in self.mounted.iter().chain(&self.pinned).chain(&self.held) {
            let index = u32::try_from(row.logical_index + 1).unwrap_or(u32::MAX);
            let state = store.semantic_state(row.host).unwrap_or_default();
            if state.position != Some((index, count)) {
                store.set_semantic_state(row.host, state.with_position(index, count));
            }
        }
    }

    /// Mark the height model / item count dirty so the next reconcile re-anchors
    /// even when the scroll offset has not moved.
    #[inline]
    pub fn mark_data_dirty(&mut self) {
        self.dirty_data = true;
        self.headers_stale = self.sticky_of.is_some() || self.full_of.is_some();
    }

    /// The logical item count.
    #[inline]
    pub fn item_count(&self) -> usize {
        self.item_count
    }

    /// The rows a viewport `extent` long scrolled `scroll` from its start
    /// shows, first and last inclusive, with the item count; `(0, 0, 0)` for
    /// an empty list.
    pub fn visible(&self, scroll: f32, extent: f32) -> (usize, usize, usize) {
        if self.item_count == 0 {
            return (0, 0, 0);
        }
        // The row under the viewport's far edge, not the one starting there.
        let (first, last) = self.flow.visible(scroll, extent);
        (first, last, self.item_count)
    }

    /// Scroll row `index` (clamped to the last row) to the viewport's start
    /// edge, or as near as the end of the list allows, at the next reconcile —
    /// which then mounts the window around it in the same frame, whether or not
    /// it was mounted before.
    pub fn scroll_to_index(&mut self, index: usize) {
        self.scroll_to_index_with(index, crate::component::ScrollBehavior::Instant);
    }

    /// [`scroll_to_index`](Self::scroll_to_index), at once or gliding: a
    /// glide aims at the row anew as the rows it passes are measured.
    pub fn scroll_to_index_with(
        &mut self,
        index: usize,
        behavior: crate::component::ScrollBehavior,
    ) {
        let smooth = behavior == crate::component::ScrollBehavior::Smooth;
        self.reveal = Some((index.min(self.item_count.saturating_sub(1)), smooth));
        self.dirty_data = true;
    }

    /// The logical index of the row keyed `key`, by a scan of the keys: cold,
    /// for a programmatic seek.
    pub fn index_of(&self, key: ItemKey) -> Option<usize> {
        (0..self.item_count).find(|&i| self.key_at(i) == key)
    }

    /// Free the parked hosts, which sit outside the tree: as the list itself is
    /// dropped.
    fn free_pool(&mut self, store: &mut NodeStore, effects: &mut EffectStore) {
        // The pinned header's and the held row's hosts are the canvas's;
        // they go with it.
        self.pinned = None;
        self.held = None;
        for host in self.pool.drain(..) {
            store.free_tree(host, effects, &mut self.scratch_free);
        }
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
    /// `lists[viewport_index]` is the list whose viewport is that node, with
    /// the viewport's generation-checked id, or `None` for a non-list node (the
    /// common case).
    lists: Vec<Option<(NodeId, Box<VirtualListState>)>>,
    /// The lists of freed viewports whose slot a new list took before a
    /// reconcile dropped them; the next reconcile frees their parked hosts.
    orphans: Vec<VirtualListState>,
    /// How many lists were ever registered, so a reconcile sees a pass that
    /// registered some.
    registered: u64,
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
        self.orphans.clear();
    }

    /// Register `state` for the list viewport `viewport`, growing the dense index
    /// as needed. Returns the list `viewport` was before; the list of a freed
    /// node whose slot `viewport` reuses is the next reconcile's to free.
    pub fn register(
        &mut self,
        viewport: NodeId,
        state: Box<VirtualListState>,
    ) -> Option<Box<VirtualListState>> {
        let i = viewport.index() as usize;
        if i >= self.lists.len() {
            self.lists.resize_with(i + 1, || None);
        }
        self.registered += 1;
        let (id, prior) = self.lists[i].replace((viewport, state))?;
        if id == viewport {
            return Some(prior);
        }
        self.orphans.push(*prior);
        None
    }

    /// Frees the parked hosts of the lists whose slot a new list took.
    fn free_orphans(&mut self, store: &mut NodeStore, effects: &mut EffectStore) {
        for mut state in self.orphans.drain(..) {
            state.free_pool(store, effects);
        }
    }

    /// Drop the list of `viewport`, freeing the hosts it keeps parked outside
    /// the tree; the viewport's own subtree (the canvas and its mounted rows)
    /// is the caller's.
    pub fn remove(&mut self, viewport: NodeId, store: &mut NodeStore, effects: &mut EffectStore) {
        let Some(slot) = self.lists.get_mut(viewport.index() as usize) else {
            return;
        };
        if slot.as_ref().is_some_and(|(id, _)| *id == viewport)
            && let Some((_, mut state)) = slot.take()
        {
            state.free_pool(store, effects);
        }
    }

    /// The list state registered for `viewport`, if any.
    #[inline]
    pub fn get(&self, viewport: NodeId) -> Option<&VirtualListState> {
        match self.lists.get(viewport.index() as usize)? {
            Some((id, state)) if *id == viewport => Some(state),
            _ => None,
        }
    }

    /// Mutable access to the list state registered for `viewport`, if any.
    #[inline]
    pub fn get_mut(&mut self, viewport: NodeId) -> Option<&mut VirtualListState> {
        match self.lists.get_mut(viewport.index() as usize)? {
            Some((id, state)) if *id == viewport => Some(state),
            _ => None,
        }
    }

    /// How many lists are registered.
    pub fn len(&self) -> usize {
        self.lists.iter().filter(|s| s.is_some()).count()
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
    // A row can declare a list of its own, registered while its list builds
    // it. A pass reaches a list registered past its cursor; one registered
    // behind it gets another pass, so a row's list mounts in the frame its row
    // does, as deep as lists nest.
    for _ in 0..MAX_NESTING {
        let registered = lists.registered;
        // Walk the dense index; each occupied slot keeps its viewport's
        // generation-checked id.
        let mut i = 0;
        while i < lists.lists.len() {
            let Some((viewport, mut state)) = lists.lists[i].take() else {
                i += 1;
                continue;
            };
            if !store.arena().is_live(viewport) {
                // The viewport was freed with its tree: drop the list and the
                // hosts it parked outside the tree.
                state.free_pool(store, effects);
                i += 1;
                continue;
            }
            let mut cx = StructureCx {
                store: &mut *store,
                states: &mut *states,
                bindings: &mut *bindings,
                effects: &mut *effects,
                lists: &mut *lists,
            };
            bound += reconcile_one(viewport, &mut state, &mut cx);
            lists.lists[i] = Some((viewport, state));
            i += 1;
        }
        lists.free_orphans(store, effects);
        if lists.registered == registered {
            break;
        }
    }
    bound
}

/// The passes one reconcile makes at most: lists nest no deeper in a frame.
const MAX_NESTING: usize = 8;

/// Reconcile a single list, taken out of the registry so the rows it builds
/// can register lists of their own.
fn reconcile_one(viewport: NodeId, state: &mut VirtualListState, cx: &mut StructureCx<'_>) -> u32 {
    let axis = state.axis;
    // A seek scrolls its row to the start edge, as near as the end of the list
    // allows, on the extent the height model holds now.
    state.take_headers();
    state.flow.settle();
    state.fit_lanes(viewport, cx);
    if state.pages {
        state.fit_pages(viewport, cx);
    }
    // A release the store left the list to aim.
    if let Some(aim) = cx.store.take_snap_aim(viewport) {
        state.aim_snap(viewport, aim, cx);
    }
    let room = (state.flow.total() - cx.store.bounds_main(viewport, axis)).max(0.0);
    let mut started = false;
    if let Some((index, smooth)) = state.reveal.take() {
        if smooth && !cx.store.reduced_motion() {
            state.gliding_to = Some(index);
            started = true;
        } else {
            state.gliding_to = None;
            let to = state.reveal_offset(index).min(room);
            cx.store.seek_scroll(viewport, axis, to);
        }
    }
    // A glide the finger or the wheel stopped, or that arrived, aims no more.
    if let Some(index) = state.gliding_to {
        if started || cx.store.is_gliding(viewport) {
            let to = state.reveal_offset(index).min(room);
            cx.store.glide_along(viewport, axis, to);
        } else {
            state.gliding_to = None;
        }
    }
    // How far the viewport is scrolled from its start: a row read right to
    // left takes negative offsets.
    let scroll_main = cx.store.scroll_distance(viewport, axis);
    debug_assert!(
        cx.store.is_scroll(viewport),
        "virtual list viewport must be a scroll node"
    );

    // A row held mounted past the window as it was lifted leaves once it
    // lies down.
    if let Some(row) = state.held.filter(|row| !cx.store.is_lifted(row.host)) {
        state.held = None;
        if cx.store.arena().is_live(row.host) {
            park(cx.store, &mut state.pool, row.host);
        }
    }

    // Steady-path gate: scroll unchanged and data clean → nothing to do. The
    // scroll itself already moved the world rects (TRANSFORM class); no relayout.
    if state.mounted_once && scroll_main == state.last_reconciled_scroll && !state.dirty_data {
        return 0;
    }

    if cx.store.snaps(viewport) {
        cx.store.note_snap_count(viewport, state.item_count);
        state.note_snap(viewport, cx);
    }

    // The visible window: the row at the viewport top, through the row at its
    // bottom, widened by overscan on each side and clamped to the item range.
    let viewport_main = cx.store.bounds_main(viewport, axis);
    let first = state.flow.window(scroll_main, viewport_main, 0).0;
    let (new_start, new_end) = state
        .flow
        .window(scroll_main, viewport_main, state.overscan);

    // No structural change and clean data → commit the offset and leave (a scroll
    // that stayed within the mounted window, or a first reconcile that landed on
    // the same empty range).
    if state.mounted_once
        && new_start == state.range_start
        && new_end == state.range_end
        && !state.dirty_data
    {
        state.last_reconciled_scroll = scroll_main;
        let bound = state.place_header(first, scroll_main, cx);
        if bound > 0 {
            state.announce_positions(cx.store);
        }
        return bound;
    }

    // The current header's row stays mounted as it leaves the window.
    state.keep = state.header_of(first).map(|(h, _)| state.key_at(h));

    // With clean data every row keeps its key and its offset, so a window that
    // overlaps the last one keeps exactly the overlap: only its ends change.
    let bound = if state.mounted_once
        && !state.dirty_data
        && new_start < state.range_end
        && state.range_start < new_end
    {
        state.slide(new_start, new_end, cx)
    } else {
        state.rebind(new_start, new_end, cx)
    };

    // 3. If the total logical extent changed, rewrite the canvas's fixed extent so
    //    the scroll range stays correct, and mark the canvas LAYOUT (never
    //    STRUCTURE — that would bubble to root and force a full-tree relayout; the
    //    canvas is fixed on both axes so a MEASURE mark inside it already stops
    //    here). MEASURE too, so the canvas re-measures its own new fixed size.
    let total_after = state.flow.total();
    if total_after != state.committed_extent {
        cx.store
            .set_absolute_rows_extent(state.canvas, axis, total_after);
        cx.store
            .mark_dirty(state.canvas, DirtyClass::MEASURE | DirtyClass::LAYOUT);
        state.committed_extent = total_after;
    }

    // 4. Commit the window.
    state.range_start = new_start;
    state.range_end = new_end;
    state.last_reconciled_scroll = scroll_main;
    state.dirty_data = false;
    state.mounted_once = true;

    // 5. Pin the header of the section the viewport starts in, then tell
    //    each mounted row its place in the collection: an entering row has
    //    none yet, and a survivor moves with a reorder or a count change.
    state.keep = None;
    let bound = bound + state.place_header(first, scroll_main, cx);
    state.announce_positions(cx.store);
    bound
}

/// Detach a row's host from the canvas and park it for reuse.
fn park(store: &mut NodeStore, pool: &mut Vec<NodeId>, host: NodeId) {
    store.unpin(host);
    store.arena_detach(host);
    store.clear_row_offset(host);
    pool.push(host);
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
        let Some((_, state)) = slot.as_mut() else {
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
                if state.flow.update(logical, h) {
                    any = true;
                }
            }
        }
        state.scratch_mounted.clear();
        // Re-estimate not-yet-seen rows from the refreshed running mean.
        if state
            .flow
            .update_default_height(state.height_cache.estimate())
        {
            any = true;
        }
        state.flow.settle();
        if any {
            state.dirty_data = true;
            changed += 1;
        }
    }
    changed
}

/// How a list lays out its rows: the axis it stacks them along, the extent a
/// row not measured yet is estimated at, and the rows mounted past each end of
/// the viewport.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ListShape {
    pub axis: Axis,
    pub estimated_row: f32,
    pub overscan: usize,
    /// Whether each row is exactly as long as the viewport: a pager's pages.
    pub pages: bool,
    /// How it lays its items in lanes, if it does: a grid.
    pub grid: Option<ListGrid>,
}

/// Make the scroll node `viewport` a virtual list of `item_count` rows that
/// `builder` authors, keyed by `key_of` (by index without): its single content
/// child becomes a canvas sized to the whole estimated collection, so the
/// scroll range spans every row while none is mounted yet; the first
/// reconcile mounts the window. Returns the list it replaces on `viewport`,
/// whose canvas the caller frees.
///
/// The canvas is fixed on **both** axes — at the collection's extent on the
/// main axis, at zero across (the real cross extent comes from the viewport at
/// layout) — which is load-bearing: it is the boundary where a row's `MEASURE`
/// stops rising, so a row remount never relays out the ancestors above the
/// list.
pub fn attach_list(
    store: &mut NodeStore,
    lists: &mut VirtualLists,
    viewport: NodeId,
    shape: ListShape,
    item_count: usize,
    builder: ItemBuilder,
    key_of: Option<KeyOf>,
) -> Option<Box<VirtualListState>> {
    let total = shape.estimated_row * item_count as f32;
    let size = match shape.axis {
        Axis::Column => Size {
            width: Length::Fixed(0.0),
            height: Length::Fixed(total),
        },
        Axis::Row => Size {
            width: Length::Fixed(total),
            height: Length::Fixed(0.0),
        },
    };
    if store.semantics(viewport).is_none() {
        store.set_semantics(viewport, Semantics::role(Role::List));
    }
    let canvas = store.alloc(
        LayoutInput::AbsoluteRows {
            axis: shape.axis,
            size,
            lanes: shape.grid.map_or(1, |g| g.lanes_in(0.0)),
            cross_gap: shape.grid.map_or(0.0, |g| g.cross_gap),
        },
        BoxStyle::NONE,
    );
    store.arena_append_child(viewport, canvas);
    store.mark_dirty(viewport, DirtyClass::MEASURE | DirtyClass::LAYOUT);
    lists.register(
        viewport,
        Box::new(VirtualListState::new(
            item_count,
            shape.estimated_row,
            shape.overscan,
            shape.axis,
            canvas,
            builder,
            key_of,
        ))
        .paged(shape.pages)
        .gridded(shape.grid),
    )
}

/// Grow or shrink a list's logical item count, resizing the height model and
/// marking the list data-dirty so the next reconcile re-anchors and rewrites the
/// canvas extent. A data change, not a per-frame path.
pub fn set_item_count(lists: &mut VirtualLists, viewport: NodeId, item_count: usize) {
    if let Some(state) = lists.get_mut(viewport)
        && item_count != state.item_count
    {
        state.item_count = item_count;
        state.flow.resize(item_count);
        state.dirty_data = true;
        state.headers_stale = state.sticky_of.is_some() || state.full_of.is_some();
    }
}

/// Let `sticky_of` say which rows of the list at `viewport` head a section
/// (none without): see [`StickyOf`].
pub fn set_sticky_of(lists: &mut VirtualLists, viewport: NodeId, sticky_of: Option<StickyOf>) {
    if let Some(state) = lists.get_mut(viewport) {
        state.set_sticky_of(sticky_of);
    }
}

/// Let `full_of` say which items of the grid at `viewport` span its line
/// (none besides its section headers without).
pub fn set_full_of(lists: &mut VirtualLists, viewport: NodeId, full_of: Option<StickyOf>) {
    if let Some(state) = lists.get_mut(viewport) {
        state.set_full_of(full_of);
    }
}

impl BuildCx<'_> {
    /// Let `is_full` say which items of the virtual grid `grid` span its
    /// whole line, starting a line of their own: a banner among the cards.
    pub fn full_line_items(
        &mut self,
        grid: crate::component::Handle,
        is_full: impl Fn(usize) -> bool + 'static,
    ) {
        if let Some(lists) = self.lists.as_mut() {
            set_full_of(lists, grid.id(), Some(Box::new(is_full)));
        }
    }

    /// Let `is_header` say which rows of the virtual list `list` head a
    /// section: each stays at the viewport's start edge while its section
    /// passes under it, until the next header pushes it on.
    pub fn section_headers(
        &mut self,
        list: crate::component::Handle,
        is_header: impl Fn(usize) -> bool + 'static,
    ) {
        if let Some(lists) = self.lists.as_mut() {
            set_sticky_of(lists, list.id(), Some(Box::new(is_header)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::OnAxis;

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
            Self::with_bodies(item_count, est_row, move |_| body_h, overscan, viewport_h)
        }

        /// Like [`Self::new_with_body`], with row `i`'s body `body_h(i)` tall.
        fn with_bodies(
            item_count: usize,
            est_row: f32,
            body_h: impl Fn(usize) -> f32 + 'static,
            overscan: u32,
            viewport_h: f32,
        ) -> Self {
            Self::gridded(item_count, est_row, body_h, overscan, viewport_h, None)
        }

        /// Like [`Self::with_bodies`], laid in lanes as `grid` says.
        fn gridded(
            item_count: usize,
            est_row: f32,
            body_h: impl Fn(usize) -> f32 + 'static,
            overscan: u32,
            viewport_h: f32,
            grid: Option<crate::virtual_grid::ListGrid>,
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
                        grid,
                        ..Default::default()
                    },
                    item_count,
                    move |i, cx| {
                        cx.leaf(LeafStyle {
                            size: Size {
                                width: Length::fill(),
                                height: Length::Fixed(body_h(i)),
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
                        ..Default::default()
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
    fn each_row_announces_its_place_in_the_collection() {
        let mut h = KeyedHarness::new(1000, 30.0, 2, 300.0);
        h.frame();
        let viewport = h.inner.viewport;
        let place = |h: &KeyedHarness, key: u64| {
            let host = h.host_for_key(ItemKey(key)).expect("mounted");
            let store = &h.inner.store;
            assert_eq!(store.semantics(host).map(|s| s.role), Some(Role::ListItem));
            store.semantic_state(host).and_then(|s| s.position)
        };
        assert_eq!(
            h.inner.store.semantics(viewport).map(|s| s.role),
            Some(Role::List)
        );
        assert_eq!(place(&h, 0), Some((1, 1000)));
        assert_eq!(place(&h, 2), Some((3, 1000)));
        let mut keys: Vec<u64> = (0..1000).collect();
        keys.swap(0, 2);
        h.reorder(keys);
        h.frame();
        assert_eq!(
            place(&h, 0),
            Some((3, 1000)),
            "a moved row tells its new place"
        );
        assert_eq!(place(&h, 2), Some((1, 1000)));
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
        assert_eq!(h.state().flow.top(top_logical), 600.0);
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
        assert_eq!(h.state().flow.top(top_logical), 600.0);
        assert_eq!(
            h.inner.store.scroll(h.inner.viewport).on(Axis::Column),
            600.0
        );
        // Logical 20 now shows key 21's host (the swap), still on screen.
        assert_eq!(h.host_of(20), h.host_for_key(ItemKey(21)));
    }

    /// A 1000-row list of 30px rows, 300px tall, overscan 2, whose every
    /// 20th row heads a section.
    fn sectioned() -> ListHarness {
        let mut h = ListHarness::new(1000, 30.0, 2, 300.0);
        h.state_mut().set_sticky_of(Some(Box::new(|i| i % 20 == 0)));
        h.frame();
        h
    }

    /// Scroll `h` to `to` and run a frame.
    fn scroll_to(h: &mut ListHarness, to: f32) {
        let now = h.store.scroll(h.viewport).y;
        h.store.scroll_by(
            h.viewport,
            Vec2 {
                x: 0.0,
                y: to - now,
            },
        );
        h.frame();
    }

    /// The host showing row `row` and the pin it shows, mounted in the window
    /// or pinned past it.
    fn header_host(h: &ListHarness, row: usize) -> (NodeId, f32) {
        let state = h.state();
        let host = state
            .mounted
            .iter()
            .chain(&state.pinned)
            .find(|m| m.logical_index == row)
            .expect("the header is mounted")
            .host;
        (host, h.store.translate(host).y)
    }

    #[test]
    fn a_section_header_stays_at_the_top_until_the_next_pushes_it_on() {
        let mut h = sectioned();
        assert_eq!(h.state().headers().len(), 50);
        // Inside section 0, the header sits pinned at the scroll offset.
        scroll_to(&mut h, 45.0);
        assert_eq!(header_host(&h, 0).1, 45.0);
        // Far past the window, row 0 is still mounted, pinned.
        scroll_to(&mut h, 500.0);
        assert!(h.state().pinned.is_some_and(|p| p.logical_index == 0));
        let (host, pin) = header_host(&h, 0);
        assert_eq!(pin, 500.0);
        assert!(h.store.is_pinned(host));
        assert!(
            h.store.is_layered(h.state().canvas()),
            "it paints over the rows"
        );
        // The next header, 600 down, pushes it: 30px short it starts to go.
        scroll_to(&mut h, 585.0);
        assert_eq!(header_host(&h, 0).1, 570.0);
        // In section 1, row 20 is the header; row 0 is gone.
        scroll_to(&mut h, 615.0);
        assert_eq!(header_host(&h, 20).1, 15.0);
        assert!(h.state().pinned.is_none(), "row 20 is in the window");
        assert!(!h.store.is_pinned(host), "the old header was unpinned");
        // Back up, the pinned row returns to the window as it was.
        scroll_to(&mut h, 900.0);
        let pinned = header_host(&h, 20).0;
        scroll_to(&mut h, 620.0);
        assert_eq!(header_host(&h, 20).0, pinned, "not rebuilt");
    }

    #[test]
    fn a_list_with_headers_paints_as_a_whole_repaint_would() {
        let mut h = sectioned();
        let mut out = Vec::new();
        h.store.repaint(h.viewport, &mut out);
        for to in [45.0, 500.0, 585.0, 615.0, 900.0, 620.0, 0.0] {
            let now = h.store.scroll(h.viewport).y;
            h.store.scroll_by(
                h.viewport,
                Vec2 {
                    x: 0.0,
                    y: to - now,
                },
            );
            for _ in 0..2 {
                reconcile(
                    &mut h.store,
                    &mut h.lists,
                    &mut h.states,
                    &mut h.bindings,
                    &mut h.effects,
                );
                h.store
                    .relayout_dirty(h.viewport, h.surface, &mut h.scratch, &mut h.redo);
                absorb_measurements(&h.store, &mut h.lists);
                h.store.repaint_dirty(h.viewport, &mut out);
                let mut whole = Vec::new();
                h.store.repaint(h.viewport, &mut whole);
                assert_eq!(out, whole, "at {to}");
                h.store.clear_dirty();
            }
        }
    }

    #[test]
    fn pinning_a_header_moves_it_without_a_relayout() {
        let mut h = sectioned();
        scroll_to(&mut h, 200.0);
        let (host, _) = header_host(&h, 0);
        h.store.scroll_by(h.viewport, Vec2 { x: 0.0, y: 5.0 });
        reconcile(
            &mut h.store,
            &mut h.lists,
            &mut h.states,
            &mut h.bindings,
            &mut h.effects,
        );
        let dirty = h.store.dirty(host);
        assert!(dirty.contains(DirtyClass::TRANSFORM));
        assert!(!dirty.intersects(DirtyClass::MEASURE | DirtyClass::LAYOUT));
        assert_eq!(h.store.translate(host).y, 205.0);
    }

    #[test]
    fn a_row_scrolled_to_shows_under_its_sections_header() {
        let mut h = sectioned();
        h.state_mut().scroll_to_index(25);
        h.frame();
        assert_eq!(h.store.scroll(h.viewport).y, 25.0 * 30.0 - 30.0);
        h.state_mut().scroll_to_index(40);
        h.frame();
        assert_eq!(
            h.store.scroll(h.viewport).y,
            40.0 * 30.0,
            "a header shows at the top"
        );
    }

    #[test]
    fn headers_follow_the_data() {
        let mut h = sectioned();
        scroll_to(&mut h, 500.0);
        h.state_mut().set_sticky_of(None);
        h.frame();
        assert!(h.state().headers().is_empty());
        assert!(h.state().pinned.is_none());
        assert!(!h.store.is_layered(h.state().canvas()));
        set_item_count(&mut h.lists, h.viewport, 10);
        h.frame();
        assert!(h.state().pool_len() > 0);
    }

    /// A 20-page column pager 300 tall, on Apple's coast.
    fn pager() -> ListHarness {
        let mut h = ListHarness::new(20, 50.0, 1, 300.0);
        let mut lists = VirtualLists::new();
        let mut text_edits = crate::text_edit::TextEdits::new();
        let mut projectors = crate::reactive::SemanticProjector::new();
        let mut store = NodeStore::new();
        store.set_scroll_physics(crate::scroll_motion::ScrollPhysics::APPLE);
        let viewport = BuildCx::with_reactive(
            &mut store,
            &mut h.states,
            &mut h.bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        )
        .virtual_list(
            VirtualListStyle {
                size: Size::fixed(100.0, 300.0),
                overscan: 1,
                estimated_row: 50.0,
                pages: true,
                snap: Some(crate::snap::ScrollSnap::PAGES),
                ..Default::default()
            },
            20,
            |_, cx| {
                cx.leaf(LeafStyle {
                    size: Size::fill(),
                    ..Default::default()
                });
            },
        )
        .id();
        store.mark_dirty(viewport, DirtyClass::MEASURE | DirtyClass::LAYOUT);
        h.store = store;
        h.lists = lists;
        h.viewport = viewport;
        h.frame();
        h.frame();
        h
    }

    /// Frames, the motion advancing in each, until `h` stands still.
    fn coast_out(h: &mut ListHarness) {
        for _ in 0..240 {
            if !h.store.is_scroll_moving() {
                return;
            }
            h.store
                .tick_scroll_motion(std::time::Duration::from_millis(16));
            h.frame();
        }
    }

    #[test]
    fn a_pager_s_pages_are_the_viewport_s_length_and_a_flick_turns_one() {
        let mut h = pager();
        assert_eq!(h.state().total_extent(), 20.0 * 300.0);
        let first = h.host_of(0).unwrap();
        assert_eq!(h.store.bounds(first).h, 300.0);
        h.store.fling(h.viewport, 4000.0);
        h.frame();
        assert!(h.store.is_gliding(h.viewport), "the list aimed the release");
        coast_out(&mut h);
        assert_eq!(h.store.scroll(h.viewport).y, 300.0);
        assert_eq!(h.store.snap_index(h.viewport), Some(1));
        // A slow drag past halfway settles on the next.
        h.store.set_scroll(h.viewport, Vec2 { x: 0.0, y: 480.0 });
        h.frame();
        h.store.settle_snaps_over(h.viewport);
        h.frame();
        coast_out(&mut h);
        assert_eq!(h.store.scroll(h.viewport).y, 600.0);
        // A shorter viewport keeps the page it shows.
        h.store
            .set_fixed_size(h.viewport, Size::fixed(100.0, 200.0));
        h.surface.h = 200.0;
        h.frame();
        h.frame();
        assert_eq!(h.state().total_extent(), 20.0 * 200.0);
        assert_eq!(h.store.scroll(h.viewport).y, 400.0, "still page 2");
        // Asked for a page far off, it glides there; past the last, the last.
        h.store.snap_to_item(h.viewport, 15);
        h.frame();
        coast_out(&mut h);
        assert_eq!(h.store.scroll(h.viewport).y, 15.0 * 200.0);
        assert_eq!(h.store.snap_index(h.viewport), Some(15));
        h.store.snap_to_item(h.viewport, 99);
        h.frame();
        coast_out(&mut h);
        assert_eq!(h.store.scroll(h.viewport).y, 19.0 * 200.0);
        assert_eq!(h.store.snap_index(h.viewport), Some(19));
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
    fn every_scroll_mounts_the_window_a_rebind_would() {
        // Mostly uniform rows, so most crossings slide the window; every seventh
        // row is taller, so measuring it dirties the model and rebinds.
        let mut h = ListHarness::with_bodies(
            5000,
            30.0,
            |i| if i % 7 == 3 { 52.0 } else { 30.0 },
            3,
            300.0,
        );
        h.frame();
        let mut seed = 0x9e37_79b9_u32;
        let mut rand = move |n: u32| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed % n
        };
        for frame in 0..400 {
            let dy = match rand(10) {
                0 => rand(20_000) as f32 - 8_000.0,
                1..=4 => rand(120) as f32 - 60.0,
                _ => rand(40) as f32,
            };
            h.store.scroll_by(h.viewport, Vec2 { x: 0.0, y: dy });
            h.frame();
            let s = h.state();
            let scroll = h.store.scroll(h.viewport).y;
            let (start, end) = s.flow.window(scroll, 300.0, 3);
            let range = start..end;
            if !s.dirty_data {
                assert_eq!(s.range_start..s.range_end, range, "frame {frame}");
            }
            let rows: Vec<_> = s.mounted.iter().map(|m| m.logical_index).collect();
            assert_eq!(rows, (s.range_start..s.range_end).collect::<Vec<_>>());
            let mut hosts: Vec<_> = s.mounted.iter().map(|m| m.host).collect();
            let mut children: Vec<_> = h.store.arena().children(s.canvas).collect();
            hosts.sort_unstable_by_key(|n| n.index());
            children.sort_unstable_by_key(|n| n.index());
            assert_eq!(hosts, children, "frame {frame}");
            for m in &s.mounted {
                assert_eq!(m.key, ItemKey(m.logical_index as u64));
                // A row measured this frame moves the rest on the next.
                if !s.dirty_data {
                    assert_eq!(m.item_top, s.flow.top(m.logical_index));
                }
            }
            let boxes = |h: &ListHarness| -> Vec<Rect> {
                h.state()
                    .mounted
                    .iter()
                    .flat_map(|m| [Some(m.host), h.store.arena().first_child(m.host)])
                    .flatten()
                    .map(|n| h.store.bounds(n))
                    .collect()
            };
            let incremental = boxes(&h);
            h.store.layout(h.viewport, h.surface, &mut h.scratch);
            assert_eq!(incremental, boxes(&h), "frame {frame}");
        }
    }

    #[test]
    fn mounting_rows_lays_out_only_the_rows_it_mounts() {
        let mut h = ListHarness::new(1000, 30.0, 4, 300.0);
        h.frame();
        h.store.scroll_by(h.viewport, Vec2 { x: 0.0, y: 3000.0 });
        h.frame();
        h.store.scroll_by(h.viewport, Vec2 { x: 0.0, y: 90.0 });
        let bound = reconcile(
            &mut h.store,
            &mut h.lists,
            &mut h.states,
            &mut h.bindings,
            &mut h.effects,
        );
        assert_eq!(bound, 3);
        let (_, laid_out) =
            h.store
                .relayout_dirty(h.viewport, h.surface, &mut h.scratch, &mut h.redo);
        let per_row = h.store.subtree_len(h.state().mounted[0].host);
        assert_eq!(laid_out, 3 * per_row, "the three new rows, not the window");
        // Where a whole layout puts every mounted row.
        let boxes = |h: &ListHarness| -> Vec<Rect> {
            h.state()
                .mounted
                .iter()
                .flat_map(|m| {
                    let body = h.store.arena().first_child(m.host);
                    [Some(m.host), body]
                })
                .flatten()
                .map(|n| h.store.bounds(n))
                .collect()
        };
        let incremental = boxes(&h);
        h.store.layout(h.viewport, h.surface, &mut h.scratch);
        assert_eq!(incremental, boxes(&h));
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
        assert_eq!(h.state().flow.extent(0), 40.0);
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
        assert_eq!(h.state().flow.top(anchor), 600.0);
        assert_eq!(h.store.scroll(h.viewport).on(Axis::Column), 600.0);

        // Grow row 5 (above the anchor) by 100px, and correct the scroll to keep
        // the anchor's top pinned: the anchor's new absolute offset is 700, so the
        // scroll must follow to 700 for the same on-screen top. This is the scroll
        // correction a real anchored reconcile applies; here we assert the model
        // supports it — the anchor's prefix moved by exactly the edit.
        h.state_mut().flow.update(5, 130.0);
        h.state_mut().mark_data_dirty();
        assert_eq!(
            h.state().flow.top(anchor),
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
            .first_child(host)
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
        h.effects.run(effect_id, &mut h.states);
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

    #[test]
    fn recycled_rows_share_one_template_and_leak_nothing() {
        // Each row keeps a state of its own bound to its body, and binds the
        // list's shared selection. Scrolling far, again and again, rebuilds
        // rows on recycled hosts: their states, edges and slots must be reused,
        // not accumulated, and every row shares one template.
        let mut h = ListHarness::new(10_000, 30.0, 2, 300.0);
        let selection = h.states.alloc(crate::state::StateValue::Int(0));
        h.state_mut().builder = Box::new(move |_i, cx| {
            let count = cx.state(crate::state::StateValue::Int(0));
            let leaf = cx.leaf(LeafStyle {
                size: Size {
                    width: Length::fill(),
                    height: Length::Fixed(30.0),
                },
                ..Default::default()
            });
            cx.bind(count, leaf, DirtyClass::PAINT);
            cx.bind(selection, leaf, DirtyClass::STYLE);
        });
        h.frame();
        let mut warm = None;
        for step in 1..40 {
            h.store.scroll_by(
                h.viewport,
                Vec2 {
                    x: 0.0,
                    y: 3000.0 * (step % 3) as f32,
                },
            );
            h.frame();
            let sizes = (
                h.store.arena().slot_count(),
                h.states.slot_count(),
                h.bindings.footprint(),
            );
            if step == 4 {
                warm = Some(sizes);
            } else if let Some(warm) = warm {
                assert_eq!(sizes, warm, "step {step}: recycling grew the stores");
            }
        }
        let footprint = h.bindings.footprint();
        assert_eq!(footprint.templates, 1);
        assert_eq!(footprint.template_edges, 2);
        assert_eq!(footprint.absolute_edges, 0);
        assert_eq!(footprint.instances, h.state().mounted_count());

        // The shared selection reaches every mounted row through the template.
        h.states.set(selection, crate::state::StateValue::Int(1));
        let mut pending = Vec::new();
        h.states.take_pending(&mut pending);
        h.store.flush_state_transactions(&pending, &h.bindings);
        for row in &h.state().mounted {
            let body = h.store.arena().first_child(row.host).unwrap();
            assert!(h.store.dirty(body).contains(DirtyClass::STYLE));
        }
    }

    fn lanes_of(
        grid_columns: crate::virtual_grid::Columns,
        masonry: bool,
    ) -> crate::virtual_grid::ListGrid {
        crate::virtual_grid::ListGrid {
            columns: grid_columns,
            gap: 10.0,
            cross_gap: 5.0,
            masonry,
        }
    }

    fn settle_frames(h: &mut ListHarness) {
        for _ in 0..4 {
            h.frame();
        }
    }

    #[test]
    fn a_grid_lays_its_items_on_lines_of_lanes() {
        use crate::virtual_grid::Columns;
        let mut h = ListHarness::gridded(
            100,
            50.0,
            |_| 50.0,
            1,
            200.0,
            Some(lanes_of(Columns::Fixed(3), false)),
        );
        settle_frames(&mut h);
        let s = h.state();
        assert_eq!(s.lanes(), 3);
        // Lines at 0, 60, 120, 180 show, and one more past them.
        assert_eq!((s.range_start, s.range_end), (0, 15));
        assert_eq!(s.total_extent(), 34.0 * 60.0 - 10.0);
        let b = |h: &ListHarness, i| h.store.bounds(h.host_of(i).unwrap());
        // Lanes 30 wide, 5 apart.
        assert_eq!(
            b(&h, 0),
            Rect {
                x: 0.0,
                y: 0.0,
                w: 30.0,
                h: 50.0
            }
        );
        assert_eq!(
            b(&h, 2),
            Rect {
                x: 70.0,
                y: 0.0,
                w: 30.0,
                h: 50.0
            }
        );
        assert_eq!(
            b(&h, 4),
            Rect {
                x: 35.0,
                y: 60.0,
                w: 30.0,
                h: 50.0
            }
        );
        h.store.set_scroll(h.viewport, Vec2 { x: 0.0, y: 600.0 });
        h.frame();
        let s = h.state();
        assert_eq!((s.range_start, s.range_end), (27, 45));
        assert_eq!(s.visible(600.0, 200.0), (30, 41, 100));
    }

    #[test]
    fn a_grid_read_right_to_left_starts_its_lanes_at_the_right() {
        use crate::virtual_grid::Columns;
        let mut h = ListHarness::gridded(
            9,
            50.0,
            |_| 50.0,
            1,
            200.0,
            Some(lanes_of(Columns::Fixed(3), false)),
        );
        h.store
            .set_layout_direction(h.viewport, Some(crate::adaptive::LayoutDirection::Rtl));
        settle_frames(&mut h);
        let x = |h: &ListHarness, i| h.store.bounds(h.host_of(i).unwrap()).x;
        assert_eq!([x(&h, 0), x(&h, 1), x(&h, 2)], [70.0, 35.0, 0.0]);
    }

    #[test]
    fn adaptive_lanes_recount_as_the_grid_resizes() {
        use crate::virtual_grid::Columns;
        let mut h = ListHarness::gridded(
            60,
            50.0,
            |_| 50.0,
            0,
            200.0,
            Some(lanes_of(Columns::Adaptive { min: 40.0 }, false)),
        );
        settle_frames(&mut h);
        assert_eq!(h.state().lanes(), 2);
        h.store.set_fixed_size(
            h.viewport,
            Size {
                width: Length::Fixed(220.0),
                height: Length::Fixed(200.0),
            },
        );
        h.surface.w = 220.0;
        settle_frames(&mut h);
        // floor((220 + 5) / 45) = 5 lanes, 40 wide.
        assert_eq!(h.state().lanes(), 5);
        assert_eq!(h.state().total_extent(), 12.0 * 60.0 - 10.0);
        let b = h.store.bounds(h.host_of(6).unwrap());
        assert_eq!((b.x, b.y, b.w), (45.0, 60.0, 40.0));
    }

    #[test]
    fn a_masonry_packs_its_items_without_overlap() {
        use crate::virtual_grid::Columns;
        let tall = |i: usize| if i.is_multiple_of(3) { 90.0 } else { 30.0 };
        let mut h = ListHarness::gridded(
            200,
            30.0,
            tall,
            1,
            300.0,
            Some(lanes_of(Columns::Fixed(2), true)),
        );
        settle_frames(&mut h);
        for scroll in [0.0, 400.0, 1500.0, 700.0] {
            h.store.set_scroll(h.viewport, Vec2 { x: 0.0, y: scroll });
            settle_frames(&mut h);
            let s = h.state();
            let mut boxes: Vec<Rect> = s.mounted.iter().map(|m| h.store.bounds(m.host)).collect();
            assert!(!boxes.is_empty());
            for m in &s.mounted {
                assert_eq!(h.store.bounds(m.host).h, tall(m.logical_index));
            }
            boxes.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
            for w in boxes.windows(2) {
                if w[0].x == w[1].x {
                    assert!(
                        w[0].y + w[0].h + 10.0 <= w[1].y + 0.01,
                        "at {scroll}: {w:?}"
                    );
                }
            }
            // Every item the viewport shows is mounted.
            let (first, last, _) = s.visible(scroll, 300.0);
            assert!(s.range_start <= first && last < s.range_end);
        }
    }

    #[test]
    fn a_grid_s_headers_span_the_line() {
        use crate::virtual_grid::Columns;
        let mut h = ListHarness::gridded(
            40,
            50.0,
            |_| 50.0,
            1,
            200.0,
            Some(lanes_of(Columns::Fixed(3), false)),
        );
        set_sticky_of(&mut h.lists, h.viewport, Some(Box::new(|i| i % 10 == 0)));
        set_full_of(&mut h.lists, h.viewport, Some(Box::new(|i| i == 5)));
        settle_frames(&mut h);
        let b = |h: &ListHarness, i| h.store.bounds(h.host_of(i).unwrap());
        assert_eq!(
            b(&h, 0),
            Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 50.0
            }
        );
        assert_eq!((b(&h, 1).x, b(&h, 1).y), (0.0, 60.0));
        assert_eq!((b(&h, 4).x, b(&h, 4).y), (0.0, 120.0));
        assert_eq!(
            b(&h, 5),
            Rect {
                x: 0.0,
                y: 180.0,
                w: 100.0,
                h: 50.0
            }
        );
        assert_eq!((b(&h, 6).x, b(&h, 6).y), (0.0, 240.0));
    }
}
