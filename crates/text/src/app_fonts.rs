//! Application-supplied font loading: register the families the manifest
//! declares from their owned byte sources.
//!
//! This is the app-side counterpart to [`crate::system_fonts`]. It hands owned
//! sfnt bytes to the [`crate::resolver`]; it does not touch platform system
//! fonts.

/// Loader for application-declared fonts.
#[derive(Debug, Default)]
pub struct AppFonts {
    // TODO(TF-P0): manifest-declared faces staged for registration.
}

impl AppFonts {
    /// Load and register every face the manifest declares.
    pub fn load_declared(&mut self) {
        todo!("TF-P0: register manifest faces from owned bytes")
    }
}
