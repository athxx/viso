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
//!   frame) decides whether they are *superseded* — `Some(_)` means the OS
//!   already draws its own buttons (macOS traffic lights), so the caption draws
//!   none of its own.
//!
//! Both signals reach the widget through [`BuildCx::chrome`], read once at build
//! time. A `SelfDrawn` window with no reported native box draws its own
//! min/max/close buttons in the trailing section, and the center title band is
//! registered as the caption's draggable region
//! ([`BuildCx::register_draggable`]) — the facade reads its post-layout world box
//! and hands it to the platform's window-drag back-channel, so a press on the
//! blank caption moves the window on every OS.
//!
//! The three sections are laid out with a single [`Axis::Row`] flex whose middle
//! child is [`Length::Fill`]: leading (`Fit`) / centered title (`Fill`) /
//! trailing (`Fit`). [`Justify`] has no `SpaceBetween`, so the middle `Fill`
//! child eating the slack is what pins the leading and trailing sections to the
//! two edges; the title centers itself within that middle child.
//!
//! The native traffic-light buttons are a platform overlay (a full-size content
//! view with a transparent titlebar): they float above the caption and take no
//! layout width, so the caption reserves **no** leading room for them. The
//! leading section stays empty on macOS, the title band spans the whole bar, and
//! `Justify::Center` places the title at the window's center. On Windows/Linux
//! the self-drawn buttons occupy the trailing section and the title centers in
//! the room that remains — the platform convention, matching the reference.

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::{
    Align, Axis, BoxStyle, BuildCx, ChromeContext, Component, EventCx, FlexStyle, Inset,
    InteractionStyle, Justify, Key, LeafStyle, Length, LineJoin, PathCmd, Point, PointerButtons,
    PointerPhase, Rgba, Role, Semantics, Size, StateValue, Stroke, Vec2, WindowChrome,
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

/// The width of a single self-drawn window button hit box, in logical points.
/// Matches the desktop convention of a wide, easy-to-hit target (makepad's
/// desktop button is 46×29); the glyph itself is small and centered within it.
const BUTTON_WIDTH: f32 = 46.0;

/// The half-extent of a window-button glyph from the button's center, in logical
/// points. The min line spans `2 * GLYPH` wide; the max box is `2 * GLYPH`
/// square; the close cross spans `2 * GLYPH` on each diagonal. Mirrors makepad's
/// `sz = 4.5` in `desktop_button.rs`.
const GLYPH: f32 = 4.5;

/// The stroke width of a window-button glyph.
const GLYPH_STROKE: f32 = 1.0;

/// The resting glyph color for the min/max buttons: a muted light gray that reads
/// against the dark bar without competing with the title.
const BUTTON_GLYPH: Rgba = Rgba {
    r: 0.75,
    g: 0.75,
    b: 0.80,
    a: 1.0,
};

/// The hover background for the min/max buttons: a subtle lightening of the bar.
const BUTTON_HOVER_BG: Rgba = Rgba {
    r: 0.24,
    g: 0.24,
    b: 0.27,
    a: 1.0,
};

/// The pressed background for the min/max buttons: a touch darker than hover.
const BUTTON_PRESSED_BG: Rgba = Rgba {
    r: 0.30,
    g: 0.30,
    b: 0.34,
    a: 1.0,
};

/// The hover background for the close button: the platform-conventional red so
/// the destructive action reads distinctly from minimize/maximize.
const CLOSE_HOVER_BG: Rgba = Rgba {
    r: 0.77,
    g: 0.16,
    b: 0.13,
    a: 1.0,
};

/// The pressed background for the close button: a darker red.
const CLOSE_PRESSED_BG: Rgba = Rgba {
    r: 0.60,
    g: 0.11,
    b: 0.09,
    a: 1.0,
};

/// A shared, mutable window-action callback, cloned into both the pointer and key
/// handlers of a self-drawn window button so a click and a keyboard activation
/// drive the same action. Pointer and key input never overlap, so the borrow is
/// uncontended. `None` means the action is unwired (an inert but focusable
/// button) — the facade wires `on_close` to the owning window's close path;
/// minimize/maximize have no runtime plumbing yet (see [`CaptionBar`]).
type WindowAction = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>)>>>>;

/// Which self-drawn window button this is — selects the glyph geometry, the hover
/// color, and the accessible name. Only built on `SelfDrawn` windows with no
/// native traffic-light box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowButtonKind {
    /// Minimize: a single horizontal line.
    Minimize,
    /// Maximize/restore: a square outline.
    Maximize,
    /// Close: two crossing diagonals.
    Close,
}

impl WindowButtonKind {
    /// The accessible name for this button.
    fn label(self) -> &'static str {
        match self {
            WindowButtonKind::Minimize => "Minimize",
            WindowButtonKind::Maximize => "Maximize",
            WindowButtonKind::Close => "Close",
        }
    }

    /// The glyph geometry, in a `2 * GLYPH`-square local space centered on the
    /// button's own center — stroked, not filled. Mirrors the three makepad
    /// `desktop_button.rs` SDF paths (line / rect / cross) as `PathCmd` strokes
    /// (divergence: vector paths, not per-button SDF shaders).
    fn glyph(self) -> Vec<PathCmd> {
        // Local space spans [0, 2*GLYPH] on each axis; `c` is its center, so the
        // geometry matches makepad's center-relative `c.x ± sz` / `c.y ± sz`.
        let c = GLYPH;
        match self {
            WindowButtonKind::Minimize => vec![
                PathCmd::MoveTo(Point::new(c - GLYPH, c)),
                PathCmd::LineTo(Point::new(c + GLYPH, c)),
            ],
            WindowButtonKind::Maximize => vec![
                PathCmd::MoveTo(Point::new(c - GLYPH, c - GLYPH)),
                PathCmd::LineTo(Point::new(c + GLYPH, c - GLYPH)),
                PathCmd::LineTo(Point::new(c + GLYPH, c + GLYPH)),
                PathCmd::LineTo(Point::new(c - GLYPH, c + GLYPH)),
                PathCmd::Close,
            ],
            WindowButtonKind::Close => vec![
                PathCmd::MoveTo(Point::new(c - GLYPH, c - GLYPH)),
                PathCmd::LineTo(Point::new(c + GLYPH, c + GLYPH)),
                PathCmd::MoveTo(Point::new(c - GLYPH, c + GLYPH)),
                PathCmd::LineTo(Point::new(c + GLYPH, c - GLYPH)),
            ],
        }
    }

    /// The hover background box for this button — red for `Close`, a neutral
    /// lightening for minimize/maximize.
    fn hover_bg(self) -> Rgba {
        match self {
            WindowButtonKind::Close => CLOSE_HOVER_BG,
            _ => BUTTON_HOVER_BG,
        }
    }

    /// The pressed background box for this button.
    fn pressed_bg(self) -> Rgba {
        match self {
            WindowButtonKind::Close => CLOSE_PRESSED_BG,
            _ => BUTTON_PRESSED_BG,
        }
    }
}

/// Drive a window-action callback if one is wired; an unwired button is a no-op.
fn fire(cb: &WindowAction, ev: &mut EventCx<'_>) {
    if let Some(f) = cb.borrow_mut().as_mut() {
        f(ev);
    }
}

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
    /// The minimize-button action. `None` until [`CaptionBar::on_minimize`] is
    /// called; unwired by default because no runtime minimize plumbing exists yet
    /// (the button still builds and is focusable, but is inert).
    on_minimize: WindowAction,
    /// The maximize/restore-button action. `None` until
    /// [`CaptionBar::on_maximize`] is called; unwired by default for the same
    /// reason as [`CaptionBar::on_minimize`].
    on_maximize: WindowAction,
    /// The close-button action. `None` until [`CaptionBar::on_close`] is called;
    /// the facade wires it to the owning window's close path (the widget tier has
    /// no window id, so the action is injected by whoever holds the window
    /// handle).
    on_close: WindowAction,
}

/// Construct a [`CaptionBar`] with the given title and default style. Chain
/// [`CaptionBar::height`], [`CaptionBar::background`], [`CaptionBar::padding`],
/// and [`CaptionBar::title_color`] to adjust it.
pub fn caption_bar(title: impl Into<String>) -> CaptionBar {
    CaptionBar {
        title: title.into(),
        style: CaptionBarStyle::default(),
        on_minimize: Rc::new(RefCell::new(None)),
        on_maximize: Rc::new(RefCell::new(None)),
        on_close: Rc::new(RefCell::new(None)),
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

    /// Set the minimize-button action, fired on a click or keyboard activation of
    /// the self-drawn minimize button. No-op on windows that don't self-draw
    /// buttons (macOS, where the native traffic lights handle it).
    pub fn on_minimize(self, handler: impl FnMut(&mut EventCx<'_>) + 'static) -> Self {
        *self.on_minimize.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Set the maximize/restore-button action. Same wiring rules as
    /// [`CaptionBar::on_minimize`].
    pub fn on_maximize(self, handler: impl FnMut(&mut EventCx<'_>) + 'static) -> Self {
        *self.on_maximize.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Set the close-button action, fired on a click or keyboard activation of the
    /// self-drawn close button. The facade wires this to the owning window's close
    /// path (the widget tier has no window id of its own).
    pub fn on_close(self, handler: impl FnMut(&mut EventCx<'_>) + 'static) -> Self {
        *self.on_close.borrow_mut() = Some(Box::new(handler));
        self
    }
}

/// Author one self-drawn window button on `cx`: a focusable, clickable box the
/// width of a desktop button, carrying a stroked [`PathCmd`] glyph and wired to
/// `action`. Mirrors the [`crate::Button`] interaction skeleton (two reactive
/// cells → interaction-style column → focusable → pointer + key handlers →
/// `Button` semantics), but paints a vector glyph instead of a text caption and
/// fires a window action instead of a generic click.
fn window_button(cx: &mut BuildCx<'_>, kind: WindowButtonKind, height: f32, action: WindowAction) {
    // Pressed/hover cells drive the interaction-style column: flipping either
    // re-selects the painted box and repaints this button alone (targeted
    // invalidation, no rebuild), priority pressed > hover > resting.
    let pressed = cx.state(StateValue::Bool(false));
    let hovered = cx.state(StateValue::Bool(false));

    // The button box: transparent at rest (the bar shows through), tinted on
    // hover/press. A row centering the glyph on both axes.
    let resting = BoxStyle::NONE;
    let hover = BoxStyle::solid(kind.hover_bg());
    let pressed_box = BoxStyle::solid(kind.pressed_bg());
    let glyph = kind.glyph();
    let root = cx.flex(
        FlexStyle {
            axis: Axis::Row,
            gap: 0.0,
            padding: Inset::default(),
            align: Align::Center,
            justify: Justify::Center,
            size: Size {
                width: Length::Fixed(BUTTON_WIDTH),
                height: Length::Fixed(height),
            },
            style: resting,
        },
        |cx| {
            // The glyph: a stroked path leaf in its own `2 * GLYPH`-square box,
            // centered by the parent row. Authored directly (no `icon()` component
            // detour) so it lands as this button's single content child.
            let extent = 2.0 * GLYPH;
            let leaf = cx.leaf(LeafStyle {
                size: Size::fixed(extent, extent),
                style: BoxStyle::NONE,
            });
            cx.path(
                leaf,
                glyph.clone(),
                None,
                Some(Stroke {
                    width: GLYPH_STROKE,
                    color: BUTTON_GLYPH,
                    join: LineJoin::Miter,
                }),
                Vec2 {
                    x: extent,
                    y: extent,
                },
            );
        },
    );

    cx.interaction_style(
        root,
        InteractionStyle {
            resting,
            hover,
            pressed: pressed_box,
            pressed_cell: Some(pressed),
            hover_cell: Some(hovered),
        },
    );
    cx.focusable(root, true);

    // Pointer activation and hover feedback — the same press-arm / release-fire
    // shape as `Button`.
    let pointer_cb = action.clone();
    cx.on_pointer(root, move |ev| {
        let Some(p) = ev.pointer() else { return };
        let primary = p.buttons.contains(PointerButtons::PRIMARY);
        match p.phase {
            PointerPhase::Down if primary => {
                ev.set(pressed, StateValue::Bool(true));
            }
            PointerPhase::Up if primary => {
                ev.set(pressed, StateValue::Bool(false));
                fire(&pointer_cb, ev);
            }
            PointerPhase::Enter => {
                ev.set(hovered, StateValue::Bool(true));
            }
            PointerPhase::Leave => {
                ev.set(hovered, StateValue::Bool(false));
                ev.set(pressed, StateValue::Bool(false));
            }
            _ => {}
        }
    });

    // Keyboard activation: Enter/Space (not an auto-repeat) fires the same action.
    let key_cb = action;
    cx.on_key(root, move |ev| {
        if let Some(k) = ev.key()
            && k.pressed
            && !k.repeat
            && matches!(k.key, Key::Enter | Key::Space)
        {
            fire(&key_cb, ev);
        }
    });

    cx.semantics(
        root,
        Semantics::role(Role::Button).with_label(kind.label().to_string()),
    );
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

        // Self-drawn min/max/close buttons are built only when the chrome mode
        // permits them *and* no native traffic-light box was reported — the exact
        // Windows/Linux case. `SelfDrawn` with a native box (a self-drawn frame that
        // still keeps OS buttons) yields to those, as does `Native`. The action
        // cells are cloned so the build closure can move them into the buttons.
        let self_draw_buttons = chrome == WindowChrome::SelfDrawn && buttons_width.is_none();
        let on_minimize = self.on_minimize.clone();
        let on_maximize = self.on_maximize.clone();
        let on_close = self.on_close.clone();

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
                // Leading section (`Fit`). Empty: the native traffic-light buttons
                // are a platform overlay (transparent titlebar over a full-size
                // content view) that floats above the caption and takes no layout
                // width, so nothing is reserved for them here. Keeping this section
                // empty lets the title band span the whole bar and center on the
                // window. Held as an explicit (empty) `Fit` section so the
                // three-section structure — and the title band's index — stays
                // stable across platforms.
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
                    |_cx| {},
                );

                // Centered title (`Fill`). The middle child eats the main-axis
                // slack, so it spans between the two `Fit` sections; its own
                // `Justify::Center` centers the reused `Label` within that span.
                // This middle band is the caption's draggable region: it holds
                // only the non-interactive title and, spanning the slack between
                // the (native/self-drawn) button sections, is exactly the blank
                // area a user grabs to move the window. Registering it hands its
                // post-layout world box to the facade, which forwards it to the
                // platform's window-drag back-channel — no `target_os` branch, the
                // same declaration on every OS (only the platform acts on it).
                let title_band = cx.flex(
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
                cx.register_draggable(title_band);

                // Trailing section (`Fit`). Holds the self-drawn min/max/close
                // buttons on Windows/Linux (`SelfDrawn` chrome with no native box);
                // empty on macOS and any window that keeps native buttons, which
                // the OS draws as an overlay outside the layout. Always built so
                // the three-section structure is stable regardless of platform.
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
                    |cx| {
                        if self_draw_buttons {
                            window_button(cx, WindowButtonKind::Minimize, height, on_minimize);
                            window_button(cx, WindowButtonKind::Maximize, height, on_maximize);
                            window_button(cx, WindowButtonKind::Close, height, on_close);
                        }
                    },
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

    /// With no native traffic-light box reported, the leading section stays empty:
    /// nothing native is yielded and nothing interactive lives here.
    #[test]
    fn no_native_buttons_reserves_no_leading_yield() {
        let chrome = ChromeContext {
            chrome: WindowChrome::SelfDrawn,
            buttons_width: None,
        };
        let (store, root) = build_with(chrome, caption_bar("Untitled"));

        let sections = children(&store, root);
        let leading = sections[0];
        for child in children(&store, leading) {
            assert!(
                !store.has_handler(child) && !store.has_key_handler(child),
                "the leading section holds only inert spacers, no interactive content"
            );
        }
    }

    /// When the platform reports a native traffic-light box (macOS), the leading
    /// section reserves no layout width at all: the native buttons are a platform
    /// overlay floating above the caption, so they take no room in the flow. The
    /// leading section stays an empty `Fit` band pinned at x=0 with zero width,
    /// leaving the title's `Fill` band free to span the whole bar and land at the
    /// window center.
    #[test]
    fn native_buttons_reserve_no_leading_yield() {
        let chrome = ChromeContext {
            chrome: WindowChrome::Native,
            buttons_width: Some(78.0),
        };
        let (mut store, root) = build_with(chrome, caption_bar("Untitled"));

        let sections = children(&store, root);
        let leading = sections[0];
        assert!(
            children(&store, leading).is_empty(),
            "the native box is an overlay ⇒ no leading spacer is reserved"
        );

        // Lay the bar out on a wide surface: the empty leading section resolves to
        // zero width and sits at the bar's origin, yielding nothing to the overlay.
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
        let box_ = store.bounds(leading);
        assert_eq!(
            box_.x, PADDING,
            "leading section pinned at the bar's leading padding edge"
        );
        assert_eq!(
            box_.w, 0.0,
            "leading section yields no width to the overlay"
        );
    }

    /// A `SelfDrawn` window with no native traffic-light box (Windows/Linux) draws
    /// its own three window buttons in the trailing section, each a focusable node
    /// with pointer and key handlers and `Button` semantics carrying its name.
    #[test]
    fn self_drawn_chrome_builds_three_window_buttons() {
        let chrome = ChromeContext {
            chrome: WindowChrome::SelfDrawn,
            buttons_width: None,
        };
        let (store, root) = build_with(chrome, caption_bar("Untitled"));

        let sections = children(&store, root);
        let trailing = sections[2];
        let buttons = children(&store, trailing);
        assert_eq!(buttons.len(), 3, "min / max / close");

        for (button, name) in buttons.iter().zip(["Minimize", "Maximize", "Close"]) {
            assert!(
                store.has_handler(*button),
                "{name} button attaches a pointer handler"
            );
            assert!(
                store.has_key_handler(*button),
                "{name} button attaches a key handler"
            );
            assert!(store.focusable(*button), "{name} button is focusable");

            let sem = store
                .semantics(*button)
                .expect("window button authors semantics");
            assert_eq!(sem.role, Role::Button, "{name} button is a Button");
            assert_eq!(
                sem.label.as_deref(),
                Some(name),
                "{name} button is named for its action"
            );
        }
    }

    /// A window that keeps native buttons — either `Native` chrome or `SelfDrawn`
    /// with a reported traffic-light box — draws no window buttons of its own: the
    /// OS renders them as an overlay outside the layout, so the trailing section
    /// holds nothing interactive.
    #[test]
    fn native_chrome_draws_no_window_buttons() {
        for chrome in [
            ChromeContext {
                chrome: WindowChrome::Native,
                buttons_width: Some(78.0),
            },
            ChromeContext {
                chrome: WindowChrome::SelfDrawn,
                buttons_width: Some(78.0),
            },
        ] {
            let (store, root) = build_with(chrome, caption_bar("Untitled"));
            let sections = children(&store, root);
            let trailing = sections[2];
            for child in children(&store, trailing) {
                assert!(
                    !store.has_handler(child) && !store.has_key_handler(child),
                    "native buttons ⇒ no self-drawn interactive window buttons"
                );
            }
        }
    }

    /// The wired `on_close` action fires when the close button is clicked (a primary
    /// press then release over it), exercising the pointer-activation path.
    #[test]
    fn close_button_fires_wired_action_on_click() {
        use std::cell::Cell;
        use std::rc::Rc;
        use viso_ui::{Modifiers, PointerEvent};

        let fired = Rc::new(Cell::new(0u32));
        let flag = fired.clone();
        let chrome = ChromeContext {
            chrome: WindowChrome::SelfDrawn,
            buttons_width: None,
        };
        let bar = caption_bar("Untitled").on_close(move |_ev| flag.set(flag.get() + 1));

        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();
        let root = {
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
            cx.root().expect("caption bar declares a root node")
        };

        let close = *children(&store, children(&store, root)[2])
            .last()
            .expect("close button");

        let primary = |phase| PointerEvent {
            x: 0.0,
            y: 0.0,
            phase,
            buttons: PointerButtons::PRIMARY,
            modifiers: Modifiers::default(),
        };
        for phase in [PointerPhase::Down, PointerPhase::Up] {
            let mut handler = store.take_handler(close).expect("pointer handler");
            {
                let ev = primary(phase);
                let mut cx = EventCx::__new_pointer(&mut states, &bindings, &ev);
                handler(&mut cx);
            }
            store.restore_handler(close, handler);
        }

        assert_eq!(fired.get(), 1, "click fires the wired close action once");
    }

    /// The bar registers exactly one draggable region — the center title band —
    /// and after layout that band spans the slack between the leading and trailing
    /// sections, i.e. the blank caption area a user grabs to move the window. The
    /// interactive trailing buttons sit outside it, so a press on a button is not a
    /// drag.
    #[test]
    fn registers_center_band_as_the_draggable_region() {
        let chrome = ChromeContext {
            chrome: WindowChrome::SelfDrawn,
            buttons_width: None,
        };
        let (mut store, root) = build_with(chrome, caption_bar("Untitled"));

        let sections = children(&store, root);
        let center = sections[1];

        // Exactly the center band is registered, nothing else.
        assert_eq!(
            store.draggable_regions(),
            &[center],
            "only the center title band is draggable"
        );

        // Lay out on a wide surface; the draggable band must be non-empty and lie
        // strictly left of the trailing buttons (its right edge ≤ the trailing
        // section's left edge), so the buttons stay outside the drag area.
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
        let band = store.bounds(center);
        assert!(band.w > 0.0, "the draggable band has real width");

        let trailing = store.bounds(sections[2]);
        assert!(
            band.x + band.w <= trailing.x + f32::EPSILON,
            "the draggable band ends at or before the trailing buttons"
        );
    }

    /// On macOS the native traffic-light box is a platform overlay, not layout: the
    /// leading section yields no width and nothing trails, so the title's `Fill`
    /// band spans the whole bar and its `Center` justify lands the title at the
    /// exact window center — regardless of the reported box width.
    #[test]
    fn title_band_is_window_centered_under_native_overlay() {
        let chrome = ChromeContext {
            chrome: WindowChrome::Native,
            buttons_width: Some(78.0),
        };
        let (mut store, root) = build_with(chrome, caption_bar("Untitled"));

        let bar_width = 800.0;
        let mut scratch = Vec::new();
        store.layout(
            root,
            Rect {
                x: 0.0,
                y: 0.0,
                w: bar_width,
                h: 38.0,
            },
            &mut scratch,
        );

        let center = children(&store, root)[1];
        let band = store.bounds(center);
        let midpoint = band.x + band.w / 2.0;
        assert!(
            (midpoint - bar_width / 2.0).abs() <= 0.5,
            "center band midpoint {midpoint} ≈ window center {}",
            bar_width / 2.0
        );
    }

    /// On Windows/Linux the self-drawn buttons occupy trailing layout width, so the
    /// title's `Fill` band spans only the room that remains and centers within it —
    /// the platform convention (title centered in the space left of the buttons,
    /// not at the raw window center). The band starts at the bar origin (empty
    /// leading) and ends where the trailing buttons begin.
    #[test]
    fn title_band_centers_in_room_left_of_trailing_buttons() {
        let chrome = ChromeContext {
            chrome: WindowChrome::SelfDrawn,
            buttons_width: None,
        };
        let (mut store, root) = build_with(chrome, caption_bar("Untitled"));

        let bar_width = 800.0;
        let mut scratch = Vec::new();
        store.layout(
            root,
            Rect {
                x: 0.0,
                y: 0.0,
                w: bar_width,
                h: 38.0,
            },
            &mut scratch,
        );

        let sections = children(&store, root);
        let center = sections[1];
        let trailing = sections[2];
        let band = store.bounds(center);
        let buttons = store.bounds(trailing);

        // Inside the bar's horizontal padding the content region is
        // [PADDING, bar_width - PADDING]; the three self-drawn buttons take
        // 3 × BUTTON_WIDTH of trailing width, so the title band spans the room
        // that remains and centers within it.
        let region_start = PADDING;
        let region_end = bar_width - PADDING;
        let remaining = (region_end - region_start) - 3.0 * BUTTON_WIDTH;
        assert_eq!(
            band.x, region_start,
            "title band starts at the leading padding edge"
        );
        assert!(
            (band.w - remaining).abs() <= 0.5,
            "title band spans the room left of the buttons: {} ≈ {remaining}",
            band.w
        );
        let midpoint = band.x + band.w / 2.0;
        let expected_mid = region_start + remaining / 2.0;
        assert!(
            (midpoint - expected_mid).abs() <= 0.5,
            "title centers in the remaining room: {midpoint} ≈ {expected_mid}"
        );
        assert!(
            band.x + band.w <= buttons.x + f32::EPSILON,
            "title band ends at or before the trailing buttons"
        );
    }
}
