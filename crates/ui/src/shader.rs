//! The brush programs a node store's nodes paint with, by slot.
//!
//! A node whose content is [`Content::Shader`](crate::content::Content::Shader)
//! names its program by a [`ProgramSlot`]; the store keeps each slot's
//! [`BrushProgram`] here. A program is found by identity: setting a node's
//! shader to a program already in a slot reuses the slot, so a list of a
//! thousand rippling rows holds one program, and a reloaded shader is another
//! program its nodes move to. Every change of what a slot
//! holds bumps the [`revision`](ShaderSlots::revision); the facade compares it
//! against the revision it last installed from and, only when it moved,
//! installs the slots that changed into the window's renderer and releases the
//! ones emptied.
//!
//! A slot counts the nodes painting with it: a node taking other content, or
//! freed, lets go of its slot, and the last one to let go empties it, so a
//! program no node paints with leaves the renderer without a walk of the tree.

use std::sync::Arc;

pub use viso_render::{
    __per_backend, BrushLayout, BrushProgram, PackedBrush, ParamKind, ParamValue, ProgramSlot,
    ShaderLang,
};

/// The programs of one node store, by slot.
#[derive(Debug, Default)]
pub struct ShaderSlots {
    slots: Vec<Option<Arc<BrushProgram>>>,
    /// How many nodes paint with each slot.
    users: Vec<u32>,
    revision: u64,
}

impl ShaderSlots {
    /// The slot holding `program`, counting one more node painting with it;
    /// the program takes the first empty slot when no slot holds it.
    pub(crate) fn take(&mut self, program: &Arc<BrushProgram>) -> ProgramSlot {
        let at = match self
            .slots
            .iter()
            .position(|s| s.as_ref().is_some_and(|p| Arc::ptr_eq(p, program)))
        {
            Some(at) => at,
            None => {
                let at = match self.slots.iter().position(Option::is_none) {
                    Some(at) => at,
                    None => {
                        self.slots.push(None);
                        self.users.push(0);
                        self.slots.len() - 1
                    }
                };
                self.slots[at] = Some(Arc::clone(program));
                self.revision += 1;
                at
            }
        };
        self.users[at] += 1;
        ProgramSlot(at as u32)
    }

    /// Counts one node fewer painting with `slot`, emptying it with the last.
    pub(crate) fn leave(&mut self, slot: ProgramSlot) {
        let at = slot.0 as usize;
        let Some(users) = self.users.get_mut(at) else {
            return;
        };
        *users = users.saturating_sub(1);
        if *users == 0 && self.slots[at].take().is_some() {
            self.revision += 1;
        }
    }

    /// The program in `slot`.
    pub fn get(&self, slot: ProgramSlot) -> Option<&Arc<BrushProgram>> {
        self.slots.get(slot.0 as usize)?.as_ref()
    }

    /// Every occupied slot and its program.
    pub fn iter(&self) -> impl Iterator<Item = (ProgramSlot, &Arc<BrushProgram>)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| Some((ProgramSlot(i as u32), s.as_ref()?)))
    }

    /// The slots there are room for, occupied or not.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether no slot was ever taken.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Moves with every change of what a slot holds.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Forgets every slot.
    pub(crate) fn clear(&mut self) {
        if self.slots.iter().any(Option::is_some) {
            self.revision += 1;
        }
        self.slots.clear();
        self.users.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program(name: &str) -> Arc<BrushProgram> {
        Arc::new(BrushProgram {
            name: name.into(),
            layout: BrushLayout {
                stride: 16,
                rect_pos: 0,
                rect_size: 8,
                uniform_size: 16,
                viewport_size: 0,
                time: None,
                textures: Vec::new(),
                samplers: Vec::new(),
                vertex_textures: false,
                params: Vec::new(),
            },
            code: Vec::new(),
        })
    }

    #[test]
    fn a_program_keeps_its_slot_while_a_node_paints_with_it() {
        let mut slots = ShaderSlots::default();
        let (a, b) = (program("A"), program("B"));
        assert_eq!(slots.take(&a), ProgramSlot(0));
        assert_eq!(slots.take(&b), ProgramSlot(1));
        let r = slots.revision();
        assert_eq!(slots.take(&a), ProgramSlot(0), "found by identity");
        assert_eq!(slots.revision(), r, "nothing changed");
        // Another program of the same name is another program.
        assert_eq!(slots.take(&program("A")), ProgramSlot(2));

        // Two nodes paint with `a`: the first to leave keeps it.
        slots.leave(ProgramSlot(0));
        assert!(slots.get(ProgramSlot(0)).is_some());
        assert_eq!(slots.revision(), r + 1);
        slots.leave(ProgramSlot(0));
        assert!(slots.get(ProgramSlot(0)).is_none());
        assert_eq!(slots.revision(), r + 2);
        assert_eq!(
            slots.take(&program("C")),
            ProgramSlot(0),
            "an emptied slot is reused"
        );

        let occupied: Vec<u32> = slots.iter().map(|(s, _)| s.0).collect();
        assert_eq!(occupied, [0, 1, 2]);

        // Leaving a slot no node paints with, or past the last, changes
        // nothing.
        slots.leave(ProgramSlot(2));
        let r = slots.revision();
        slots.leave(ProgramSlot(2));
        slots.leave(ProgramSlot(9));
        assert!(slots.get(ProgramSlot(2)).is_none());
        assert_eq!(slots.revision(), r);
    }
}
