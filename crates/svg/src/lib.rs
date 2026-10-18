//! `viso-svg` — the SVG input lane (§13): bytes parsed once into a normalized
//! vector document, with no renderer, so tooling checks an SVG asset without
//! linking one. The renderer lowers the document to its primitives
//! (`viso_render::svg`).
//!
//! The crate owns the whole parse, dependency-free (std only):
//!
//! ```text
//! bytes ─► xml ─► css cascade ─► convert ─► tree::Tree
//! ```
//!
//! [`tree::Tree`] is fully normalized — shapes are paths, `<use>`/`<symbol>`/
//! `<switch>` are expanded, units and percentages are resolved, and paint
//! servers, clip paths, masks, markers and filters are in the referencing
//! element's user space. Not supported: `<text>`, external (non-`data:`)
//! resources, SVGZ.

#![forbid(unsafe_op_in_unsafe_fn)]

mod color;
mod convert;
mod css;
mod data_url;
mod filter;
mod geom;
mod marker;
mod paint;
mod path_data;
mod style;
mod syntax;
pub mod tree;
mod xml;

use std::fmt;

/// Why an SVG failed to parse or lower.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SvgError {
    /// The bytes are not a valid SVG document.
    Parse(String),
}

impl fmt::Display for SvgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SvgError::Parse(msg) => write!(f, "invalid SVG: {msg}"),
        }
    }
}

impl std::error::Error for SvgError {}

/// Parse SVG `bytes` into its normalized [`tree::Tree`].
///
/// # Errors
///
/// Bytes that are not a valid SVG document.
pub fn parse(bytes: &[u8]) -> Result<tree::Tree, SvgError> {
    tree::Tree::from_data(bytes).map_err(|e| SvgError::Parse(e.to_string()))
}
