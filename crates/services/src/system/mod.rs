//! The OS implementations, one module per target.

#[cfg(target_os = "android")]
mod android;
#[cfg(target_vendor = "apple")]
mod apple;
#[cfg(target_os = "ios")]
mod ios;
#[cfg(target_vendor = "apple")]
mod keychain;
#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_vendor = "apple")]
mod user_notifications;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod web;
#[cfg(target_os = "windows")]
mod win32;

use crate::registry::Services;

#[cfg(target_os = "macos")]
pub(crate) fn services(app: &str) -> Services {
    macos::services(app)
}

#[cfg(target_os = "ios")]
pub(crate) fn services(app: &str) -> Services {
    ios::services(app)
}

#[cfg(target_os = "windows")]
pub(crate) fn services(app: &str) -> Services {
    win32::services(app)
}

#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
pub(crate) fn services(app: &str) -> Services {
    linux::services(app)
}

#[cfg(target_os = "android")]
pub(crate) fn services(app: &str) -> Services {
    android::services(app)
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub(crate) fn services(app: &str) -> Services {
    web::services(app)
}

#[cfg(not(any(
    all(target_arch = "wasm32", target_os = "unknown"),
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_os = "android",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
)))]
pub(crate) fn services(_app: &str) -> Services {
    Services::unsupported()
}

/// The directory an app keeps its data in; see [`crate::app_data_dir`].
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn app_data_dir(app: &str) -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let env = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if cfg!(target_os = "macos") {
        Some(env("HOME")?.join("Library/Application Support").join(app))
    } else if cfg!(target_os = "ios") {
        // The sandbox's home is the app's own container.
        Some(env("HOME")?.join("Library/Application Support"))
    } else if cfg!(target_os = "windows") {
        Some(env("LOCALAPPDATA").or_else(|| env("APPDATA"))?.join(app))
    } else if cfg!(target_os = "android") {
        android_files_dir()
    } else if cfg!(unix) {
        let state = env("XDG_STATE_HOME").or_else(|| Some(env("HOME")?.join(".local/state")))?;
        Some(state.join(app))
    } else {
        None
    }
}

#[cfg(target_os = "android")]
fn android_files_dir() -> Option<std::path::PathBuf> {
    android::files_dir()
}

#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
fn android_files_dir() -> Option<std::path::PathBuf> {
    None
}

/// Read a picked file in full.
#[cfg(any(
    target_vendor = "apple",
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
fn read_picked(path: std::path::PathBuf) -> crate::reply::ServiceResult<crate::files::PickedFile> {
    let contents =
        std::fs::read(&path).map_err(|e| crate::reply::ServiceError::Failed(e.to_string()))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(crate::files::PickedFile {
        name,
        path: Some(path),
        contents,
    })
}

/// Permissions for an OS that gates none of these capabilities.
#[cfg(any(
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
struct Ungated;

#[cfg(any(
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
impl crate::permissions::Permissions for Ungated {
    fn status(
        &self,
        _: crate::permissions::Permission,
    ) -> crate::reply::Reply<crate::permissions::PermissionState> {
        crate::reply::Reply::ready(Ok(crate::permissions::PermissionState::Granted))
    }

    fn request(
        &self,
        _: crate::permissions::Permission,
    ) -> crate::reply::Reply<crate::permissions::PermissionState> {
        crate::reply::Reply::ready(Ok(crate::permissions::PermissionState::Granted))
    }
}
