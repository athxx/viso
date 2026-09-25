//! The system share sheet.

use crate::reply::Reply;

/// What to hand to another app.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ShareItem {
    Text(String),
    Url(String),
}

/// The system share sheet. The reply answers `Ok` once the system has taken
/// the item, or [`ServiceError::Cancelled`](crate::ServiceError::Cancelled)
/// where the OS reports the user backing out. Which target the user chose is
/// not reported.
pub trait Share {
    fn share(&self, item: ShareItem) -> Reply<()>;
}
