//! A vector map view.
//!
//! The map is a pyramid of square tiles fetched from any [`TileSource`].
//! Each tile is decoded and tessellated once, off the loop thread, into a
//! retained drawing on one node; panning, zooming between whole levels and
//! rotating only move the transform root the tiles sit in, so a steady pan
//! repaints, uploads and allocates nothing.

pub mod source;

pub use source::{
    Completer, MemorySource, Reply, ServiceError, ServiceResult, TileId, TileReply, TileSource,
    reply,
};
