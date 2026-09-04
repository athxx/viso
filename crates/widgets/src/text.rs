//! Text controls — the [`Label`] widget.
//!
//! `Label` is the base static-text control: a single leaf node that draws a run
//! of text and reports it to assistive technology. It is the widget-layer face
//! of the `viso-ui` text seam — it declares an unshaped [`TextRequest`] on its
//! node and leaves the shaping to the tier that owns a font stack (the facade,
//! which drains the requests after build and writes back the shaped
//! [`viso_ui::Content::Text`]). `viso-ui` never shapes text itself, so `Label`
//! stays a thin, allocation-cheap declaration.
//!
//! It maps to exactly one `viso-ui` node: a leaf with a transparent box (the
//! text is the content), a text request, and a [`Role::Label`] semantics node
//! whose accessible name is the visible text. Its size defaults to
//! [`Length::Fit`] on both axes, so the leaf measures to the shaped run's
//! natural size once the facade has shaped it.

use viso_ui::{
    BoxStyle, BuildCx, Component, LeafStyle, Length, Rgba, Role, Semantics, Size, TextRequest,
};

/// The default label font size in logical pixels.
const DEFAULT_FONT_SIZE: f32 = 14.0;

/// A near-black default text color (straight linear RGBA).
const DEFAULT_COLOR: Rgba = Rgba {
    r: 0.05,
    g: 0.05,
    b: 0.06,
    a: 1.0,
};

/// The visual and layout parameters of a [`Label`]: the font size and color of
/// the text, plus the leaf's own size request within its parent.
///
/// `size` defaults to [`Length::Fit`] on both axes, so a label shrinks to the
/// shaped run's natural size. Override it with a [`Length::Fixed`] or
/// [`Length::Fill`] axis to give the label a fixed or flexible box (text
/// wrapping and overflow within a constrained box are a later slice).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LabelStyle {
    /// Font size in logical pixels (scaled by DPI at shape time).
    pub font_size: f32,
    /// Straight linear RGBA text color (a = opacity).
    pub color: Rgba,
    /// The label's own size request within its parent. Defaults to `Fit` on
    /// both axes.
    pub size: Size,
}

impl Default for LabelStyle {
    fn default() -> Self {
        LabelStyle {
            font_size: DEFAULT_FONT_SIZE,
            color: DEFAULT_COLOR,
            size: Size {
                width: Length::Fit,
                height: Length::Fit,
            },
        }
    }
}

/// A static text label: one leaf node that draws a run of text and exposes it as
/// an accessible [`Role::Label`].
///
/// Construct one with [`label`] and adjust it with the chainable setters:
///
/// ```
/// use viso_widgets::label;
/// use viso_ui::{BuildCx, Component, NodeStore, Rgba};
///
/// let heading = label("Save")
///     .font_size(18.0)
///     .color(Rgba { r: 0.9, g: 0.9, b: 0.95, a: 1.0 });
///
/// let mut store = NodeStore::new();
/// let mut cx = BuildCx::new(&mut store);
/// heading.build(&mut cx);
/// ```
///
/// Invalidation: `Label` declares a text request that the shaping tier turns
/// into node content; changing the text is expressed by rebuilding the label
/// (reactive text binding is a later slice). Semantics are [`Role::Label`] with
/// the visible text as the accessible name.
pub struct Label {
    text: String,
    style: LabelStyle,
}

/// Construct a [`Label`] with the given text and default style. Chain
/// [`Label::font_size`], [`Label::color`], and [`Label::size`] to adjust it.
pub fn label(text: impl Into<String>) -> Label {
    Label {
        text: text.into(),
        style: LabelStyle::default(),
    }
}

impl Label {
    /// Set the font size in logical pixels.
    pub fn font_size(mut self, font_size: f32) -> Self {
        self.style.font_size = font_size;
        self
    }

    /// Set the text color (straight linear RGBA).
    pub fn color(mut self, color: Rgba) -> Self {
        self.style.color = color;
        self
    }

    /// Set the label's own size request within its parent (defaults to `Fit`).
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }
}

impl Component for Label {
    fn build(&self, cx: &mut BuildCx<'_>) {
        // One leaf: a transparent box (the text is the content), carrying the
        // text request the shaping tier drains, plus a Label semantics node so
        // the visible text is the accessible name. `build` takes `&self`, so the
        // text is cloned into the request and label — a cold, build-time cost,
        // not a per-frame hot path.
        let handle = cx.leaf(LeafStyle {
            size: self.style.size,
            style: BoxStyle::NONE,
        });
        cx.text_request(
            handle,
            TextRequest {
                text: self.text.clone(),
                font_size: self.style.font_size,
                color: self.style.color,
            },
        );
        cx.semantics(
            handle,
            Semantics::role(Role::Label).with_label(self.text.clone()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_ui::{BuildCx, NodeId, NodeStore};

    /// Count a node's direct children by walking the arena sibling chain.
    fn child_count(store: &NodeStore, parent: NodeId) -> usize {
        let arena = store.arena();
        let mut n = 0;
        let mut child = arena.links(parent).and_then(|l| l.first_child);
        while let Some(c) = child {
            n += 1;
            child = arena.links(c).and_then(|l| l.next_sibling);
        }
        n
    }

    /// A label builds a single leaf carrying the declared text request and a
    /// `Label` semantics node whose name is the visible text.
    #[test]
    fn label_builds_one_leaf_with_text_request_and_label_semantics() {
        let widget = label("Hello");

        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        widget.build(&mut cx);
        let root = cx.root().expect("label declares a root node");

        // A label is a single content leaf — no children.
        assert_eq!(child_count(&store, root), 0, "a label maps to one leaf");

        let request = store
            .text_request(root)
            .expect("label declares a text request on its leaf");
        assert_eq!(request.text, "Hello");
        assert_eq!(request.font_size, DEFAULT_FONT_SIZE);
        assert_eq!(request.color, DEFAULT_COLOR);

        let sem = store
            .semantics(root)
            .expect("label authors semantics on its leaf");
        assert_eq!(sem.role, Role::Label);
        assert_eq!(sem.label.as_deref(), Some("Hello"));
    }

    /// The chainable setters override font size, color, and size, and the
    /// default size is `Fit` on both axes.
    #[test]
    fn setters_override_style_default_size_is_fit() {
        let default_style = LabelStyle::default();
        assert_eq!(default_style.size.width, Length::Fit);
        assert_eq!(default_style.size.height, Length::Fit);

        let color = Rgba {
            r: 1.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        };
        let widget = label("X")
            .font_size(24.0)
            .color(color)
            .size(Size::fixed(120.0, 32.0));
        // The chained size lands in the style the leaf is built from.
        assert_eq!(widget.style.size, Size::fixed(120.0, 32.0));

        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        widget.build(&mut cx);
        let root = cx.root().expect("label declares a root node");

        let request = store.text_request(root).expect("text request present");
        assert_eq!(request.font_size, 24.0);
        assert_eq!(request.color, color);
    }

    /// A label is static, non-interactive text: it registers no pointer handler,
    /// so it derives a `Label` role, never the `Button` an interactive node
    /// would default to.
    #[test]
    fn label_is_non_interactive() {
        let widget = label("Static");

        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        widget.build(&mut cx);
        let root = cx.root().expect("label declares a root node");

        assert!(
            !store.has_handler(root),
            "a label attaches no pointer handler"
        );
        assert!(
            !store.has_key_handler(root),
            "a label attaches no key handler"
        );
        let sem = store.semantics(root).expect("semantics present");
        assert_eq!(sem.role, Role::Label, "a non-interactive label is a Label");
    }
}
