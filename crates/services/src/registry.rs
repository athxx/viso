//! The set of services an app calls.

use std::rc::Rc;

use crate::files::FileDialogs;
use crate::haptics::Haptics;
use crate::notifications::Notifications;
use crate::permissions::Permissions;
use crate::secure_storage::SecureStorage;
use crate::share::Share;
use crate::system;
use crate::unsupported::Unsupported;

/// One implementation of each service protocol.
///
/// Services are called on the UI thread: several OS implementations hold
/// main-thread objects, so the registry is not `Send`. Cloning shares the
/// implementations. Tests replace any of them, or all through
/// [`Mock`](crate::Mock).
#[derive(Clone)]
pub struct Services {
    files: Rc<dyn FileDialogs>,
    share: Rc<dyn Share>,
    notifications: Rc<dyn Notifications>,
    permissions: Rc<dyn Permissions>,
    secure_storage: Rc<dyn SecureStorage>,
    haptics: Rc<dyn Haptics>,
}

impl Services {
    /// This OS's implementations. `app` identifies the app to the OS: it
    /// scopes secure storage and names the notification sender.
    pub fn system(app: &str) -> Self {
        system::services(app)
    }

    /// Every call answers [`ServiceError::Unsupported`](crate::ServiceError::Unsupported).
    pub fn unsupported() -> Self {
        let none = Rc::new(Unsupported);
        Self {
            files: none.clone(),
            share: none.clone(),
            notifications: none.clone(),
            permissions: none.clone(),
            secure_storage: none.clone(),
            haptics: none,
        }
    }

    pub fn files(&self) -> &dyn FileDialogs {
        &*self.files
    }

    pub fn share(&self) -> &dyn Share {
        &*self.share
    }

    pub fn notifications(&self) -> &dyn Notifications {
        &*self.notifications
    }

    pub fn permissions(&self) -> &dyn Permissions {
        &*self.permissions
    }

    pub fn secure_storage(&self) -> &dyn SecureStorage {
        &*self.secure_storage
    }

    pub fn haptics(&self) -> &dyn Haptics {
        &*self.haptics
    }

    pub fn with_files(mut self, files: impl FileDialogs + 'static) -> Self {
        self.files = Rc::new(files);
        self
    }

    pub fn with_share(mut self, share: impl Share + 'static) -> Self {
        self.share = Rc::new(share);
        self
    }

    pub fn with_notifications(mut self, notifications: impl Notifications + 'static) -> Self {
        self.notifications = Rc::new(notifications);
        self
    }

    pub fn with_permissions(mut self, permissions: impl Permissions + 'static) -> Self {
        self.permissions = Rc::new(permissions);
        self
    }

    pub fn with_secure_storage(mut self, secure_storage: impl SecureStorage + 'static) -> Self {
        self.secure_storage = Rc::new(secure_storage);
        self
    }

    pub fn with_haptics(mut self, haptics: impl Haptics + 'static) -> Self {
        self.haptics = Rc::new(haptics);
        self
    }

    /// Build a registry from shared implementations.
    pub(crate) fn from_parts(
        files: Rc<dyn FileDialogs>,
        share: Rc<dyn Share>,
        notifications: Rc<dyn Notifications>,
        permissions: Rc<dyn Permissions>,
        secure_storage: Rc<dyn SecureStorage>,
        haptics: Rc<dyn Haptics>,
    ) -> Self {
        Self {
            files,
            share,
            notifications,
            permissions,
            secure_storage,
            haptics,
        }
    }
}
