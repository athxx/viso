//! The services Viso's tools share: the `viso` command and Studio both run
//! them, and each renders what they report its own way ([`report`]).
//!
//! - [`build`]: how the app is compiled and packaged for a target.
//! - [`dev`]: the dev session of `viso run` — the watcher, the source graph,
//!   the connections to the running apps and their control endpoint.
//! - [`inspect`]: the client asking a running app what it shows.
//! - [`device`] and [`toolchain`]: the simulators and emulators, and the
//!   platform toolchains installed on this machine.
//! - [`delivery`] and [`deploy`]: what a package carries, and where a dev
//!   artifact runs.
//! - [`process`], [`atomic`], [`interrupt`], [`secret`]: running programs,
//!   writing files whole, the user's interrupt, and keeping secrets out of
//!   what is shown.
//! - [`error`]: the exit codes and the codes the services raise.

pub mod atomic;
pub mod build;
pub mod delivery;
pub mod deploy;
pub mod dev;
pub mod device;
pub mod error;
pub mod inspect;
pub mod interrupt;
pub mod process;
pub mod report;
pub mod secret;
pub mod toolchain;

pub use report::{DevIds, Reporter, Source};
