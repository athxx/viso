//! The 3D scenes a node store's nodes show, by slot.
//!
//! A node whose content is [`Content::Scene`](crate::content::Content::Scene)
//! owns one slot here: the [`SceneFrame`] it shows, the revision of that
//! frame, and whatever state its filler keeps beside it. Whoever draws the
//! scene — a game session filling its viewports after its frame, or app code
//! — writes the frame and [`commit`](SceneSlots::commit)s it; the store then
//! marks the node `PAINT` with the new revision, and the facade hands each
//! committed frame to the window's renderer
//! ([`Renderer::swap_scene`](viso_render::Renderer::swap_scene)), getting the
//! renderer's previous frame back in its place. A commit therefore hands a
//! frame over whole: refill the frame completely before the next commit.
//!
//! A node taking other content, or freed, lets go of its slot; the facade
//! releases the renderer's side of it.
//!
//! A slot's filler draws glTF models with
//! [`NodeStore::scene_model`](crate::component::NodeStore::scene_model): the
//! slot's node holds each model it asks for in the store's asset table, which
//! the facade decodes and uploads into the window's renderer, for as long as
//! the node shows the scene.

use std::any::Any;

pub use viso_render::SceneSlot;
pub use viso_render::scene3d::SceneFrame;

use crate::NodeId;
use crate::node::NodeArena;
use viso_render::asset::ResourceId;

struct Entry {
    node: NodeId,
    frame: SceneFrame,
    revision: u64,
    /// Committed since the renderer last took the frame.
    unsent: bool,
    /// Not filled since the slot was taken or reset.
    fresh: bool,
    state: Option<Box<dyn Any>>,
    /// The models its node holds in the asset table.
    models: Vec<ResourceId>,
}

/// The scenes of one node store, by slot.
#[derive(Default)]
pub struct SceneSlots {
    entries: Vec<Option<Entry>>,
    /// Slots emptied since the facade last synced.
    released: Vec<SceneSlot>,
    /// The nodes whose frames were committed since the store last marked them.
    committed: Vec<(NodeId, u64)>,
}

impl std::fmt::Debug for SceneSlots {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SceneSlots")
            .field("slots", &self.entries.iter().flatten().count())
            .finish_non_exhaustive()
    }
}

/// What a renderer brings itself to in [`SceneSlots::sync`].
pub enum SceneSync<'a> {
    /// The slot was emptied.
    Released(SceneSlot),
    /// The slot's committed frame, to swap for the renderer's.
    Frame(SceneSlot, &'a mut SceneFrame),
}

/// A slot a filler writes: its frame, the state the filler keeps beside it,
/// and whether it was filled since it was taken or reset.
pub struct SceneTarget<'a> {
    pub slot: SceneSlot,
    pub frame: &'a mut SceneFrame,
    pub state: &'a mut Option<Box<dyn Any>>,
    pub fresh: bool,
}

impl SceneSlots {
    /// A new slot for `node`'s scene.
    pub(crate) fn take(&mut self, node: NodeId) -> SceneSlot {
        let entry = Entry {
            node,
            frame: SceneFrame::new(),
            revision: 0,
            unsent: false,
            fresh: true,
            state: None,
            models: Vec::new(),
        };
        let at = match self.entries.iter().position(Option::is_none) {
            Some(at) => at,
            None => {
                self.entries.push(None);
                self.entries.len() - 1
            }
        };
        self.entries[at] = Some(entry);
        SceneSlot(at as u32)
    }

    /// Empties `slot`, returning the models its node held.
    pub(crate) fn leave(&mut self, slot: SceneSlot) -> Vec<ResourceId> {
        match self.entries.get_mut(slot.0 as usize).and_then(Option::take) {
            Some(entry) => {
                self.released.push(slot);
                entry.models
            }
            None => Vec::new(),
        }
    }

    /// The node of `slot`, and whether it held model `id` already, recording
    /// that it does.
    pub(crate) fn hold_model(&mut self, slot: SceneSlot, id: ResourceId) -> Option<(NodeId, bool)> {
        let entry = self.entry_mut(slot)?;
        let held = entry.models.contains(&id);
        if !held {
            entry.models.push(id);
        }
        Some((entry.node, held))
    }

    /// The node of `slot`, if it held model `id`, no longer holding it.
    pub(crate) fn release_model(&mut self, slot: SceneSlot, id: ResourceId) -> Option<NodeId> {
        let entry = self.entry_mut(slot)?;
        let at = entry.models.iter().position(|&m| m == id)?;
        entry.models.swap_remove(at);
        Some(entry.node)
    }

    /// The frame `slot` shows.
    pub fn frame(&self, slot: SceneSlot) -> Option<&SceneFrame> {
        Some(&self.entry(slot)?.frame)
    }

    /// The frame of `slot`, to refill.
    pub fn frame_mut(&mut self, slot: SceneSlot) -> Option<&mut SceneFrame> {
        Some(&mut self.entry_mut(slot)?.frame)
    }

    /// The state the filler of `slot` keeps beside it.
    pub fn state_mut(&mut self, slot: SceneSlot) -> Option<&mut Option<Box<dyn Any>>> {
        Some(&mut self.entry_mut(slot)?.state)
    }

    /// The revision of the frame `slot` shows.
    pub fn revision(&self, slot: SceneSlot) -> u64 {
        self.entry(slot).map_or(0, |e| e.revision)
    }

    /// Marks `slot` as not yet filled, so its filler draws it afresh.
    pub fn reset(&mut self, slot: SceneSlot) {
        if let Some(entry) = self.entry_mut(slot) {
            entry.fresh = true;
        }
    }

    /// Hands the frame of `slot` over as its next revision: its node paints
    /// it and the renderer takes it.
    pub fn commit(&mut self, slot: SceneSlot) {
        let Some(entry) = self.entry_mut(slot) else {
            return;
        };
        entry.revision += 1;
        entry.unsent = true;
        entry.fresh = false;
        let mark = (entry.node, entry.revision);
        self.committed.push(mark);
    }

    /// Calls `fill` with every slot whose node lies under `owner` (or is it),
    /// committing each it reports changed.
    pub(crate) fn fill_under(
        &mut self,
        arena: &NodeArena,
        owner: NodeId,
        fill: &mut dyn FnMut(SceneTarget<'_>) -> bool,
    ) {
        for at in 0..self.entries.len() {
            let Some(entry) = self.entries[at].as_mut() else {
                continue;
            };
            if !within(arena, entry.node, owner) {
                continue;
            }
            let target = SceneTarget {
                slot: SceneSlot(at as u32),
                frame: &mut entry.frame,
                state: &mut entry.state,
                fresh: entry.fresh,
            };
            if fill(target) {
                self.commit(SceneSlot(at as u32));
            }
        }
    }

    /// The commits since the last call: each node and its new revision.
    pub(crate) fn take_committed(&mut self) -> std::vec::Drain<'_, (NodeId, u64)> {
        self.committed.drain(..)
    }

    /// Brings a renderer to the slots: `sync` takes each slot emptied and
    /// each frame committed since the last sync (handing back the renderer's
    /// previous frame in its place).
    pub fn sync(&mut self, mut sync: impl FnMut(SceneSync<'_>)) {
        for slot in self.released.drain(..) {
            sync(SceneSync::Released(slot));
        }
        for (at, entry) in self.entries.iter_mut().enumerate() {
            if let Some(entry) = entry
                && std::mem::take(&mut entry.unsent)
            {
                sync(SceneSync::Frame(SceneSlot(at as u32), &mut entry.frame));
            }
        }
    }

    /// How many slots are taken.
    pub fn len(&self) -> usize {
        self.entries.iter().flatten().count()
    }

    /// Whether no slot is taken.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn entry(&self, slot: SceneSlot) -> Option<&Entry> {
        self.entries.get(slot.0 as usize)?.as_ref()
    }

    fn entry_mut(&mut self, slot: SceneSlot) -> Option<&mut Entry> {
        self.entries.get_mut(slot.0 as usize)?.as_mut()
    }
}

/// Whether `node` is `owner` or lies under it.
fn within(arena: &NodeArena, node: NodeId, owner: NodeId) -> bool {
    let mut at = Some(node);
    while let Some(n) = at {
        if n == owner {
            return true;
        }
        at = arena.parent(n);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(scenes: &mut SceneSlots) -> (Vec<SceneSlot>, Vec<SceneSlot>) {
        let (mut frames, mut released) = (Vec::new(), Vec::new());
        scenes.sync(|s| match s {
            SceneSync::Frame(slot, _) => frames.push(slot),
            SceneSync::Released(slot) => released.push(slot),
        });
        (frames, released)
    }

    #[test]
    fn a_commit_reaches_the_renderer_once_and_a_left_slot_is_released() {
        let mut arena = NodeArena::new();
        let (a, b) = (arena.alloc(), arena.alloc());
        let mut scenes = SceneSlots::default();
        let (sa, sb) = (scenes.take(a), scenes.take(b));
        scenes.commit(sa);
        scenes.commit(sa);
        assert_eq!(scenes.revision(sa), 2);
        assert_eq!(
            scenes.take_committed().collect::<Vec<_>>(),
            [(a, 1), (a, 2)]
        );
        assert_eq!(frames(&mut scenes), (vec![sa], vec![]));
        assert_eq!(frames(&mut scenes), (vec![], vec![]));

        scenes.leave(sb);
        scenes.leave(sb);
        assert_eq!(frames(&mut scenes), (vec![], vec![sb]));
        assert_eq!(scenes.take(b), sb, "a released slot is reused");
        assert_eq!(scenes.revision(sb), 0);
    }

    #[test]
    fn a_fill_reaches_only_the_slots_under_its_owner() {
        let mut arena = NodeArena::new();
        let (root, inside, outside) = (arena.alloc(), arena.alloc(), arena.alloc());
        arena.append_child(root, inside);
        let mut scenes = SceneSlots::default();
        let (si, so) = (scenes.take(inside), scenes.take(outside));
        let mut seen = Vec::new();
        scenes.fill_under(&arena, root, &mut |t| {
            seen.push((t.slot, t.fresh));
            true
        });
        assert_eq!(seen, [(si, true)]);
        assert_eq!((scenes.revision(si), scenes.revision(so)), (1, 0));

        seen.clear();
        scenes.fill_under(&arena, root, &mut |t| {
            seen.push((t.slot, t.fresh));
            false
        });
        assert_eq!(seen, [(si, false)], "filled, so no longer fresh");
        assert_eq!(scenes.revision(si), 1, "unchanged, not committed");
        scenes.reset(si);
        scenes.fill_under(&arena, inside, &mut |t| {
            assert!(t.fresh, "a reset slot draws afresh");
            false
        });
    }
}
