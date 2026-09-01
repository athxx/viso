//! Warm-tier paint style for a node (§8.4 warm, §16 paint-facing data).
//!
//! [`BoxStyle`] is the paint-time description a leaf (or a container with a
//! background) lowers to a [`viso_render::Quad`]. It is a plain value copied
//! into the node's warm side-storage; it carries no behavior and no heap data,
//! so the paint walk reads flat structs (§7.1).

use viso_render::{Border, Rgba};

/// The visual fill of a box: background color, corner radius, and border.
///
/// A `fill.a == 0` box paints nothing (a bare layout container). This mirrors
/// the renderer's [`viso_render::Quad`] fields so [`crate::paint::paint_tree`]
/// can lower a node with one field copy and no translation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxStyle {
    /// Fill color; transparent means "no background".
    pub fill: Rgba,
    /// Corner radius in pixels (0 = sharp).
    pub radius: f32,
    /// Border stroke.
    pub border: Border,
}

impl BoxStyle {
    /// A transparent, borderless, sharp-cornered box — a pure layout container
    /// that paints nothing itself.
    pub const NONE: BoxStyle = BoxStyle {
        fill: Rgba::TRANSPARENT,
        radius: 0.0,
        border: Border::NONE,
    };

    /// A solid fill with no border and sharp corners.
    pub const fn solid(fill: Rgba) -> Self {
        BoxStyle {
            fill,
            radius: 0.0,
            border: Border::NONE,
        }
    }

    /// This style with a corner radius applied.
    pub const fn with_radius(mut self, radius: f32) -> Self {
        self.radius = radius;
        self
    }

    /// Whether this box contributes any pixels (a visible fill or a border).
    #[inline]
    pub fn is_visible(&self) -> bool {
        self.fill.a > 0.0 || self.border.width > 0.0
    }
}

impl Default for BoxStyle {
    fn default() -> Self {
        BoxStyle::NONE
    }
}
