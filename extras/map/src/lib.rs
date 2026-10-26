//! A vector map view.
//!
//! The map is a pyramid of square tiles fetched from any [`TileSource`].
//! Each tile is decoded and tessellated once, off the loop thread, into a
//! retained drawing on one node; panning, zooming between whole levels and
//! rotating only move the transform root the tiles sit in, so a steady pan
//! repaints, uploads and allocates nothing.

pub mod build;
pub mod camera;
pub mod geo;
pub mod mvt;
pub mod source;
pub mod style;
pub mod tess;

pub use camera::{Camera, Insets, Limits, Motion, TILE_SIZE, View};
pub use geo::{LngLat, LngLatBounds, WorldPoint};
pub use source::{
    Completer, MemorySource, Reply, ServiceError, ServiceResult, TileId, TileReply, TileSource,
    reply,
};
