//! The implementation of every protocol for an OS without the capability.

use std::path::PathBuf;

use crate::files::{FileDialogs, OpenOptions, PickedFile, SaveOptions};
use crate::haptics::{Haptic, Haptics};
use crate::notifications::{Notification, Notifications};
use crate::permissions::{Permission, PermissionState, Permissions};
use crate::reply::{Reply, ServiceError, ServiceResult};
use crate::secure_storage::SecureStorage;
use crate::share::{Share, ShareItem};

/// Answers every call with [`ServiceError::Unsupported`].
pub(crate) struct Unsupported;

impl FileDialogs for Unsupported {
    fn open(&self, _: OpenOptions) -> Reply<Vec<PickedFile>> {
        Reply::err(ServiceError::Unsupported)
    }

    fn save(&self, _: SaveOptions, _: Vec<u8>) -> Reply<Option<PathBuf>> {
        Reply::err(ServiceError::Unsupported)
    }
}

impl Share for Unsupported {
    fn share(&self, _: ShareItem) -> Reply<()> {
        Reply::err(ServiceError::Unsupported)
    }
}

impl Notifications for Unsupported {
    fn notify(&self, _: Notification) -> Reply<()> {
        Reply::err(ServiceError::Unsupported)
    }

    fn withdraw(&self, _: &str) {}
}

impl Permissions for Unsupported {
    fn status(&self, _: Permission) -> Reply<PermissionState> {
        Reply::err(ServiceError::Unsupported)
    }

    fn request(&self, _: Permission) -> Reply<PermissionState> {
        Reply::err(ServiceError::Unsupported)
    }
}

impl SecureStorage for Unsupported {
    fn get(&self, _: &str) -> Reply<Option<Vec<u8>>> {
        Reply::err(ServiceError::Unsupported)
    }

    fn set(&self, _: &str, _: &[u8]) -> Reply<()> {
        Reply::err(ServiceError::Unsupported)
    }

    fn remove(&self, _: &str) -> Reply<()> {
        Reply::err(ServiceError::Unsupported)
    }
}

impl Haptics for Unsupported {
    fn play(&self, _: Haptic) -> ServiceResult<()> {
        Err(ServiceError::Unsupported)
    }
}
