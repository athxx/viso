//! Order-safe batch planning and render chunks (§9.6).
//!
//! The renderer lowers a paint-ordered primitive stream into the fewest
//! contiguous draw commands that still replay the scene in submission order.
//! [`planner`] packs the GPU state a single draw fixes into an integer
//! [`BatchKey`] and decides, on integer equality plus a structural clip match,
//! whether an adjacent primitive joins the draw before it (§16.2 — packed key,
//! never a string; §8.6 — paint order preserved). [`chunk`] is the emitted unit:
//! one [`RenderChunk`] per draw, carrying its paint-order span so a local change
//! rebuilds only its chunk.

pub mod chunk;
pub mod planner;

pub use chunk::{RenderChunk, RenderChunkId};
pub use planner::{BatchFamily, BatchItem, BatchKey, BatchTarget, joins};
