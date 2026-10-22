//! Pointer cursors: what the mouse shows over each node.
//!
//! A node may name the cursor the mouse shows over it and its subtree; the
//! nearest one at or above the node under the mouse wins. While a node holds
//! the mouse captured — a drag, a selection under way — the cursor stays the
//! one it showed as the capture began, wherever the mouse goes. A text field names the I-beam and a
//! number field without the focus the horizontal resize its scrub moves
//! along, unless they name another; a link shows the hand; a disabled node shows the default. The
//! runtime asks for the cursor once a frame and hands the platform a new one
//! only when it changes. The table is cold and small: one entry for each node
//! that names a cursor.

pub use viso_runtime::CursorIcon;

use crate::component::NodeStore;
use crate::input::PointerId;
use crate::node::NodeId;

/// The cursors nodes name. Cold.
#[derive(Debug, Default)]
pub(crate) struct Cursors {
    named: Vec<(NodeId, CursorIcon)>,
    /// The cursor the mouse showed as a node captured it, while one holds it.
    held: Option<CursorIcon>,
}

impl Cursors {
    pub(crate) fn clear(&mut self) {
        self.named.clear();
        self.held = None;
    }

    /// Forgets the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        if !self.named.is_empty() {
            self.named.retain(|(n, _)| n.index() != index);
        }
    }
}

impl NodeStore {
    /// Makes the mouse show `cursor` over node `id` and its subtree, or with
    /// `None` what the node would show of itself.
    pub fn set_cursor(&mut self, id: NodeId, cursor: Option<CursorIcon>) {
        let named = &mut self.cursors.named;
        let at = named.iter().position(|(n, _)| *n == id);
        match (at, cursor) {
            (Some(at), Some(cursor)) => named[at].1 = cursor,
            (Some(at), None) => {
                named.swap_remove(at);
            }
            (None, Some(cursor)) if self.arena().is_live(id) => {
                self.cursors.named.push((id, cursor));
            }
            _ => {}
        }
    }

    /// The cursor node `id` names, if any.
    pub fn cursor(&self, id: NodeId) -> Option<CursorIcon> {
        if self.cursors.named.is_empty() {
            return None;
        }
        self.cursors
            .named
            .iter()
            .find(|(n, _)| *n == id)
            .map(|&(_, c)| c)
    }

    /// The cursor the mouse shows: the one it showed as a node captured it,
    /// while one holds it, else that of the node under it.
    pub fn mouse_cursor(&self) -> CursorIcon {
        match self.cursors.held {
            Some(held) if self.capture_of(PointerId::MOUSE).is_some() => held,
            _ => self.cursor_over(self.hovered()),
        }
    }

    /// The mouse is about to be captured by a node, or (`captured` false) let
    /// go; a capture passed from one node to another keeps its cursor.
    pub(crate) fn hold_cursor(&mut self, captured: bool) {
        let held = self
            .cursors
            .held
            .filter(|_| self.capture_of(PointerId::MOUSE).is_some());
        self.cursors.held = captured.then(|| held.unwrap_or(self.cursor_over(self.hovered())));
    }

    /// The cursor the mouse shows over `node`.
    fn cursor_over(&self, node: Option<NodeId>) -> CursorIcon {
        let Some(start) = node.filter(|&n| self.arena().is_live(n)) else {
            return CursorIcon::Default;
        };
        if self.inert(start) {
            return CursorIcon::Default;
        }
        if self.over_link(start) {
            return CursorIcon::Pointer;
        }
        let mut node = Some(start);
        while let Some(n) = node {
            if let Some(cursor) = self.cursor(n) {
                return cursor;
            }
            if self.field_marks(n).is_some() {
                return if self.is_number_field(n) && self.focused() != Some(n) {
                    CursorIcon::ResizeEw
                } else {
                    CursorIcon::Text
                };
            }
            node = self.parent(n);
        }
        CursorIcon::Default
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::BuildCx;
    use crate::{FlexStyle, LeafStyle, Size};

    #[test]
    fn the_nearest_named_cursor_wins_and_fields_show_the_beam() {
        let mut store = NodeStore::new();
        let (mut leaf, mut field) = (None, None);
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    size: Size::fixed(100.0, 100.0),
                    ..Default::default()
                },
                |cx| {
                    leaf = Some(cx.leaf(LeafStyle::default()).id());
                    field = Some(cx.leaf(LeafStyle::default()).id());
                },
            );
            cx.root().unwrap()
        };
        let (leaf, field) = (leaf.unwrap(), field.unwrap());
        store.make_field(field);
        assert_eq!(store.mouse_cursor(), CursorIcon::Default, "nothing hovered");
        store.set_hovered(Some(leaf));
        assert_eq!(store.mouse_cursor(), CursorIcon::Default);
        store.set_cursor(root, Some(CursorIcon::Pointer));
        assert_eq!(store.mouse_cursor(), CursorIcon::Pointer, "from above");
        store.set_cursor(leaf, Some(CursorIcon::Grab));
        assert_eq!(store.mouse_cursor(), CursorIcon::Grab, "the nearest");
        store.set_hovered(Some(field));
        assert_eq!(store.mouse_cursor(), CursorIcon::Text);
        store.set_number_field(field, true);
        assert_eq!(store.mouse_cursor(), CursorIcon::ResizeEw, "a scrub");
        store.set_capture(PointerId::MOUSE, Some(root));
        store.set_hovered(Some(leaf));
        assert_eq!(
            store.mouse_cursor(),
            CursorIcon::ResizeEw,
            "the one it showed as the capture began"
        );
        store.set_capture(PointerId::MOUSE, None);
        assert_eq!(store.mouse_cursor(), CursorIcon::Grab);
        store.set_hovered(Some(field));
        store.set_enabled(field, false);
        assert_eq!(store.mouse_cursor(), CursorIcon::Default, "disabled");
        store.set_cursor(leaf, None);
        assert_eq!(store.cursor(leaf), None);
    }
}
