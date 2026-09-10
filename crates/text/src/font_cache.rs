//! The reconstructable-face cache: keep hot `ttf-parser` / `rustybuzz` faces
//! alive keyed by [`crate::FontFaceId`], reconstructing them from owned sfnt
//! bytes on demand.
//!
//! Faces are cheap to rebuild from the owned bytes the [`crate::resolver`]
//! holds, so this is a bounded reconstruction cache, not the source of truth for
//! face bytes.

/// The reconstructable-face cache.
#[derive(Debug, Default)]
pub struct FontCache {
    // TODO(TF-P0): bounded map FontFaceId -> reconstructed parser/shaper face.
}

impl FontCache {
    /// Get or reconstruct the parser/shaper face for an id.
    pub fn get_or_build(&mut self) {
        todo!("TF-P0: reconstruct face from owned sfnt bytes, bounded cache")
    }
}
