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
}
