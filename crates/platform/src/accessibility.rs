//! The accessibility bridge boundary.
//!
//! Outbound, the app pushes [`SemanticsDelta`]s through
//! [`PlatformApp::update_accessibility`](crate::PlatformApp::update_accessibility):
//! Viso's own vocabulary of typed node ids, roles and the facts that changed.
//! Each adapter lowers a delta to what its OS reads. The AccessKit adapters
//! (macOS, Windows, Linux, Android) convert it in one private module; the
//! adapters Viso builds itself (iOS, Web) fold it into their mirror directly.
//! Inbound, the backend reports what an assistive technology asked for as a
//! [`RawEvent::Accessibility`](crate::RawEvent::Accessibility) carrying an
//! [`AccessRequest`].

#[cfg(any(
    target_os = "macos",
    target_os = "windows",
    target_os = "android",
    all(
        any(
            target_os = "linux",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        ),
        any(feature = "x11", feature = "wayland")
    )
))]
pub(crate) mod accesskit_bridge;

/// A node's identity in the published tree, assigned by the producer and
/// stable for as long as the node lives. Ids name nodes across deltas: a delta
/// refers to a node it does not resend by its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct SemanticsId(pub u64);

/// What a node is to an assistive technology.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SemanticsRole {
    /// The root of a window's tree.
    Window,
    /// A plain layout container with no name, left out of what platforms
    /// announce.
    #[default]
    Container,
    /// A named structural grouping.
    Group,
    Button,
    CheckBox,
    Slider,
    /// One option of a radio group.
    Radio,
    /// Static text.
    Label,
    Image,
    /// An editable text field.
    TextField,
    /// An editable text field whose text is masked and never read out.
    SecureField,
    /// An editable number that steps up and down.
    SpinButton,
    /// A menu of commands popped up over the content.
    Menu,
    /// One command of a menu.
    MenuItem,
    Tab,
    TabList,
    Navigation,
    Dialog,
    /// A live status message.
    Status,
    Region,
    Tree,
    TreeItem,
    Heading,
    List,
    ListItem,
    /// A stretch of text that leads somewhere.
    Link,
    /// A menu command that turns something on or off.
    MenuItemCheckBox,
    /// A menu command, one of a set of which one is chosen.
    MenuItemRadio,
    MenuBar,
    /// A line between groups of items.
    Separator,
    /// A list of options to choose from.
    ListBox,
    /// One choice of a list box.
    ListOption,
    /// A field that opens a list box to fill it from.
    ComboBox,
    Tooltip,
    Table,
    Row,
    Cell,
    ColumnHeader,
    /// A text field that opens a list box of suggestions for it.
    EditableComboBox,
    /// How far work has come: its value within its range, or none while
    /// work of unknown length runs.
    ProgressIndicator,
    /// A text field that searches, perhaps with a list of suggestions.
    SearchField,
    /// The panel a tab shows.
    TabPanel,
    /// A table whose cells take the focus and can be selected.
    Grid,
    /// A cell of a grid.
    GridCell,
}

/// How an assistive technology announces a change to a node's content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SemanticsLive {
    #[default]
    Off,
    Polite,
    Assertive,
}

/// A node's box in the window, in physical pixels, top-left origin.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SemanticsRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// The live state a node announces besides its role and name. Every facet is
/// optional: a node carries only what its role defines.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SemanticsState {
    /// Checked or not: a checkbox, a toggle, a radio option.
    pub toggled: Option<bool>,
    /// A slider's current value.
    pub numeric_value: Option<f32>,
    /// A slider's inclusive `(min, max)` bounds.
    pub numeric_range: Option<(f32, f32)>,
    /// Whether a disclosure is expanded.
    pub expanded: Option<bool>,
    /// Whether a selectable item is part of the selection.
    pub selected: Option<bool>,
    /// Its 1-based place in its collection and the collection's size.
    pub position: Option<(u32, u32)>,
    /// Whether it is disabled: shown, but taking no input.
    pub disabled: bool,
    /// Whether it must be given a value.
    pub required: bool,
    /// Whether the value it holds is refused.
    pub invalid: bool,
    /// A table's or grid's rows and columns, counting those not shown.
    pub table_extent: Option<(u32, u32)>,
    /// A row's, cell's or column header's 0-based row and column in its
    /// table; a row reads only its row.
    pub table_place: Option<(u32, u32)>,
    /// How a column header's column is sorted, if it is.
    pub sort: Option<SemanticsSort>,
}

/// How a column is sorted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticsSort {
    Ascending,
    Descending,
}

/// The set of [`AccessAction`]s a node supports, one bit each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AccessActions(u8);

impl AccessActions {
    /// No action.
    pub const NONE: Self = Self(0);

    /// This set with `action` in it.
    pub const fn with(self, action: AccessAction) -> Self {
        Self(self.0 | action.bit())
    }

    /// Whether `action` is in the set.
    pub const fn contains(self, action: AccessAction) -> bool {
        self.0 & action.bit() != 0
    }

    /// Whether the set is empty.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The actions in the set, in [`AccessAction::ALL`] order.
    pub fn iter(self) -> impl Iterator<Item = AccessAction> {
        AccessAction::ALL
            .into_iter()
            .filter(move |&action| self.contains(action))
    }
}

/// One node as a delta carries it: everything an adapter announces about it.
/// A delta carries a node whole whenever any of its facts changed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AccessNode {
    pub id: SemanticsId,
    pub role: SemanticsRole,
    /// The accessible name.
    pub label: Option<String>,
    /// What acting on the node does, read after its name.
    pub description: Option<String>,
    /// Its value as text, where its state does not say it.
    pub value: Option<String>,
    pub live: SemanticsLive,
    /// A heading's level, 1 the topmost.
    pub level: Option<u8>,
    /// The letter it answers to with Alt held, lowercase.
    pub access_key: Option<char>,
    pub bounds: SemanticsRect,
    pub state: SemanticsState,
    pub actions: AccessActions,
    /// The node it shows as active while it keeps the focus itself: a combo
    /// box's highlighted option.
    pub active_descendant: Option<SemanticsId>,
    /// The node whose text says why its value is refused.
    pub error_message: Option<SemanticsId>,
    /// Its children in document order.
    pub children: Vec<SemanticsId>,
}

/// One publication of a window's semantics tree.
///
/// `nodes` holds the nodes the receiver does not have yet or whose facts
/// changed; a node no longer reachable from the root leaves the tree, so a
/// removal is the parent resent without the child. `root` is set when the
/// delta starts a tree (the first publication after activation, or a new
/// root), and `focus` is always the node with focus — the root when nothing
/// has it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SemanticsDelta {
    pub root: Option<SemanticsId>,
    pub nodes: Vec<AccessNode>,
    pub focus: SemanticsId,
}

/// What an assistive technology asked of a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessRequest {
    /// An assistive technology started listening: the app sends a full tree
    /// with the next frame and keeps it current until [`Self::Deactivated`].
    Activated,
    /// No assistive technology listens any more: the app stops publishing.
    Deactivated,
    /// Perform `action` on the node published as `target`.
    Action {
        target: SemanticsId,
        action: AccessAction,
    },
}

/// An action an assistive technology performs on a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessAction {
    Focus,
    Click,
    Increment,
    Decrement,
    /// Refresh a viewport's content, as pulling it down past its start
    /// does: offered as a custom action named "Refresh".
    Refresh,
    /// Remove the node from what holds it, as Delete on it does — a chip's
    /// cross: offered as a custom action named "Remove".
    Remove,
}

impl AccessAction {
    /// Every action, in declaration order.
    pub const ALL: [AccessAction; 6] = [
        AccessAction::Focus,
        AccessAction::Click,
        AccessAction::Increment,
        AccessAction::Decrement,
        AccessAction::Refresh,
        AccessAction::Remove,
    ];

    const fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// A request as two machine words, for backends that carry it through a
/// native message queue: a kind and the target id (zero when there is none).
#[cfg(any(target_os = "windows", test))]
impl AccessRequest {
    pub(crate) fn to_words(self) -> (usize, u64) {
        match self {
            Self::Activated => (0, 0),
            Self::Deactivated => (1, 0),
            Self::Action { target, action } => {
                let kind = match action {
                    AccessAction::Focus => 2,
                    AccessAction::Click => 3,
                    AccessAction::Increment => 4,
                    AccessAction::Decrement => 5,
                    AccessAction::Refresh => 6,
                    AccessAction::Remove => 7,
                };
                (kind, target.0)
            }
        }
    }

    pub(crate) fn from_words(kind: usize, target: u64) -> Option<Self> {
        let action = match kind {
            0 => return Some(Self::Activated),
            1 => return Some(Self::Deactivated),
            2 => AccessAction::Focus,
            3 => AccessAction::Click,
            4 => AccessAction::Increment,
            5 => AccessAction::Decrement,
            6 => AccessAction::Refresh,
            7 => AccessAction::Remove,
            _ => return None,
        };
        Some(Self::Action {
            target: SemanticsId(target),
            action,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_survive_the_two_word_packing() {
        let requests = [
            AccessRequest::Activated,
            AccessRequest::Deactivated,
            AccessRequest::Action {
                target: SemanticsId(u64::MAX),
                action: AccessAction::Focus,
            },
            AccessRequest::Action {
                target: SemanticsId(3 | 9 << 32),
                action: AccessAction::Decrement,
            },
            AccessRequest::Action {
                target: SemanticsId(5),
                action: AccessAction::Refresh,
            },
            AccessRequest::Action {
                target: SemanticsId(6),
                action: AccessAction::Remove,
            },
        ];
        for request in requests {
            let (kind, target) = request.to_words();
            assert_eq!(AccessRequest::from_words(kind, target), Some(request));
        }
        assert_eq!(AccessRequest::from_words(8, 0), None);
    }

    #[test]
    fn an_action_set_holds_each_action_once() {
        let set = AccessActions::NONE
            .with(AccessAction::Click)
            .with(AccessAction::Decrement)
            .with(AccessAction::Click);
        assert!(set.contains(AccessAction::Click) && set.contains(AccessAction::Decrement));
        assert!(!set.contains(AccessAction::Focus));
        assert_eq!(
            set.iter().collect::<Vec<_>>(),
            [AccessAction::Click, AccessAction::Decrement]
        );
        assert!(AccessActions::NONE.is_empty() && !set.is_empty());
    }
}
