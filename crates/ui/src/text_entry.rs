//! Text entry for a node that edits text itself.
//!
//! A text field's buffer lives in the [`TextEdits`](crate::text_edit::TextEdits)
//! registry, and the runtime finds what the platform's keyboard and input
//! method need from it: where the caret is, what the field holds, whether it
//! holds lines. A control that keeps its own document instead — a code
//! editor over a rope, a terminal — has no such buffer, so it says the same
//! thing here: while it, or a node inside it, has the focus, the platform
//! shows its keyboard and input method, the candidate window anchors at the
//! caret it declares, and typed and composed text reach its key handler as
//! [`ImeEvent`](crate::input::ImeEvent)s. Pasted text reaches it the same
//! way.
//!
//! The table is cold and small: one entry for each such node, rewritten as
//! its caret moves.

use viso_render::Rect;
use viso_runtime::InputKind;

use crate::component::NodeStore;
use crate::node::NodeId;

/// What a node that takes text itself asks of the platform while the focus
/// is in it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextEntryRequest {
    /// Its caret's line box in the node's own space: where an input method's
    /// candidate window and a soft keyboard's suggestions anchor.
    pub caret: Rect,
    /// What it holds: the keys a soft keyboard shows.
    pub kind: InputKind,
    /// Whether it holds lines: a soft keyboard's return key breaks a line.
    pub multiline: bool,
}

/// The nodes that take text themselves.
#[derive(Debug, Default)]
pub(crate) struct TextEntries {
    list: Vec<(NodeId, TextEntryRequest)>,
}

impl TextEntries {
    pub(crate) fn clear(&mut self) {
        self.list.clear();
    }

    /// Forgets the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        if !self.list.is_empty() {
            self.list.retain(|(n, _)| n.index() != index);
        }
    }
}

impl NodeStore {
    /// Make node `id` take text itself, as `entry` says, or with `None` stop.
    /// Called again as its caret moves; the platform hears of the change at
    /// the frame's end.
    pub fn set_text_entry(&mut self, id: NodeId, entry: Option<TextEntryRequest>) {
        if !self.arena().is_live(id) {
            return;
        }
        let list = &mut self.text_entries.list;
        let at = list.iter().position(|(n, _)| *n == id);
        match (at, entry) {
            (Some(at), Some(entry)) => list[at].1 = entry,
            (None, Some(entry)) => list.push((id, entry)),
            (Some(at), None) => {
                list.swap_remove(at);
            }
            (None, None) => {}
        }
    }

    /// What node `id` asks of text entry, if it takes text itself.
    pub fn text_entry(&self, id: NodeId) -> Option<TextEntryRequest> {
        let list = &self.text_entries.list;
        if list.is_empty() {
            return None;
        }
        list.iter().find(|(n, _)| *n == id).map(|(_, e)| *e)
    }

    /// The nearest node at or above `from` that takes text itself, and what
    /// it asks.
    pub fn text_entry_from(&self, from: NodeId) -> Option<(NodeId, TextEntryRequest)> {
        if self.text_entries.list.is_empty() {
            return None;
        }
        let mut at = Some(from);
        while let Some(id) = at {
            if let Some(entry) = self.text_entry(id) {
                return Some((id, entry));
            }
            at = self.parent(id);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{BuildCx, FlexStyle, LeafStyle};
    use crate::layout::Size;
    use crate::style::BoxStyle;

    fn entry(x: f32) -> TextEntryRequest {
        TextEntryRequest {
            caret: Rect {
                x,
                y: 0.0,
                w: 1.0,
                h: 16.0,
            },
            kind: InputKind::Text,
            multiline: true,
        }
    }

    #[test]
    fn the_nearest_node_taking_text_answers_for_the_focus_inside_it() {
        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        let mut leaf = None;
        let root = cx.flex(FlexStyle::default(), |cx| {
            leaf = Some(cx.leaf(LeafStyle {
                size: Size::fixed(10.0, 10.0),
                style: BoxStyle::NONE,
            }));
        });
        let (root, leaf) = (root.id(), leaf.unwrap().id());
        assert_eq!(store.text_entry_from(leaf), None);
        store.set_text_entry(root, Some(entry(3.0)));
        assert_eq!(store.text_entry_from(leaf), Some((root, entry(3.0))));
        store.set_text_entry(root, Some(entry(7.0)));
        assert_eq!(store.text_entry(root), Some(entry(7.0)));
        store.set_text_entry(root, None);
        assert_eq!(store.text_entry_from(leaf), None);
    }

    #[test]
    fn a_freed_node_takes_no_text() {
        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        let root = cx
            .leaf(LeafStyle {
                size: Size::fixed(10.0, 10.0),
                style: BoxStyle::NONE,
            })
            .id();
        store.set_text_entry(root, Some(entry(1.0)));
        let mut freed = Vec::new();
        store.free_tree(root, &mut crate::reactive::EffectStore::new(), &mut freed);
        let mut cx = BuildCx::new(&mut store);
        let again = cx
            .leaf(LeafStyle {
                size: Size::fixed(10.0, 10.0),
                style: BoxStyle::NONE,
            })
            .id();
        assert_eq!(store.text_entry(again), None);
    }
}
