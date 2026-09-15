//! The persistent GPU data path (§9.1).
//!
//! A frame's instance data lives in long-lived device buffers that persist
//! across frames, not in buffers rebuilt from scratch every frame. The renderer
//! lowers the retained scene into dense per-family instance arrays in draw
//! order (unchanged between frames when nothing moves); the [`InstancePool`]
//! owns the device buffer backing each family and uploads only the slots whose
//! bytes actually changed since the previous frame.
//!
//! This is the difference between "hover dirties one quad" costing a single
//! small [`write_buffer`] and costing a full-buffer re-upload of every instance
//! in the scene. A pool keeps a CPU shadow of what currently sits in its device
//! buffer, diffs the freshly lowered array against it slot by slot, and writes
//! back only the changed runs — so an unchanged frame performs zero uploads and
//! a one-slot change performs one minimal upload (§9.1).
//!
//! [`write_buffer`]: viso_gpu::GpuBackend::write_buffer

mod coalescer;
mod instance_pool;

pub use coalescer::{GAP_THRESHOLD, Range, coalesce};
pub use instance_pool::InstancePool;
