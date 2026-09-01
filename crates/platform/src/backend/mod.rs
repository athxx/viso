//! Backend selection.
//!
//! One always-present deterministic backend ([`headless`]) plus at most one
//! native backend, chosen at compile time by target. Nothing here does dynamic
//! `dyn` backend dispatch beyond the single `Box<dyn PlatformApp>` the runtime
//! already holds — the choice is resolved by `cfg`, not at runtime.

use crate::PlatformApp;
use crate::control::PlatformError;

pub mod headless;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(all(unix, not(target_os = "macos")))]
pub mod x11;

/// Build the native platform app for this target, if one is compiled.
pub fn create_native() -> Result<Box<dyn PlatformApp>, PlatformError> {
    #[cfg(target_os = "macos")]
    {
        macos::MacApp::new().map(|a| Box::new(a) as Box<dyn PlatformApp>)
    }
    #[cfg(target_os = "windows")]
    {
        windows::WinApp::new().map(|a| Box::new(a) as Box<dyn PlatformApp>)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        x11::X11App::new().map(|a| Box::new(a) as Box<dyn PlatformApp>)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", unix)))]
    {
        Err(PlatformError::NoBackend)
    }
}
