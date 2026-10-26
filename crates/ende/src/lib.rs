//! `viso-ende` — Viso-owned Encode/Decode infrastructure (DAG leaf).
//!
//! The low-level shared facility for turning typed values into bytes and back:
//!
//! - a compact binary wire format ([`Encoder`] / [`Decoder`]) with a **bounded**
//!   decoder — every read validates the remaining length, so decoding untrusted
//!   input never panics and never reads past the slice;
//! - a minimal JSON emitter for diagnostics and tool interchange, and a
//!   bounded reader for a tool reading another tool's JSON;
//! - the canonical wire representation of core typed IDs (the *format* — a stable
//!   tag plus a fixed-width payload — not the identity type itself, which is
//!   owned elsewhere, so no edge back into those crates is needed);
//! - execute-in-place support: zero padding to an alignment boundary
//!   ([`Encoder::align`] / [`Decoder::align`]), a section table that splits a
//!   blob into [`SECTION_ALIGN`]-aligned sections a reader locates directly
//!   ([`SectionWriter`] / [`Sections`]), and [`Aligned`], which places a
//!   `static` blob at an aligned address. The decoder borrows every byte
//!   string and string from its input, so a blob in memory-mapped flash is
//!   read where it lies.
//!
//! It is intentionally low-level and framework-agnostic: it depends on no other
//! viso crate and takes no third-party dependency. It does not implement RON, it
//! does not carry image/audio/video media codecs, and it is not the frame's
//! internal data model. Serde compatibility, when needed, is an optional
//! feature of the extension crates outside this repository (`viso-ext`), not
//! here. It is not on the frame data-flow main chain.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]

extern crate alloc;
#[cfg(any(feature = "std", test))]
extern crate std;

mod aligned;
mod decode;
mod encode;
mod json;
mod json_value;
mod section;
mod wire;

pub use aligned::{Aligned, SECTION_ALIGN};
pub use decode::{Decode, DecodeError, Decoder};
pub use encode::{Encode, Encoder};
pub use json::JsonWriter;
pub use json_value::{JsonError, JsonValue};
pub use section::{Section, SectionWriter, Sections};
pub use wire::{IdKind, ProtocolTag, WIRE_VERSION, WireId, content_hash};
