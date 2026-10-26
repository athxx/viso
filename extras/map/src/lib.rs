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
pub mod geojson;
pub mod icons;
pub mod label;
pub mod load;
pub mod mvt;
pub mod overlay;
pub mod pyramid;
pub mod query;
pub mod raster;
pub mod source;
pub mod style;
pub mod tess;
pub mod view;

pub use camera::{Camera, Insets, Limits, Motion, TILE_SIZE, View};
pub use geo::{LngLat, LngLatBounds, WorldPoint};
pub use geojson::{GeoFeature, GeoJson, GeoJsonError, Geometry};
pub use icons::IconSet;
pub use overlay::{Circle, Polygon, Polyline, Puck, Shape, Stroke};
pub use query::QueriedFeature;
pub use raster::{Dem, RasterSource, Shading};
pub use source::{
    Completer, MemorySource, Reply, ServiceError, ServiceResult, TileId, TileReply, TileSource,
    reply,
};
pub use view::{
    CameraCells, Cluster, ClusterContent, MapHandle, MapHandleSlot, MapPoint, MapTiles, MapView,
    Marker, MarkerContent, MarkerHandler, PlaceHandler, ShapeId, ViewportHandler, map_view,
};
