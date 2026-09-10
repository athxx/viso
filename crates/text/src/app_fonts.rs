//! Application-supplied font loading: register the families the manifest
//! declares from their owned byte sources.
//!
//! This is the app-side counterpart to [`crate::system_fonts`]. It hands owned
//! sfnt bytes to the [`crate::resolver`]; it does not touch platform system
//! fonts. Bytes come from an [`AssetSource`] seam so this crate stays free of
//! any file/packaging specifics — the facade supplies the actual asset store.

use crate::FontFaceId;
use crate::font_manifest::{AssetRef, FontManifest};
use crate::resolver::FontResolver;

/// The seam that yields packaged font bytes for an [`AssetRef`].
///
/// The real asset store lives outside this crate; a test can supply an
/// in-memory source. Reading is lazy and on demand, never eager at startup.
pub trait AssetSource {
    /// Read the bytes for an asset, if it exists.
    fn read(&self, asset: AssetRef) -> Option<Vec<u8>>;
}

/// Loader for application-declared fonts.
#[derive(Debug, Default)]
pub struct AppFonts;

impl AppFonts {
    /// Register a single declared face's bytes with the resolver, returning its
    /// id. Used to fill a face lazily on first use.
    pub fn load_face(
        resolver: &mut FontResolver,
        source: &dyn AssetSource,
        asset: AssetRef,
        face_index: u32,
    ) -> Option<FontFaceId> {
        let bytes = source.read(asset)?;
        Some(resolver.register_app_face(asset, face_index, bytes))
    }

    /// Eagerly register every face the manifest declares.
    ///
    /// This is an explicit opt-in for callers that want app faces resident up
    /// front; the default path is lazy [`load_face`] on first use, so ordinary
    /// startup does not parse every declared font.
    ///
    /// [`load_face`]: AppFonts::load_face
    pub fn load_declared(
        resolver: &mut FontResolver,
        source: &dyn AssetSource,
        manifest: &FontManifest,
    ) {
        for entry in manifest.entries() {
            if let Some(bytes) = source.read(entry.asset) {
                resolver.register_app_face(entry.asset, entry.face_index, bytes);
            }
        }
    }
}
