//! The Linux and BSD services, all over the session bus: the desktop
//! portal's file chooser, the freedesktop notification server, the Secret
//! Service (`desktop`). None of these is gated by a permission; there is no
//! desktop share sheet and no haptics. Without `desktop` (an embedded build,
//! no session bus) every one of them is unsupported.

#[cfg(feature = "desktop")]
mod bus;
#[cfg(feature = "desktop")]
mod desktop_notifications;
#[cfg(feature = "desktop")]
mod file_chooser;
#[cfg(feature = "desktop")]
mod secret_service;

use std::rc::Rc;

use super::Ungated;
use crate::registry::Services;
use crate::unsupported::Unsupported;

#[cfg(not(feature = "desktop"))]
pub(crate) fn services(_app: &str) -> Services {
    Services::from_parts(
        Rc::new(Unsupported),
        Rc::new(Unsupported),
        Rc::new(Unsupported),
        Rc::new(Ungated),
        Rc::new(Unsupported),
        Rc::new(Unsupported),
    )
}

#[cfg(feature = "desktop")]
pub(crate) fn services(app: &str) -> Services {
    Services::from_parts(
        Rc::new(file_chooser::PortalDialogs),
        Rc::new(Unsupported),
        Rc::new(desktop_notifications::DesktopNotifications::new(app)),
        Rc::new(Ungated),
        Rc::new(secret_service::SecretService::new(app)),
        Rc::new(Unsupported),
    )
}
