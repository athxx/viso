//! The accessibility bridge boundary (ADR 0030).
//!
//! Outbound, the app pushes [`accesskit::TreeUpdate`]s through
//! [`PlatformApp::update_accessibility`](crate::PlatformApp::update_accessibility).
//! Inbound, the backend reports what an assistive technology asked for as a
//! [`RawEvent::Accessibility`](crate::RawEvent::Accessibility) carrying an
//! [`AccessRequest`], so `RawEvent` stays free of AccessKit types.

pub use accesskit;

/// What an assistive technology asked of a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessRequest {
    /// An assistive technology started listening: the app sends a full tree
    /// with the next frame and keeps it current until [`Self::Deactivated`].
    Activated,
    /// No assistive technology listens any more: the app stops publishing.
    Deactivated,
    /// Perform `action` on the node published with id `target`.
    Action { target: u64, action: AccessAction },
}

/// An action an assistive technology performs on a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessAction {
    Focus,
    Click,
    Increment,
    Decrement,
}

impl AccessAction {
    /// The neutral action for an AccessKit one; `None` for actions the bridge
    /// does not route.
    pub fn from_accesskit(action: accesskit::Action) -> Option<Self> {
        Some(match action {
            accesskit::Action::Focus => Self::Focus,
            accesskit::Action::Click => Self::Click,
            accesskit::Action::Increment => Self::Increment,
            accesskit::Action::Decrement => Self::Decrement,
            _ => return None,
        })
    }
}

impl AccessRequest {
    /// The request for an AccessKit action; `None` when the action is not routed.
    pub fn from_accesskit(request: &accesskit::ActionRequest) -> Option<Self> {
        Some(Self::Action {
            target: request.target_node.0,
            action: AccessAction::from_accesskit(request.action)?,
        })
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
                };
                (kind, target)
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
            _ => return None,
        };
        Some(Self::Action { target, action })
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
                target: u64::MAX,
                action: AccessAction::Focus,
            },
            AccessRequest::Action {
                target: 3 | 9 << 32,
                action: AccessAction::Decrement,
            },
        ];
        for request in requests {
            let (kind, target) = request.to_words();
            assert_eq!(AccessRequest::from_words(kind, target), Some(request));
        }
        assert_eq!(AccessRequest::from_words(6, 0), None);
    }

    #[test]
    fn routed_accesskit_actions_map_to_neutral_requests() {
        let request = accesskit::ActionRequest {
            action: accesskit::Action::Click,
            target_tree: accesskit::TreeId::ROOT,
            target_node: accesskit::NodeId(7),
            data: None,
        };
        assert_eq!(
            AccessRequest::from_accesskit(&request),
            Some(AccessRequest::Action {
                target: 7,
                action: AccessAction::Click
            })
        );
    }

    #[test]
    fn unrouted_accesskit_actions_are_dropped() {
        let request = accesskit::ActionRequest {
            action: accesskit::Action::ScrollIntoView,
            target_tree: accesskit::TreeId::ROOT,
            target_node: accesskit::NodeId(7),
            data: None,
        };
        assert_eq!(AccessRequest::from_accesskit(&request), None);
    }
}
