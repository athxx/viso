//! Keyboard scrolling: the arrow keys, Page Up and Page Down, Space, Home and
//! End scroll the viewport the keyboard is in when nothing takes them; a
//! viewer the keyboard is in pans and zooms first ([`viewer`](crate::viewer)).
//!
//! A key scrolls only when no handler on the focused node's route stopped it
//! and the focus is not in a field that edits text: what a control does with
//! a key comes first, as a slider's arrows or a button's Space. The viewport is
//! the nearest one about the focused node that scrolls the key's way, else the
//! nearest about the node under the pointer, else the window's outermost. An
//! arrow scrolls a line, a page key or Space a page less a sliver of overlap
//! kept in view, Home and End to either end; it glides there, and a key held
//! or pressed again before the glide arrives heads on from where the glide was
//! headed. A snapping viewport moves an item per arrow, the item nearest a
//! page on per page key (a pager's page), and to its first or last item.

use crate::component::NodeStore;
use crate::input::{Key, KeyEvent};
use crate::layout::{Axis, OnAxis, Vec2};
use crate::node::NodeId;

/// How far an arrow key scrolls (points).
pub const LINE_STEP: f32 = 40.0;
/// The share of the viewport a page key scrolls at least.
const PAGE_SHARE: f32 = 0.875;
/// The overlap a page key keeps in view at most (points).
const PAGE_OVERLAP: f32 = 40.0;

/// What a key asks of a viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// A line back (`false`) or on (`true`).
    Line(bool),
    /// A page back or on.
    Page(bool),
    /// To the start or the end.
    End(bool),
}

/// The step key `ev` asks for and the axis it scrolls, `None` for a key that
/// does not scroll. `apple` reads Apple's conventions: Command with an arrow
/// for the ends and Option with one for a page; elsewhere Control with Home
/// or End too.
fn step_of(ev: &KeyEvent, apple: bool) -> Option<(Step, Option<Axis>)> {
    if !ev.pressed {
        return None;
    }
    let m = ev.modifiers;
    let bare = !m.shift && !m.control && !m.alt && !m.logo;
    let (far, page) = if apple {
        (
            m.logo && !m.alt && !m.control,
            m.alt && !m.logo && !m.control,
        )
    } else {
        (false, false)
    };
    let vertical = Some(Axis::Column);
    let horizontal = Some(Axis::Row);
    Some(match ev.key {
        Key::Up if far => (Step::End(false), vertical),
        Key::Down if far => (Step::End(true), vertical),
        Key::Up if page => (Step::Page(false), vertical),
        Key::Down if page => (Step::Page(true), vertical),
        Key::Up if bare => (Step::Line(false), vertical),
        Key::Down if bare => (Step::Line(true), vertical),
        Key::Left if bare => (Step::Line(false), horizontal),
        Key::Right if bare => (Step::Line(true), horizontal),
        Key::PageUp if bare => (Step::Page(false), None),
        Key::PageDown if bare => (Step::Page(true), None),
        Key::Space if bare => (Step::Page(true), None),
        Key::Space if m.shift && !m.control && !m.alt && !m.logo => (Step::Page(false), None),
        Key::Home if bare || (!apple && m.control && !m.alt && !m.logo) => (Step::End(false), None),
        Key::End if bare || (!apple && m.control && !m.alt && !m.logo) => (Step::End(true), None),
        _ => return None,
    })
}

impl NodeStore {
    /// A key for a gesture the keyboard carries on: a drag carrying data,
    /// a lifted row, an open swipe row. Returns whether one took it; the
    /// window offers Tab and Escape here before moving the focus or
    /// dismissing a layer.
    pub fn carry_key(&mut self, ev: &KeyEvent) -> bool {
        self.drag_key(ev) || self.reorder_key(ev) || self.swipe_key(ev)
    }

    /// Scroll the viewport the keyboard is in under `root` as key `ev` asks,
    /// if it is one that scrolls: see [`key_scroll`](crate::key_scroll).
    /// `apple` reads Apple's key conventions. Returns whether a viewport
    /// took it.
    pub fn scroll_by_key(&mut self, root: NodeId, ev: &KeyEvent, apple: bool) -> bool {
        if self.drag_key(ev)
            || self.reorder_key(ev)
            || self.viewer_key(root, ev)
            || self.swipe_key(ev)
        {
            return true;
        }
        let Some((step, axis)) = step_of(ev, apple) else {
            return false;
        };
        let from = self
            .focused()
            .or_else(|| self.hovered())
            .filter(|&n| self.arena().is_live(n) && self.within(n, root));
        let Some((id, axis)) = self.key_viewport(from, root, axis) else {
            return false;
        };
        if self.snaps(id) {
            self.snap_by_key(id, axis, step);
        } else {
            self.glide_by_key(id, axis, step);
        }
        true
    }

    /// The viewport a key along `axis` (any, for `None`) scrolls: the
    /// nearest about `from` that scrolls that way, else the outermost under
    /// `root`.
    fn key_viewport(
        &self,
        from: Option<NodeId>,
        root: NodeId,
        axis: Option<Axis>,
    ) -> Option<(NodeId, Axis)> {
        let takes = |n: NodeId| {
            let along = axis.unwrap_or(self.scroll_axis(n)?);
            (self.scrolls_on(n, along) && self.scroll_range(n, along) > 0.0).then_some((n, along))
        };
        let mut at = from;
        while let Some(n) = at {
            if let Some(found) = takes(n) {
                return Some(found);
            }
            at = self.arena().parent(n);
        }
        // Outermost first: a breadth-first walk from the root.
        let mut queue = std::collections::VecDeque::from([root]);
        while let Some(n) = queue.pop_front() {
            if let Some(found) = takes(n) {
                return Some(found);
            }
            queue.extend(self.arena().children(n));
        }
        None
    }

    /// The page step of viewport `id` along `axis`: its extent less a
    /// sliver of overlap kept in view.
    pub(crate) fn page_step(&self, id: NodeId, axis: Axis) -> f32 {
        let extent = self.bounds_main(id, axis);
        (extent * PAGE_SHARE).max(extent - PAGE_OVERLAP).max(1.0)
    }

    /// Glide viewport `id` a step along `axis`, on from where it is headed.
    fn glide_by_key(&mut self, id: NodeId, axis: Axis, step: Step) {
        let (least, most) = self.scroll_span(id, axis);
        let range = most.max(least.abs());
        // Distances from the start, which a row read right to left takes
        // leftward.
        let sign = if least < 0.0 { -1.0 } else { 1.0 };
        let headed = (self.scroll_target(id).on(axis) * sign).clamp(0.0, range);
        let to = match step {
            Step::Line(on) => headed + if on { LINE_STEP } else { -LINE_STEP },
            Step::Page(on) => {
                let page = self.page_step(id, axis);
                headed + if on { page } else { -page }
            }
            Step::End(on) => {
                if on {
                    range
                } else {
                    0.0
                }
            }
        };
        let to = to.clamp(0.0, range) * sign;
        self.stop_coast(id);
        let target = match axis {
            Axis::Row => Vec2 {
                x: to,
                ..self.scroll_target(id)
            },
            Axis::Column => Vec2 {
                y: to,
                ..self.scroll_target(id)
            },
        };
        self.glide_to(id, target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{BuildCx, FlexStyle, LeafStyle, ScrollStyle};
    use crate::input::Modifiers;
    use crate::layout::Sizing;
    use crate::snap::ScrollSnap;
    use crate::style::BoxStyle;
    use std::time::Duration;
    use viso_render::Rect;

    /// A 100-tall column viewport over ten 100-tall items under a root, and
    /// the items.
    fn column(snap: Option<ScrollSnap>) -> (NodeStore, NodeId, NodeId, Vec<NodeId>) {
        let mut store = NodeStore::new();
        let mut items = Vec::new();
        let mut viewport = None;
        let mut cx = BuildCx::new(&mut store);
        let root = cx
            .flex(FlexStyle::default(), |cx| {
                let v = cx.scroll(
                    ScrollStyle {
                        size: Sizing::fixed(100.0, 100.0),
                        snap,
                        ..ScrollStyle::default()
                    },
                    |cx| {
                        cx.flex(
                            FlexStyle {
                                axis: Axis::Column,
                                ..FlexStyle::default()
                            },
                            |cx| {
                                for _ in 0..10 {
                                    let item = cx.leaf(LeafStyle {
                                        size: Sizing::fixed(100.0, 100.0),
                                        style: BoxStyle::NONE,
                                    });
                                    items.push(item.id());
                                }
                            },
                        );
                    },
                );
                viewport = Some(v.id());
            })
            .id();
        lay_out(&mut store, root);
        (store, root, viewport.unwrap(), items)
    }

    fn lay_out(store: &mut NodeStore, root: NodeId) {
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0,
        };
        store.layout(root, surface, &mut Vec::new());
    }

    fn press(key: Key) -> KeyEvent {
        KeyEvent {
            key,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::default(),
        }
    }

    /// Frames until the viewport stands still.
    fn settle(store: &mut NodeStore, root: NodeId) {
        for _ in 0..240 {
            if !store.is_scroll_moving() {
                return;
            }
            store.tick_scroll_motion(Duration::from_millis(16));
            lay_out(store, root);
        }
    }

    #[test]
    fn keys_glide_a_line_a_page_and_to_either_end() {
        let (mut store, root, viewport, _) = column(None);
        let at = |store: &mut NodeStore, key: Key| {
            assert!(store.scroll_by_key(root, &press(key), false));
            settle(store, root);
            store.scroll(viewport).y
        };
        assert_eq!(at(&mut store, Key::Down), 40.0);
        assert_eq!(at(&mut store, Key::PageDown), 127.5);
        assert_eq!(at(&mut store, Key::Space), 215.0);
        assert_eq!(at(&mut store, Key::End), 900.0);
        assert_eq!(at(&mut store, Key::PageDown), 900.0, "held at the end");
        assert_eq!(at(&mut store, Key::Up), 860.0);
        assert_eq!(at(&mut store, Key::Home), 0.0);
        assert!(
            !store.scroll_by_key(root, &press(Key::Left), false),
            "nothing scrolls across"
        );
    }

    #[test]
    fn a_key_pressed_again_heads_on_from_where_the_glide_was_headed() {
        let (mut store, root, viewport, _) = column(None);
        store.scroll_by_key(root, &press(Key::PageDown), false);
        store.tick_scroll_motion(Duration::from_millis(16));
        store.scroll_by_key(root, &press(Key::PageDown), false);
        assert_eq!(store.scroll_target(viewport).y, 175.0);
        settle(&mut store, root);
        assert_eq!(store.scroll(viewport).y, 175.0);
    }

    #[test]
    fn apple_s_command_and_option_arrows_reach_the_ends_and_page() {
        let (mut store, root, viewport, _) = column(None);
        let with = |key, logo, alt| KeyEvent {
            modifiers: Modifiers {
                logo,
                alt,
                ..Modifiers::default()
            },
            ..press(key)
        };
        store.scroll_by_key(root, &with(Key::Down, true, false), true);
        assert_eq!(store.scroll_target(viewport).y, 900.0);
        store.scroll_by_key(root, &with(Key::Up, false, true), true);
        assert_eq!(store.scroll_target(viewport).y, 812.5);
        assert!(
            !store.scroll_by_key(root, &with(Key::Down, true, false), false),
            "the Windows key and an arrow scroll nothing elsewhere"
        );
    }

    #[test]
    fn a_snapping_viewport_moves_an_item_a_key() {
        let (mut store, root, viewport, _) = column(Some(ScrollSnap::START));
        for (key, rests) in [
            (Key::Down, 100.0),
            (Key::Down, 200.0),
            (Key::PageDown, 300.0),
            (Key::End, 900.0),
            (Key::Up, 800.0),
            (Key::Home, 0.0),
        ] {
            store.scroll_by_key(root, &press(key), false);
            settle(&mut store, root);
            assert_eq!(store.scroll(viewport).y, rests, "after {key:?}");
        }
        // Pressed again before it arrives, it heads an item further.
        store.scroll_by_key(root, &press(Key::Down), false);
        store.scroll_by_key(root, &press(Key::Down), false);
        settle(&mut store, root);
        assert_eq!(store.scroll(viewport).y, 200.0);
    }

    #[test]
    fn the_focused_node_s_viewport_takes_the_keys() {
        let (mut store, root, viewport, items) = column(None);
        store.set_focusable(items[3], true);
        crate::input::focus_node(&mut store, items[3]);
        let before = store.scroll_target(viewport).y;
        assert!(store.scroll_by_key(root, &press(Key::Down), false));
        assert_eq!(store.scroll_target(viewport).y, before + 40.0);
    }
}
