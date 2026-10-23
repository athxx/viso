//! The ring the framework draws round the focused node while the keyboard or
//! an assistive technology moved the focus there, in this system's style.
//!
//! A pointer press focuses without a ring; Tab, an arrow or an assistive
//! technology brings it. The ring goes round the node's box just outside it,
//! rounded as its corners are, over its subtree, and moves and turns with it.
//! A node that shows its focus its own way opts out
//! ([`NodeStore::set_focus_ring`]); a text field shows its caret instead,
//! and a text's focused link wears the ring round its own boxes.
//!
//! [`FocusRingStyle::native`] is each system's ring: an accent halo that
//! settles in on Apple platforms, Windows' two-tone outline, a translucent
//! accent outline on Linux, Material's thick indicator on Android, the
//! browser's on the web. The window's appearance supplies the accent where
//! the system has one.

use core::time::Duration;

use viso_render::{Border, Primitive, Quad, Rect, Rgba, Srgb};

use crate::component::NodeStore;
use crate::dirty::DirtyClass;
use crate::node::NodeId;

/// How the focus ring looks and appears.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FocusRingStyle {
    /// The width of its stroke.
    pub width: f32,
    /// How far outside the box its inner edge sits; inside it below zero.
    pub offset: f32,
    /// Its stroke's color.
    pub color: Rgba,
    /// A second stroke inside the first, `(width, color)`, which keeps the
    /// ring seen on any background.
    pub inner: Option<(f32, Rgba)>,
    /// How long it takes to settle in, fading in from `grow` further out;
    /// zero shows it at once, as reduced motion does.
    pub appear: Duration,
    /// How far further out it starts while it appears.
    pub grow: f32,
}

/// An sRGB color, opaque.
fn srgb(r: f32, g: f32, b: f32) -> Rgba {
    Srgb { r, g, b, a: 1.0 }.into_linear_straight()
}

/// `color` at `alpha` of its opacity.
fn faded(color: Rgba, alpha: f32) -> Rgba {
    Rgba {
        a: color.a * alpha,
        ..color
    }
}

impl FocusRingStyle {
    /// macOS and iOS: a translucent accent halo hugging the box, settling in
    /// from further out.
    pub fn apple(accent: Option<Rgba>) -> Self {
        FocusRingStyle {
            width: 3.0,
            offset: 0.0,
            color: faded(accent.unwrap_or_else(|| srgb(0.0, 0.478, 1.0)), 0.55),
            inner: None,
            appear: Duration::from_millis(200),
            grow: 6.0,
        }
    }

    /// Windows: a two-pixel outline in the text's color round a one-pixel
    /// one in the background's, just outside the box.
    pub fn windows(dark: bool) -> Self {
        let (outer, inner) = if dark {
            (Rgba::new(1.0, 1.0, 1.0, 1.0), Rgba::new(0.0, 0.0, 0.0, 0.7))
        } else {
            (Rgba::new(0.0, 0.0, 0.0, 0.9), Rgba::new(1.0, 1.0, 1.0, 0.7))
        };
        FocusRingStyle {
            width: 2.0,
            offset: 0.0,
            color: outer,
            inner: Some((1.0, inner)),
            appear: Duration::ZERO,
            grow: 0.0,
        }
    }

    /// Linux (Adwaita): a translucent accent outline fading in.
    pub fn adwaita(accent: Option<Rgba>, dark: bool) -> Self {
        let accent = accent.unwrap_or_else(|| {
            if dark {
                srgb(0.47, 0.68, 0.93)
            } else {
                srgb(0.21, 0.52, 0.89)
            }
        });
        FocusRingStyle {
            width: 2.0,
            offset: 1.0,
            color: faded(accent, 0.5),
            inner: None,
            appear: Duration::from_millis(200),
            grow: 0.0,
        }
    }

    /// Android (Material 3): a thick indicator two points out.
    pub fn material(accent: Option<Rgba>, dark: bool) -> Self {
        let accent = accent.unwrap_or_else(|| {
            if dark {
                srgb(0.80, 0.76, 0.86)
            } else {
                srgb(0.38, 0.36, 0.44)
            }
        });
        FocusRingStyle {
            width: 3.0,
            offset: 2.0,
            color: accent,
            inner: None,
            appear: Duration::ZERO,
            grow: 0.0,
        }
    }

    /// The web: the browser's two-pixel outline.
    pub fn browser(accent: Option<Rgba>, dark: bool) -> Self {
        let color = accent.unwrap_or_else(|| {
            if dark {
                srgb(0.6, 0.78, 1.0)
            } else {
                srgb(0.06, 0.06, 0.06)
            }
        });
        FocusRingStyle {
            width: 2.0,
            offset: 1.0,
            color,
            inner: Some((
                1.0,
                faded(srgb(1.0, 1.0, 1.0), if dark { 0.0 } else { 0.9 }),
            )),
            appear: Duration::ZERO,
            grow: 0.0,
        }
    }

    /// This system's ring, in its `accent` (where it has one) and a light or
    /// `dark` appearance.
    pub fn native(accent: Option<Rgba>, dark: bool) -> Self {
        if cfg!(any(target_os = "macos", target_os = "ios")) {
            Self::apple(accent)
        } else if cfg!(target_os = "windows") {
            Self::windows(dark)
        } else if cfg!(target_os = "android") {
            Self::material(accent, dark)
        } else if cfg!(target_arch = "wasm32") {
            Self::browser(accent, dark)
        } else {
            Self::adwaita(accent, dark)
        }
    }

    /// Draws the ring round `rect`, whose corners round by `radius`,
    /// `progress` of the way in.
    pub(crate) fn paint(&self, rect: Rect, radius: f32, progress: f32, out: &mut Vec<Primitive>) {
        let settled = 1.0 - (1.0 - progress.clamp(0.0, 1.0)).powi(3);
        let out_by = self.offset + self.grow * (1.0 - settled);
        let mut stroke = |inset: f32, width: f32, color: Rgba| {
            let at = out_by + width + inset;
            if width <= 0.0 || color.a <= 0.0 {
                return;
            }
            out.push(Primitive::Quad(Quad {
                rect: rect.inflate(at),
                color: Rgba::TRANSPARENT,
                radius: (radius + at).max(0.0),
                border: Border {
                    width,
                    color: faded(color, settled),
                },
            }));
        };
        let inner = self.inner.map_or(0.0, |(width, _)| width);
        stroke(inner, self.width, self.color);
        if let Some((width, color)) = self.inner {
            stroke(0.0, width, color);
        }
    }
}

impl Default for FocusRingStyle {
    fn default() -> Self {
        Self::native(None, false)
    }
}

/// A window's focus ring: how it looks, how far it has appeared, and the
/// nodes that show their focus their own way.
#[derive(Debug, Default)]
pub(crate) struct FocusRing {
    style: FocusRingStyle,
    /// How long the ring has been appearing, while it does.
    appearing: Option<Duration>,
    /// The slots that wear no ring, ascending.
    ringless: Vec<u32>,
}

impl FocusRing {
    pub(crate) fn clear(&mut self) {
        self.appearing = None;
        self.ringless.clear();
    }

    /// Forgets the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        if let Ok(at) = self.ringless.binary_search(&index) {
            self.ringless.remove(at);
        }
    }

    /// Whether the ring is settling in.
    pub(crate) fn appearing(&self) -> bool {
        self.appearing.is_some()
    }
}

impl NodeStore {
    /// How this window's focus ring looks; [`FocusRingStyle::native`] unless
    /// set.
    pub fn focus_ring_style(&self) -> FocusRingStyle {
        self.ring.style
    }

    /// Make this window's focus ring look as `style`, the one shown too.
    pub fn set_focus_ring_style(&mut self, style: FocusRingStyle) {
        if self.ring.style == style {
            return;
        }
        self.ring.style = style;
        if let Some(node) = self.focused() {
            self.mark_dirty(node, DirtyClass::PAINT);
        }
    }

    /// Whether node `id` wears the focus ring while it holds focus the
    /// keyboard moved there (default `true`).
    pub fn wears_focus_ring(&self, id: NodeId) -> bool {
        self.ring.ringless.binary_search(&id.index()).is_err()
    }

    /// Whether node `id` wears the focus ring: `false` for a node that shows
    /// its focus its own way. A live-guarded write.
    pub fn set_focus_ring(&mut self, id: NodeId, wears: bool) {
        if !self.arena().is_live(id) {
            return;
        }
        let ringless = &mut self.ring.ringless;
        match (ringless.binary_search(&id.index()), wears) {
            (Ok(at), true) => {
                ringless.remove(at);
            }
            (Err(at), false) => ringless.insert(at, id.index()),
            _ => return,
        }
        if self.focused() == Some(id) {
            self.mark_dirty(id, DirtyClass::PAINT);
        }
    }

    /// How far the ring round node `id` has appeared, `None` while it shows
    /// none: it holds focus the keyboard moved there, wears the ring, and
    /// draws none of its own.
    pub(crate) fn focus_ring_on(&self, id: NodeId) -> Option<f32> {
        if self.focused() != Some(id)
            || !self.focus_shows_visible()
            || !self.wears_focus_ring(id)
            || self.field_marks(id).is_some()
            || self.focused_link(id).is_some()
        {
            return None;
        }
        let appear = self.ring.style.appear;
        Some(match self.ring.appearing {
            Some(t) if !appear.is_zero() => t.as_secs_f32() / appear.as_secs_f32(),
            _ => 1.0,
        })
    }

    /// Starts the ring appearing on the focused node, which repaints.
    pub(crate) fn ring_landed(&mut self) {
        let Some(node) = self.focused() else { return };
        if !self.focus_shows_visible() {
            return;
        }
        let appear = self.ring.style.appear;
        self.ring.appearing =
            (!appear.is_zero() && !self.reduced_motion()).then_some(Duration::ZERO);
        self.mark_dirty(node, DirtyClass::PAINT);
    }

    /// Advances the ring's appearing by `delta`, repainting the focused node.
    pub(crate) fn tick_ring(&mut self, delta: Duration) {
        let Some(t) = self.ring.appearing else { return };
        let t = t + delta;
        self.ring.appearing = (t < self.ring.style.appear).then_some(t);
        match self.focused() {
            Some(node) => self.mark_dirty(node, DirtyClass::PAINT),
            None => self.ring.appearing = None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ring_settles_in_from_further_out_and_hugs_the_box_once_in() {
        let style = FocusRingStyle::apple(None);
        let rect = Rect::new(10.0, 10.0, 40.0, 20.0);
        let ring = |p| {
            let mut out = Vec::new();
            style.paint(rect, 4.0, p, &mut out);
            match out[..] {
                [Primitive::Quad(q)] => q,
                _ => panic!("{out:?}"),
            }
        };
        let settled = ring(1.0);
        assert_eq!(settled.rect, rect.inflate(3.0));
        assert_eq!(settled.radius, 7.0);
        assert_eq!(settled.border.width, 3.0);
        let start = ring(0.0);
        assert_eq!(start.rect, rect.inflate(9.0));
        assert_eq!(start.border.color.a, 0.0);
        assert!(ring(0.5).border.color.a > 0.0);
    }

    #[test]
    fn the_keyboard_s_focus_wears_the_ring_round_the_box_and_a_press_s_does_not() {
        use crate::component::{BuildCx, FlexStyle, LeafStyle};
        use crate::layout::Size;
        use crate::paint::paint_tree;
        use crate::style::BoxStyle;

        let mut store = NodeStore::new();
        let mut leaf = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                leaf = Some(
                    cx.leaf(LeafStyle {
                        size: Size::fixed(40.0, 20.0),
                        style: BoxStyle::NONE.with_radius(4.0),
                    })
                    .id(),
                );
            });
            cx.root().unwrap()
        };
        let leaf = leaf.unwrap();
        store.set_focusable(leaf, true);
        store.layout(root, Rect::new(0.0, 0.0, 200.0, 100.0), &mut Vec::new());
        store.set_focus_ring_style(FocusRingStyle::apple(None));
        let rings = |store: &NodeStore| {
            let mut out = Vec::new();
            paint_tree(store, root, &mut out);
            out.into_iter()
                .filter_map(|p| match p {
                    Primitive::Quad(q) if q.border.width > 0.0 => Some(q),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };

        store.set_focus_visible(false);
        store.set_focused(Some(leaf));
        assert!(rings(&store).is_empty(), "a press focuses without a ring");

        store.set_focus_visible(true);
        assert!(store.is_transitioning(), "the ring settles in");
        let start = rings(&store);
        assert_eq!(start.len(), 1);
        assert_eq!(start[0].border.color.a, 0.0);
        store.tick_transitions(Duration::from_millis(250));
        assert!(!store.is_transitioning());
        let world = store.world(leaf);
        assert_eq!(rings(&store)[0].rect, world.inflate(3.0));
        assert_eq!(rings(&store)[0].radius, 7.0);

        store.set_focus_ring(leaf, false);
        assert!(
            rings(&store).is_empty(),
            "a node can show its focus its own way"
        );
        store.set_focus_ring(leaf, true);
        store.set_reduced_motion(true);
        store.set_focused(None);
        store.set_focused(Some(leaf));
        assert!(!store.is_transitioning(), "reduced motion shows it at once");
        assert_eq!(
            rings(&store)[0].border.color.a,
            FocusRingStyle::apple(None).color.a
        );
    }

    #[test]
    fn a_two_tone_ring_strokes_its_inner_line_inside_the_outer() {
        let mut out = Vec::new();
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
        FocusRingStyle::windows(false).paint(rect, 0.0, 1.0, &mut out);
        let [Primitive::Quad(outer), Primitive::Quad(inner)] = out[..] else {
            panic!("{out:?}");
        };
        assert_eq!(outer.rect, rect.inflate(3.0));
        assert_eq!(inner.rect, rect.inflate(1.0));
        assert_eq!((outer.border.width, inner.border.width), (2.0, 1.0));
    }
}
