//! The AccessKit side of the bridge: the one conversion from a
//! [`SemanticsDelta`] to the `accesskit::TreeUpdate` the macOS, Windows, Linux
//! and Android adapters apply, and from an AccessKit action request back to
//! an [`AccessRequest`].
//!
//! A delta and a tree update share their incremental model — changed nodes
//! whole, a tree root only when one starts, the focus every time — so the
//! conversion is per node and keeps no state.

use accesskit::{
    Action, ActionData, ActionRequest, CustomAction, Live, Node, NodeId, Rect, Role, TreeId,
    TreeInfo, TreeUpdate,
};

use super::{
    AccessAction, AccessNode, AccessRequest, SemanticsDelta, SemanticsId, SemanticsLive,
    SemanticsRole,
};

/// The tree update carrying `delta`.
pub(crate) fn tree_update(delta: SemanticsDelta) -> TreeUpdate {
    TreeUpdate {
        nodes: delta
            .nodes
            .into_iter()
            .map(|node| (node_id(node.id), node_of(node)))
            .collect(),
        tree: delta.root.map(|root| TreeInfo::new(node_id(root))),
        tree_id: TreeId::ROOT,
        focus: node_id(delta.focus),
    }
}

/// The request for an AccessKit action; `None` when the action is not routed.
pub(crate) fn request(request: &ActionRequest) -> Option<AccessRequest> {
    Some(AccessRequest::Action {
        target: SemanticsId(request.target_node.0),
        action: action_of(request.action, request.data.as_ref())?,
    })
}

fn node_id(id: SemanticsId) -> NodeId {
    NodeId(id.0)
}

/// An access key as a shortcut reads: `Alt+F`.
fn alt_shortcut(key: char) -> String {
    let mut out = String::from("Alt+");
    out.extend(key.to_uppercase());
    out
}

fn node_of(node: AccessNode) -> Node {
    let mut out = Node::new(role(node.role));
    if let Some(label) = node.label {
        out.set_label(label);
    }
    if let Some(description) = node.description {
        out.set_description(description);
    }
    // A secure field's text is never read out, whatever its producer says.
    if let Some(value) = node.value
        && node.role != SemanticsRole::SecureField
    {
        out.set_value(value);
    }
    match node.live {
        SemanticsLive::Off => {}
        SemanticsLive::Polite => out.set_live(Live::Polite),
        SemanticsLive::Assertive => out.set_live(Live::Assertive),
    }
    if let Some(level) = node.level {
        out.set_level(usize::from(level));
    }
    if let Some(key) = node.access_key {
        out.set_access_key(alt_shortcut(key));
    }
    let b = node.bounds;
    out.set_bounds(Rect {
        x0: f64::from(b.x),
        y0: f64::from(b.y),
        x1: f64::from(b.x + b.width),
        y1: f64::from(b.y + b.height),
    });
    let state = node.state;
    if let Some(toggled) = state.toggled {
        out.set_toggled(toggled.into());
    }
    if let Some(value) = state.numeric_value {
        out.set_numeric_value(f64::from(value));
    }
    if let Some((min, max)) = state.numeric_range {
        out.set_min_numeric_value(f64::from(min));
        out.set_max_numeric_value(f64::from(max));
    }
    if let Some(expanded) = state.expanded {
        out.set_expanded(expanded);
    }
    if let Some(selected) = state.selected {
        out.set_selected(selected);
    }
    if let Some((index, count)) = state.position {
        out.set_position_in_set(index as usize);
        out.set_size_of_set(count as usize);
    }
    if state.disabled {
        out.set_disabled();
    }
    if let Some(active) = node.active_descendant {
        out.set_active_descendant(node_id(active));
    }
    for action in node.actions.iter() {
        out.add_action(accesskit_action(action));
        if let Some((id, name)) = custom(action) {
            out.push_custom_action(CustomAction {
                id,
                description: name.into(),
            });
        }
    }
    out.set_children(node.children.into_iter().map(node_id).collect::<Vec<_>>());
    out
}

fn role(role: SemanticsRole) -> Role {
    match role {
        SemanticsRole::Window => Role::Window,
        SemanticsRole::Container => Role::GenericContainer,
        SemanticsRole::Group => Role::Group,
        SemanticsRole::Button => Role::Button,
        SemanticsRole::CheckBox => Role::CheckBox,
        SemanticsRole::Slider => Role::Slider,
        SemanticsRole::Radio => Role::RadioButton,
        SemanticsRole::Label => Role::Label,
        SemanticsRole::Image => Role::Image,
        SemanticsRole::TextField => Role::TextInput,
        SemanticsRole::SecureField => Role::PasswordInput,
        SemanticsRole::SpinButton => Role::SpinButton,
        SemanticsRole::Menu => Role::Menu,
        SemanticsRole::MenuItem => Role::MenuItem,
        SemanticsRole::Link => Role::Link,
        SemanticsRole::MenuItemCheckBox => Role::MenuItemCheckBox,
        SemanticsRole::MenuItemRadio => Role::MenuItemRadio,
        SemanticsRole::MenuBar => Role::MenuBar,
        SemanticsRole::Separator => Role::Splitter,
        SemanticsRole::ListBox => Role::ListBox,
        SemanticsRole::ListOption => Role::ListBoxOption,
        SemanticsRole::ComboBox => Role::ComboBox,
        SemanticsRole::Tooltip => Role::Tooltip,
        SemanticsRole::Table => Role::Table,
        SemanticsRole::Row => Role::Row,
        SemanticsRole::Cell => Role::Cell,
        SemanticsRole::ColumnHeader => Role::ColumnHeader,
        SemanticsRole::EditableComboBox => Role::EditableComboBox,
        SemanticsRole::ProgressIndicator => Role::ProgressIndicator,
        SemanticsRole::Tab => Role::Tab,
        SemanticsRole::TabList => Role::TabList,
        SemanticsRole::Navigation => Role::Navigation,
        SemanticsRole::Dialog => Role::Dialog,
        SemanticsRole::Status => Role::Status,
        SemanticsRole::Region => Role::Region,
        SemanticsRole::Tree => Role::Tree,
        SemanticsRole::TreeItem => Role::TreeItem,
        SemanticsRole::Heading => Role::Heading,
        SemanticsRole::List => Role::List,
        SemanticsRole::ListItem => Role::ListItem,
    }
}

/// The ids of the custom actions a refreshable viewport and a removable
/// node offer.
const REFRESH: i32 = 1;
const REMOVE: i32 = 2;

/// The id and name of the custom action `action` is offered as, `None` for
/// one the platform names itself.
fn custom(action: AccessAction) -> Option<(i32, &'static str)> {
    match action {
        AccessAction::Refresh => Some((REFRESH, "Refresh")),
        AccessAction::Remove => Some((REMOVE, "Remove")),
        _ => None,
    }
}

fn accesskit_action(action: AccessAction) -> Action {
    match action {
        AccessAction::Focus => Action::Focus,
        AccessAction::Click => Action::Click,
        AccessAction::Increment => Action::Increment,
        AccessAction::Decrement => Action::Decrement,
        AccessAction::Refresh | AccessAction::Remove => Action::CustomAction,
    }
}

/// The action `action` with `data` asks for.
fn action_of(action: Action, data: Option<&ActionData>) -> Option<AccessAction> {
    Some(match action {
        Action::Focus => AccessAction::Focus,
        Action::Click => AccessAction::Click,
        Action::Increment => AccessAction::Increment,
        Action::Decrement => AccessAction::Decrement,
        Action::CustomAction => match data {
            Some(&ActionData::CustomAction(REFRESH)) => AccessAction::Refresh,
            Some(&ActionData::CustomAction(REMOVE)) => AccessAction::Remove,
            _ => return None,
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accessibility::{AccessActions, SemanticsRect, SemanticsState};
    use accesskit::Toggled;

    fn slider() -> AccessNode {
        AccessNode {
            id: SemanticsId(2 | 1 << 32),
            role: SemanticsRole::Slider,
            label: Some("Volume".into()),
            description: Some("Sets the level".into()),
            value: Some("Loud".into()),
            live: SemanticsLive::Polite,
            level: None,
            access_key: None,
            bounds: SemanticsRect {
                x: 10.0,
                y: 4.0,
                width: 80.0,
                height: 20.0,
            },
            state: SemanticsState {
                numeric_value: Some(0.5),
                numeric_range: Some((0.0, 1.0)),
                ..SemanticsState::default()
            },
            actions: AccessActions::NONE
                .with(AccessAction::Focus)
                .with(AccessAction::Increment)
                .with(AccessAction::Decrement),
            active_descendant: None,
            children: Vec::new(),
        }
    }

    fn found(update: &TreeUpdate, id: u64) -> &Node {
        &update
            .nodes
            .iter()
            .find(|(n, _)| n.0 == id)
            .expect("node in update")
            .1
    }

    #[test]
    fn a_full_delta_becomes_a_tree_with_its_root_and_focus() {
        let root = AccessNode {
            id: SemanticsId(1),
            role: SemanticsRole::Window,
            children: vec![slider().id],
            ..AccessNode::default()
        };
        let update = tree_update(SemanticsDelta {
            root: Some(SemanticsId(1)),
            nodes: vec![root, slider()],
            focus: slider().id,
        });
        assert_eq!(update.tree, Some(TreeInfo::new(NodeId(1))));
        assert_eq!(update.tree_id, TreeId::ROOT);
        assert_eq!(update.focus, NodeId(2 | 1 << 32));
        let window = found(&update, 1);
        assert_eq!(window.role(), Role::Window);
        assert_eq!(window.children(), [NodeId(2 | 1 << 32)]);

        let volume = found(&update, 2 | 1 << 32);
        assert_eq!(volume.role(), Role::Slider);
        assert_eq!(volume.label(), Some("Volume"));
        assert_eq!(volume.description(), Some("Sets the level"));
        assert_eq!(volume.value(), Some("Loud"));
        assert_eq!(volume.live(), Some(Live::Polite));
        assert_eq!(volume.numeric_value(), Some(0.5));
        assert_eq!(volume.min_numeric_value(), Some(0.0));
        assert_eq!(volume.max_numeric_value(), Some(1.0));
        let bounds = volume.bounds().expect("bounds");
        assert_eq!(
            (bounds.x0, bounds.y0, bounds.x1, bounds.y1),
            (10.0, 4.0, 90.0, 24.0)
        );
        assert!(volume.supports_action(Action::Focus));
        assert!(volume.supports_action(Action::Increment));
        assert!(volume.supports_action(Action::Decrement));
        assert!(!volume.supports_action(Action::Click));
    }

    #[test]
    fn an_incremental_delta_names_no_root_and_resends_a_parent_without_its_removed_child() {
        let parent = AccessNode {
            id: SemanticsId(1),
            role: SemanticsRole::Window,
            children: vec![SemanticsId(3)],
            ..AccessNode::default()
        };
        let update = tree_update(SemanticsDelta {
            root: None,
            nodes: vec![parent],
            focus: SemanticsId(1),
        });
        assert_eq!(update.tree, None);
        assert_eq!(update.nodes.len(), 1);
        assert_eq!(found(&update, 1).children(), [NodeId(3)]);
    }

    #[test]
    fn roles_and_states_map_to_their_accesskit_counterparts() {
        let cases = [
            (SemanticsRole::Container, Role::GenericContainer),
            (SemanticsRole::Group, Role::Group),
            (SemanticsRole::Radio, Role::RadioButton),
            (SemanticsRole::TextField, Role::TextInput),
            (SemanticsRole::SecureField, Role::PasswordInput),
            (SemanticsRole::Heading, Role::Heading),
        ];
        for (ours, theirs) in cases {
            assert_eq!(role(ours), theirs);
        }
        let node = node_of(AccessNode {
            role: SemanticsRole::TreeItem,
            level: Some(2),
            access_key: Some('f'),
            live: SemanticsLive::Assertive,
            state: SemanticsState {
                toggled: Some(true),
                expanded: Some(false),
                selected: Some(true),
                disabled: true,
                ..SemanticsState::default()
            },
            actions: AccessActions::NONE.with(AccessAction::Click),
            ..AccessNode::default()
        });
        assert!(node.is_disabled());
        assert_eq!(node.toggled(), Some(Toggled::True));
        assert_eq!(node.is_expanded(), Some(false));
        assert_eq!(node.is_selected(), Some(true));
        assert_eq!(node.level(), Some(2));
        assert_eq!(node.access_key(), Some("Alt+F"));
        assert_eq!(node.live(), Some(Live::Assertive));
        assert!(node.supports_action(Action::Click));
        let quiet = node_of(AccessNode::default());
        assert_eq!(quiet.live(), None, "an off live setting is left unset");
        assert_eq!(quiet.toggled(), None);
        assert!(!quiet.is_disabled());
    }

    #[test]
    fn routed_actions_map_to_requests_and_others_are_dropped() {
        let ask = |action| ActionRequest {
            action,
            target_tree: TreeId::ROOT,
            target_node: NodeId(7),
            data: None,
        };
        assert_eq!(
            request(&ask(Action::Click)),
            Some(AccessRequest::Action {
                target: SemanticsId(7),
                action: AccessAction::Click
            })
        );
        for action in AccessAction::ALL {
            let data = custom(action).map(|(id, _)| ActionData::CustomAction(id));
            assert_eq!(
                action_of(accesskit_action(action), data.as_ref()),
                Some(action)
            );
        }
        assert_eq!(
            request(&ask(Action::CustomAction)),
            None,
            "no custom action named"
        );
        assert_eq!(request(&ask(Action::ScrollIntoView)), None);
    }

    #[test]
    fn a_combo_box_names_its_highlighted_option_as_its_active_descendant() {
        let node = node_of(AccessNode {
            id: SemanticsId(3),
            role: SemanticsRole::EditableComboBox,
            active_descendant: Some(SemanticsId(9)),
            ..AccessNode::default()
        });
        assert_eq!(node.role(), Role::EditableComboBox);
        assert_eq!(node.active_descendant(), Some(NodeId(9)));
    }
}
