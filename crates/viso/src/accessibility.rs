//! The facade half of the accessibility OS bridge (ADR 0030): turns a window's
//! derived [`SemanticsTree`] into AccessKit tree updates for the platform, and
//! an assistive technology's actions back into ordinary input.
//!
//! A window holds an [`AccessBridge`] only while an assistive technology
//! listens. The first update carries the whole tree; later ones carry only the
//! nodes that differ from the last publication. The bridge re-derives only on
//! a frame that dirtied semantics, layout or transforms, so with no assistive
//! technology running the cost is the `Option` check in the frame loop.

use std::collections::HashMap;

use viso_platform::accesskit as ak;
use viso_platform::{AccessAction, WindowId};
use viso_runtime::{
    InputSample, Key, KeySample, Modifiers, PointerId, PointerKind, PointerPhase, PointerSample,
};
use viso_ui::{DirtyClass, NodeId, NodeStore, PointerButtons, Role, SemanticsNode, SemanticsTree};

/// The dirty classes that can change what the published tree says: roles,
/// labels, state and focus (SEMANTICS), and where nodes sit (LAYOUT,
/// TRANSFORM).
fn republish() -> DirtyClass {
    DirtyClass::SEMANTICS | DirtyClass::LAYOUT | DirtyClass::TRANSFORM
}

/// One window's published accessibility tree.
pub(crate) struct AccessBridge {
    /// Every node last sent, by AccessKit id: the base the next diff runs
    /// against.
    published: HashMap<u64, ak::Node>,
    /// The retained node behind each published id, for routing actions back.
    targets: HashMap<u64, NodeId>,
    /// The root and focus last sent; `None` until the first publication.
    root: Option<ak::NodeId>,
    focus: Option<ak::NodeId>,
}

impl AccessBridge {
    pub(crate) fn new() -> Self {
        Self {
            published: HashMap::new(),
            targets: HashMap::new(),
            root: None,
            focus: None,
        }
    }

    /// The update that brings the platform's copy of the tree under `root` up
    /// to date, or `None` when it already is. `scale` turns the store's
    /// logical points into the physical pixels AccessKit bounds are in. Call
    /// after layout and transforms resolved, before the frame's dirty classes
    /// are cleared.
    pub(crate) fn update(
        &mut self,
        store: &NodeStore,
        root: NodeId,
        scale: f32,
    ) -> Option<ak::TreeUpdate> {
        let root_id = access_id(root);
        if self.root == Some(root_id) && !store.any_dirty_class(republish()) {
            return None;
        }
        let tree = store.derive_semantics(root);
        if tree.is_empty() {
            return None;
        }
        let mut walk = Walk {
            store,
            tree: &tree,
            scale,
            nodes: HashMap::with_capacity(tree.len()),
            targets: HashMap::with_capacity(tree.len()),
            focus: root_id,
        };
        walk.visit(0);
        let Walk {
            nodes,
            targets,
            focus,
            ..
        } = walk;

        let mut changed: Vec<(ak::NodeId, ak::Node)> = nodes
            .iter()
            .filter(|(id, node)| self.published.get(id) != Some(node))
            .map(|(&id, node)| (ak::NodeId(id), node.clone()))
            .collect();
        changed.sort_unstable_by_key(|(id, _)| *id);
        let new_root = self.root != Some(root_id);
        let focus_moved = self.focus != Some(focus);
        self.published = nodes;
        self.targets = targets;
        self.root = Some(root_id);
        self.focus = Some(focus);
        if changed.is_empty() && !new_root && !focus_moved {
            return None;
        }
        Some(ak::TreeUpdate {
            nodes: changed,
            tree: new_root.then(|| ak::TreeInfo::new(root_id)),
            tree_id: ak::TreeId::ROOT,
            focus,
        })
    }

    /// The retained node last published as `target`.
    pub(crate) fn target(&self, target: u64) -> Option<NodeId> {
        self.targets.get(&target).copied()
    }
}

/// The input that performs `action` on `node` in `window`, after moving focus
/// where the action needs it. `Focus` needs no input; `Click` is a primary
/// press and release at the node's center; `Increment`/`Decrement` are the
/// arrow keys a focused slider steps on. Positions are physical pixels, as the
/// scheduler delivers them.
pub(crate) fn perform(
    store: &mut NodeStore,
    window: WindowId,
    node: NodeId,
    action: AccessAction,
    scale: f32,
) -> Option<[InputSample; 2]> {
    let key = match action {
        AccessAction::Focus => {
            viso_ui::focus_node(store, node);
            return None;
        }
        AccessAction::Click => {
            let rect = store.world(node);
            let x = (rect.x + rect.w * 0.5) * scale;
            let y = (rect.y + rect.h * 0.5) * scale;
            let pointer = |phase, buttons: PointerButtons, pressure| {
                InputSample::Pointer(PointerSample {
                    window,
                    pointer: PointerId::MOUSE,
                    kind: PointerKind::Mouse,
                    x,
                    y,
                    pressure,
                    buttons: buttons.0,
                    modifiers: Modifiers::default(),
                    phase,
                })
            };
            return Some([
                pointer(PointerPhase::Down, PointerButtons::PRIMARY, 1.0),
                pointer(PointerPhase::Up, PointerButtons::NONE, 0.0),
            ]);
        }
        AccessAction::Increment => Key::Right,
        AccessAction::Decrement => Key::Left,
    };
    viso_ui::focus_node(store, node);
    let press = |pressed| {
        InputSample::Key(KeySample {
            window,
            key,
            pressed,
            repeat: false,
            modifiers: Modifiers::default(),
        })
    };
    Some([press(true), press(false)])
}

/// The AccessKit id of a retained node: its index and generation packed, so a
/// recycled slot never answers for the node it replaced.
fn access_id(id: NodeId) -> ak::NodeId {
    ak::NodeId(u64::from(id.index()) | u64::from(id.generation()) << 32)
}

/// The AccessKit role for a Viso one. An unnamed group is a plain layout
/// container, which platforms leave out of what they announce.
fn access_role(role: Role, named: bool) -> ak::Role {
    match role {
        Role::Group if named => ak::Role::Group,
        Role::Group => ak::Role::GenericContainer,
        Role::Button => ak::Role::Button,
        Role::CheckBox => ak::Role::CheckBox,
        Role::Slider => ak::Role::Slider,
        Role::Radio => ak::Role::RadioButton,
        Role::Label => ak::Role::Label,
        Role::TextField => ak::Role::TextInput,
        Role::Tab => ak::Role::Tab,
        Role::TabList => ak::Role::TabList,
        Role::Navigation => ak::Role::Navigation,
        Role::Dialog => ak::Role::Dialog,
        Role::Status => ak::Role::Status,
        Role::Region => ak::Role::Region,
        Role::Tree => ak::Role::Tree,
        Role::TreeItem => ak::Role::TreeItem,
    }
}

/// One pre-order conversion of a derived tree.
struct Walk<'a> {
    store: &'a NodeStore,
    tree: &'a SemanticsTree,
    scale: f32,
    nodes: HashMap<u64, ak::Node>,
    targets: HashMap<u64, NodeId>,
    focus: ak::NodeId,
}

impl Walk<'_> {
    /// Convert `tree.nodes[index]` and its visible descendants; returns its id.
    fn visit(&mut self, index: usize) -> ak::NodeId {
        let row = &self.tree.nodes[index];
        let id = access_id(row.id);
        let mut node = self.node(row, index == 0);
        let children: Vec<ak::NodeId> = row
            .children
            .iter()
            .filter(|&&child| !self.store.hidden(self.tree.nodes[child].id))
            .map(|&child| self.visit(child))
            .collect();
        node.set_children(children);
        if row.focused {
            self.focus = id;
        }
        self.nodes.insert(id.0, node);
        self.targets.insert(id.0, row.id);
        id
    }

    fn node(&self, row: &SemanticsNode, root: bool) -> ak::Node {
        let role = if root {
            ak::Role::Window
        } else {
            access_role(row.role, row.label.is_some())
        };
        let mut node = ak::Node::new(role);
        if let Some(label) = &row.label {
            node.set_label(label.as_str());
        }
        let rect = self.store.world(row.id);
        let scale = f64::from(self.scale);
        node.set_bounds(ak::Rect {
            x0: f64::from(rect.x) * scale,
            y0: f64::from(rect.y) * scale,
            x1: f64::from(rect.x + rect.w) * scale,
            y1: f64::from(rect.y + rect.h) * scale,
        });
        if let Some(state) = row.state {
            if let Some(checked) = state.checked {
                node.set_toggled(checked.into());
            }
            if let Some(value) = state.value {
                node.set_numeric_value(f64::from(value));
            }
            if let Some((min, max)) = state.range {
                node.set_min_numeric_value(f64::from(min));
                node.set_max_numeric_value(f64::from(max));
            }
            if let Some(expanded) = state.expanded {
                node.set_expanded(expanded);
            }
            if let Some(selected) = state.selected {
                node.set_selected(selected);
            }
        }
        if self.store.focusable(row.id) {
            node.add_action(ak::Action::Focus);
        }
        if self.store.has_handler(row.id) {
            node.add_action(ak::Action::Click);
        }
        if row.role == Role::Slider {
            node.add_action(ak::Action::Increment);
            node.add_action(ak::Action::Decrement);
        }
        node
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use viso_render::Rect;
    use viso_ui::{Axis, BuildCx, FlexStyle, LeafStyle, SemanticState, Semantics, Size};

    /// A 100×20 row holding a named button and a slider, laid out at the
    /// origin. Returns (store, root, [button, slider]).
    fn scene() -> (NodeStore, NodeId, [NodeId; 2]) {
        let mut store = NodeStore::new();
        let kids = Rc::new(RefCell::new(Vec::new()));
        let root = {
            let mut cx = BuildCx::new(&mut store);
            let sink = kids.clone();
            cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    size: Size::fixed(100.0, 20.0),
                    ..Default::default()
                },
                |cx| {
                    for _ in 0..2 {
                        let leaf = cx.leaf(LeafStyle {
                            size: Size::fixed(40.0, 20.0),
                            ..Default::default()
                        });
                        sink.borrow_mut().push(leaf.id());
                    }
                },
            );
            cx.root().unwrap()
        };
        let [button, slider] = [kids.borrow()[0], kids.borrow()[1]];
        store.set_semantics(button, Semantics::role(Role::Button).with_label("Add"));
        store.set_focusable(button, true);
        store.set_semantics(slider, Semantics::role(Role::Slider).with_label("Volume"));
        store.set_semantic_state(slider, SemanticState::slider(0.5, 0.0, 1.0));
        store.set_focusable(slider, true);
        let mut scratch = Vec::new();
        store.layout(
            root,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 20.0,
            },
            &mut scratch,
        );
        store.resolve_transforms(root);
        (store, root, [button, slider])
    }

    fn node(update: &ak::TreeUpdate, id: NodeId) -> &ak::Node {
        let id = access_id(id);
        &update.nodes.iter().find(|(n, _)| *n == id).expect("node").1
    }

    #[test]
    fn first_update_is_the_whole_tree_in_physical_pixels() {
        let (store, root, [button, slider]) = scene();
        let mut bridge = AccessBridge::new();
        let update = bridge.update(&store, root, 2.0).expect("first update");
        assert_eq!(update.tree, Some(ak::TreeInfo::new(access_id(root))));
        assert_eq!(update.nodes.len(), 3);
        assert_eq!(update.focus, access_id(root), "nothing focused yet");

        let window = node(&update, root);
        assert_eq!(window.role(), ak::Role::Window);
        assert_eq!(window.children(), [access_id(button), access_id(slider)]);

        let add = node(&update, button);
        assert_eq!(add.role(), ak::Role::Button);
        assert_eq!(add.label(), Some("Add"));
        assert!(add.supports_action(ak::Action::Focus));
        let bounds = add.bounds().expect("bounds");
        assert_eq!((bounds.x0, bounds.x1, bounds.y1), (0.0, 80.0, 40.0));

        let volume = node(&update, slider);
        assert_eq!(volume.role(), ak::Role::Slider);
        assert_eq!(volume.numeric_value(), Some(0.5));
        assert_eq!(volume.max_numeric_value(), Some(1.0));
        assert!(volume.supports_action(ak::Action::Increment));
        assert_eq!(bridge.target(access_id(slider).0), Some(slider));
    }

    #[test]
    fn a_clean_frame_sends_nothing() {
        let (mut store, root, _) = scene();
        let mut bridge = AccessBridge::new();
        bridge.update(&store, root, 1.0).expect("first update");
        store.clear_dirty();
        assert!(bridge.update(&store, root, 1.0).is_none());
    }

    #[test]
    fn a_state_change_sends_only_that_node() {
        let (mut store, root, [_, slider]) = scene();
        let mut bridge = AccessBridge::new();
        bridge.update(&store, root, 1.0).expect("first update");
        store.clear_dirty();
        store.set_semantic_state(slider, SemanticState::slider(0.75, 0.0, 1.0));
        let update = bridge.update(&store, root, 1.0).expect("changed");
        assert_eq!(update.tree, None);
        assert_eq!(update.nodes.len(), 1);
        assert_eq!(node(&update, slider).numeric_value(), Some(0.75));
    }

    #[test]
    fn focus_moves_and_hidden_subtrees_leave_the_tree() {
        let (mut store, root, [button, slider]) = scene();
        let mut bridge = AccessBridge::new();
        bridge.update(&store, root, 1.0).expect("first update");
        store.clear_dirty();

        viso_ui::focus_node(&mut store, slider);
        let update = bridge.update(&store, root, 1.0).expect("focus moved");
        assert_eq!(update.focus, access_id(slider));
        store.clear_dirty();

        store.set_hidden(button, true);
        let update = bridge.update(&store, root, 1.0).expect("child hidden");
        assert_eq!(node(&update, root).children(), [access_id(slider)]);
        assert!(
            update.nodes.iter().all(|(id, _)| *id != access_id(button)),
            "a removed node is not resent"
        );
    }

    #[test]
    fn actions_lower_to_ordinary_input() {
        let (mut store, _, [button, slider]) = scene();
        let window = WindowId(0);
        let click = perform(&mut store, window, button, AccessAction::Click, 2.0).expect("click");
        let [InputSample::Pointer(down), InputSample::Pointer(up)] = click else {
            panic!("a click is a press and a release");
        };
        assert_eq!(
            (down.phase, up.phase),
            (PointerPhase::Down, PointerPhase::Up)
        );
        assert_eq!((down.x, down.y), (40.0, 20.0), "center, physical pixels");

        let step = perform(&mut store, window, slider, AccessAction::Increment, 1.0);
        assert!(matches!(
            step,
            Some([
                InputSample::Key(KeySample {
                    key: Key::Right,
                    pressed: true,
                    ..
                }),
                _
            ])
        ));
        assert_eq!(
            store.focused(),
            Some(slider),
            "the step lands on the slider"
        );

        assert!(perform(&mut store, window, button, AccessAction::Focus, 1.0).is_none());
        assert_eq!(store.focused(), Some(button));
    }
}
