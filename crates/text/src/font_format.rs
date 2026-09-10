//! Font container format detection and face enumeration: sfnt (TrueType /
//! OpenType), collections, and the packaged/compressed containers.
//!
//! Given owned bytes, this identifies the container and enumerates the faces it
//! holds so the resolver can register each as a [`crate::FontFaceId`]. It does
//! not shape or rasterize.

/// A detected font container format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontFormat {
    /// A single sfnt face (TrueType / OpenType).
    Sfnt,
    /// An sfnt collection holding multiple faces.
    Collection,
    // TODO(TF-P4): packaged / compressed container formats.
}

/// Detect the container format of owned font bytes.
pub fn detect(_bytes: &[u8]) -> Option<FontFormat> {
    todo!("TF-P0: detect sfnt / collection; TF-P4 packaged formats")
}
