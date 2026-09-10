//! The typed font request: the family / weight / width / style / features a
//! caller asks for, before any face is resolved.
//!
//! A request is intent, not identity. The resolver turns a request into a
//! concrete [`crate::FontFaceId`]; a request never carries a resolved face or a
//! loaded byte buffer.

/// A named font role the application can bind a family to (body, mono, and so
/// on) so callers request by role rather than repeating family strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FontRole {
    /// Default proportional body text.
    Body,
    /// Monospace / code.
    Mono,
    // TODO(TF-P0): remaining application roles per the manifest.
}

/// A resolved-independent request for a face: what the caller wants to render
/// with, prior to resolution and fallback.
#[derive(Debug, Clone)]
pub struct FontRequest {
    // TODO(TF-P0): role/family, weight, width, slant, and requested OpenType
    // features.
}
