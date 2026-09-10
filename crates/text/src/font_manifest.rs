//! The application font manifest: the declared set of app-supplied families,
//! roles, and their sources, parsed once at startup.
//!
//! The manifest is the authority for which families the application ships and
//! how a [`crate::font_request::FontRole`] binds to a family. It never touches
//! platform system fonts — those arrive through the provider seam.

/// The parsed application font manifest.
#[derive(Debug, Default)]
pub struct FontManifest {
    // TODO(TF-P0): declared families, role bindings, and their source refs.
}

impl FontManifest {
    /// Parse a manifest from its declared form.
    pub fn parse() -> Self {
        todo!("TF-P0: parse the application font manifest")
    }
}
