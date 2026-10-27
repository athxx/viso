//! The facade half of the accessibility OS bridge: turns a window's node
//! model into the [`SemanticsDelta`]s the platform publishes, and an
//! assistive technology's actions back into ordinary input.
//!
//! A window holds an [`AccessBridge`] only while an assistive technology
//! listens on a backend with a bridge. The first delta carries the whole tree;
//! later ones carry only the nodes that differ from the last publication. The
//! bridge keeps what it last published and, on a frame that dirtied only
//! semantics, re-derives just the SEMANTICS-marked branches: a label change
//! re-derives the node and its ancestors, not the tree. A frame that moved
//! or restructured nodes re-derives the whole tree and diffs it. With no
//! assistive technology running the cost is the `Option` check in the frame
//! loop.

use std::collections::HashMap;

use viso_platform::{
    AccessAction, AccessActions, AccessNode, SemanticsDelta, SemanticsId, SemanticsLive,
    SemanticsRect, SemanticsRole, SemanticsScroll, SemanticsSort, SemanticsState, WindowId,
};
use viso_runtime::{
    InputSample, Key, KeySample, Modifiers, PointerId, PointerKind, PointerPhase, PointerSample,
};
use viso_ui::{
    Axis, DirtyClass, LiveRegion, NodeId, NodeStore, PointerButtons, Role, ScrollBehavior,
    ScrollRequest, SemanticState, SemanticsNode,
};

/// The dirty classes after which the whole tree is re-derived: they move
/// nodes (LAYOUT, TRANSFORM), or add and take them away (STRUCTURE), without
/// a SEMANTICS mark on each. A SEMANTICS mark alone re-derives the marked
/// branches; a frame with neither publishes at most a moved focus.
fn whole() -> DirtyClass {
    DirtyClass::LAYOUT | DirtyClass::TRANSFORM | DirtyClass::STRUCTURE
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
    /// The rows the last update derived.
    #[cfg(test)]
    derived: u32,
}

impl AccessBridge {
    pub(crate) fn new() -> Self {
        Self {
            published: HashMap::new(),
            targets: HashMap::new(),
            root: None,
            focus: None,
            #[cfg(test)]
            derived: 0,
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
        self.sync(store, root, scale, false)
    }

    /// [`update`](Self::update), re-deriving the whole tree when `whole`.
    fn sync(
        &mut self,
        store: &NodeStore,
        root: NodeId,
        scale: f32,
        whole_tree: bool,
    ) -> Option<SemanticsDelta> {
        let root_id = semantics_id(root);
        let new_root = self.root != Some(root_id);
        #[cfg(test)]
        {
            self.derived = 0;
        }
        if !store.arena().is_live(root) {
            return None;
        }
        let mut changed = Vec::new();
        if new_root || whole_tree || store.any_dirty_class(whole()) {
            let mut sync = Sync {
                store,
                root,
                scale,
                before: Some(std::mem::take(&mut self.published)),
                published: &mut self.published,
                targets: &mut self.targets,
                changed: &mut changed,
                derived: 0,
            };
            sync.targets.clear();
            sync.visit(root, store.inert(root), true, false);
            #[cfg(test)]
            {
                self.derived = sync.derived;
            }
        } else if store.any_dirty_class(DirtyClass::SEMANTICS) {
            let mut sync = Sync {
                store,
                root,
                scale,
                before: None,
                published: &mut self.published,
                targets: &mut self.targets,
                changed: &mut changed,
                derived: 0,
            };
            sync.visit(root, store.inert(root), false, false);
            #[cfg(test)]
            {
                self.derived = sync.derived;
            }
        }
        changed.sort_unstable_by_key(|node| node.id);
        let focus = self.focus_of(store, root_id);
        let focus_moved = self.focus != Some(focus);
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

    /// The published node holding the focus: the focused node, or the link
    /// of its text the focus is on; the root when that is not published.
    fn focus_of(&self, store: &NodeStore, root: SemanticsId) -> SemanticsId {
        store
            .focused()
            .and_then(|f| {
                let id = if store.links(f).is_empty() {
                    semantics_id(f)
                } else {
                    link_id(f, store.focused_link(f)?)
                };
                self.published.contains_key(&id).then_some(id)
            })
            .unwrap_or(root)
    }

    /// Every node last published.
    #[cfg(feature = "headless")]
    pub(crate) fn published(&self) -> impl Iterator<Item = &AccessNode> {
        self.published.values()
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
/// past its start makes it; `Remove` is Delete on the focused node. A
/// scroll needs none either: a page turns on the nearest viewport about
/// the node that can move that way, and `ScrollIntoView` reveals the node
/// once the next layout has placed it.
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
        AccessAction::ScrollUp
        | AccessAction::ScrollDown
        | AccessAction::ScrollLeft
        | AccessAction::ScrollRight => {
            let (axis, forward) = match action {
                AccessAction::ScrollUp => (Axis::Column, false),
                AccessAction::ScrollDown => (Axis::Column, true),
                AccessAction::ScrollLeft => (Axis::Row, false),
                _ => (Axis::Row, true),
            };
            store.scroll_page(node, axis, forward);
            return None;
        }
        AccessAction::ScrollIntoView => {
            store.request_scroll_with(node, ScrollRequest::Reveal, ScrollBehavior::Instant);
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
pub(crate) fn semantics_id(id: NodeId) -> SemanticsId {
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
        Role::Grid => SemanticsRole::Grid,
        Role::GridCell => SemanticsRole::GridCell,
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
        ..SemanticsState::default()
    }
}

/// One pass bringing the published nodes up to date.
struct Sync<'a> {
    store: &'a NodeStore,
    root: NodeId,
    scale: f32,
    /// What was published before, when the whole tree is re-derived into an
    /// empty `published`; `None` when `published` is updated in place.
    before: Option<HashMap<SemanticsId, AccessNode>>,
    published: &'a mut HashMap<SemanticsId, AccessNode>,
    targets: &'a mut HashMap<SemanticsId, Target>,
    changed: &'a mut Vec<AccessNode>,
    derived: u32,
}

impl Sync<'_> {
    /// Re-derive node `id` and publish it with the children the tree reads.
    /// A child is re-derived too when `all`, when it is SEMANTICS-marked,
    /// when it was not published, or when `id`'s `disabled`, which it
    /// inherits, changed; any other keeps its published subtree. `inert` is
    /// whether an ancestor is disabled, `scrolled` whether one is a viewport.
    fn visit(&mut self, id: NodeId, inert: bool, all: bool, scrolled: bool) {
        let store = self.store;
        let row = store.semantics_row(id, inert);
        self.derived += 1;
        let disabled = row.disabled;
        let own = semantics_id(id);
        let all = all || self.prior(own).map(|n| n.state.disabled) != Some(disabled);
        let mut children = Vec::new();
        let within = scrolled || store.scroll_axis(id).is_some();
        for c in store.read_children(id) {
            let child = semantics_id(c);
            if all
                || store.dirty(c).intersects(DirtyClass::SEMANTICS)
                || !self.published.contains_key(&child)
            {
                self.visit(c, disabled, all, within);
            }
            children.push(child);
        }
        for link in 0..store.links(id).len() as u16 {
            let row = store.semantics_link_row(id, link, disabled);
            self.derived += 1;
            let node = self.node(row, false, scrolled, Vec::new());
            children.push(node.id);
            self.publish(node, (id, Some(link)));
        }
        if self.before.is_none()
            && let Some(old) = self.published.get(&own)
            && old.children != children
        {
            let gone: Vec<SemanticsId> = old
                .children
                .iter()
                .filter(|c| !children.contains(c))
                .copied()
                .collect();
            for child in gone {
                self.unpublish(child);
            }
        }
        let node = self.node(row, id == self.root, scrolled, children);
        self.publish(node, (id, None));
    }

    /// What was last published as `id`.
    fn prior(&self, id: SemanticsId) -> Option<&AccessNode> {
        match &self.before {
            Some(before) => before.get(&id),
            None => self.published.get(&id),
        }
    }

    /// Publish `node`, noting it in the delta when it differs from what was.
    fn publish(&mut self, node: AccessNode, target: Target) {
        if self.prior(node.id) != Some(&node) {
            self.changed.push(node.clone());
        }
        self.targets.insert(node.id, target);
        self.published.insert(node.id, node);
    }

    /// Take `id` and its published subtree out of what is published: the
    /// tree no longer reads them.
    fn unpublish(&mut self, id: SemanticsId) {
        self.targets.remove(&id);
        if let Some(node) = self.published.remove(&id) {
            for child in node.children {
                self.unpublish(child);
            }
        }
    }

    /// The published node for `row`: the window when `root`; `scrolled`
    /// when a viewport is about it.
    fn node(
        &self,
        row: SemanticsNode,
        root: bool,
        scrolled: bool,
        children: Vec<SemanticsId>,
    ) -> AccessNode {
        let store = self.store;
        let label = row.label;
        let role = if root {
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
        if live
            && (matches!(row.role, Role::Slider | Role::SpinButton)
                || row.role == Role::Separator && store.focusable(row.id))
        {
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
        if live && scrolled {
            actions = actions.with(AccessAction::ScrollIntoView);
        }
        // A viewport says where it stands on each axis it scrolls along,
        // and turns a page each way it can still move.
        let mut scroll = [None, None];
        if row.link.is_none() && store.scroll_axis(row.id).is_some() {
            let pages = [
                (
                    Axis::Row,
                    AccessAction::ScrollLeft,
                    AccessAction::ScrollRight,
                ),
                (
                    Axis::Column,
                    AccessAction::ScrollUp,
                    AccessAction::ScrollDown,
                ),
            ];
            for (at, (axis, back, on)) in pages.into_iter().enumerate() {
                if !store.scrolls_on(row.id, axis) || store.scroll_range(row.id, axis) <= 0.0 {
                    continue;
                }
                let (least, most) = store.scroll_span(row.id, axis);
                let offset = store.scroll(row.id);
                let offset = if axis == Axis::Row {
                    offset.x
                } else {
                    offset.y
                };
                scroll[at] = Some(SemanticsScroll {
                    at: offset * self.scale,
                    least: least * self.scale,
                    most: most * self.scale,
                });
                if live && store.can_scroll(row.id, axis, false) {
                    actions = actions.with(back);
                }
                if live && store.can_scroll(row.id, axis, true) {
                    actions = actions.with(on);
                }
            }
        }
        // A refused value's reason is read with the node's description: the
        // error relation alone is not read by every reader.
        let validity = row.validity;
        let error = validity.error.filter(|_| validity.invalid);
        let reason = error.and_then(|e| store.semantics(e)?.label.as_deref());
        let description = match (reason, row.hint) {
            (Some(reason), Some(hint)) => Some(format!("{reason}. {hint}")),
            (Some(reason), None) => Some(reason.to_owned()),
            (None, hint) => hint,
        };
        let mut state = semantics_state(row.state, row.disabled);
        state.required = validity.required;
        state.invalid = validity.invalid;
        state.table_extent = row.table.extent;
        state.table_place = row.table.place;
        state.sort = row.table.sort.map(|sort| match sort {
            viso_ui::SortOrder::Ascending => SemanticsSort::Ascending,
            viso_ui::SortOrder::Descending => SemanticsSort::Descending,
        });
        [state.scroll_x, state.scroll_y] = scroll;
        AccessNode {
            id: match row.link {
                Some(link) => link_id(row.id, link),
                None => semantics_id(row.id),
            },
            role,
            label,
            description,
            value: row.value,
            live: semantics_live(row.live),
            level: row.heading_level,
            access_key: store.access_key(row.id).filter(|_| row.link.is_none()),
            bounds: SemanticsRect {
                x: rect.x * self.scale,
                y: rect.y * self.scale,
                width: rect.w * self.scale,
                height: rect.h * self.scale,
            },
            state,
            actions,
            active_descendant: row.active.map(semantics_id),
            error_message: error.map(semantics_id),
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
    use viso_ui::{Axis, BuildCx, FlexStyle, LeafStyle, Semantics, Sizing};

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
                    size: Sizing::fixed(100.0, 20.0),
                    ..Default::default()
                },
                |cx| {
                    for _ in 0..2 {
                        let leaf = cx.leaf(LeafStyle {
                            size: Sizing::fixed(40.0, 20.0),
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
                lane: viso_render::GlyphLane::CoverageA8,
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
    fn a_focusable_separator_steps_like_a_slider() {
        let (mut store, root, [rule, divider]) = scene();
        store.set_semantics(rule, Semantics::role(Role::Separator));
        store.set_focusable(rule, false);
        store.set_semantics(
            divider,
            Semantics::role(Role::Separator).with_label("Sidebar"),
        );
        let mut bridge = AccessBridge::new();
        let update = bridge.update(&store, root, 1.0).expect("first update");
        assert!(
            !node(&update, rule)
                .actions
                .contains(AccessAction::Increment)
        );
        let divider_node = node(&update, divider);
        assert!(divider_node.actions.contains(AccessAction::Increment));
        assert!(divider_node.actions.contains(AccessAction::Decrement));
        let step = perform(
            &mut store,
            WindowId(1),
            divider,
            None,
            AccessAction::Decrement,
            1.0,
        );
        let Some([InputSample::Key(press), _]) = step else {
            panic!("a key press and release");
        };
        assert_eq!(press.key, Key::Left);
    }

    #[test]
    fn a_refused_field_reads_its_reason_with_its_description() {
        let (mut store, root, [reason, field]) = scene();
        store.set_semantics(reason, Semantics::role(Role::Label).with_label("Too loud"));
        store.set_semantics(
            field,
            Semantics::role(Role::Slider)
                .with_label("Volume")
                .with_hint("Sets the level"),
        );
        let validity = viso_ui::Validity {
            required: true,
            invalid: true,
            error: Some(reason),
        };
        store.set_validity(field, validity);
        let mut bridge = AccessBridge::new();
        let update = bridge.update(&store, root, 1.0).expect("first update");
        let volume = node(&update, field);
        assert!(volume.state.required && volume.state.invalid);
        assert_eq!(volume.error_message, Some(semantics_id(reason)));
        assert_eq!(
            volume.description.as_deref(),
            Some("Too loud. Sets the level")
        );
        store.set_validity(
            field,
            viso_ui::Validity {
                invalid: false,
                ..validity
            },
        );
        let update = bridge
            .update(&store, root, 1.0)
            .expect("its validity changed");
        let volume = node(&update, field);
        assert!(volume.state.required && !volume.state.invalid);
        assert_eq!(volume.error_message, None);
        assert_eq!(volume.description.as_deref(), Some("Sets the level"));
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

    /// `groups` groups of `per` leaves under one root, laid out: a leaf of
    /// an even index is a named label, the others plain. Returns (store,
    /// root, groups, leaves by group).
    fn wide(groups: usize, per: usize) -> (NodeStore, NodeId, Vec<NodeId>, Vec<Vec<NodeId>>) {
        let mut store = NodeStore::new();
        let made = Rc::new(RefCell::new((Vec::new(), Vec::new())));
        let root = {
            let mut cx = BuildCx::new(&mut store);
            let sink = made.clone();
            cx.flex(FlexStyle::default(), |cx| {
                for _ in 0..groups {
                    let leaves = Rc::new(RefCell::new(Vec::new()));
                    let into = leaves.clone();
                    let group = cx.flex(
                        FlexStyle {
                            axis: Axis::Row,
                            ..Default::default()
                        },
                        |cx| {
                            for _ in 0..per {
                                let leaf = cx.leaf(LeafStyle {
                                    size: Sizing::fixed(2.0, 2.0),
                                    ..Default::default()
                                });
                                into.borrow_mut().push(leaf.id());
                            }
                        },
                    );
                    let mut made = sink.borrow_mut();
                    made.0.push(group.id());
                    made.1.push(leaves.take());
                }
            });
            cx.root().unwrap()
        };
        let (groups, leaves) = made.take();
        for (g, row) in leaves.iter().enumerate() {
            for (i, &leaf) in row.iter().enumerate() {
                if i % 2 == 0 {
                    let label = format!("{g}.{i}");
                    store.set_semantics(leaf, Semantics::role(Role::Label).with_label(label));
                }
            }
        }
        let mut scratch = Vec::new();
        store.layout(root, Rect::new(0.0, 0.0, 400.0, 400.0), &mut scratch);
        (store, root, groups, leaves)
    }

    /// Publish through `kept`, which re-derives what is marked, and through
    /// `whole`, which re-derives every node; both must say the same and keep
    /// the same tree. Ends the frame. Returns the delta and the rows `kept`
    /// derived.
    fn publish_both(
        store: &mut NodeStore,
        root: NodeId,
        kept: &mut AccessBridge,
        whole: &mut AccessBridge,
    ) -> (Option<SemanticsDelta>, u32) {
        let delta = kept.update(store, root, 1.0);
        assert_eq!(delta, whole.sync(store, root, 1.0, true));
        assert!(kept.published == whole.published, "the same tree published");
        assert_eq!(kept.targets, whole.targets);
        store.clear_dirty();
        (delta, kept.derived)
    }

    #[test]
    fn one_label_change_re_derives_its_branch_and_publishes_what_a_whole_re_derive_would() {
        let (mut store, root, _, leaves) = wide(100, 100);
        let (mut kept, mut whole) = (AccessBridge::new(), AccessBridge::new());
        let (first, derived) = publish_both(&mut store, root, &mut kept, &mut whole);
        assert_eq!(first.expect("the whole tree").nodes.len(), 10_101);
        assert_eq!(derived, 10_101);

        let leaf = leaves[42][8];
        store.set_semantics(leaf, Semantics::role(Role::Label).with_label("changed"));
        let (delta, derived) = publish_both(&mut store, root, &mut kept, &mut whole);
        assert_eq!(derived, 3, "the leaf, its group and the root");
        assert_eq!(whole.derived, 10_101);
        let delta = delta.expect("the label changed");
        assert_eq!(delta.nodes.len(), 1);
        assert_eq!(delta.nodes[0].label.as_deref(), Some("changed"));

        let (delta, derived) = publish_both(&mut store, root, &mut kept, &mut whole);
        assert_eq!((delta, derived), (None, 0), "a clean frame derives nothing");
    }

    #[test]
    fn every_kind_of_semantic_change_publishes_what_a_whole_re_derive_would() {
        let (mut store, root, groups, leaves) = wide(4, 6);
        let (mut kept, mut whole) = (AccessBridge::new(), AccessBridge::new());
        publish_both(&mut store, root, &mut kept, &mut whole);
        let all = 1 + 4 + 4 * 6;
        type Change<'a> = (&'a str, Box<dyn Fn(&mut NodeStore) + 'a>);
        let changes: Vec<Change> = vec![
            (
                "live state",
                Box::new(|s| s.set_semantic_state(leaves[0][0], SemanticState::checked(true))),
            ),
            ("disabled", Box::new(|s| s.set_enabled(groups[1], false))),
            ("enabled", Box::new(|s| s.set_enabled(groups[1], true))),
            (
                "hidden",
                Box::new(|s| {
                    let named = Semantics::role(Role::Label).with_label("2.2");
                    s.set_semantics(leaves[2][2], named.with_hidden(true));
                }),
            ),
            (
                "shown",
                Box::new(|s| {
                    s.set_semantics(leaves[2][2], Semantics::role(Role::Label).with_label("2.2"))
                }),
            ),
            (
                "focusable",
                Box::new(|s| s.set_focusable(leaves[3][1], true)),
            ),
            (
                "a handler",
                Box::new(|s| s.set_pointer_handler(leaves[3][3], Box::new(|_| {}))),
            ),
            (
                "handlers gone",
                Box::new(|s| s.clear_event_handlers(leaves[3][3])),
            ),
            (
                "an access key",
                Box::new(|s| s.set_access_key(leaves[0][2], Some('k'))),
            ),
            (
                "a refusal",
                Box::new(|s| {
                    s.set_validity(
                        leaves[1][1],
                        viso_ui::Validity {
                            required: true,
                            invalid: true,
                            error: Some(leaves[0][4]),
                        },
                    )
                }),
            ),
            (
                "its reason",
                Box::new(|s| {
                    s.set_semantics(leaves[0][4], Semantics::role(Role::Label).with_label("why"))
                }),
            ),
            ("the focus", Box::new(|s| s.set_focused(Some(leaves[3][1])))),
        ];
        for (what, change) in changes {
            change(&mut store);
            let (delta, derived) = publish_both(&mut store, root, &mut kept, &mut whole);
            assert!(delta.is_some(), "{what} is published");
            assert!(derived < all, "{what} re-derived {derived} of {all} rows");
        }
    }
}
