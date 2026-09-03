//! `viso-ende` — Viso-owned Encode/Decode infrastructure (DAG leaf).
//!
//! The low-level shared facility for turning typed values into bytes and back:
//!
//! - a compact binary wire format ([`Encoder`] / [`Decoder`]) with a **bounded**
//!   decoder — every read validates the remaining length, so decoding untrusted
//!   input never panics and never reads past the slice;
//! - a minimal JSON emitter for diagnostics and tool interchange;
//! - the canonical wire representation of core typed IDs (the *format* — a stable
//!   tag plus a fixed-width payload — not the identity type itself, which is
//!   owned elsewhere, so no edge back into those crates is needed).
//!
//! It is intentionally low-level and framework-agnostic: it depends on no other
//! viso crate and takes no third-party dependency. It does not implement RON, it
//! does not carry image/audio/video media codecs, and it is not the frame's
//! internal data model. Serde compatibility, when needed, lives in
//! `integrations/serde`, not here. It is not on the frame data-flow main chain.

#![forbid(unsafe_op_in_unsafe_fn)]

mod decode;
mod encode;
mod json;
mod wire;

pub use decode::{Decode, DecodeError, Decoder};
pub use encode::{Encode, Encoder};
pub use json::JsonWriter;
pub use wire::{IdKind, ProtocolTag, WIRE_VERSION, WireId};
