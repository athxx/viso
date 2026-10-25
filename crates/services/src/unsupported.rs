//! The implementation of every protocol for an OS without the capability.

use crate::haptics::{Haptic, Haptics};
use crate::launcher::Launcher;
use crate::reply::{Reply, ServiceError, ServiceResult};

/// Answers every call with [`ServiceError::Unsupported`].
pub(crate) struct Unsupported;

impl Launcher for Unsupported {
    fn open_url(&self, _: &str) -> Reply<()> {
        Reply::err(ServiceError::Unsupported)
    }
}

impl Haptics for Unsupported {
    fn play(&self, _: Haptic) -> ServiceResult<()> {
        Err(ServiceError::Unsupported)
    }
}
