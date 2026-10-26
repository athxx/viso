//! Pointer lock: one node takes the mouse for relative motion, as a game's
//! view or a 3D viewport's orbit does.
//!
//! A node asks for the lock ([`NodeStore::lock_pointer`], or
//! `EventCx::lock_pointer` from a handler). Once the platform engages it the
//! cursor hides and stays put, and the mouse's motion reaches the node's key
//! handler as a delta in logical points (`EventCx::pointer_motion`) instead
//! of moving the pointer; the mouse's buttons and wheel go to the node too,
//! wherever it was. The lock ends when the node releases it, when the node is
//! freed, when the window loses focus, when the user presses Escape, or when
//! the platform takes it back; the node hears each change through its key
//! handler (`EventCx::pointer_lock_change`). A platform without pointer lock
//! (`PlatformCaps::pointer_lock`) answers each ask with the lock ended.

use crate::component::NodeStore;
use crate::node::NodeId;

/// The node that holds or asked for the lock, and the platform's own state.
/// Cold.
#[derive(Debug, Default)]
pub(crate) struct PointerLock {
    /// The node that asked for the lock, while it wants it.
    owner: Option<NodeId>,
    /// Whether the platform holds the lock, as it last told.
    locked: bool,
    /// What the platform is to be asked, not yet taken.
    ask: Option<bool>,
}

impl PointerLock {
    /// Forgets the owner, asking the platform to let go of a lock it holds.
    pub(crate) fn clear(&mut self) {
        self.owner = None;
        self.ask = self.locked.then_some(false);
    }
}

impl NodeStore {
    /// Asks for the mouse's pointer to lock to node `id` (`lock` true), or
    /// lets go of the lock it holds or asked for. A node asking while another
    /// holds the lock takes it over. A no-op for a stale handle, or a release
    /// by a node that holds no lock.
    pub fn lock_pointer(&mut self, id: NodeId, lock: bool) {
        if lock {
            if !self.arena().is_live(id) {
                return;
            }
            let lock_state = &mut self.pointer_lock;
            lock_state.owner = Some(id);
            lock_state.ask = (!lock_state.locked).then_some(true);
        } else if self.pointer_lock.owner == Some(id) {
            let lock_state = &mut self.pointer_lock;
            lock_state.owner = None;
            lock_state.ask = lock_state.locked.then_some(false);
        }
    }

    /// The node the mouse's pointer is locked to, while the platform holds
    /// the lock for it.
    pub fn pointer_lock(&self) -> Option<NodeId> {
        let lock = &self.pointer_lock;
        lock.owner
            .filter(|&n| lock.locked && self.arena().is_live(n))
    }

    /// What the platform is to be asked since this was last called: to lock
    /// the pointer (`true`) or to let it go. An owner freed since lets go of
    /// the lock it held.
    pub fn take_pointer_lock_ask(&mut self) -> Option<bool> {
        let stale = self
            .pointer_lock
            .owner
            .is_some_and(|n| !self.arena().is_live(n));
        let lock = &mut self.pointer_lock;
        if stale {
            lock.owner = None;
            lock.ask = lock.locked.then_some(false);
        }
        lock.ask.take()
    }

    /// Ends the lock for its owner, as Escape or the window losing focus
    /// does: the platform is asked to let go, and the owner is returned to be
    /// told.
    pub fn end_pointer_lock(&mut self) -> Option<NodeId> {
        let owner = self.pointer_lock.owner.filter(|&n| self.arena().is_live(n));
        let lock = &mut self.pointer_lock;
        lock.owner = None;
        lock.ask = lock.locked.then_some(false);
        owner
    }

    /// The platform locked the pointer, or let it go. Returns the node to
    /// tell, with what to tell it: the owner, of the lock engaged or ended.
    /// A lock engaged that no node wants any more is asked back.
    pub fn pointer_lock_changed(&mut self, locked: bool) -> Option<(NodeId, bool)> {
        let owner = self.pointer_lock.owner.filter(|&n| self.arena().is_live(n));
        let lock = &mut self.pointer_lock;
        let was = lock.locked;
        lock.locked = locked;
        if locked {
            if owner.is_none() {
                lock.owner = None;
                lock.ask = Some(false);
                return None;
            }
            lock.ask = None;
            return owner.filter(|_| !was).map(|n| (n, true));
        }
        // The lock ended, by the app's asking or not: the owner wants it no
        // more, nor is the platform to be asked again.
        lock.owner = None;
        if lock.ask == Some(false) {
            lock.ask = None;
        }
        owner.map(|n| (n, false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FlexStyle;
    use crate::component::{BuildCx, LeafStyle};
    use crate::layout::Sizing;
    use crate::style::BoxStyle;

    fn two_leaves() -> (NodeStore, NodeId, NodeId) {
        let mut store = NodeStore::new();
        let mut leaves = Vec::new();
        {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                for _ in 0..2 {
                    leaves.push(
                        cx.leaf(LeafStyle {
                            size: Sizing::fixed(10.0, 10.0),
                            style: BoxStyle::NONE,
                        })
                        .id(),
                    );
                }
            });
        }
        (store, leaves[0], leaves[1])
    }

    #[test]
    fn a_lock_is_asked_engaged_and_released() {
        let (mut store, a, _) = two_leaves();
        store.lock_pointer(a, true);
        assert_eq!(store.pointer_lock(), None, "not yet engaged");
        assert_eq!(store.take_pointer_lock_ask(), Some(true));
        assert_eq!(store.take_pointer_lock_ask(), None);
        assert_eq!(store.pointer_lock_changed(true), Some((a, true)));
        assert_eq!(store.pointer_lock(), Some(a));

        store.lock_pointer(a, false);
        assert_eq!(store.take_pointer_lock_ask(), Some(false));
        assert_eq!(store.pointer_lock(), None);
        assert_eq!(store.pointer_lock_changed(false), None, "it let go itself");
    }

    #[test]
    fn the_platform_ending_the_lock_tells_the_owner() {
        let (mut store, a, b) = two_leaves();
        store.lock_pointer(a, true);
        store.take_pointer_lock_ask();
        store.pointer_lock_changed(true);
        // Another node takes the lock over without asking the platform again.
        store.lock_pointer(b, true);
        assert_eq!(store.take_pointer_lock_ask(), None);
        assert_eq!(store.pointer_lock(), Some(b));
        // Focus lost, Escape, or a refusal: the owner hears it ended.
        assert_eq!(store.pointer_lock_changed(false), Some((b, false)));
        assert_eq!(store.pointer_lock(), None);
        assert_eq!(store.take_pointer_lock_ask(), None);
        // A release by a node that holds nothing does nothing.
        store.lock_pointer(a, false);
        assert_eq!(store.take_pointer_lock_ask(), None);
    }

    #[test]
    fn escape_ends_the_lock_and_tells_the_owner_once() {
        let (mut store, a, _) = two_leaves();
        store.lock_pointer(a, true);
        store.take_pointer_lock_ask();
        store.pointer_lock_changed(true);
        assert_eq!(store.end_pointer_lock(), Some(a));
        assert_eq!(store.pointer_lock(), None);
        assert_eq!(store.take_pointer_lock_ask(), Some(false));
        assert_eq!(store.pointer_lock_changed(false), None, "told already");
        assert_eq!(store.end_pointer_lock(), None);
        assert_eq!(store.take_pointer_lock_ask(), None);
    }

    #[test]
    fn a_lock_no_node_wants_is_asked_back() {
        let (mut store, a, _) = two_leaves();
        store.lock_pointer(a, true);
        store.take_pointer_lock_ask();
        store.lock_pointer(a, false);
        assert_eq!(store.take_pointer_lock_ask(), None, "never engaged");
        assert_eq!(store.pointer_lock_changed(true), None);
        assert_eq!(store.take_pointer_lock_ask(), Some(false));

        // An owner freed while it holds the lock lets go of it.
        let (mut store, a, _) = two_leaves();
        store.lock_pointer(a, true);
        store.take_pointer_lock_ask();
        store.pointer_lock_changed(true);
        let (mut effects, mut scratch) = (crate::reactive::EffectStore::default(), Vec::new());
        store.free_tree(a, &mut effects, &mut scratch);
        assert_eq!(store.pointer_lock(), None);
        assert_eq!(store.take_pointer_lock_ask(), Some(false));
    }
}
