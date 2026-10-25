//! The Linux and BSD services: the desktop portal opening URLs, over the
//! session bus (`desktop`). There are no haptics. Without `desktop` (an
//! embedded build, no session bus) opening a URL is unsupported.

#[cfg(feature = "desktop")]
mod open_uri;

use std::rc::Rc;

use crate::registry::Services;
use crate::unsupported::Unsupported;

#[cfg(not(feature = "desktop"))]
pub(crate) fn services(_app: &str) -> Services {
    Services::from_parts(Rc::new(Unsupported), Rc::new(Unsupported))
}

#[cfg(feature = "desktop")]
pub(crate) fn services(_app: &str) -> Services {
    Services::from_parts(Rc::new(Unsupported), Rc::new(open_uri::PortalLauncher))
}
