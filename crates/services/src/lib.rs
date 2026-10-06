//! `viso-services` — the app service protocols (§49, ADR 0031).
//!
//! Low-frequency OS capabilities an app calls through one registry,
//! [`Services`]: file dialogs, share, notifications, permissions, secure
//! storage, haptics. Each is a small trait with one implementation per OS
//! and a scripted [`Mock`] for headless tests, so app code carries no
//! `cfg(target_os)` branches. These are cold paths: trait objects and
//! allocation are fine here (§7.2).
//!
//! Calls return at once; an answer that needs the user or the OS arrives
//! later through a [`Reply`].
//!
//! [`app_data_dir`] names where an app keeps what it carries between runs.

#![forbid(unsafe_op_in_unsafe_fn)]

mod app_data;
mod files;
mod haptics;
mod mock;
mod notifications;
mod permissions;
mod registry;
mod reply;
mod secure_storage;
mod share;
mod system;
mod unsupported;

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub use app_data::LocalStorage;
#[cfg(not(target_arch = "wasm32"))]
pub use app_data::app_data_dir;
pub use files::{FileDialogs, FileFilter, OpenOptions, PickedFile, SaveOptions};
pub use haptics::{Haptic, Haptics};
pub use mock::{Call, Mock};
pub use notifications::{Notification, Notifications};
pub use permissions::{Permission, PermissionState, Permissions};
pub use registry::Services;
pub use reply::{Completer, Reply, ServiceError, ServiceResult, reply};
pub use secure_storage::SecureStorage;
pub use share::{Share, ShareItem};
