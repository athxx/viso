//! The Linux and BSD services, all over the session bus: the desktop
//! portal's file chooser, the freedesktop notification server, the Secret
//! Service. None of these is gated by a permission; there is no desktop
//! share sheet and no haptics.

mod bus;
mod desktop_notifications;
mod file_chooser;
mod secret_service;

use std::rc::Rc;

use super::Ungated;
use crate::registry::Services;
use crate::unsupported::Unsupported;

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
