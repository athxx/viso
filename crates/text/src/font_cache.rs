//! The Font Face Cache: a byte-budgeted Segmented LRU over loaded faces.
//!
//! A loaded face is expensive — owned decoded bytes, parsed tables, shaping
//! state, a coverage accelerator — and cost varies enormously (a CJK or color
//! face dwarfs a Latin one), so the cache is budgeted in *bytes*, never in a
//! fixed face count. It keeps hot faces resident and lets one-shot faces fall
//! out without evicting the working set.
//!
//! # Segmented LRU
//!
//! A face lives in one of two segments:
//!
//! - Probation — newly admitted or cold faces.
//! - Protected — faces that earned a second meaningful reuse.
//!
//! A new face enters Probation. A second reuse promotes it to Protected. When
//! Protected grows past its byte target the coldest Protected face is demoted
//! back to Probation. When the total exceeds the budget, cold Probation faces
//! are evicted first — a font-picker previewing hundreds of one-shot faces
//! cannot flush the hot working set out of Protected (anti scan-pollution).
//!
//! # Recency is epoch-merged, never per-glyph
//!
//! A face is touched once per shaping run / paragraph epoch, not once per glyph.
//! Touches within an epoch are folded into a single recency update by
//! [`advance_epoch`]; drawing one face ten thousand times in a frame is one
//! recency update, and never mutates any intrusive list per glyph. This is the
//! steady-state guard: glyph draw does not reach into this cache.
//!
//! # Pinning
//!
//! The current default UI face, in-flight shaping/raster faces, and an
//! explicitly pinned document face can be pinned so they are never demoted or
//! evicted while their scope is live. Pinning is scoped, not a permanent
//! residency API.
//!
//! [`advance_epoch`]: FontCache::advance_epoch

use std::collections::HashMap;
use std::collections::HashSet;

use crate::FontFaceId;

/// Which SLRU segment a resident face is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Segment {
    /// Newly admitted or cold: evicted first under budget pressure.
    Probation,
    /// Earned a second reuse: demoted before eviction, never evicted directly.
    Protected,
}

/// One resident face's bookkeeping: its segment, byte cost, recency, and reuse
/// state. The parsed face payload itself is reconstructed elsewhere from the
/// resolver's owned bytes; this entry accounts for its resident cost.
#[derive(Debug)]
struct FaceEntry {
    segment: Segment,
    /// Estimated resident cost in bytes: owned bytes plus parsed/shaping/
    /// coverage state. Drives the budget, since face count does not reflect
    /// cost.
    cost_bytes: u64,
    /// The epoch this face was last used in, for coldest-first selection.
    last_touched_epoch: u64,
    /// Whether the face has been reused since admission; a reuse while already
    /// reused-or-protected promotes it to Protected.
    reused: bool,
}

/// A byte-budgeted Segmented LRU over loaded font faces.
#[derive(Debug)]
pub struct FontCache {
    entries: HashMap<FontFaceId, FaceEntry>,
    /// Faces touched in the current epoch, folded into recency by
    /// [`advance_epoch`]. This is the epoch-merge buffer that keeps recency off
    /// the per-glyph path.
    used_this_epoch: HashSet<FontFaceId>,
    /// Scoped pins: never demoted or evicted while present.
    pinned: HashSet<FontFaceId>,
    /// Total resident cost of all entries.
    total_bytes: u64,
    /// Total byte budget; exceeding it drives demotion/eviction.
    budget_bytes: u64,
    /// Byte target for the Protected segment; exceeding it demotes the coldest
    /// Protected face to Probation.
    protected_target_bytes: u64,
    /// Monotonic epoch counter; advanced once per frame / paragraph pass.
    epoch: u64,
}

impl FontCache {
    /// A cache with the given total byte budget.
    ///
    /// The Protected segment targets roughly 80% of the budget, leaving
    /// headroom for Probation churn; the remaining budget absorbs one-shot
    /// faces without displacing the protected working set.
    pub fn with_budget(budget_bytes: u64) -> Self {
        Self {
            entries: HashMap::new(),
            used_this_epoch: HashSet::new(),
            pinned: HashSet::new(),
            total_bytes: 0,
            budget_bytes,
            protected_target_bytes: budget_bytes / 5 * 4,
            epoch: 0,
        }
    }

    /// The total resident cost across both segments.
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// The number of resident faces (diagnostic; the budget is in bytes).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache holds no faces.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether a face is currently resident.
    pub fn contains(&self, id: FontFaceId) -> bool {
        self.entries.contains_key(&id)
    }

    /// Admit a face with an estimated resident cost, or record a reuse if it is
    /// already resident.
    ///
    /// A first admission enters Probation. Admitting an already-resident face is
    /// a reuse: the second meaningful reuse promotes it to Protected. Either way
    /// the face is marked used in the current epoch. Budget is enforced after
    /// admission.
    pub fn admit(&mut self, id: FontFaceId, cost_bytes: u64) {
        self.used_this_epoch.insert(id);

        if let Some(entry) = self.entries.get_mut(&id) {
            // Reuse: the second meaningful reuse earns Protected.
            if entry.segment == Segment::Probation {
                if entry.reused {
                    entry.segment = Segment::Protected;
                } else {
                    entry.reused = true;
                }
            }
            return;
        }

        self.entries.insert(
            id,
            FaceEntry {
                segment: Segment::Probation,
                cost_bytes,
                last_touched_epoch: self.epoch,
                reused: false,
            },
        );
        self.total_bytes += cost_bytes;
        self.enforce_budget();
    }

    /// Mark a face used in the current epoch without changing its cost.
    ///
    /// This is the shaping-run / paragraph-epoch touch. Many glyph draws that
    /// use the same face collapse to one touch per epoch; recency is applied
    /// only by [`advance_epoch`], so nothing here mutates per glyph.
    pub fn touch(&mut self, id: FontFaceId) {
        if self.entries.contains_key(&id) {
            self.used_this_epoch.insert(id);
        }
    }

    /// Fold this epoch's touches into recency and advance the epoch.
    ///
    /// Every face touched during the epoch has its `last_touched_epoch` set to
    /// the current epoch exactly once, regardless of how many times it was used.
    /// A touched Probation face that had already been used earns promotion to
    /// Protected on this fold. Call once per frame / paragraph pass.
    pub fn advance_epoch(&mut self) {
        for id in self.used_this_epoch.drain() {
            if let Some(entry) = self.entries.get_mut(&id) {
                entry.last_touched_epoch = self.epoch;
                if entry.segment == Segment::Probation {
                    if entry.reused {
                        entry.segment = Segment::Protected;
                    } else {
                        entry.reused = true;
                    }
                }
            }
        }
        self.epoch += 1;
        self.enforce_protected_target();
        self.enforce_budget();
    }

    /// Pin a face so it is never demoted or evicted while pinned.
    pub fn pin(&mut self, id: FontFaceId) {
        self.pinned.insert(id);
    }

    /// Release a scoped pin, returning the face to normal eviction eligibility.
    pub fn unpin(&mut self, id: FontFaceId) {
        self.pinned.remove(&id);
    }

    /// Whether a face is currently pinned.
    pub fn is_pinned(&self, id: FontFaceId) -> bool {
        self.pinned.contains(&id)
    }

    /// Demote the coldest Protected faces to Probation while Protected exceeds
    /// its byte target. Pinned faces are never demoted.
    fn enforce_protected_target(&mut self) {
        loop {
            let protected_bytes: u64 = self
                .entries
                .iter()
                .filter(|(_, e)| e.segment == Segment::Protected)
                .map(|(_, e)| e.cost_bytes)
                .sum();
            if protected_bytes <= self.protected_target_bytes {
                break;
            }
            let Some(coldest) = self.coldest(Segment::Protected) else {
                break;
            };
            self.entries.get_mut(&coldest).unwrap().segment = Segment::Probation;
        }
    }

    /// Evict cold Probation faces (then, if still over, demote-and-evict) until
    /// the total is within budget. Pinned faces are never evicted.
    fn enforce_budget(&mut self) {
        while self.total_bytes > self.budget_bytes {
            // Prefer evicting cold Probation; only if none remains, demote the
            // coldest Protected into Probation so it can be evicted next.
            if let Some(victim) = self.coldest(Segment::Probation) {
                let entry = self.entries.remove(&victim).unwrap();
                self.total_bytes -= entry.cost_bytes;
            } else if let Some(coldest) = self.coldest(Segment::Protected) {
                self.entries.get_mut(&coldest).unwrap().segment = Segment::Probation;
            } else {
                // Everything left is pinned; the budget cannot be met without
                // violating a live scope, so stop rather than evict a pin.
                break;
            }
        }
    }

    /// The coldest (oldest `last_touched_epoch`) unpinned face in a segment.
    fn coldest(&self, segment: Segment) -> Option<FontFaceId> {
        self.entries
            .iter()
            .filter(|(id, e)| e.segment == segment && !self.pinned.contains(id))
            .min_by_key(|(_, e)| e.last_touched_epoch)
            .map(|(id, _)| *id)
    }

    /// The segment a resident face is in, for tests / diagnostics.
    #[cfg(test)]
    fn segment_of(&self, id: FontFaceId) -> Option<Segment> {
        self.entries.get(&id).map(|e| e.segment)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u32) -> FontFaceId {
        FontFaceId(n)
    }

    #[test]
    fn admit_accounts_bytes_not_count() {
        let mut cache = FontCache::with_budget(10_000);
        cache.admit(id(0), 100);
        cache.admit(id(1), 4_000);
        assert_eq!(cache.total_bytes(), 4_100);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn second_reuse_promotes_to_protected() {
        let mut cache = FontCache::with_budget(10_000);
        cache.admit(id(0), 100);
        assert_eq!(cache.segment_of(id(0)), Some(Segment::Probation));
        // First reuse keeps it in Probation but marks it reused.
        cache.admit(id(0), 100);
        assert_eq!(cache.segment_of(id(0)), Some(Segment::Probation));
        // Second meaningful reuse earns Protected.
        cache.admit(id(0), 100);
        assert_eq!(cache.segment_of(id(0)), Some(Segment::Protected));
    }

    #[test]
    fn budget_evicts_cold_probation_first() {
        let mut cache = FontCache::with_budget(300);
        // Three probation faces at 100 each fit exactly.
        cache.admit(id(0), 100);
        cache.advance_epoch();
        cache.admit(id(1), 100);
        cache.advance_epoch();
        cache.admit(id(2), 100);
        cache.advance_epoch();
        assert_eq!(cache.len(), 3);

        // A fourth pushes over budget; the coldest probation face (id 0) goes.
        cache.admit(id(3), 100);
        assert!(!cache.contains(id(0)));
        assert!(cache.contains(id(3)));
        assert_eq!(cache.total_bytes(), 300);
    }

    #[test]
    fn scan_of_one_shot_faces_does_not_flush_protected() {
        // Budget holds a hot protected face plus some probation churn.
        let mut cache = FontCache::with_budget(500);
        let hot = id(1000);
        // Promote the hot face to Protected via reuse across epochs.
        cache.admit(hot, 100);
        cache.advance_epoch();
        cache.admit(hot, 100);
        cache.advance_epoch();
        cache.admit(hot, 100);
        cache.advance_epoch();
        assert_eq!(cache.segment_of(hot), Some(Segment::Protected));

        // A font-picker previews 50 one-shot faces, each used once.
        for n in 0..50 {
            cache.admit(id(n), 100);
            cache.advance_epoch();
        }

        // The hot protected face survived the scan.
        assert!(cache.contains(hot));
        assert_eq!(cache.segment_of(hot), Some(Segment::Protected));
    }

    #[test]
    fn recency_is_epoch_merged_not_per_glyph() {
        let mut cache = FontCache::with_budget(10_000);
        cache.admit(id(0), 100);
        cache.advance_epoch();
        let after_admit = cache.entries[&id(0)].last_touched_epoch;

        // Simulate 10_000 glyph draws in one epoch: many touches, and touch must
        // not itself move recency — only advance_epoch does, once.
        for _ in 0..10_000 {
            cache.touch(id(0));
        }
        assert_eq!(
            cache.entries[&id(0)].last_touched_epoch,
            after_admit,
            "touch must not update recency per glyph"
        );

        cache.advance_epoch();
        assert!(
            cache.entries[&id(0)].last_touched_epoch > after_admit,
            "advance_epoch folds the epoch's touches into one recency update"
        );
    }

    #[test]
    fn pinned_face_is_never_evicted() {
        let mut cache = FontCache::with_budget(200);
        let pinned = id(0);
        cache.admit(pinned, 200);
        cache.pin(pinned);
        cache.advance_epoch();

        // Admitting more than the budget cannot evict the pinned face.
        cache.admit(id(1), 200);
        cache.admit(id(2), 200);
        assert!(cache.contains(pinned));

        // Once unpinned it becomes eligible again.
        cache.unpin(pinned);
        cache.admit(id(3), 200);
        assert!(!cache.contains(pinned));
    }

    #[test]
    fn protected_over_target_demotes_coldest() {
        // Budget 1000, protected target 800. Fill protected past the target so a
        // demotion must occur.
        let mut cache = FontCache::with_budget(1_000);
        // Promote five 200-byte faces to Protected across distinct epochs so
        // they have a clear cold-to-hot ordering.
        for n in 0..5 {
            cache.admit(id(n), 200);
            cache.advance_epoch();
            cache.admit(id(n), 200);
            cache.advance_epoch();
        }
        // Protected target is 800 bytes = 4 faces; the coldest (id 0) is demoted.
        let protected_count = (0..5)
            .filter(|&n| cache.segment_of(id(n)) == Some(Segment::Protected))
            .count();
        assert!(protected_count <= 4);
        assert_eq!(cache.segment_of(id(0)), Some(Segment::Probation));
    }
}
