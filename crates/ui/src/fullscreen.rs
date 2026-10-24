//! Full screen: one node at a time shown over the whole window, the window
//! itself asked to fill the screen.
//!
//! A node put full screen ([`NodeStore::set_fullscreen`]) becomes a
//! [layer](crate::layer) laid out over the whole surface, whatever size it
//! asks, on an opaque backdrop, the focus kept inside it; the window is asked
//! to go full screen unless it is already. Coming back puts the node back in
//! its parent's flow as it was, and the window as it was. The window leaving
//! full screen on its own — the system's control, a gesture — brings the
//! node back too, and so does the node being freed. A watcher of the node
//! hears each change ([`Notice::Fullscreen`](crate::notice::Notice)). Where
//! windows always fill the screen the node still covers the window.

use viso_render::Rgba;

use crate::component::NodeStore;
use crate::node::NodeId;

/// What a node full screen shows behind its own content.
const BACKDROP: Rgba = Rgba {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

/// The node shown full screen, and the window's own state. Cold.
#[derive(Debug, Default)]
pub(crate) struct Fullscreen {
    node: Option<NodeId>,
    /// The fill the node had, given back when it comes back.
    fill: Rgba,
    /// Whether the node was a layer already, which it stays.
    layer: bool,
    /// Whether going full screen took the window full screen, which coming
    /// back undoes.
    took: bool,
    /// Whether the window is full screen, as last told.
    window: bool,
    /// What the window is to be asked, not yet taken.
    ask: Option<bool>,
}

impl Fullscreen {
    pub(crate) fn clear(&mut self) {
        *self = Fullscreen {
            window: self.window,
            ..Fullscreen::default()
        };
    }
}

impl NodeStore {
    /// Shows node `id` full screen, or brings it back. Another node full
    /// screen comes back first, the window staying full screen. A no-op for
    /// a stale handle, or a node that is already as asked.
    pub fn set_fullscreen(&mut self, id: NodeId, on: bool) {
        if !self.arena().is_live(id) || (self.fullscreen() == Some(id)) == on {
            return;
        }
        if !on {
            self.leave_fullscreen(true);
            return;
        }
        let took = self.fullscreen.took;
        self.leave_fullscreen(false);
        self.fullscreen.node = Some(id);
        self.fullscreen.fill = self.style(id).fill;
        self.fullscreen.layer = self.is_layer(id);
        self.set_layer(id, true);
        // A layer already is laid out again over the surface it covers.
        self.shown_changed(id);
        self.set_fill(id, BACKDROP);
        self.trap_focus(id, true);
        self.fullscreen.took = took || !self.fullscreen.window;
        if self.fullscreen.took && !self.fullscreen.window {
            self.fullscreen.ask = Some(true);
        }
    }

    /// The node shown full screen, if any.
    pub fn fullscreen(&self) -> Option<NodeId> {
        self.fullscreen.node.filter(|&n| self.arena().is_live(n))
    }

    /// Whether the window is full screen, as the platform last told.
    pub fn window_fullscreen(&self) -> bool {
        self.fullscreen.window
    }

    /// The window went full screen, or left it: leaving brings back the
    /// node shown full screen.
    pub fn window_fullscreen_changed(&mut self, on: bool) {
        self.fullscreen.window = on;
        if !on {
            self.fullscreen.took = false;
            self.leave_fullscreen(false);
        }
    }

    /// What the window is to be asked since this was last called: to go
    /// full screen (`true`) or to come back. A node full screen that was
    /// freed since gives back the window it took.
    pub fn take_fullscreen_ask(&mut self) -> Option<bool> {
        if self.fullscreen.node.is_some() && self.fullscreen().is_none() {
            self.fullscreen.node = None;
            if self.fullscreen.took {
                self.fullscreen.took = false;
                self.fullscreen.ask = Some(false);
            }
        }
        self.fullscreen.ask.take()
    }

    /// Whether node `index` lays out over the whole surface as the node
    /// shown full screen.
    #[inline]
    pub(crate) fn covers_surface(&self, index: u32) -> bool {
        self.fullscreen.node.is_some_and(|n| n.index() == index)
    }

    /// Brings back the node shown full screen, asking the window back too
    /// when `window` and the node took it.
    fn leave_fullscreen(&mut self, window: bool) {
        let Some(node) = self.fullscreen.node.take() else {
            return;
        };
        if self.arena().is_live(node) {
            self.trap_focus(node, false);
            if self.fullscreen.layer {
                self.shown_changed(node);
            } else {
                self.set_layer(node, false);
            }
            self.set_fill(node, self.fullscreen.fill);
        }
        if window && self.fullscreen.took {
            self.fullscreen.took = false;
            self.fullscreen.ask = Some(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FlexStyle;
    use crate::component::{BuildCx, LeafStyle};
    use crate::layout::Size;
    use crate::style::BoxStyle;
    use viso_render::Rect;

    const SURFACE: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 400.0,
        h: 300.0,
    };

    /// A column holding a 40 × 30 leaf after a 100 tall one, laid out.
    fn built() -> (NodeStore, NodeId, NodeId) {
        let mut store = NodeStore::new();
        let mut leaf = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                cx.leaf(LeafStyle {
                    size: Size::fixed(40.0, 100.0),
                    style: BoxStyle::NONE,
                });
                leaf = Some(
                    cx.leaf(LeafStyle {
                        size: Size::fixed(40.0, 30.0),
                        style: BoxStyle::solid(Rgba::new(1.0, 0.0, 0.0, 1.0)),
                    })
                    .id(),
                );
            });
            cx.root().unwrap()
        };
        store.layout(root, SURFACE, &mut Vec::new());
        (store, root, leaf.unwrap())
    }

    fn rect(store: &NodeStore, id: NodeId) -> Rect {
        store.world(id)
    }

    #[test]
    fn a_node_full_screen_covers_the_window_and_comes_back_where_it_was() {
        let (mut store, root, leaf) = built();
        let was = rect(&store, leaf);
        store.set_fullscreen(leaf, true);
        assert_eq!(store.fullscreen(), Some(leaf));
        assert_eq!(store.take_fullscreen_ask(), Some(true));
        assert_eq!(store.take_fullscreen_ask(), None);
        store.layout(root, SURFACE, &mut Vec::new());
        assert_eq!(rect(&store, leaf), SURFACE);
        assert_eq!(store.style(leaf).fill, BACKDROP);
        assert!(store.is_overlay(leaf));

        // The platform answers; coming back asks the window back.
        store.window_fullscreen_changed(true);
        store.set_fullscreen(leaf, false);
        assert_eq!(store.fullscreen(), None);
        assert_eq!(store.take_fullscreen_ask(), Some(false));
        store.layout(root, SURFACE, &mut Vec::new());
        assert_eq!(rect(&store, leaf), was);
        assert_eq!(store.style(leaf).fill, Rgba::new(1.0, 0.0, 0.0, 1.0));
        assert!(!store.is_layer(leaf));
    }

    #[test]
    fn the_window_leaving_full_screen_brings_the_node_back_and_one_already_full_keeps_it() {
        let (mut store, root, leaf) = built();
        store.set_fullscreen(leaf, true);
        store.take_fullscreen_ask();
        store.window_fullscreen_changed(true);
        store.window_fullscreen_changed(false);
        assert_eq!(store.fullscreen(), None);
        assert_eq!(store.take_fullscreen_ask(), None, "the window is back");

        // A window full screen already is neither asked nor given back.
        store.window_fullscreen_changed(true);
        store.set_fullscreen(leaf, true);
        assert_eq!(store.take_fullscreen_ask(), None);
        store.set_fullscreen(leaf, false);
        assert_eq!(store.take_fullscreen_ask(), None);
        assert!(store.window_fullscreen());

        // A node freed while full screen gives back the window it took.
        store.window_fullscreen_changed(false);
        store.set_fullscreen(leaf, true);
        store.take_fullscreen_ask();
        let (mut effects, mut scratch) = (crate::reactive::EffectStore::default(), Vec::new());
        store.free_tree(leaf, &mut effects, &mut scratch);
        assert_eq!(store.take_fullscreen_ask(), Some(false));
        let _ = root;
    }
}
