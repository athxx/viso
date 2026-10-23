//! Dirty invalidation classes.
//!
//! Every property/state binding must declare what it invalidates, using these
//! explicit classes rather than a single coarse `dirty = true`. Propagation
//! must stop at valid boundaries — a paint-only change must not make ancestors
//! layout-dirty.

pub use viso_schema::DirtyClass;

use crate::node::NodeId;
use crate::state::StateId;

/// Per-class dirty worklists: for each class, the slots that gained
/// it since the last [`clear`](DirtyLists::clear), each appended once on that
/// class's 0→1 transition. A pass reads its class's list instead of scanning
/// every node, so an idle frame's passes cost O(1) and a busy frame's cost the
/// nodes it marked. The buffers keep their capacity frame to frame.
///
/// A list may name a slot whose class was since reset (a node freed and its
/// slot reused within the frame); a reader checks the slot's current classes,
/// so such an entry costs one load and changes nothing.
#[derive(Debug, Default)]
pub struct DirtyLists {
    by_class: [Vec<u32>; 8],
    /// Every slot marked at all, once: what `clear` resets.
    marked: Vec<u32>,
    /// Per slot, the classes whose list holds it. Survives a slot reset within
    /// the frame, so no list holds a slot twice.
    listed: Vec<DirtyClass>,
}

impl DirtyLists {
    /// Record that slot `index` gained `added`. A class the slot is already
    /// listed under appends nothing.
    #[inline]
    pub fn note(&mut self, index: u32, added: DirtyClass) {
        let i = index as usize;
        if i >= self.listed.len() {
            self.listed.resize(i + 1, DirtyClass::EMPTY);
        }
        let listed = self.listed[i];
        let new = added.without(listed);
        if new.is_empty() {
            return;
        }
        if listed.is_empty() {
            self.marked.push(index);
        }
        self.listed[i] = listed | new;
        let mut bits = new.bits();
        while bits != 0 {
            let bit = bits.trailing_zeros() as usize;
            self.by_class[bit].push(index);
            bits &= bits - 1;
        }
    }

    /// The slots listed under the single class `class`, in marking order.
    #[inline]
    pub fn of(&self, class: DirtyClass) -> &[u32] {
        debug_assert_eq!(class.bits().count_ones(), 1, "one class at a time");
        &self.by_class[class.bits().trailing_zeros() as usize]
    }

    /// Every slot marked since the last clear, once each.
    #[inline]
    pub fn marked(&self) -> &[u32] {
        &self.marked
    }

    /// Whether some slot listed under a class of `class` still carries it in
    /// `dirty`. O(1) when those lists are empty.
    pub fn any(&self, class: DirtyClass, dirty: &[DirtyClass]) -> bool {
        let mut bits = class.bits();
        while bits != 0 {
            let bit = bits.trailing_zeros() as usize;
            let one = DirtyClass::bit(bit as u32);
            if self.by_class[bit]
                .iter()
                .any(|&i| dirty[i as usize].intersects(one))
            {
                return true;
            }
            bits &= bits - 1;
        }
        false
    }

    /// Reserve room for `slots` slots, so marking allocates nothing.
    pub fn reserve_slots(&mut self, slots: usize) {
        self.listed.reserve(slots.saturating_sub(self.listed.len()));
    }

    /// Reset every marked slot's classes in `dirty` and empty the lists,
    /// keeping their capacity. Costs the marked slots, not the store.
    pub fn clear(&mut self, dirty: &mut [DirtyClass]) {
        for &i in &self.marked {
            let i = i as usize;
            if let Some(d) = dirty.get_mut(i) {
                *d = DirtyClass::EMPTY;
            }
            self.listed[i] = DirtyClass::EMPTY;
        }
        self.marked.clear();
        for list in &mut self.by_class {
            list.clear();
        }
    }

    /// Forget everything, for a store dropping every node.
    pub fn reset(&mut self) {
        self.marked.clear();
        self.listed.clear();
        for list in &mut self.by_class {
            list.clear();
        }
    }
}

/// Why a node was marked dirty, as an inspector's journal records it: the
/// frame-loop phase that marked it, or the state cell whose bindings fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DirtyCause {
    /// A mark outside any phase the frame loop names: a build, a handler's
    /// own write, a structural edit.
    #[default]
    Other,
    /// An input event routed through the tree: hover, press, focus, a
    /// handler it ran.
    Input,
    /// A state cell changed and its binding edges fired.
    State(StateId),
    /// A computed value re-evaluated.
    Computed,
    /// An animation or transition advanced.
    Animation,
    /// A timer fired or a task finished.
    Task,
    /// A hot reload committed.
    Reload,
    /// Text shaping or an edit committed.
    Text,
    /// An asset arrived or decoded.
    Asset,
    /// The window's size, scale or environment changed.
    Window,
    /// Layout reconciled a virtual list or settled an adaptive scope.
    Layout,
}

/// One direct mark: the classes `node` was marked dirty for and why. What
/// the classes bubble to its ancestors follows from the tree and is not
/// recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirtyMark {
    pub node: NodeId,
    pub class: DirtyClass,
    pub cause: DirtyCause,
}

/// The marks a store records while an inspector watches it, bounded: past
/// [`DirtyJournal::MAX_MARKS`] it counts what it drops.
#[cfg(feature = "inspect")]
#[derive(Debug, Default)]
pub(crate) struct DirtyJournal {
    pub(crate) cause: DirtyCause,
    pub(crate) marks: Vec<DirtyMark>,
    pub(crate) dropped: u32,
}

#[cfg(feature = "inspect")]
impl DirtyJournal {
    /// The most marks held between two drains.
    pub(crate) const MAX_MARKS: usize = 16_384;

    /// Records a mark of `class` on `node` under the current cause, folded
    /// into the last one when it marks the same node for the same cause.
    #[inline]
    pub(crate) fn record(&mut self, node: NodeId, class: DirtyClass) {
        let cause = self.cause;
        if let Some(last) = self.marks.last_mut()
            && last.node == node
            && last.cause == cause
        {
            last.class |= class;
            return;
        }
        if self.marks.len() < Self::MAX_MARKS {
            self.marks.push(DirtyMark { node, class, cause });
        } else {
            self.dropped = self.dropped.saturating_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DirtyClass, DirtyLists};

    #[test]
    fn a_worklist_lists_each_slot_once_per_class() {
        let mut lists = DirtyLists::default();
        let mut dirty = vec![DirtyClass::EMPTY; 4];
        for (slot, class) in [
            (2, DirtyClass::PAINT | DirtyClass::LAYOUT),
            (2, DirtyClass::PAINT),
            (0, DirtyClass::PAINT),
            (2, DirtyClass::STYLE),
        ] {
            dirty[slot as usize] |= class;
            lists.note(slot, class);
        }
        assert_eq!(lists.of(DirtyClass::PAINT), [2, 0]);
        assert_eq!(lists.of(DirtyClass::LAYOUT), [2]);
        assert_eq!(lists.of(DirtyClass::STYLE), [2]);
        assert_eq!(lists.marked(), [2, 0]);
        assert!(lists.any(DirtyClass::LAYOUT | DirtyClass::MEASURE, &dirty));
        assert!(!lists.any(DirtyClass::MEASURE, &dirty));

        // A slot reset within the frame is not listed again when re-marked.
        dirty[2] = DirtyClass::EMPTY;
        assert!(!lists.any(DirtyClass::LAYOUT, &dirty));
        dirty[2] |= DirtyClass::PAINT;
        lists.note(2, DirtyClass::PAINT);
        assert_eq!(lists.of(DirtyClass::PAINT), [2, 0]);

        lists.clear(&mut dirty);
        assert!(dirty.iter().all(|d| d.is_empty()));
        assert!(lists.marked().is_empty());
        assert!(lists.of(DirtyClass::PAINT).is_empty());
        lists.note(1, DirtyClass::PAINT);
        assert_eq!(lists.of(DirtyClass::PAINT), [1]);
    }

    #[test]
    fn compose_and_query() {
        let d = DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT;
        assert!(d.contains(DirtyClass::LAYOUT));
        assert!(!d.contains(DirtyClass::TRANSFORM));
        assert!(!d.is_empty());
        assert!(DirtyClass::EMPTY.is_empty());
    }

    #[test]
    fn each_class_renders_its_own_name() {
        for (class, name) in [
            (DirtyClass::STRUCTURE, "STRUCTURE"),
            (DirtyClass::STYLE, "STYLE"),
            (DirtyClass::MEASURE, "MEASURE"),
            (DirtyClass::LAYOUT, "LAYOUT"),
            (DirtyClass::TRANSFORM, "TRANSFORM"),
            (DirtyClass::PAINT, "PAINT"),
            (DirtyClass::HIT_TEST, "HIT_TEST"),
            (DirtyClass::SEMANTICS, "SEMANTICS"),
        ] {
            let names: Vec<_> = class.iter_names().collect();
            assert_eq!(names, [name], "one class renders exactly its own name");
            assert_eq!(class.to_string(), name);
        }
    }

    #[test]
    fn names_are_low_bit_first_and_joined() {
        // Composed out of order; the readout is always low-bit-first.
        let d = DirtyClass::PAINT | DirtyClass::MEASURE | DirtyClass::LAYOUT;
        let names: Vec<_> = d.iter_names().collect();
        assert_eq!(names, ["MEASURE", "LAYOUT", "PAINT"]);
        assert_eq!(d.to_string(), "MEASURE | LAYOUT | PAINT");
    }

    #[test]
    fn empty_has_no_names() {
        assert_eq!(DirtyClass::EMPTY.iter_names().count(), 0);
        assert_eq!(DirtyClass::EMPTY.to_string(), "EMPTY");
    }

    #[test]
    fn names_round_trip_through_the_wire_byte() {
        // The readable surface reflects exactly the bits from_bits keeps, so a
        // reconstructed AOT class set renders the same names it was written with.
        let d = DirtyClass::STRUCTURE | DirtyClass::SEMANTICS;
        let back = DirtyClass::from_bits(d.bits());
        assert_eq!(
            d.iter_names().collect::<Vec<_>>(),
            back.iter_names().collect::<Vec<_>>()
        );
    }
}
