//! The platform toolchains Viso installs and selects on this machine: the
//! Android SDK component sets `viso android use` installs, and the iOS
//! Simulator runtimes `viso ios use` has Xcode install. Machine state,
//! never a project's: it lives under `~/.viso/<platform>/` (`$VISO_HOME`).
//!
//! Every archive is pinned and verified before a byte of it is unpacked
//! ([`download`]), and unpacked with [`zip`] into a staging directory that
//! takes its place in one rename.

pub mod android;
pub mod download;
pub mod ios;
pub mod zip;
