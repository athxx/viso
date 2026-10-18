//! The property to dirty-class mapping table (AGENTS section 11).
//!
//! Every property/state binding must declare exactly what it invalidates, so a
//! paint-only change never forces its ancestors to re-measure. This module owns
//! the canonical table: given a property's leading name, it returns the compact
//! set of dirty classes a write to that property invalidates.
//!
//! The class set is `viso_schema::DirtyClass`, the one definition the
//! compiler and the runtime share; the emitter renders each bit as the matching
//! `::viso_schema::DirtyClass` constant in the `bind` call.

pub use viso_schema::DirtyClass;

/// The dirty classes a write to the property named `name` invalidates.
///
/// `name` is the property's leading path segment (`text`, `color`, `width`, …).
/// The mapping follows AGENTS section 11: text content re-measures, re-lays-out,
/// repaints, and updates semantics; a pure color/background repaints only; a
/// dimension re-measures and re-lays-out; a transform moves its transform root
/// and nothing else (ADR 0043); accessibility text updates semantics only.
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
        | "opacity" | "shadow" | "tint" | "shader" | "fit" => D::PAINT,

        // An image's source moves its natural size with its pixels.
        "source" => D::MEASURE | D::LAYOUT | D::PAINT,

        // Dimensions and box-model spacing feed measurement and layout.
        "width" | "height" | "min_width" | "min_height" | "max_width" | "max_height" | "size"
        | "padding" | "margin" | "gap" | "spacing" | "inset" => D::MEASURE | D::LAYOUT,

        // Flex/grid arrangement changes the box layout but not intrinsic size.
        "align" | "justify" | "axis" | "direction" | "wrap" | "flex" | "weight" | "columns"
        | "rows" | "column_gap" | "row_gap" => D::LAYOUT,

        // Transform-only changes move the node's transform root: no measure,
        // layout, paint or hit-test work (section 8.7, ADR 0043).
        "transform" | "translate" | "scale" | "rotate" | "offset" | "scroll" => D::TRANSFORM,

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
