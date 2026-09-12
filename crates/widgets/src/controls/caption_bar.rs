//! The [`CaptionBar`] widget — a unified title-bar content strip.
//!
//! A `CaptionBar` carries the *content* of a window's title bar (a centered
//! title, and — in later slices — a leading tab/icon area and trailing window
//! buttons) as one widget that looks identical on every OS. The window *buttons*
//! diverge by platform convention, but that divergence is driven by a **data
//! contract**, never by `target_os` (AGENTS section 24):
//!
//! - [`WindowChrome`](viso_ui::WindowChrome) (the chrome mode, from the window
//!   config) decides whether self-drawn min/max/close buttons are *allowed* at all;
//! - the presence of a native traffic-light box width in
//!   [`ChromeContext::buttons_width`] (delivered by the platform on a later
//!   frame) decides whether they are *superseded* — `Some(w)` means the OS
//!   already draws its own buttons in a `w`-wide leading box (macOS traffic
//!   lights), so the caption reserves that width as a leading spacer and draws
//!   none of its own.
//!
//! Both signals reach the widget through [`BuildCx::chrome`], read once at build
//! time. This slice delivers the three-段 content layout and the native-button
//! leading reserve; self-drawn buttons (step 3) and the draggable-region
//! back-channel (step 4) build on top of it.
//!
//! The three sections are laid out with a single [`Axis::Row`] flex whose middle
//! child is [`Length::Fill`]: leading (`Fit`) / centered title (`Fill`) /
//! trailing (`Fit`). [`Justify`] has no `SpaceBetween`, so the middle `Fill`
//! child eating the slack is what pins the leading and trailing sections to the
//! two edges; the title centers itself within that middle child.

use viso_ui::{
    Align, Axis, BoxStyle, BuildCx, ChromeContext, Component, FlexStyle, Inset, Justify, LeafStyle,
    Length, Rgba, Role, Semantics, Size,
};

use crate::label;

/// The default caption-bar height in logical points — tall enough to clear the
/// macOS traffic lights and read as a standard title bar.
const HEIGHT: f32 = 38.0;

/// The default caption background: a neutral dark bar.
const BACKGROUND: Rgba = Rgba {
    r: 0.12,
    g: 0.12,
    b: 0.14,
    a: 1.0,
};

/// The default title color: near-white for contrast against the bar.
const TITLE: Rgba = Rgba {
    r: 0.92,
    g: 0.92,
    b: 0.95,
    a: 1.0,
};

/// The default horizontal padding at each end of the bar.
const PADDING: f32 = 8.0;

/// The visual and layout parameters of a [`CaptionBar`]: the bar height, its
/// background box, the end padding, and the title color. All fields are `Copy`.
///
/// `height` defaults to a standard title-bar height, `background` to a neutral
/// dark bar, `padding` to a small horizontal inset, and `title_color` to
/// near-white.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaptionBarStyle {
    /// The bar's fixed height in logical points.
    pub height: f32,
    /// The bar's background box (fill, radius, border).
    pub background: BoxStyle,
    /// Horizontal padding applied at each end of the bar.
    pub padding: f32,
    /// The title text color.
    pub title_color: Rgba,
}

impl Default for CaptionBarStyle {
    fn default() -> Self {
        CaptionBarStyle {
            height: HEIGHT,
            background: BoxStyle::solid(BACKGROUND),
            padding: PADDING,
            title_color: TITLE,
        }
    }
}

/// A unified window title-bar content strip.
///
/// Construct one with [`caption_bar`] and adjust it with the chainable setters.
/// It maps to a full-width [`Axis::Row`] flex (the bar) with three children:
///
/// - a **leading** `Fit` section — a native-button spacer (when the platform has
///   reported a traffic-light box, macOS) plus, in later slices, tabs/icons;
/// - a **centered title** — a [`Length::Fill`] section whose own
///   [`Justify::Center`] centers the reused [`crate::Label`];
/// - a **trailing** `Fit` section — empty in this slice; self-drawn min/max/close
///   buttons land here (step 3) when `chrome == SelfDrawn` and no native buttons
///   were reported.
///
/// Its accessible role is [`Role::Group`] — the bar is a non-interactive
/// container; the interactive window buttons carry their own `Button` semantics
/// when they exist.
///
/// Invalidation: the bar is static content built once. The native-button spacer
/// starts at width 0 when no traffic-light geometry has arrived yet and is grown
/// later by the facade through a targeted `LAYOUT | PAINT` size update (step 4),
/// never a rebuild.
pub struct CaptionBar {
    /// The window title, shown centered and used as the accessible name.
    title: String,
    style: CaptionBarStyle,
}

/// Construct a [`CaptionBar`] with the given title and default style. Chain
/// [`CaptionBar::height`], [`CaptionBar::background`], [`CaptionBar::padding`],
/// and [`CaptionBar::title_color`] to adjust it.
pub fn caption_bar(title: impl Into<String>) -> CaptionBar {
    CaptionBar {
        title: title.into(),
        style: CaptionBarStyle::default(),
    }
}

impl CaptionBar {
    /// Replace the whole [`CaptionBarStyle`].
    pub fn style(mut self, style: CaptionBarStyle) -> Self {
        self.style = style;
        self
    }

    /// Set the bar's fixed height in logical points.
    pub fn height(mut self, height: f32) -> Self {
        self.style.height = height;
        self
    }

    /// Set the bar's background box (defaults to a neutral dark bar).
    pub fn background(mut self, background: BoxStyle) -> Self {
        self.style.background = background;
        self
    }

    /// Set the horizontal padding applied at each end of the bar.
    pub fn padding(mut self, padding: f32) -> Self {
        self.style.padding = padding;
        self
    }

    /// Set the title text color (defaults to near-white).
    pub fn title_color(mut self, color: Rgba) -> Self {
        self.style.title_color = color;
        self
    }
}

impl Component for CaptionBar {
    fn build(&self, cx: &mut BuildCx<'_>) {
        // Read the per-window chrome facts once, at build time. Two forward-flowing
        // signals decide the leading reserve and (in later slices) whether to
        // self-draw window buttons — driven by the data contract, never `target_os`
        // (AGENTS section 24).
        let ChromeContext {
            chrome,
            buttons_width,
        } = cx.chrome();

        // `build` takes `&self`, so `Copy` style fields are read by value and the
        // title is cloned here (a cold, build-time cost, not a per-frame path).
        let bar_style = self.style.background;
        let title = self.title.clone();
        let title_color = self.style.title_color;
        let padding = self.style.padding;
        let height = self.style.height;

        // The bar: a full-width, fixed-height row centering its three sections on
        // the cross axis. Its own `justify` never fires — the middle `Fill` child
        // consumes the main slack — so the two `Fit` sections pin to the edges.
        let root = cx.flex(
            FlexStyle {
                axis: Axis::Row,
                gap: 0.0,
                padding: Inset {
                    left: padding,
                    right: padding,
                    top: 0.0,
                    bottom: 0.0,
                },
                align: Align::Center,
                justify: Justify::Start,
                size: Size {
                    width: Length::fill(),
                    height: Length::Fixed(height),
                },
                style: bar_style,
            },
            |cx| {
                // Leading section (`Fit`). When the platform has reported a native
                // traffic-light box (`Some(w)`, macOS), reserve that exact width as
                // a leading spacer so the caption content yields to the OS buttons
                // and never overlaps them; a self-drawn-chrome window with no native
                // box gets no spacer. The spacer is a zero-box leaf whose width the
                // facade can later grow in place (step 4) when the geometry arrives
                // after first build.
                cx.flex(
                    FlexStyle {
                        axis: Axis::Row,
                        gap: 0.0,
                        padding: Inset::default(),
                        align: Align::Center,
                        justify: Justify::Start,
                        size: Size {
                            width: Length::Fit,
                            height: Length::Fixed(height),
                        },
                        style: BoxStyle::NONE,
                    },
                    |cx| {
                        if let Some(w) = buttons_width {
                            cx.leaf(LeafStyle {
                                size: Size::fixed(w, 0.0),
                                style: BoxStyle::NONE,
                            });
                        }
                    },
                );

                // Centered title (`Fill`). The middle child eats the main-axis
                // slack, so it spans between the two `Fit` sections; its own
                // `Justify::Center` centers the reused `Label` within that span.
                cx.flex(
                    FlexStyle {
                        axis: Axis::Row,
                        gap: 0.0,
                        padding: Inset::default(),
                        align: Align::Center,
                        justify: Justify::Center,
                        size: Size {
                            width: Length::fill(),
                            height: Length::Fixed(height),
                        },
                        style: BoxStyle::NONE,
                    },
                    |cx| {
                        label(title.clone()).color(title_color).build(cx);
                    },
                );

                // Trailing section (`Fit`). Empty in this slice — self-drawn
                // min/max/close buttons land here in step 3 when `chrome` is
                // `SelfDrawn` and no native buttons were reported. Built now so the
                // three-段 structure is stable across slices; `chrome` is bound here
                // to document the step-3 gate without yet acting on it.
                let _ = chrome;
                cx.flex(
                    FlexStyle {
                        axis: Axis::Row,
                        gap: 0.0,
                        padding: Inset::default(),
                        align: Align::Center,
                        justify: Justify::End,
                        size: Size {
                            width: Length::Fit,
                            height: Length::Fixed(height),
                        },
                        style: BoxStyle::NONE,
                    },
                    |_cx| {},
                );
            },
        );

        // The bar is a non-interactive container: a `Group` with the title as its
        // accessible name. Interactive window buttons carry their own `Button`
        // semantics when they exist (step 3).
        cx.semantics(
            root,
            Semantics::role(Role::Group).with_label(self.title.clone()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_ui::{
        BindingTable, ChromeContext, NodeId, NodeStore, Rect, SemanticProjector, StateStore,
        TextEdits, VirtualLists, WindowChrome,
    };

    /// Build a caption bar against a fresh reactive cx seeded with `chrome`,
    /// returning the store and the bar's root so a test can inspect the tree.
    fn build_with(chrome: ChromeContext, bar: CaptionBar) -> (NodeStore, NodeId) {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        )
        .with_chrome(chrome);
        bar.build(&mut cx);
        let root = cx.root().expect("caption bar declares a root node");
        (store, root)
    }

    /// Collect a node's direct children by walking the arena sibling chain.
    fn children(store: &NodeStore, parent: NodeId) -> Vec<NodeId> {
        let arena = store.arena();
        let mut out = Vec::new();
        let mut child = arena.links(parent).and_then(|l| l.first_child);
        while let Some(c) = child {
            out.push(c);
            child = arena.links(c).and_then(|l| l.next_sibling);
        }
        out
    }

    /// The bar always builds three sections — leading / center / trailing — and
    /// carries a `Group` semantics node with the title as its accessible name.
    #[test]
    fn builds_three_sections_with_group_semantics() {
        let (store, root) = build_with(ChromeContext::default(), caption_bar("Untitled"));

        let sections = children(&store, root);
        assert_eq!(sections.len(), 3, "leading / center / trailing");

        let sem = store
            .semantics(root)
            .expect("caption bar authors semantics");
        assert_eq!(sem.role, Role::Group, "the bar is a non-interactive Group");
    }

    /// With no native traffic-light box reported, the leading section is empty —
    /// no spacer is reserved.
    #[test]
    fn no_native_buttons_reserves_no_leading_spacer() {
        let chrome = ChromeContext {
            chrome: WindowChrome::SelfDrawn,
            buttons_width: None,
        };
        let (store, root) = build_with(chrome, caption_bar("Untitled"));

        let sections = children(&store, root);
        let leading = sections[0];
        assert_eq!(
            children(&store, leading).len(),
            0,
            "no native box ⇒ no leading spacer"
        );
    }

    /// When the platform reports a native traffic-light box (macOS), the leading
    /// section reserves a single spacer whose laid-out width matches the reported
    /// box, so the caption content yields exactly that much room to the OS buttons.
    #[test]
    fn native_buttons_reserve_leading_spacer_of_button_width() {
        let chrome = ChromeContext {
            chrome: WindowChrome::Native,
            buttons_width: Some(78.0),
        };
        let (mut store, root) = build_with(chrome, caption_bar("Untitled"));

        let sections = children(&store, root);
        let leading = sections[0];
        let spacer = children(&store, leading);
        assert_eq!(spacer.len(), 1, "native box ⇒ one leading spacer");

        // Lay the bar out on a wide surface and read the spacer's resolved box:
        // its `Length::Fixed(78)` width must survive to the computed bounds.
        let mut scratch = Vec::new();
        store.layout(
            root,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 800.0,
                h: 38.0,
            },
            &mut scratch,
        );
        let box_ = store.bounds(spacer[0]);
        assert_eq!(box_.w, 78.0, "spacer laid out to the native button width");
    }
}
