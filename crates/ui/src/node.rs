//! Node identity and the retained tree arena.

/// Compact generational node identity.
///
/// Using an integer id + generation instead of a heap pointer buys us: fewer
/// per-node allocations, no pointer chasing, no runtime borrow panics, dense
/// dirty bitsets, and — via `generation` — detectable stale handles that the
/// inspector and DSL runtime can safely hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId {
    index: u32,
    generation: u32,
}

impl NodeId {
    #[inline]
    pub fn index(self) -> u32 {
        self.index
    }
    #[inline]
    pub fn generation(self) -> u32 {
        self.generation
    }
}

/// Explicit UI ancestry. Layout, focus, clipping, semantics, and event
/// propagation all depend on this — the tree is a tree, not a generic ECS.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NodeLinks {
    pub parent: Option<NodeId>,
    pub first_child: Option<NodeId>,
    pub last_child: Option<NodeId>,
    pub prev_sibling: Option<NodeId>,
    pub next_sibling: Option<NodeId>,
}

/// A slot in the arena. Real optimized storage will split
/// generation/occupied/links/flags into separate arrays; this shape fixes the
/// contract.
struct NodeSlot {
    generation: u32,
    occupied: bool,
    links: NodeLinks,
}

/// Generational arena backing the retained UI tree.
///
/// Phase 0 implements only allocate / free / id-validation so the identity
/// contract can be tested. Hot side-storage arrays (bounds, transform, dirty
/// flags, …) are layered on in later phases.
#[derive(Default)]
pub struct NodeArena {
    slots: Vec<NodeSlot>,
    free: Vec<u32>,
}

impl NodeArena {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate a fresh node, reusing a free slot when possible.
    pub fn alloc(&mut self) -> NodeId {
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            debug_assert!(!slot.occupied);
            slot.occupied = true;
            slot.links = NodeLinks::default();
            NodeId {
                index,
                generation: slot.generation,
            }
        } else {
            let index = self.slots.len() as u32;
            self.slots.push(NodeSlot {
                generation: 0,
                occupied: true,
                links: NodeLinks::default(),
            });
            NodeId {
                index,
                generation: 0,
            }
        }
    }

    /// Free a node. Bumps the slot generation so any surviving [`NodeId`]
    /// becomes detectably stale.
    pub fn free(&mut self, id: NodeId) -> bool {
        match self.slots.get_mut(id.index as usize) {
            Some(slot) if slot.occupied && slot.generation == id.generation => {
                slot.occupied = false;
                slot.generation = slot.generation.wrapping_add(1);
                self.free.push(id.index);
                true
            }
            _ => false,
        }
    }

    /// The live [`NodeId`] for a slot index, pairing the index with the slot's
    /// current generation. `None` if the slot is empty or out of range. Lets a
    /// caller working in bare indices (dense side-storage) recover a full id.
    #[inline]
    pub fn live_id(&self, index: u32) -> Option<NodeId> {
        let slot = self.slots.get(index as usize)?;
        slot.occupied.then_some(NodeId {
            index,
            generation: slot.generation,
        })
    }

    /// Whether an id refers to a currently-live node (generation + occupancy).
    #[inline]
    pub fn is_live(&self, id: NodeId) -> bool {
        matches!(
            self.slots.get(id.index as usize),
            Some(slot) if slot.occupied && slot.generation == id.generation
        )
    }

    /// Ancestry links for a live node.
    pub fn links(&self, id: NodeId) -> Option<&NodeLinks> {
        let slot = self.slots.get(id.index as usize)?;
        (slot.occupied && slot.generation == id.generation).then_some(&slot.links)
    }

    /// Mutable ancestry links for a live node, so the tree builder can attach
    /// parent/child/sibling relationships after allocation.
    pub fn links_mut(&mut self, id: NodeId) -> Option<&mut NodeLinks> {
        let slot = self.slots.get_mut(id.index as usize)?;
        (slot.occupied && slot.generation == id.generation).then_some(&mut slot.links)
    }

    /// Append `child` as the last child of `parent`, wiring the sibling chain.
    /// Both must be live; a no-op returning `false` otherwise.
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) -> bool {
        if !self.is_live(parent) || !self.is_live(child) {
            return false;
        }
        let prev_last = self.links(parent).and_then(|l| l.last_child);
        if let Some(prev) = prev_last
            && let Some(prev_links) = self.links_mut(prev)
        {
            prev_links.next_sibling = Some(child);
        }
        if let Some(child_links) = self.links_mut(child) {
            child_links.parent = Some(parent);
            child_links.prev_sibling = prev_last;
            child_links.next_sibling = None;
        }
        if let Some(parent_links) = self.links_mut(parent) {
            if parent_links.first_child.is_none() {
                parent_links.first_child = Some(child);
            }
            parent_links.last_child = Some(child);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_produces_live_ids() {
        let mut arena = NodeArena::new();
        let a = arena.alloc();
        let b = arena.alloc();
        assert!(arena.is_live(a));
        assert!(arena.is_live(b));
        assert_ne!(a, b);
    }

    #[test]
    fn freed_id_becomes_stale() {
        let mut arena = NodeArena::new();
        let a = arena.alloc();
        assert!(arena.free(a));
        assert!(!arena.is_live(a), "stale handle must be detectable");
        // Double free is rejected.
        assert!(!arena.free(a));
    }

    #[test]
    fn reused_slot_bumps_generation() {
        let mut arena = NodeArena::new();
        let a = arena.alloc();
        arena.free(a);
        let b = arena.alloc();
        // Same index reused, but generation differs — the old id is stale.
        assert_eq!(a.index(), b.index());
        assert_ne!(a.generation(), b.generation());
        assert!(!arena.is_live(a));
        assert!(arena.is_live(b));
    }
}
