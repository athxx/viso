//! The runtime side of the development session (`Viso_Hot_Reload.md` §9,
//! §55, §57): the dev channel's wire protocol and the rules a running app
//! checks a host's messages against, the typed patch the host plans, and the
//! engine that commits it to a live mount with no compiler present. Only a
//! dev artifact (`hot-reload`) compiles it.

pub mod commit;
mod patch;
pub mod wire;
