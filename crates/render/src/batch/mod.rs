//! Order-safe batch planning and render chunks.
//!
//! The renderer lowers a paint-ordered primitive stream into the fewest
//! contiguous draw commands that still replay the scene in submission order.
//! [`planner`] packs the GPU state a single draw fixes into an integer
//! [`BatchKey`] and decides, on integer equality plus a structural clip match,
//! whether an adjacent primitive joins the run before it (a packed key,
//! never a string); [`plan`] folds those runs into draws, moving a run onto an
//! earlier draw only past draws it does not overlap, so every pixel still
//! composites in paint order. [`chunk`] is the emitted unit:
//! one [`RenderChunk`] per draw, carrying its paint-order span so a local change
//! rebuilds only its chunk.

pub mod chunk;
pub mod planner;

pub use chunk::{RenderChunk, RenderChunkId};
pub use planner::{BatchFamily, BatchItem, BatchKey, BatchTarget, Draw, Run, joins, plan};
