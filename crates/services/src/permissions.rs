//! Runtime permissions.

use crate::reply::Reply;

/// A capability the user grants at run time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Permission {
    Notifications,
}

/// Where a permission stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionState {
    Granted,
    Denied,
    /// Not decided yet: requesting it asks the user.
    Prompt,
}

/// Query and request runtime permissions. On an OS that never asks for a
/// permission, it is always granted.
pub trait Permissions {
    fn status(&self, permission: Permission) -> Reply<PermissionState>;

    /// Ask the user if the permission is undecided; answers the resulting
    /// state.
    fn request(&self, permission: Permission) -> Reply<PermissionState>;
}
