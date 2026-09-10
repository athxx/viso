//! The unified font provider: the single seam shaping and fallback query for a
//! face, dispatching between application fonts and the platform system-font
//! provider.
//!
//! Callers ask the provider for a face; the provider decides whether an
//! app-declared family or a resolved system face answers, and hides that split
//! from the shaping and fallback paths.

/// The unified provider over application fonts plus the platform system-font
/// seam.
#[derive(Debug, Default)]
pub struct FontProvider {
    // TODO(TF-P1): app font registry handle + system provider handle.
}

impl FontProvider {
    /// Provide a face for the request, from app fonts or the system provider.
    pub fn provide(&self) {
        todo!("TF-P1: dispatch app fonts vs system provider")
    }
}
