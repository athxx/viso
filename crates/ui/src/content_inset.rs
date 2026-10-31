//! A leaf's padding: how far its content — a button's caption, a field's
//! text — keeps from the edges of its box. Its box measures to the content
//! and the padding round it, the content shapes to the box's width less the
//! padding, and paint, caret placement and link hits all read the content
//! box. The table is cold: one entry for each padded leaf, kept in slot order
//! so a lookup is a binary search and a tree with none checks an empty table.

use crate::component::NodeStore;
use crate::layout::Inset;
use crate::node::NodeId;
use viso_render::Rect;

/// The padded leaves, in slot order.
#[derive(Debug, Default)]
pub(crate) struct ContentInsets {
    entries: Vec<(u32, Inset)>,
}

impl ContentInsets {
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    /// Forgets the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        if let Ok(at) = self.find(index) {
            self.entries.remove(at);
        }
    }

    fn find(&self, index: u32) -> Result<usize, usize> {
        self.entries.binary_search_by_key(&index, |e| e.0)
    }

    #[inline]
    pub(crate) fn get(&self, index: u32) -> Inset {
        if self.entries.is_empty() {
            return Inset::default();
        }
        self.find(index)
            .map_or(Inset::default(), |at| self.entries[at].1)
    }

    /// Pads slot `index` by `inset`; whether that moved it.
    pub(crate) fn set(&mut self, index: u32, inset: Inset) -> bool {
        let none = inset == Inset::default();
        match (self.find(index), none) {
            (Ok(at), _) if self.entries[at].1 == inset => false,
            (Ok(at), true) => {
                self.entries.remove(at);
                true
            }
            (Ok(at), false) => {
                self.entries[at].1 = inset;
                true
            }
            (Err(_), true) => false,
            (Err(at), false) => {
                self.entries.insert(at, (index, inset));
                true
            }
        }
    }
}

impl NodeStore {
    /// How far leaf `id`'s content keeps from the edges of its box.
    pub fn content_inset(&self, id: NodeId) -> Inset {
        self.content_insets.get(id.index())
    }

    /// Where leaf `id`'s content lies on screen: its box less its padding.
    pub fn content_box(&self, id: NodeId) -> Rect {
        inset_rect(self.world(id), self.content_insets.get(id.index()))
    }
}

/// `rect` less `inset`, never inside out.
pub(crate) fn inset_rect(rect: Rect, inset: Inset) -> Rect {
    if inset == Inset::default() {
        return rect;
    }
    Rect::new(
        rect.x + inset.left,
        rect.y + inset.top,
        (rect.w - inset.left - inset.right).max(0.0),
        (rect.h - inset.top - inset.bottom).max(0.0),
    )
}
