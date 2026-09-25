//! Local notifications.

use crate::reply::Reply;

/// A notification shown by the system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    /// Identifies the notification: posting another with the same id
    /// replaces it.
    pub id: String,
    pub title: String,
    pub body: String,
}

/// Local notifications. Posting needs
/// [`Permission::Notifications`](crate::Permission::Notifications) where the
/// OS asks for it; without it the reply is
/// [`ServiceError::Denied`](crate::ServiceError::Denied).
pub trait Notifications {
    /// Show `notification`; the reply answers once the system accepted it.
    fn notify(&self, notification: Notification) -> Reply<()>;

    /// Withdraw a shown notification.
    fn withdraw(&self, id: &str);
}
