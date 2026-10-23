//! The facade half of the accessibility OS bridge (ADR 0030, ADR 0042): turns
//! a window's derived [`SemanticsTree`] into the [`SemanticsDelta`]s the
//! platform publishes, and an assistive technology's actions back into
//! ordinary input.
//!
//! A window holds an [`AccessBridge`] only while an assistive technology
//! listens on a backend with a bridge. The first delta carries the whole tree;
//! later ones carry only the nodes that differ from the last publication. The
//! bridge re-derives only on a frame that dirtied semantics, layout or
//! transforms, so with no assistive technology running the cost is the
//! `Option` check in the frame loop.

use std::collections::HashMap;

use viso_platform::{
    AccessAction, AccessActions, AccessNode, SemanticsDelta, SemanticsId, SemanticsLive,
    SemanticsRect, SemanticsRole, SemanticsState, WindowId,
};
use viso_runtime::{
    InputSample, Key, KeySample, Modifiers, PointerId, PointerKind, PointerPhase, PointerSample,
};
use viso_ui::{
    DirtyClass, LiveRegion, NodeId, NodeStore, PointerButtons, Role, SemanticState, SemanticsTree,
};

/// The dirty classes that can change what the published tree says: roles,
/// labels, state and focus (SEMANTICS), and where nodes sit (LAYOUT,
/// TRANSFORM).
fn republish() -> DirtyClass {
    DirtyClass::SEMANTICS | DirtyClass::LAYOUT | DirtyClass::TRANSFORM
}

/// One window's published semantics tree.
pub(crate) struct AccessBridge {
    /// Every node last sent, by id: the base the next delta is taken against.
    published: HashMap<SemanticsId, AccessNode>,
    /// The retained node behind each published id, and which of its links
    /// the id is, for routing actions back.
    targets: HashMap<SemanticsId, Target>,
    /// The root and focus last sent; `None` until the first publication.
    root: Option<SemanticsId>,
    focus: Option<SemanticsId>,
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

    /// The delta that brings the platform's copy of the tree under `root` up
    /// to date, or `None` when it already is. `scale` turns the store's
    /// logical points into the physical pixels the delta's bounds are in.
    /// Call after layout and transforms resolved, before the frame's dirty
    /// classes are cleared.
    pub(crate) fn update(
        &mut self,
        store: &NodeStore,
        root: NodeId,
        scale: f32,
    ) -> Option<SemanticsDelta> {
        let root_id = semantics_id(root);
        if self.root == Some(root_id) && !store.any_dirty_class(republish()) {
            return None;
        }
        let mut tree = store.derive_semantics(root);
        if tree.is_empty() {
            return None;
        }
        let mut walk = Walk {
            store,
            nodes: HashMap::with_capacity(tree.len()),
            targets: HashMap::with_capacity(tree.len()),
            focus: root_id,
            scale,
            tree: &mut tree,
        };
        walk.visit(0);
        let Walk {
            nodes,
            targets,
            focus,
            ..
        } = walk;

        let mut changed: Vec<AccessNode> = nodes
            .iter()
            .filter(|(id, node)| self.published.get(id) != Some(node))
            .map(|(_, node)| node.clone())
            .collect();
        changed.sort_unstable_by_key(|node| node.id);
        let new_root = self.root != Some(root_id);
        let focus_moved = self.focus != Some(focus);
        self.published = nodes;
        self.targets = targets;
        self.root = Some(root_id);
        self.focus = Some(focus);
        if changed.is_empty() && !new_root && !focus_moved {
            return None;
        }
        Some(SemanticsDelta {
            root: new_root.then_some(root_id),
            nodes: changed,
            focus,
        })
    }

    /// The retained node last published as `target`, and which of its links
    /// it is.
    pub(crate) fn target(&self, target: SemanticsId) -> Option<Target> {
        self.targets.get(&target).copied()
    }
}

/// A published node's retained node, and which of its links it is.
pub(crate) type Target = (NodeId, Option<u16>);

/// The input that performs `action` on `node` (or its link `link`) in
/// `window`, after moving focus where the action needs it. `Focus` needs no
/// input; `Click` is a primary press and release at the node's center (the
/// link's first line's); `Increment`/`Decrement` are the
/// arrow keys a focused slider steps on, Up and Down for a spin button;
/// `Refresh` needs no input, a refreshable viewport refreshing as a pull
/// past its start makes it; `Remove` is Delete on the focused node.
/// Positions are physical pixels, as the
/// scheduler delivers them. An inert node takes none.
pub(crate) fn perform(
    store: &mut NodeStore,
    window: WindowId,
    node: NodeId,
    link: Option<u16>,
    action: AccessAction,
    scale: f32,
) -> Option<[InputSample; 2]> {
    if store.inert(node) {
        return None;
    }
    let key = match action {
        AccessAction::Focus => {
            viso_ui::focus_node(store, node);
            if let Some(link) = link {
                store.focus_link(node, link);
            }
            return None;
        }
        AccessAction::Click => {
            let rect = link
                .and_then(|link| store.link_boxes(node, link).next())
                .unwrap_or_else(|| store.seen(node));
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
        AccessAction::Refresh => {
            store.begin_refresh(node);
            return None;
        }
        AccessAction::Remove => Key::Delete,
        AccessAction::Increment | AccessAction::Decrement => {
            let spin = store
                .semantics(node)
                .is_some_and(|s| s.role == Role::SpinButton);
            match (action == AccessAction::Increment, spin) {
                (true, false) => Key::Right,
                (false, false) => Key::Left,
                (true, true) => Key::Up,
                (false, true) => Key::Down,
            }
        }
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

/// The published id of a retained node: its index and generation packed, so
/// a recycled slot never answers for the node it replaced.
fn semantics_id(id: NodeId) -> SemanticsId {
    SemanticsId(u64::from(id.index()) | u64::from(id.generation()) << 32)
}

/// The published id of link `link` of a retained node: the top bit set, the
/// link in the 15 bits below it, then the generation's low 16 bits and the
/// index. A node's generation never reaches the top bit.
fn link_id(id: NodeId, link: u16) -> SemanticsId {
    SemanticsId(
        1 << 63
            | u64::from(link & 0x7fff) << 48
            | u64::from(id.generation() & 0xffff) << 32
            | u64::from(id.index()),
    )
}

/// The published id of `row`.
fn row_id(row: &viso_ui::SemanticsNode) -> SemanticsId {
    match row.link {
        Some(link) => link_id(row.id, link),
        None => semantics_id(row.id),
    }
}

/// The published role for a Viso one. An unnamed group is a plain layout
/// container, which platforms leave out of what they announce.
fn semantics_role(role: Role, named: bool) -> SemanticsRole {
    match role {
        Role::Group if named => SemanticsRole::Group,
        Role::Group => SemanticsRole::Container,
        Role::Button => SemanticsRole::Button,
        Role::CheckBox => SemanticsRole::CheckBox,
        Role::Slider => SemanticsRole::Slider,
        Role::Radio => SemanticsRole::Radio,
        Role::Label => SemanticsRole::Label,
        Role::Image => SemanticsRole::Image,
        Role::TextField => SemanticsRole::TextField,
        Role::SecureField => SemanticsRole::SecureField,
        Role::SpinButton => SemanticsRole::SpinButton,
        Role::Menu => SemanticsRole::Menu,
        Role::MenuItem => SemanticsRole::MenuItem,
        Role::Link => SemanticsRole::Link,
        Role::MenuItemCheckBox => SemanticsRole::MenuItemCheckBox,
        Role::MenuItemRadio => SemanticsRole::MenuItemRadio,
        Role::MenuBar => SemanticsRole::MenuBar,
        Role::Separator => SemanticsRole::Separator,
        Role::ListBox => SemanticsRole::ListBox,
        Role::ListOption => SemanticsRole::ListOption,
        Role::ComboBox => SemanticsRole::ComboBox,
        Role::Tooltip => SemanticsRole::Tooltip,
        Role::Table => SemanticsRole::Table,
        Role::Row => SemanticsRole::Row,
        Role::Cell => SemanticsRole::Cell,
        Role::ColumnHeader => SemanticsRole::ColumnHeader,
        Role::EditableComboBox => SemanticsRole::EditableComboBox,
        Role::ProgressIndicator => SemanticsRole::ProgressIndicator,
        Role::SearchField => SemanticsRole::SearchField,
        Role::TabPanel => SemanticsRole::TabPanel,
        Role::Tab => SemanticsRole::Tab,
        Role::TabList => SemanticsRole::TabList,
        Role::Navigation => SemanticsRole::Navigation,
        Role::Dialog => SemanticsRole::Dialog,
        Role::Status => SemanticsRole::Status,
        Role::Region => SemanticsRole::Region,
        Role::Tree => SemanticsRole::Tree,
        Role::TreeItem => SemanticsRole::TreeItem,
        Role::Heading => SemanticsRole::Heading,
        Role::List => SemanticsRole::List,
        Role::ListItem => SemanticsRole::ListItem,
    }
}

fn semantics_live(live: LiveRegion) -> SemanticsLive {
    match live {
        LiveRegion::Off => SemanticsLive::Off,
        LiveRegion::Polite => SemanticsLive::Polite,
        LiveRegion::Assertive => SemanticsLive::Assertive,
    }
}

fn semantics_state(state: Option<SemanticState>, disabled: bool) -> SemanticsState {
    let Some(state) = state else {
        return SemanticsState {
            disabled,
            ..SemanticsState::default()
        };
    };
    SemanticsState {
        toggled: state.checked,
        numeric_value: state.value,
        numeric_range: state.range,
        expanded: state.expanded,
        selected: state.selected,
        position: state.position,
        disabled,
    }
}

/// One pre-order conversion of a derived tree. The tree is the walk's own
/// copy, so each row's text moves into its node rather than being cloned.
struct Walk<'a> {
    store: &'a NodeStore,
    tree: &'a mut SemanticsTree,
    scale: f32,
    nodes: HashMap<SemanticsId, AccessNode>,
    targets: HashMap<SemanticsId, Target>,
    focus: SemanticsId,
}

impl Walk<'_> {
    /// Convert `tree.nodes[index]` and its visible descendants; returns its id.
    fn visit(&mut self, index: usize) -> SemanticsId {
        let rows = std::mem::take(&mut self.tree.nodes[index].children);
        let mut children = Vec::with_capacity(rows.len());
        for &child in &rows {
            let row = &self.tree.nodes[child];
            if row.link.is_some() || !self.store.hidden(row.id) {
                children.push(self.visit(child));
            }
        }
        let node = self.node(index, children);
        let id = node.id;
        let row = &self.tree.nodes[index];
        if row.focused {
            self.focus = id;
        }
        self.targets.insert(id, (row.id, row.link));
        self.nodes.insert(id, node);
        id
    }

    fn node(&mut self, index: usize, children: Vec<SemanticsId>) -> AccessNode {
        let store = self.store;
        let row = &mut self.tree.nodes[index];
        let label = row.label.take();
        let role = if index == 0 {
            SemanticsRole::Window
        } else {
            semantics_role(row.role, label.is_some())
        };
        let rect = match row.link {
            Some(link) => store
                .link_boxes(row.id, link)
                .reduce(viso_ui::Rect::union)
                .unwrap_or_else(|| store.world(row.id)),
            None => store.world(row.id),
        };
        // A disabled node offers no action.
        let mut actions = AccessActions::NONE;
        let live = !row.disabled;
        if live && store.focusable(row.id) {
            actions = actions.with(AccessAction::Focus);
        }
        if live && (row.link.is_some() || store.has_handler(row.id)) {
            actions = actions.with(AccessAction::Click);
        }
        if live && matches!(row.role, Role::Slider | Role::SpinButton) {
            actions = actions
                .with(AccessAction::Increment)
                .with(AccessAction::Decrement);
        }
        if live && store.is_refreshable(row.id) && !store.is_refreshing(row.id) {
            actions = actions.with(AccessAction::Refresh);
        }
        if live && store.check_mark(row.id).is_some_and(|m| m.removes()) {
            actions = actions.with(AccessAction::Remove);
        }
        AccessNode {
            id: row_id(row),
            role,
            label,
            description: row.hint.take(),
            value: row.value.take(),
            live: semantics_live(row.live),
            level: row.heading_level,
            access_key: store.access_key(row.id).filter(|_| row.link.is_none()),
            bounds: SemanticsRect {
                x: rect.x * self.scale,
                y: rect.y * self.scale,
                width: rect.w * self.scale,
                height: rect.h * self.scale,
            },
            state: semantics_state(row.state, row.disabled),
            actions,
            active_descendant: row.active.map(semantics_id),
            children,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use viso_render::Rect;
    use viso_ui::{Axis, BuildCx, FlexStyle, LeafStyle, Semantics, Size};

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
        (store, root, [button, slider])
    }

    fn node(delta: &SemanticsDelta, id: NodeId) -> &AccessNode {
        let id = semantics_id(id);
        delta.nodes.iter().find(|n| n.id == id).expect("node")
    }

    #[test]
    fn each_link_of_a_text_is_its_own_node_that_a_click_follows() {
        let (mut store, root, [text, _]) = scene();
        store.set_text_request(
            text,
            viso_ui::TextRequest {
                text: "a go".into(),
                font_size: viso_ui::FontSize::Fixed(14.0),
                color: viso_ui::Rgba::TRANSPARENT,
                soft_wrap: false,
                locale: None,
                style: Default::default(),
                spans: vec![viso_ui::TextSpan {
                    range: 2..4,
                    style: viso_ui::SpanStyle::default().link("/go"),
                }],
            },
        );
        store.set_content_payload(
            text,
            viso_ui::Content::Text {
                ink: Some(Box::new(viso_ui::TextInk {
                    links: vec![viso_ui::LinkBox {
                        link: 0,
                        rect: Rect::new(10.0, 0.0, 20.0, 20.0),
                    }],
                    ..Default::default()
                })),
                glyphs: Vec::new(),
                atlas: viso_render::TextureId::new(1),
                color_glyphs: Vec::new(),
                color_atlas: None,
                color: viso_ui::Rgba::TRANSPARENT,
                natural: viso_ui::Vec2 { x: 40.0, y: 20.0 },
                baseline: 15.0,
                shaped_at_width: None,
                width_use: viso_ui::WidthUse::Ignored,
                min_content: 40.0,
                max_content: 40.0,
            },
        );
        let mut bridge = AccessBridge::new();
        let update = bridge.update(&store, root, 2.0).expect("first update");
        let id = link_id(text, 0);
        assert_ne!(id, semantics_id(text));
        assert!(node(&update, text).children.contains(&id));
        let link = update.nodes.iter().find(|n| n.id == id).expect("the link");
        assert_eq!(link.role, SemanticsRole::Link);
        assert_eq!(link.label.as_deref(), Some("go"));
        assert_eq!(link.bounds.x, 20.0, "in physical pixels");
        assert!(link.actions.contains(AccessAction::Click));
        assert_eq!(bridge.target(id), Some((text, Some(0))));
        let window = WindowId(1);
        let click = perform(&mut store, window, text, Some(0), AccessAction::Click, 2.0);
        let Some([InputSample::Pointer(down), _]) = click else {
            panic!("a click");
        };
        assert_eq!((down.x, down.y), (40.0, 20.0), "at the link's centre");
    }

    #[test]
    fn a_removable_chip_offers_remove_which_presses_delete_on_it() {
        let (mut store, root, [chip, slider]) = scene();
        let mark = |removable| viso_ui::CheckMark {
            shape: viso_ui::CheckShape::Chip { removable },
            checked: false,
        };
        store.set_check_mark(chip, Some(mark(true)));
        let mut bridge = AccessBridge::new();
        let update = bridge.update(&store, root, 1.0).expect("first update");
        assert!(node(&update, chip).actions.contains(AccessAction::Remove));
        assert!(!node(&update, slider).actions.contains(AccessAction::Remove));
        let window = WindowId(1);
        let remove = perform(&mut store, window, chip, None, AccessAction::Remove, 1.0);
        let Some([InputSample::Key(press), InputSample::Key(release)]) = remove else {
            panic!("a key press and release");
        };
        assert_eq!(
            (press.key, press.pressed, release.pressed),
            (Key::Delete, true, false)
        );
        assert_eq!(store.focused(), Some(chip));
        store.set_check_mark(chip, Some(mark(false)));
        let update = bridge
            .update(&store, root, 1.0)
            .expect("its actions changed");
        assert!(!node(&update, chip).actions.contains(AccessAction::Remove));
    }

    #[test]
    fn first_update_is_the_whole_tree_in_physical_pixels() {
        let (store, root, [button, slider]) = scene();
        let mut bridge = AccessBridge::new();
        let update = bridge.update(&store, root, 2.0).expect("first update");
        assert_eq!(update.root, Some(semantics_id(root)));
        assert_eq!(update.nodes.len(), 3);
        assert_eq!(update.focus, semantics_id(root), "nothing focused yet");

        let window = node(&update, root);
        assert_eq!(window.role, SemanticsRole::Window);
        assert_eq!(
            window.children,
            [semantics_id(button), semantics_id(slider)]
        );

        let add = node(&update, button);
        assert_eq!(add.role, SemanticsRole::Button);
        assert_eq!(add.label.as_deref(), Some("Add"));
        assert!(add.actions.contains(AccessAction::Focus));
        assert!(!add.actions.contains(AccessAction::Increment));
        let bounds = add.bounds;
        assert_eq!(
            (bounds.x, bounds.width, bounds.height),
            (0.0, 80.0, 40.0),
            "physical pixels at scale 2"
        );

        let volume = node(&update, slider);
        assert_eq!(volume.role, SemanticsRole::Slider);
        assert_eq!(volume.state.numeric_value, Some(0.5));
        assert_eq!(volume.state.numeric_range, Some((0.0, 1.0)));
        assert!(volume.actions.contains(AccessAction::Increment));
        assert_eq!(bridge.target(semantics_id(slider)), Some((slider, None)));
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
        assert_eq!(update.root, None);
        assert_eq!(update.nodes.len(), 1);
        assert_eq!(node(&update, slider).state.numeric_value, Some(0.75));
    }

    #[test]
    fn focus_moves_and_hidden_subtrees_leave_the_tree() {
        let (mut store, root, [button, slider]) = scene();
        let mut bridge = AccessBridge::new();
        bridge.update(&store, root, 1.0).expect("first update");
        store.clear_dirty();

        viso_ui::focus_node(&mut store, slider);
        let update = bridge.update(&store, root, 1.0).expect("focus moved");
        assert_eq!(update.focus, semantics_id(slider));
        store.clear_dirty();

        store.set_hidden(button, true);
        let update = bridge.update(&store, root, 1.0).expect("child hidden");
        assert_eq!(node(&update, root).children, [semantics_id(slider)]);
        assert!(
            update.nodes.iter().all(|n| n.id != semantics_id(button)),
            "a removed node is not resent"
        );
    }

    #[test]
    fn hints_values_live_settings_and_levels_are_published_and_hidden_ones_are_not() {
        let (mut store, root, [button, slider]) = scene();
        store.set_semantics(
            button,
            Semantics::role(Role::Heading)
                .with_label("Totals")
                .with_hint("Opens the details")
                .with_value("Step 2")
                .with_live(LiveRegion::Assertive)
                .with_heading_level(2),
        );
        store.set_semantics(
            slider,
            Semantics::role(Role::Slider)
                .with_label("Volume")
                .with_hidden(true),
        );
        let mut bridge = AccessBridge::new();
        let update = bridge.update(&store, root, 1.0).expect("first update");
        let totals = node(&update, button);
        assert_eq!(totals.role, SemanticsRole::Heading);
        assert_eq!(totals.description.as_deref(), Some("Opens the details"));
        assert_eq!(totals.value.as_deref(), Some("Step 2"));
        assert_eq!(totals.live, SemanticsLive::Assertive);
        assert_eq!(totals.level, Some(2));
        assert_eq!(
            node(&update, root).children,
            [semantics_id(button)],
            "a node hidden from assistive technology is left out"
        );
        assert!(update.nodes.iter().all(|n| n.id != semantics_id(slider)));
    }

    #[test]
    fn live_state_and_unnamed_groups_map_to_the_delta_vocabulary() {
        let (mut store, root, [button, slider]) = scene();
        store.set_semantics(button, Semantics::role(Role::CheckBox).with_label("Agree"));
        store.set_semantic_state(
            button,
            SemanticState::checked(true)
                .with_expanded(false)
                .with_selected(true),
        );
        store.set_semantics(slider, Semantics::role(Role::Group));
        let mut bridge = AccessBridge::new();
        let update = bridge.update(&store, root, 1.0).expect("first update");
        let agree = node(&update, button);
        assert_eq!(agree.role, SemanticsRole::CheckBox);
        assert_eq!(
            agree.state,
            SemanticsState {
                toggled: Some(true),
                expanded: Some(false),
                selected: Some(true),
                ..SemanticsState::default()
            }
        );
        assert_eq!(
            node(&update, slider).role,
            SemanticsRole::Container,
            "an unnamed group is a plain container"
        );
        assert!(
            !node(&update, slider)
                .actions
                .contains(AccessAction::Increment),
            "only a slider steps"
        );
    }

    #[test]
    fn actions_lower_to_ordinary_input() {
        let (mut store, _, [button, slider]) = scene();
        let window = WindowId(0);
        let click =
            perform(&mut store, window, button, None, AccessAction::Click, 2.0).expect("click");
        let [InputSample::Pointer(down), InputSample::Pointer(up)] = click else {
            panic!("a click is a press and a release");
        };
        assert_eq!(
            (down.phase, up.phase),
            (PointerPhase::Down, PointerPhase::Up)
        );
        assert_eq!((down.x, down.y), (40.0, 20.0), "center, physical pixels");

        let step = perform(
            &mut store,
            window,
            slider,
            None,
            AccessAction::Increment,
            1.0,
        );
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

        assert!(perform(&mut store, window, button, None, AccessAction::Focus, 1.0).is_none());
        assert_eq!(store.focused(), Some(button));
    }
}
