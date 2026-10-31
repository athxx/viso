//! What a text field draws besides its text: the selection behind it, the
//! composition's underline and the caret over it, and how far its text is
//! scrolled inside its box.
//!
//! A field's text is clipped to its box and drawn shifted by its scroll, and
//! its marks with it. The marks are computed by the runtime once the field's
//! text is laid out ([`crate::text_edit::Buffer::marks`]), in the node's
//! space with its text unscrolled; setting different ones repaints only the
//! field. The table is cold and small: one entry for each text field.
//!
//! A number field with the focus takes the wheel to step its number, a step
//! for every [`WHEEL_PX`] of scroll, up for a scroll toward earlier content.
//!
//! A selection a touch made shows a handle at each end, drawn over the field
//! and reaching past its box: on Apple systems a stem along the line with a
//! knob above the start and below the end, elsewhere a teardrop below the
//! line pointing at it. A finger that lands within reach of one drags that
//! end of the selection — the router steers it before any node sees the
//! contact, so no pan or long press takes it — and its lift shows the edit
//! menu. A handle whose end is scrolled out of the field's box hides.

use viso_render::{Corners, Rect, Rgba};
use viso_runtime::primary_is_logo;

use crate::component::NodeStore;
use crate::dirty::DirtyClass;
use crate::input::{PointerId, PointerPhase};
use crate::layout::Vec2;
use crate::node::NodeId;
use crate::text_edit::{EditIntent, SelectionEnd, TouchEdit};

/// How far the wheel scrolls for one step of a focused number field: a line.
pub const WHEEL_PX: f32 = 16.0;

/// A selection handle's color: the theme's primary.
pub(crate) const HANDLE: Rgba = crate::ColorToken::Primary.rgba();

/// The width of an Apple handle's stem and the size of its knob.
const STEM: f32 = 2.0;
const KNOB: f32 = 10.0;
/// The size of a teardrop handle.
const TEAR: f32 = 22.0;
/// How far from a handle's knob a finger still takes it: half of the 44
/// points a touch target needs.
const REACH: f32 = 22.0;

/// A selection handle placed in window space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HandleShape {
    /// The stem along the line, on Apple systems.
    pub(crate) stem: Option<Rect>,
    /// The knob: a circle on Apple systems, elsewhere a teardrop.
    pub(crate) knob: Rect,
    /// The teardrop's corners, sharp where it points at the line.
    pub(crate) tear: Option<Corners>,
    /// Where a finger takes it.
    pub(crate) reach: Rect,
    /// The point on the line it stands for.
    pub(crate) anchor: Vec2,
}

/// The handle at `end` of a selection whose stem there is `stem`, in window
/// space; `None` when the stem lies outside `view`, the field's box.
pub(crate) fn handle_shape(end: SelectionEnd, stem: Rect, view: Rect) -> Option<HandleShape> {
    let (x, top, h) = (stem.x, stem.y, stem.h);
    let mid = top + h * 0.5;
    let inside =
        x >= view.x - 0.5 && x <= view.x + view.w + 0.5 && mid >= view.y && mid <= view.y + view.h;
    if !inside {
        return None;
    }
    let anchor = Vec2 { x, y: mid };
    let around = |c: Vec2| Rect::new(c.x - REACH, c.y - REACH, REACH * 2.0, REACH * 2.0);
    Some(if primary_is_logo() {
        let knob_y = match end {
            SelectionEnd::Start => top - KNOB + 1.0,
            SelectionEnd::End => top + h - 1.0,
        };
        let knob = Rect::new(x - KNOB * 0.5, knob_y, KNOB, KNOB);
        let stem = Rect::new(x - STEM * 0.5, top, STEM, h);
        let center = Vec2 {
            x,
            y: knob_y + KNOB * 0.5,
        };
        HandleShape {
            stem: Some(stem),
            knob,
            tear: None,
            reach: around(center).union(Rect::new(x - REACH * 0.5, top, REACH, h)),
            anchor,
        }
    } else {
        let round = TEAR * 0.5;
        let (left, tear) = match end {
            SelectionEnd::Start => (
                x - TEAR,
                Corners {
                    left_top: round,
                    right_top: 0.0,
                    right_bottom: round,
                    left_bottom: round,
                },
            ),
            SelectionEnd::End => (
                x,
                Corners {
                    left_top: 0.0,
                    right_top: round,
                    right_bottom: round,
                    left_bottom: round,
                },
            ),
        };
        let knob = Rect::new(left, top + h, TEAR, TEAR);
        let center = Vec2 {
            x: left + round,
            y: top + h + round,
        };
        HandleShape {
            stem: None,
            knob,
            tear: Some(tear),
            reach: around(center),
            anchor,
        }
    })
}

/// A finger dragging an end of a field's selection by its handle, held
/// `offset` from the point on the line that end stands for.
#[derive(Debug, Clone, Copy)]
struct Grab {
    pointer: PointerId,
    node: NodeId,
    end: SelectionEnd,
    offset: Vec2,
}

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
    /// The selection's handles while a touch made it: the line-tall stem at
    /// its start, then at its end, each of no width.
    pub handles: Option<[Rect; 2]>,
    /// What the edit menu points at while it shows: the selection's bounds,
    /// or the caret.
    pub menu: Option<Rect>,
    /// Where the context menu pops up while one is asked for: the pointer
    /// that asked, or under the caret; a point, of no size.
    pub context: Option<Rect>,
    /// The finger and the caret a magnifier follows while a finger carries
    /// the caret or drags a handle.
    pub loupe: Option<(Vec2, Rect)>,
    /// Whether it wears a focus ring round its box: a read-only field the
    /// keyboard brought the focus to, which shows no editing box of its own.
    pub ring: bool,
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
            handles: None,
            menu: None,
            context: None,
            loupe: None,
            ring: false,
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
    /// The number fields, with the wheel scroll not yet a whole step.
    steppers: Vec<(NodeId, f32)>,
    /// The handle a finger drags.
    grab: Option<Grab>,
    /// The nodes that stopped being fields, whose buffers reconcile drops.
    dropped: Vec<NodeId>,
}

impl Fields {
    /// The nodes that draw field marks.
    pub(crate) fn nodes(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.marks.iter().map(|(id, _)| *id)
    }

    pub(crate) fn clear(&mut self) {
        self.marks.clear();
        self.steppers.clear();
        self.grab = None;
        self.dropped.clear();
    }

    /// Forgets the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        if !self.marks.is_empty() {
            self.marks.retain(|(n, _)| n.index() != index);
        }
        if !self.steppers.is_empty() {
            self.steppers.retain(|(n, _)| n.index() != index);
        }
        if self.grab.is_some_and(|g| g.node.index() == index) {
            self.grab = None;
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
    /// with the marks it is given, and it counts its clicks. A field already
    /// is one.
    pub fn make_field(&mut self, id: NodeId) {
        if !self.arena().is_live(id) || self.fields.get(id).is_some() {
            return;
        }
        self.fields.marks.push((id, FieldMarks::default()));
        self.count_clicks(id);
        self.set_touch_long_pressable(id, true);
        self.mark_dirty(id, DirtyClass::PAINT);
    }

    /// Makes text field `id` a plain text again: it draws its text as it is
    /// given, counts no clicks, and its edit buffer goes at the next
    /// reconcile. A node that is no field stays as it is.
    pub fn unmake_field(&mut self, id: NodeId) {
        let Some(at) = self.fields.marks.iter().position(|(n, _)| *n == id) else {
            return;
        };
        self.fields.marks.swap_remove(at);
        self.fields.steppers.retain(|(n, _)| *n != id);
        if self.fields.grab.is_some_and(|g| g.node == id) {
            self.fields.grab = None;
        }
        self.fields.dropped.push(id);
        self.uncount_clicks(id);
        self.set_touch_long_pressable(id, false);
        self.set_number_field(id, false);
        self.mark_dirty(id, DirtyClass::PAINT);
    }

    /// Move the nodes that stopped being fields since the last call into
    /// `out` (cleared first).
    pub(crate) fn take_dropped_fields(&mut self, out: &mut Vec<NodeId>) {
        out.clear();
        out.append(&mut self.fields.dropped);
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

    /// Makes text field `id` a number field, which a drag scrubs and the
    /// wheel steps while it has the focus, or a plain one.
    pub fn set_number_field(&mut self, id: NodeId, number: bool) {
        let at = self.fields.steppers.iter().position(|(n, _)| *n == id);
        match (at, number) {
            (None, true) if self.arena().is_live(id) => {
                self.fields.steppers.push((id, 0.0));
                self.set_draggable(id, true);
            }
            (Some(at), false) => {
                self.fields.steppers.swap_remove(at);
                self.set_draggable(id, false);
            }
            _ => {}
        }
    }

    /// Whether node `id` is a number field.
    pub fn is_number_field(&self, id: NodeId) -> bool {
        self.fields.steppers.iter().any(|(n, _)| *n == id)
    }

    /// Steps the focused number field at or above `target` by the wheel's
    /// `delta_y`, reporting whether there is one to take it.
    pub(crate) fn wheel_number_field(&mut self, target: NodeId, delta_y: f32) -> bool {
        if self.fields.steppers.is_empty() {
            return false;
        }
        let Some(focused) = self.focused() else {
            return false;
        };
        let mut node = Some(target);
        while let Some(n) = node {
            if n == focused {
                break;
            }
            node = self.parent(n);
        }
        let Some((_, left)) = self
            .fields
            .steppers
            .iter_mut()
            .find(|(n, _)| Some(*n) == node)
        else {
            return false;
        };
        *left -= delta_y;
        let steps = (*left / WHEEL_PX).trunc();
        *left -= steps * WHEEL_PX;
        if steps != 0.0 {
            self.queue_edit(focused, EditIntent::Step(f64::from(steps)));
        }
        true
    }

    /// The handles field `id` shows, in window space.
    pub(crate) fn field_handles(&self, id: NodeId) -> [Option<HandleShape>; 2] {
        let Some(marks) = self.fields.get(id) else {
            return [None, None];
        };
        let Some([start, end]) = marks.handles else {
            return [None, None];
        };
        let view = self.content_box(id);
        let place = |r: Rect| Rect {
            x: view.x - marks.scroll.x + r.x,
            y: view.y - marks.scroll.y + r.y,
            ..r
        };
        [
            handle_shape(SelectionEnd::Start, place(start), view),
            handle_shape(SelectionEnd::End, place(end), view),
        ]
    }

    /// Steers a direct contact `pointer` at `at` that drags a selection
    /// handle: a press within reach of one takes it, a move drags its end of
    /// the selection, a lift lets it go. Returns whether the contact holds a
    /// handle, which no node then sees.
    pub(crate) fn steer_field_handle(
        &mut self,
        pointer: PointerId,
        phase: PointerPhase,
        at: [f32; 2],
    ) -> bool {
        let at = Vec2 { x: at[0], y: at[1] };
        match phase {
            PointerPhase::Down => {
                if self.fields.grab.is_some() {
                    return false;
                }
                let mut nearest: Option<(f32, Grab)> = None;
                for i in 0..self.fields.marks.len() {
                    let (node, marks) = &self.fields.marks[i];
                    if marks.handles.is_none() {
                        continue;
                    }
                    let node = *node;
                    let handles = self.field_handles(node);
                    let ends = [SelectionEnd::Start, SelectionEnd::End];
                    for (shape, end) in handles.into_iter().zip(ends) {
                        let Some(shape) = shape.filter(|h| h.reach.contains(at.x, at.y)) else {
                            continue;
                        };
                        let c = Vec2 {
                            x: shape.knob.x + shape.knob.w * 0.5,
                            y: shape.knob.y + shape.knob.h * 0.5,
                        };
                        let d = (at.x - c.x).hypot(at.y - c.y);
                        if nearest.is_none_or(|(best, _)| d < best) {
                            let offset = Vec2 {
                                x: at.x - shape.anchor.x,
                                y: at.y - shape.anchor.y,
                            };
                            nearest = Some((
                                d,
                                Grab {
                                    pointer,
                                    node,
                                    end,
                                    offset,
                                },
                            ));
                        }
                    }
                }
                let Some((_, grab)) = nearest else {
                    return false;
                };
                self.fields.grab = Some(grab);
                true
            }
            _ => {
                let Some(grab) = self.fields.grab.filter(|g| g.pointer == pointer) else {
                    return false;
                };
                let edit = match phase {
                    PointerPhase::Move => TouchEdit::Handle {
                        end: grab.end,
                        x: at.x - grab.offset.x,
                        y: at.y - grab.offset.y,
                    },
                    PointerPhase::Up | PointerPhase::Leave => {
                        self.fields.grab = None;
                        TouchEdit::Lift
                    }
                    _ => return true,
                };
                if self.arena().is_live(grab.node) {
                    self.queue_edit(grab.node, EditIntent::Touch(edit));
                }
                true
            }
        }
    }

    /// Scrolls text field `id` by `delta` within its text, inside its box.
    /// Returns how far it moved on each axis: one that cannot scroll that
    /// way takes nothing, so the scroll reaches what holds it.
    pub fn scroll_field(&mut self, id: NodeId, delta: Vec2) -> Vec2 {
        let view = {
            let b = self.content_box(id);
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
