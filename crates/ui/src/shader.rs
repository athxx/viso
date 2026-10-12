//! The brush programs a node store's nodes paint with, by slot.
//!
//! A node whose content is [`Content::Shader`](crate::content::Content::Shader)
//! names its program by a [`ProgramSlot`]; the store keeps each slot's
//! [`BrushProgram`] here. A program is found by identity: setting a node's
//! shader to a program already in a slot reuses the slot, so a list of a
//! thousand rippling rows holds one program. Every change of what a slot
//! holds bumps the [`revision`](ShaderSlots::revision); the facade compares it
//! against the revision it last installed from and, only when it moved,
//! installs the slots that changed into the window's renderer and releases the
//! ones emptied.
//!
//! Slots are kept while the store lives; [`sweep`](ShaderSlots::sweep) empties
//! the ones no live node paints with, which a structural change may leave.

use std::sync::Arc;

use viso_render::{BrushProgram, ProgramSlot};

/// The programs of one node store, by slot.
#[derive(Debug, Default)]
pub struct ShaderSlots {
    slots: Vec<Option<Arc<BrushProgram>>>,
    revision: u64,
}

impl ShaderSlots {
    /// The slot holding `program`, putting it in the first empty slot when no
    /// slot does.
    pub fn slot(&mut self, program: &Arc<BrushProgram>) -> ProgramSlot {
        if let Some(i) = self
            .slots
            .iter()
            .position(|s| s.as_ref().is_some_and(|p| Arc::ptr_eq(p, program)))
        {
            return ProgramSlot(i as u32);
        }
        let i = match self.slots.iter().position(Option::is_none) {
            Some(i) => i,
            None => {
                self.slots.push(None);
                self.slots.len() - 1
            }
        };
        self.slots[i] = Some(Arc::clone(program));
        self.revision += 1;
        ProgramSlot(i as u32)
    }

    /// Puts `program` in `slot` in place of what it held, so every node
    /// painting with the slot paints with `program`: a reloaded shader.
    pub fn replace(&mut self, slot: ProgramSlot, program: Arc<BrushProgram>) {
        let i = slot.0 as usize;
        if self.slots.len() <= i {
            self.slots.resize(i + 1, None);
        }
        self.slots[i] = Some(program);
        self.revision += 1;
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

    /// Empties every slot `used` does not report in use.
    pub fn sweep(&mut self, used: impl Fn(ProgramSlot) -> bool) {
        let mut emptied = false;
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.is_some() && !used(ProgramSlot(i as u32)) {
                *slot = None;
                emptied = true;
            }
        }
        if emptied {
            self.revision += 1;
        }
    }

    /// Forgets every slot.
    pub(crate) fn clear(&mut self) {
        if self.slots.iter().any(Option::is_some) {
            self.revision += 1;
        }
        self.slots.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_render::BrushLayout;

    fn program(name: &str) -> Arc<BrushProgram> {
        Arc::new(BrushProgram {
            name: name.into(),
            layout: BrushLayout {
                stride: 16,
                rect_pos: 0,
                rect_size: 8,
                uniform_size: 16,
                viewport_size: 0,
                params: Vec::new(),
            },
            code: Vec::new(),
        })
    }

    #[test]
    fn a_program_keeps_its_slot_and_a_change_moves_the_revision() {
        let mut slots = ShaderSlots::default();
        let (a, b) = (program("A"), program("B"));
        assert_eq!(slots.slot(&a), ProgramSlot(0));
        assert_eq!(slots.slot(&b), ProgramSlot(1));
        let r = slots.revision();
        assert_eq!(slots.slot(&a), ProgramSlot(0), "found by identity");
        assert_eq!(slots.revision(), r, "nothing changed");
        // Another program of the same name is another program.
        assert_eq!(slots.slot(&program("A")), ProgramSlot(2));

        slots.sweep(|s| s != ProgramSlot(0));
        assert!(slots.get(ProgramSlot(0)).is_none());
        assert!(slots.revision() > r);
        assert_eq!(slots.slot(&b), ProgramSlot(1));
        assert_eq!(
            slots.slot(&program("C")),
            ProgramSlot(0),
            "an emptied slot is reused"
        );

        let r = slots.revision();
        let reloaded = program("B");
        slots.replace(ProgramSlot(1), Arc::clone(&reloaded));
        assert!(Arc::ptr_eq(slots.get(ProgramSlot(1)).unwrap(), &reloaded));
        assert!(slots.revision() > r);
        let occupied: Vec<u32> = slots.iter().map(|(s, _)| s.0).collect();
        assert_eq!(occupied, [0, 1, 2]);
    }
}
