//! Font fallback: choose a face that covers a run the primary face cannot,
//! walking the fallback chain (app fonts then platform system fonts) by script
//! and coverage.
//!
//! Fallback runs during shaping preparation, off the main thread. It uses
//! [`crate::coverage`] to test whether a face covers a codepoint run and the
//! [`crate::font_provider`] to obtain candidate faces.

/// The fallback chain resolver.
#[derive(Debug, Default)]
pub struct FontFallback {
    // TODO(TF-P1): ordered fallback chain + per-script preferences.
}

impl FontFallback {
    /// Choose a covering face for a run the primary face cannot render.
    pub fn resolve_run(&self) {
        todo!("TF-P1: script/coverage-driven fallback chain walk")
    }
}
