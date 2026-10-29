//! `viso-services` — the OS services the UI itself calls.
//!
//! Haptic feedback a control plays, the app a link's URL opens in, and where
//! an app keeps what it carries between runs: each a small trait with one
//! implementation per OS and a recording [`Mock`] for headless tests, so app
//! code carries no `cfg(target_os)` branches. Everything else an app asks the
//! OS for — file dialogs, sharing, notifications, secure storage — is not UI
//! and lives in the extension crates (`viso-ext`). These are cold paths:
//! trait objects and allocation are fine here.
//!
//! Calls return at once; an answer that needs the OS arrives later through
//! a [`Reply`].

#![forbid(unsafe_op_in_unsafe_fn)]

mod app_data;
mod haptics;
mod launcher;
mod mock;
mod registry;
mod reply;
mod system;
mod unsupported;

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub use app_data::LocalStorage;
#[cfg(not(target_arch = "wasm32"))]
pub use app_data::{app_data_dir, documents_dir};
pub use haptics::{Haptic, Haptics};
pub use launcher::Launcher;
pub use mock::{Call, Mock};
pub use registry::Services;
pub use reply::{Completer, Reply, ServiceError, ServiceResult, reply};
