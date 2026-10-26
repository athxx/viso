//! Access keys: the letter a control answers to with Alt held — `&File`
//! underlines its F — and the menu bar that Alt alone or F10 takes the
//! keyboard to.
//!
//! Off Apple systems, a letter or digit pressed with Alt held that the
//! focused node's handlers did not take reaches the control whose access key
//! it is ([`Access::Key`]): the first in tree order, or the next after the
//! focus when several share it; one hidden, disabled or outside an open
//! dialog does not hear it. Alt pressed and let go with nothing else between,
//! or F10, reaches the window's menu bar ([`Access::MenuBar`]). While Alt is
//! held the controls show their cues: each control with an access key, and
//! the menu bar, hears them come and go ([`Access::Cues`]).
//!
//! The table is cold: a few entries a window registers as it builds, read
//! on the rare Alt press.

use crate::component::NodeStore;
use crate::node::NodeId;

/// What an access-key dispatch tells a node ([`EventCx::access`]).
///
/// [`EventCx::access`]: crate::context::EventCx::access
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Its access key was pressed with Alt held.
    Key,
    /// Alt alone or F10 was pressed for the window's menu bar.
    MenuBar,
    /// Alt went down (`true`) or up: show the access keys' cues, or hide them.
    Cues(bool),
}

/// The access keys of a window's nodes, its menu bar, and whether the cues
/// show.
#[derive(Debug, Default)]
pub(crate) struct AccessKeys {
    keys: Vec<(NodeId, char)>,
    menu_bar: Option<NodeId>,
    cues: bool,
}

impl AccessKeys {
    pub(crate) fn clear(&mut self) {
        *self = AccessKeys::default();
    }

    /// Forgets the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        if self.keys.is_empty() && self.menu_bar.is_none() {
            return;
        }
        self.keys.retain(|(id, _)| id.index() != index);
        if self.menu_bar.is_some_and(|id| id.index() == index) {
            self.menu_bar = None;
        }
    }
}

impl NodeStore {
    /// Makes `key` the access key of node `id`, or takes its key away. A
    /// letter is matched whatever its case. A live-guarded write.
    pub fn set_access_key(&mut self, id: NodeId, key: Option<char>) {
        if !self.arena().is_live(id) {
            return;
        }
        let keys = &mut self.access.keys;
        keys.retain(|&(n, _)| n != id);
        if let Some(key) = key {
            keys.push((id, fold(key)));
        }
    }

    /// The access key of node `id`.
    pub fn access_key(&self, id: NodeId) -> Option<char> {
        self.access
            .keys
            .iter()
            .find(|&&(n, _)| n == id)
            .map(|&(_, key)| key)
    }

    /// Makes node `id` the window's menu bar, which Alt alone and F10 reach,
    /// or makes the window have none.
    pub fn set_menu_bar(&mut self, id: Option<NodeId>) {
        self.access.menu_bar = id.filter(|&id| self.arena().is_live(id));
    }

    /// The window's menu bar, while it shows.
    pub fn menu_bar(&self) -> Option<NodeId> {
        self.access
            .menu_bar
            .filter(|&id| self.arena().is_live(id) && !self.hidden(id) && !self.inert(id))
    }

    /// Whether any node has an access key, or the window a menu bar.
    pub fn has_access_keys(&self) -> bool {
        !self.access.keys.is_empty() || self.menu_bar().is_some()
    }

    /// Whether the access keys' cues show: Alt is held.
    pub fn access_cues(&self) -> bool {
        self.access.cues
    }

    /// Notes whether the cues show; returns whether that changed.
    pub(crate) fn set_access_cues(&mut self, cues: bool) -> bool {
        std::mem::replace(&mut self.access.cues, cues) != cues
    }

    /// The nodes that hear the cues come and go: those with access keys,
    /// and the menu bar.
    pub(crate) fn cued(&self) -> Vec<NodeId> {
        let mut out: Vec<NodeId> = self.access.keys.iter().map(|&(id, _)| id).collect();
        out.extend(self.access.menu_bar);
        out.retain(|&id| self.arena().is_live(id));
        out.dedup();
        out
    }

    /// The node `key` reaches under `root`: of the shown, enabled nodes with
    /// it as their access key, in tree order within the focus scope, the
    /// next after the focused one, else the first.
    pub fn access_target(&self, root: NodeId, key: char) -> Option<NodeId> {
        let key = fold(key);
        if !self.access.keys.iter().any(|&(_, k)| k == key) {
            return None;
        }
        let scope = self
            .focus_scope()
            .filter(|&s| self.arena().is_live(s))
            .unwrap_or(root);
        if self.inert(scope) {
            return None;
        }
        let mut order = Vec::new();
        let mut stack = vec![scope];
        while let Some(node) = stack.pop() {
            if self.hidden(node) || !self.enabled(node) {
                continue;
            }
            if self.access_key(node) == Some(key) {
                order.push(node);
            }
            let at = stack.len();
            stack.extend(self.arena().children(node));
            stack[at..].reverse();
        }
        let after = self
            .focused()
            .and_then(|f| order.iter().position(|&n| n == f))
            .map_or(0, |at| at + 1);
        order.get(after).or(order.first()).copied()
    }
}

/// `key` as access keys match it: lowercase.
fn fold(key: char) -> char {
    key.to_lowercase().next().unwrap_or(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{BuildCx, LeafStyle};
    use crate::layout::Sizing;
    use crate::style::BoxStyle;

    fn leaf(cx: &mut BuildCx<'_>) -> NodeId {
        cx.leaf(LeafStyle {
            size: Sizing::fixed(10.0, 10.0),
            style: BoxStyle::NONE,
        })
        .id()
    }

    #[test]
    fn a_key_reaches_the_next_shown_enabled_node_holding_it() {
        let mut store = NodeStore::new();
        let (root, [a, b, c]) = {
            let mut cx = BuildCx::new(&mut store);
            let mut kids = [None; 3];
            let root = cx.flex(Default::default(), |cx| {
                for kid in &mut kids {
                    *kid = Some(leaf(cx));
                }
            });
            (root.id(), kids.map(Option::unwrap))
        };
        store.set_access_key(a, Some('F'));
        store.set_access_key(b, Some('e'));
        store.set_access_key(c, Some('f'));
        assert_eq!(store.access_key(a), Some('f'), "folded");
        assert_eq!(store.access_target(root, 'f'), Some(a));
        assert_eq!(store.access_target(root, 'E'), Some(b));
        assert_eq!(store.access_target(root, 'x'), None);
        store.set_focused(Some(a));
        assert_eq!(store.access_target(root, 'f'), Some(c), "the next one");
        store.set_focused(Some(c));
        assert_eq!(store.access_target(root, 'f'), Some(a), "wrapping");
        store.set_hidden(a, true);
        assert_eq!(store.access_target(root, 'f'), Some(c));
        store.set_enabled(c, false);
        assert_eq!(store.access_target(root, 'f'), None);
        store.set_access_key(b, None);
        assert_eq!(store.access_target(root, 'e'), None);
    }

    #[test]
    fn the_menu_bar_is_reached_while_it_shows() {
        let mut store = NodeStore::new();
        let bar = {
            let mut cx = BuildCx::new(&mut store);
            leaf(&mut cx)
        };
        assert!(!store.has_access_keys());
        store.set_menu_bar(Some(bar));
        assert_eq!(store.menu_bar(), Some(bar));
        assert!(store.has_access_keys());
        store.set_hidden(bar, true);
        assert_eq!(store.menu_bar(), None);
    }
}
