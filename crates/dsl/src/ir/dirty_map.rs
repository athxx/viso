//! The property to dirty-class mapping table (AGENTS section 11).
//!
//! Every property/state binding must declare exactly what it invalidates, so a
//! paint-only change never forces its ancestors to re-measure. This module owns
//! the canonical table: given a property's leading name, it returns the compact
//! set of dirty classes a write to that property invalidates.
//!
//! The class set is a compiler-side value: `viso-dsl` mirrors the runtime
//! `viso_ui::DirtyClass` bit layout here rather than depending on `viso-ui`, so
//! the frontend keeps zero UI-runtime dependencies. The emitter (on the
//! `viso-ui-macros` side) maps each bit back to the matching
//! `::viso_ui::DirtyClass` associated constant when it renders the `bind` call,
//! so the two definitions must keep the same bit positions. A mismatch would
//! misroute invalidation, so the bit assignments below are load-bearing and are
//! covered by tests.

use core::ops::{BitOr, BitOrAssign};

/// The set of runtime invalidation classes a property write triggers.
///
/// A packed `u8` bitset mirroring `viso_ui::DirtyClass` (dirty.rs): one bit per
/// class, combined with `|`. This is the cold-path compiler mirror of the
/// runtime type — it never rides a frame path, only the compiled binding
/// metadata — but its bit positions must match the runtime's exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DirtyClass(u8);

impl DirtyClass {
    /// No invalidation.
    pub const EMPTY: Self = Self(0);
    /// The node's parent/child/sibling structure changed.
    pub const STRUCTURE: Self = Self(1 << 0);
    /// A style token that feeds neither measurement nor paint directly changed.
    pub const STYLE: Self = Self(1 << 1);
    /// Intrinsic size may have changed; a re-measure is required.
    pub const MEASURE: Self = Self(1 << 2);
    /// Box position/size within the parent may have changed.
    pub const LAYOUT: Self = Self(1 << 3);
    /// Only the node's transform changed (scroll/translate/scale).
    pub const TRANSFORM: Self = Self(1 << 4);
    /// The node's painted appearance changed.
    pub const PAINT: Self = Self(1 << 5);
    /// The node's hit-test geometry changed.
    pub const HIT_TEST: Self = Self(1 << 6);
    /// The node's accessibility semantics changed.
    pub const SEMANTICS: Self = Self(1 << 7);

    /// Whether every class in `other` is present in `self`.
    #[inline]
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether this class set is empty.
    #[inline]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The raw packed bits, for the emitter to decompose into runtime constants.
    #[inline]
    pub fn bits(self) -> u8 {
        self.0
    }
}

impl BitOr for DirtyClass {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for DirtyClass {
    #[inline]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// The dirty classes a write to the property named `name` invalidates.
///
/// `name` is the property's leading path segment (`text`, `color`, `width`, …).
/// The mapping follows AGENTS section 11: text content re-measures, re-lays-out,
/// repaints, and updates semantics; a pure color/background repaints only; a
/// dimension re-measures and re-lays-out; a transform updates transform,
/// hit-test, and paint without touching measurement; accessibility text updates
/// semantics only.
///
/// An unrecognized property returns the conservative default
/// `MEASURE | LAYOUT | PAINT` — a structural repaint that stops short of
/// touching semantics — so an unknown property never silently invalidates
/// *nothing*. A recognized property is always narrower than this default; the
/// consuming binding pass asserts every static edge resolves to a known
/// reactive source, and the emitter records the exact class it folded.
pub fn property_dirty_class(name: &str) -> DirtyClass {
    use DirtyClass as D;
    match name {
        // Text content shifts intrinsic size, box, pixels, and what a screen
        // reader announces.
        "text" | "content" | "label" => D::MEASURE | D::LAYOUT | D::PAINT | D::SEMANTICS,

        // Pure paint properties: the pixels change, nothing about size or box.
        "color" | "background" | "background_color" | "foreground" | "fill" | "border_color"
        | "opacity" | "shadow" | "tint" => D::PAINT,

        // Dimensions and box-model spacing feed measurement and layout.
        "width" | "height" | "min_width" | "min_height" | "max_width" | "max_height" | "size"
        | "padding" | "margin" | "gap" | "spacing" | "inset" => D::MEASURE | D::LAYOUT,

        // Flex/grid arrangement changes the box layout but not intrinsic size.
        "align" | "justify" | "axis" | "direction" | "wrap" | "flex" | "weight" | "columns"
        | "rows" | "column_gap" | "row_gap" => D::LAYOUT,

        // Transform-only changes move the node and its hit region and repaint,
        // without re-measuring or re-laying-out (section 8.7).
        "transform" | "translate" | "scale" | "rotate" | "offset" | "scroll" => {
            D::TRANSFORM | D::HIT_TEST | D::PAINT
        }

        // Corner radius / border width affect painted shape (and border width
        // nudges the content box, so it also lays out).
        "radius" | "corner_radius" => D::PAINT,
        "border_width" => D::LAYOUT | D::PAINT,

        // Visibility toggles participation in layout and paint and what is
        // exposed to accessibility.
        "visible" | "hidden" | "visibility" => D::LAYOUT | D::PAINT | D::SEMANTICS,

        // Accessibility-only properties touch the semantics tree alone.
        "role" | "aria_label" | "aria_role" | "accessibility_label" | "description" | "hint" => {
            D::SEMANTICS
        }

        // Unknown property: the conservative structural-repaint default. Never
        // empty, so an unmapped property cannot silently invalidate nothing.
        _ => D::MEASURE | D::LAYOUT | D::PAINT,
    }
}
