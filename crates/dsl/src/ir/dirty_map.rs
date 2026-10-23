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
        // reader announces: a field's placeholder, masking and lines change
        // the text it draws and what it is announced as.
        "text" | "spans" | "content" | "label" | "placeholder" | "secure" | "multiline" => {
            D::MEASURE | D::LAYOUT | D::PAINT | D::SEMANTICS
        }

        // Pure paint properties: the pixels change, nothing about size or box.
        "color" | "background" | "background_color" | "foreground" | "fill" | "border_color"
        | "opacity" | "shadow" | "tint" | "shader" | "fit" | "camera" => D::PAINT,

        // An image's source moves its natural size with its pixels.
        "source" => D::MEASURE | D::LAYOUT | D::PAINT,

        // Dimensions and box-model spacing feed measurement and layout.
        "width" | "height" | "min_width" | "min_height" | "max_width" | "max_height" | "size"
        | "padding" | "margin" | "gap" | "spacing" | "inset" => D::MEASURE | D::LAYOUT,

        // Flex/grid arrangement changes the box layout but not intrinsic size.
        "align" | "justify" | "axis" | "direction" | "wrap" | "flex" | "weight" | "columns"
        | "rows" | "column_gap" | "row_gap" => D::LAYOUT,

        // A text's overflow ends a line it cuts off differently, which
        // places and paints the run again without resizing it.
        "overflow" => D::LAYOUT | D::PAINT,

        // Transform-only changes move the node's transform root: no measure,
        // layout, paint or hit-test work (section 8.7, ADR 0043).
        "transform" | "translate" | "scale" | "rotation" | "transform_origin" | "offset"
        | "scroll" => D::TRANSFORM,

        // Corner radius and border affect the painted shape only: a border
        // strokes inside the box (§U3.3).
        "radius" | "corner_radius" | "border" | "border_width" => D::PAINT,

        // Clipping cuts what the children paint and where they are hit to
        // the node's box, rounded by its corner radius.
        "clip" => D::PAINT | D::HIT_TEST,

        // A refresh starting or ending moves the indicator and the gap it
        // holds open, and repaints the spinner.
        "refreshing" => D::TRANSFORM | D::PAINT,
        "sticky" => D::TRANSFORM | D::PAINT | D::HIT_TEST,

        // The page a navigation stack shows enters layout, paint and the
        // semantics tree; how its pages move and the tag a node flies by are
        // read only when the stack next turns.
        "current" => D::MEASURE | D::LAYOUT | D::PAINT | D::SEMANTICS,
        "page_transition" | "hero" => D::EMPTY,
        // An item spanning a grid's line moves the items after it.
        "list.full_line" => D::LAYOUT | D::PAINT,

        // Visibility toggles participation in layout and paint and what is
        // exposed to accessibility.
        "visible" | "hidden" | "visibility" => D::LAYOUT | D::PAINT | D::SEMANTICS,

        // Whether a node takes focus is what it is announced as; a mounting
        // node's autofocus moves the focus, which re-reads semantics.
        "focusable" | "autofocus" => D::SEMANTICS,
        // The focus ring is drawn round the focused node.
        "focus_ring" => D::PAINT,
        // An access key is announced, and cued in the node's text.
        "access_key" => D::PAINT | D::SEMANTICS,
        "drag_axis" => D::HIT_TEST,
        // What a node carries and takes is read as a drag lifts and passes.
        "drag" | "drag_kind" | "drop_accepts" | "drop_operation" => D::EMPTY,
        // A viewer's limits keep its content's transform within them; the
        // scale a double tap zooms to is read as it lands.
        "min_scale" | "max_scale" | "pan_margin" => D::TRANSFORM,
        "double_tap" => D::EMPTY,
        // A swipe row reads which edges a full swipe runs on as one is let
        // go; a panel's edge places it past that side of the row.
        "full_swipe" => D::EMPTY,
        "edge" => D::LAYOUT | D::TRANSFORM,
        // A sheet springs to its detent; how it rests and whether a drag
        // dismisses it are read as a drag lets go.
        "detent" => D::TRANSFORM | D::PAINT,
        "detents" | "dismissible" => D::EMPTY,

        // Turning input off restyles the node through its `disabled`
        // selector and announces it disabled; what hit testing finds is
        // unchanged, since a disabled subtree still takes its pointer.
        "enabled" => D::STYLE | D::SEMANTICS,

        // What a field takes and the keyboard it asks for draw nothing; the
        // input method reads them with the field's other facts.
        "input" | "max_length" => D::SEMANTICS,

        // The cursor paints nothing; it is what the pointer over the node
        // shows, read with what the pointer hits.
        "cursor" => D::HIT_TEST,

        // Accessibility-only properties touch the semantics tree alone.
        "role" | "aria_label" | "aria_role" | "accessibility_label" | "description" | "hint" => {
            D::SEMANTICS
        }

        // Unknown property: the conservative structural-repaint default. Never
        // empty, so an unmapped property cannot silently invalidate nothing.
        _ => D::MEASURE | D::LAYOUT | D::PAINT,
    }
}
