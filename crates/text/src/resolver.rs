//! Face resolution: turn a [`crate::font_request::FontRequest`] into a concrete
//! [`crate::FontFaceId`], registering faces from owned sfnt bytes on first use.
//!
//! The resolver owns the request -> face-id mapping and the registry of loaded
//! faces. String / family lookup happens here, at resolution time only; it
//! never appears on a steady-state shaping or paint path.

use crate::FontFaceId;

/// The face registry and request resolver.
#[derive(Debug, Default)]
pub struct FontResolver {
    // TODO(TF-P0): owned sfnt buffers keyed by FontFaceId, request cache.
}

impl FontResolver {
    /// Register a face from owned sfnt bytes, returning its stable id.
    pub fn register(&mut self, _sfnt: Vec<u8>) -> FontFaceId {
        todo!("TF-P0: register owned sfnt bytes, assign FontFaceId")
    }
}
