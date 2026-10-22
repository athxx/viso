//! What a text field draws besides its text: the selection behind it, the
//! composition's underline and the caret over it, and how far its text is
//! scrolled inside its box.
//!
//! A field's text is clipped to its box and drawn shifted by its scroll, and
//! its marks with it. The marks are computed by the runtime once the field's
//! text is laid out ([`crate::text_edit::Buffer::marks`]), in the node's
//! space with its text unscrolled; setting different ones repaints only the
//! field. The table is cold and small: one entry for each text field.

use viso_render::{Rect, Rgba};

use crate::component::NodeStore;
use crate::dirty::DirtyClass;
use crate::layout::Vec2;
use crate::node::NodeId;

/// What a text field draws besides its text, in its node's space with the
/// text unscrolled.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldMarks {
    /// How far the text is scrolled: drawn shifted left and up by it.
    pub scroll: Vec2,
    /// The extent of the laid-out text, which bounds the scroll.
    pub content: Vec2,
    /// The selection's highlight, a rectangle for each piece of each line.
    pub selection: Vec<Rect>,
    /// The highlight's color.
    pub selection_color: Rgba,
    /// The composition's underline, a rectangle for each piece.
    pub underlines: Vec<Rect>,
    /// The caret, while it shows.
    pub caret: Option<Rect>,
    /// The caret's and the underline's color.
    pub ink: Rgba,
}

impl Default for FieldMarks {
    fn default() -> Self {
        FieldMarks {
            scroll: Vec2::ZERO,
            content: Vec2::ZERO,
            selection: Vec::new(),
            selection_color: Rgba::TRANSPARENT,
            underlines: Vec::new(),
            caret: None,
            ink: Rgba::TRANSPARENT,
        }
    }
}

impl FieldMarks {
    /// The scroll that keeps the text inside a box of `view`: none past the
    /// text's start, none further than its end shows at the box's end.
    pub fn clamped(&self, scroll: Vec2, view: Vec2) -> Vec2 {
        Vec2 {
            x: scroll.x.clamp(0.0, (self.content.x - view.x).max(0.0)),
            y: scroll.y.clamp(0.0, (self.content.y - view.y).max(0.0)),
        }
    }
}

/// The text fields, by node. Cold and small.
#[derive(Debug, Default)]
pub(crate) struct Fields {
    marks: Vec<(NodeId, FieldMarks)>,
}

impl Fields {
    pub(crate) fn clear(&mut self) {
        self.marks.clear();
    }

    /// Forgets the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        if !self.marks.is_empty() {
            self.marks.retain(|(n, _)| n.index() != index);
        }
    }

    #[inline]
    fn get(&self, id: NodeId) -> Option<&FieldMarks> {
        if self.marks.is_empty() {
            return None;
        }
        self.marks.iter().find(|(n, _)| *n == id).map(|(_, m)| m)
    }

    fn get_mut(&mut self, id: NodeId) -> Option<&mut FieldMarks> {
        self.marks
            .iter_mut()
            .find(|(n, _)| *n == id)
            .map(|(_, m)| m)
    }
}

impl NodeStore {
    /// Makes node `id` a text field: its text is clipped to its box and drawn
    /// with the marks it is given. A field already is one.
    pub fn make_field(&mut self, id: NodeId) {
        if !self.arena().is_live(id) || self.fields.get(id).is_some() {
            return;
        }
        self.fields.marks.push((id, FieldMarks::default()));
        self.mark_dirty(id, DirtyClass::PAINT);
    }

    /// The marks text field `id` draws, `None` for a node that is no field.
    #[inline]
    pub fn field_marks(&self, id: NodeId) -> Option<&FieldMarks> {
        self.fields.get(id)
    }

    /// Gives text field `id` the marks it draws, handing back the ones it
    /// drew in `marks`; different ones repaint it, the same leave `marks`
    /// as it was. A node that is no field takes none.
    pub fn swap_field_marks(&mut self, id: NodeId, marks: &mut FieldMarks) {
        let Some(slot) = self.fields.get_mut(id) else {
            return;
        };
        if *slot == *marks {
            return;
        }
        std::mem::swap(slot, marks);
        self.mark_dirty(id, DirtyClass::PAINT);
    }

    /// Scrolls text field `id` by `delta` within its text, inside its box.
    /// Returns how far it moved on each axis: one that cannot scroll that
    /// way takes nothing, so the scroll reaches what holds it.
    pub fn scroll_field(&mut self, id: NodeId, delta: Vec2) -> Vec2 {
        let view = {
            let b = self.world(id);
            Vec2 { x: b.w, y: b.h }
        };
        let Some(marks) = self.fields.get_mut(id) else {
            return Vec2::ZERO;
        };
        let scroll = marks.clamped(
            Vec2 {
                x: marks.scroll.x + delta.x,
                y: marks.scroll.y + delta.y,
            },
            view,
        );
        let moved = Vec2 {
            x: scroll.x - marks.scroll.x,
            y: scroll.y - marks.scroll.y,
        };
        if moved != Vec2::ZERO {
            marks.scroll = scroll;
            self.mark_dirty(id, DirtyClass::PAINT);
        }
        moved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scroll_stays_within_the_text() {
        let marks = FieldMarks {
            content: Vec2 { x: 300.0, y: 20.0 },
            ..FieldMarks::default()
        };
        let view = Vec2 { x: 100.0, y: 20.0 };
        assert_eq!(
            marks.clamped(Vec2 { x: 250.0, y: 5.0 }, view),
            Vec2 { x: 200.0, y: 0.0 }
        );
        assert_eq!(marks.clamped(Vec2 { x: -4.0, y: 0.0 }, view), Vec2::ZERO);
    }
}
