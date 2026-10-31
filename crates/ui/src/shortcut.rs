//! Keyboard shortcuts: nodes that hear a key no focused node took.
//!
//! A shortcut node is invisible and never focused, and one inside a hidden
//! subtree hears nothing. A key press the focus
//! chain lets through is offered to the shortcuts in turn — first those whose
//! parent lies on the focus chain, nearest the focus first, then those of the
//! whole window — and the first whose key handler stops it takes it; that
//! handler decides whether the key is its chord. While the focus takes text,
//! a key held with no Control, Alt or Command is the text's, and no shortcut
//! hears it.
//!
//! The table is cold and small: one entry for each shortcut node.

use crate::component::NodeStore;
use crate::node::NodeId;

/// Where a shortcut hears keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShortcutScope {
    /// While the focus is in its parent's subtree; with no focus, while its
    /// parent is the window's root.
    #[default]
    Parent,
    /// Anywhere in its window.
    Window,
}

/// The shortcut nodes, in the order they were made shortcuts.
#[derive(Debug, Default)]
pub(crate) struct Shortcuts {
    list: Vec<(NodeId, ShortcutScope)>,
}

impl Shortcuts {
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
    /// Make node `id` a shortcut heard in `scope`, or with `None` stop.
    pub fn set_shortcut(&mut self, id: NodeId, scope: Option<ShortcutScope>) {
        if !self.arena().is_live(id) {
            return;
        }
        let list = &mut self.shortcuts.list;
        let at = list.iter().position(|(n, _)| *n == id);
        match (at, scope) {
            (Some(at), Some(scope)) => list[at].1 = scope,
            (None, Some(scope)) => list.push((id, scope)),
            (Some(at), None) => {
                list.remove(at);
            }
            (None, None) => {}
        }
    }

    /// Where node `id` hears keys, if it is a shortcut.
    pub fn shortcut(&self, id: NodeId) -> Option<ShortcutScope> {
        self.shortcuts
            .list
            .iter()
            .find(|(n, _)| *n == id)
            .map(|&(_, scope)| scope)
    }

    /// The shortcuts a key the focus chain let through is offered to, in
    /// turn, into `out`: none while the focus takes text and the key is
    /// held with no Control, Alt or Command.
    pub(crate) fn shortcuts_for(
        &self,
        root: NodeId,
        modifiers: crate::input::Modifiers,
        out: &mut Vec<NodeId>,
    ) {
        out.clear();
        let list = &self.shortcuts.list;
        if list.is_empty() {
            return;
        }
        let focused = self.focused();
        let chorded = modifiers.control || modifiers.alt || modifiers.logo;
        if !chorded
            && focused
                .is_some_and(|f| self.text_entry_from(f).is_some() || self.field_marks(f).is_some())
        {
            return;
        }
        let mut at = Some(focused.unwrap_or(root));
        while let Some(node) = at {
            out.extend(list.iter().filter_map(|&(shortcut, scope)| {
                (scope == ShortcutScope::Parent
                    && self.parent(shortcut) == Some(node)
                    && self.heard(shortcut))
                .then_some(shortcut)
            }));
            at = self.parent(node);
        }
        out.extend(list.iter().filter_map(|&(shortcut, scope)| {
            (scope == ShortcutScope::Window && self.within(shortcut, root) && self.heard(shortcut))
                .then_some(shortcut)
        }));
    }

    /// Whether shortcut `id` lies in what shows: no ancestor of it hidden, as
    /// a page not current or a branch folded away is.
    fn heard(&self, id: NodeId) -> bool {
        let mut at = self.parent(id);
        while let Some(node) = at {
            if self.hidden(node) {
                return false;
            }
            at = self.parent(node);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::ShortcutScope;
    use crate::{
        BindingTable, BuildCx, EventCx, FlexStyle, Key, KeyEvent, KeyRouter, LeafStyle, Modifiers,
        NodeId, NodeStore, Sizing, StateStore, TextEdits,
    };

    /// A root holding a panel with a focusable leaf and a shortcut, and two
    /// shortcuts of its own: one heard in it, one in the whole window.
    struct Tree {
        store: NodeStore,
        root: NodeId,
        leaf: NodeId,
        shortcuts: [NodeId; 3],
    }

    fn tree() -> Tree {
        let mut store = NodeStore::new();
        let mut ids = Vec::new();
        {
            let mut cx = BuildCx::new(&mut store);
            let leaf = || LeafStyle {
                size: Sizing::fixed(10.0, 10.0),
                ..Default::default()
            };
            cx.flex(FlexStyle::default(), |cx| {
                cx.flex(FlexStyle::default(), |cx| {
                    ids.push(cx.leaf(leaf()).id());
                    ids.push(cx.leaf(leaf()).id());
                });
                ids.push(cx.leaf(leaf()).id());
                ids.push(cx.leaf(leaf()).id());
            });
        }
        let root = store
            .arena()
            .parent(store.arena().parent(ids[0]).unwrap())
            .unwrap();
        let [leaf, inner, outer, window] = ids[..] else {
            unreachable!()
        };
        store.set_focusable(leaf, true);
        store.set_shortcut(inner, Some(ShortcutScope::Parent));
        store.set_shortcut(outer, Some(ShortcutScope::Parent));
        store.set_shortcut(window, Some(ShortcutScope::Window));
        Tree {
            store,
            root,
            leaf,
            shortcuts: [inner, outer, window],
        }
    }

    /// The shortcuts a press of `key` with `modifiers` is offered to, each
    /// logging itself and taking it when it is `taker`.
    fn offered(
        tree: &mut Tree,
        key: Key,
        modifiers: Modifiers,
        taker: Option<usize>,
    ) -> Vec<usize> {
        let log = Rc::new(RefCell::new(Vec::new()));
        for (i, &node) in tree.shortcuts.iter().enumerate() {
            let log = Rc::clone(&log);
            tree.store.set_key_handler(
                node,
                Box::new(move |cx: &mut EventCx<'_>| {
                    log.borrow_mut().push(i);
                    if taker == Some(i) {
                        cx.stop_propagation();
                    }
                }),
            );
        }
        let event = KeyEvent {
            key,
            pressed: true,
            repeat: false,
            modifiers,
        };
        KeyRouter::route_key_taken(
            &mut tree.store,
            &mut StateStore::new(),
            &BindingTable::new(),
            &mut TextEdits::new(),
            tree.root,
            event,
            &mut Vec::new(),
        );
        log.take()
    }

    #[test]
    fn the_shortcut_nearest_the_focus_hears_a_key_first() {
        let mut tree = tree();
        let none = Modifiers::default();
        assert_eq!(
            offered(&mut tree, Key::S, none, None),
            [1, 2],
            "no focus: the root's"
        );
        tree.store.set_focused(Some(tree.leaf));
        assert_eq!(offered(&mut tree, Key::S, none, None), [0, 1, 2]);
        assert_eq!(
            offered(&mut tree, Key::S, none, Some(0)),
            [0],
            "the nearest takes it"
        );
        tree.store.make_field(tree.leaf);
        assert_eq!(
            offered(&mut tree, Key::S, none, None),
            Vec::<usize>::new(),
            "a bare key is the text's"
        );
        let command = Modifiers {
            logo: true,
            ..Modifiers::default()
        };
        assert_eq!(offered(&mut tree, Key::S, command, None), [0, 1, 2]);
        tree.store.set_shortcut(tree.shortcuts[0], None);
        assert_eq!(offered(&mut tree, Key::S, command, None), [1, 2]);
        tree.store.set_focused(None);
        tree.store.set_hidden(tree.root, true);
        assert_eq!(
            offered(&mut tree, Key::S, command, None),
            Vec::<usize>::new(),
            "a hidden subtree's shortcuts hear nothing"
        );
    }
}
