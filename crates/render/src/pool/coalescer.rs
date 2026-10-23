//! Dirty-range coalescing: turn the slots that changed this frame into a few
//! contiguous upload ranges rather than one `write_buffer` per changed slot.
//!
//! # Why bridge clean slots
//!
//! Diffing a lowered instance array against the CPU shadow yields a set of
//! changed slots. Uploading each maximal run of *adjacent* changed slots as one
//! range (what the raw diff produces) is already far better than one copy per
//! slot — but it splits on the very first clean slot. A row of controls where
//! every other slot changed would then issue a separate `write_buffer` for each
//! changed slot, and a `write_buffer` has a fixed per-call cost (encoder record,
//! offset/size validation, driver bookkeeping) that dwarfs the bytes when the
//! run is one instance wide.
//!
//! The coalescer trades a few redundantly re-uploaded clean slots for far fewer
//! calls: two changed runs separated by a gap of at most [`GAP_THRESHOLD`] clean
//! slots merge into **one** range that re-uploads the clean slots in between.
//! Past that threshold the gap is large enough that shipping the clean bytes
//! costs more than a second call, so the range splits. `GAP_THRESHOLD` is the
//! break-even point between "one bigger copy" and "two calls".
//!
//! # Allocation
//!
//! Coalescing is a hot-path, per-frame step. [`Runs`] folds the
//! changed slots in as the diff meets them and hands back each range as it
//! closes, so the pool uploads while it diffs and keeps no list of changed
//! slots or ranges: whatever changed, no frame touches the heap. A frame with
//! no changed slots produces zero ranges and therefore issues zero uploads
//! for that family.

/// The most clean slots the coalescer will re-upload to bridge two changed runs
/// into one range instead of splitting into two `write_buffer` calls.
///
/// A gap of `g` clean slots between two changed runs costs `g` slots' worth of
/// redundant bytes if bridged, versus one extra `write_buffer` call if split.
/// Below this many clean slots the bridge wins; at or above it the split wins.
/// Chosen for the per-slot instance strides in this renderer (tens of bytes),
/// where one avoided call is worth re-shipping a handful of clean slots.
pub const GAP_THRESHOLD: usize = 4;

/// A contiguous run of slots to upload as one `write_buffer`, measured in slots
/// (element units, not bytes): slots `[start, start + len)`.
///
/// The caller multiplies by the family's stride to get the byte offset and byte
/// length for the actual upload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    /// First slot in the run.
    pub start: usize,
    /// Number of slots in the run (always non-zero for an emitted range).
    pub len: usize,
}

/// Coalesces changed slots met in strictly increasing order into upload
/// ranges: two changed slots whose gap of clean slots between them is at most
/// [`GAP_THRESHOLD`] fall in one range spanning both (and the clean slots
/// between them); a larger gap closes the range and opens another.
#[derive(Clone, Copy, Debug, Default)]
pub struct Runs {
    open: Option<Range>,
}

impl Runs {
    /// Fold in the changed `slot`; the range it closed, if its gap from the
    /// open range is too wide to bridge.
    #[inline]
    pub fn push(&mut self, slot: usize) -> Option<Range> {
        let Some(open) = &mut self.open else {
            self.open = Some(Range {
                start: slot,
                len: 1,
            });
            return None;
        };
        let end = open.start + open.len;
        debug_assert!(slot >= end, "dirty slots must be strictly increasing");
        if slot - end <= GAP_THRESHOLD {
            open.len = slot + 1 - open.start;
            None
        } else {
            self.open.replace(Range {
                start: slot,
                len: 1,
            })
        }
    }

    /// The range still open, once every changed slot is in.
    #[inline]
    pub fn finish(self) -> Option<Range> {
        self.open
    }
}

/// Coalesce a sorted, de-duplicated list of changed slot indices into upload
/// ranges, appending them to `out` (which is cleared first), as [`Runs`]
/// does. An empty `dirty` yields zero ranges (the unchanged-frame /
/// zero-upload case).
pub fn coalesce(dirty: &[usize], out: &mut Vec<Range>) {
    out.clear();
    let mut runs = Runs::default();
    for &slot in dirty {
        out.extend(runs.push(slot));
    }
    out.extend(runs.finish());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collect ranges for a dirty list into a fresh vec (test convenience).
    fn ranges(dirty: &[usize]) -> Vec<Range> {
        let mut out = Vec::new();
        coalesce(dirty, &mut out);
        out
    }

    #[test]
    fn no_dirty_slots_yield_no_ranges() {
        assert!(ranges(&[]).is_empty());
    }

    #[test]
    fn single_dirty_slot_is_one_minimal_range() {
        // The hover case: one primitive repainted -> one one-slot range.
        assert_eq!(ranges(&[7]), vec![Range { start: 7, len: 1 }]);
    }

    #[test]
    fn adjacent_slots_are_one_range() {
        assert_eq!(ranges(&[3, 4, 5]), vec![Range { start: 3, len: 3 }]);
    }

    #[test]
    fn slots_within_the_gap_threshold_bridge_into_one_range() {
        // Gap of exactly GAP_THRESHOLD clean slots between the two changed slots
        // is bridged: one range covering the clean slots in between.
        let a = 2;
        let b = a + 1 + GAP_THRESHOLD; // GAP_THRESHOLD clean slots between them.
        assert_eq!(
            ranges(&[a, b]),
            vec![Range {
                start: a,
                len: b - a + 1,
            }]
        );
    }

    #[test]
    fn slots_past_the_gap_threshold_split_into_separate_ranges() {
        // One more clean slot than the threshold: too wide to bridge -> two ranges.
        let a = 2;
        let b = a + 2 + GAP_THRESHOLD; // GAP_THRESHOLD + 1 clean slots between them.
        assert_eq!(
            ranges(&[a, b]),
            vec![Range { start: a, len: 1 }, Range { start: b, len: 1 },]
        );
    }

    #[test]
    fn scattered_slots_merge_and_split_by_gap() {
        // 0,1 adjacent; small gap to 3 (bridged); wide gap to 20 (split);
        // 21 adjacent to 20.
        assert_eq!(
            ranges(&[0, 1, 3, 20, 21]),
            vec![
                Range { start: 0, len: 4 },  // 0..=3, bridging clean slot 2
                Range { start: 20, len: 2 }, // 20..=21
            ]
        );
    }

    #[test]
    fn reused_out_buffer_is_cleared_each_call() {
        let mut out = Vec::new();
        coalesce(&[1, 2, 3], &mut out);
        assert_eq!(out.len(), 1);
        // A second call with a different dirty set overwrites, never appends.
        coalesce(&[10, 100], &mut out);
        assert_eq!(
            out,
            vec![Range { start: 10, len: 1 }, Range { start: 100, len: 1 }]
        );
    }

    #[test]
    fn reused_out_buffer_does_not_grow_within_high_water() {
        // Warm to a range count, then a frame within that count reuses capacity.
        let mut out = Vec::new();
        coalesce(&[0, 10, 20, 30], &mut out); // 4 ranges (all wide gaps)
        let warm_cap = out.capacity();
        coalesce(&[0, 10], &mut out); // 2 ranges, fits
        assert_eq!(
            out.capacity(),
            warm_cap,
            "no reallocation within high-water"
        );
    }
}
