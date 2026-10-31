//! Backdrops: a box of glass, which blurs, saturates and tints what lies
//! behind it, and may even its light, bend it at the rim and catch a glint.
//!
//! A backdrop follows the user's settings: with less transparency asked
//! for it frosts denser and drops its bend and glint; with more contrast
//! it goes near black or white with a contrasting rim.
//!
//! A backdrop is drawn as the node's frosted surface under its own fill,
//! outside the layer a translucent node fades through, so the glass fades
//! by its own opacity and keeps its blur. Glass boxes over the same
//! background at the same blur share one capture of it and one blur. The
//! table is cold: one entry for each node with a backdrop, kept in slot
//! order so paint finds a node's in a binary search and a tree with none
//! checks an empty table.

use crate::component::NodeStore;
use crate::node::NodeId;
use viso_render::{
    ColorMatrix, ColorOp, Corners, FrostedMaterial, Glint, Lens, Luminosity, Rect, Rgba,
};

/// What a box does to the content behind it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Backdrop {
    /// How far the content behind blurs, as CSS's blur radius: twice the
    /// Gaussian's standard deviation.
    pub blur: f32,
    /// How its saturation scales: `1` keeps it, `0` greys it, more deepens
    /// it.
    pub saturation: f32,
    /// A colour laid over it by the colour's alpha, which keeps the glass
    /// readable over busy content.
    pub tint: Rgba,
    /// A grain over the glass, `0..=1`.
    pub noise: f32,
    /// How far, `0..=1`, its light gives way to the tint's, so what lies on
    /// it reads the same over light and dark: acrylic's luminosity layer.
    pub luminosity: f32,
    /// How far, in dp, what lies behind shifts inward at the edge: liquid
    /// glass's bend; `0` for flat glass.
    pub refraction: f32,
    /// How deep, in dp, the rounded bevel the bend crosses reaches in.
    pub bevel: f32,
    /// How far the bend splits the colors, `0..=1`.
    pub dispersion: f32,
    /// The thin glint the rim catches, `0..=1`.
    pub highlight: f32,
    /// Where the light comes from, in radians: CSS's angle, `0` up and
    /// clockwise.
    pub light: f32,
    /// Whether it follows what lies behind it and its own size: its tint's
    /// tone dark over dark content and light over light, its hue kept; its
    /// bend, bevel and blur deeper the larger it is.
    pub adaptive: bool,
}

/// The glint's width in dp; a faint bleed reaches four times as deep.
const GLINT_WIDTH: f32 = 1.0;

/// The shorter side, in dp, at which adaptive glass is as thick as its
/// values say: a toolbar button's.
const THIN: f32 = 44.0;

/// The user's settings every backdrop follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct GlassAccess {
    /// Less transparency: a denser frost, no bend, glint or luminosity.
    pub(crate) reduced_transparency: bool,
    /// More contrast: near black or white, with a contrasting rim.
    pub(crate) high_contrast: bool,
}

/// How much of what lies behind a denser frost still shows: its tint
/// covers this much more of the rest.
const DENSER: f32 = 0.7;
/// How much farther a denser frost blurs.
const FROSTIER: f32 = 1.5;

impl Backdrop {
    /// The standard glass: a strong blur, deepened colour and a light
    /// frosting.
    pub const GLASS: Backdrop = Backdrop {
        blur: 30.0,
        saturation: 1.8,
        tint: Rgba {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            // `#ffffff40`, as the DSL's `Backdrop` defaults to.
            a: 64.0 / 255.0,
        },
        noise: 0.0,
        luminosity: 0.0,
        refraction: 0.0,
        bevel: 20.0,
        dispersion: 0.0,
        highlight: 0.0,
        // `315deg`, from the upper left, as `to_radians` makes it.
        light: 315.0 * (core::f32::consts::PI / 180.0),
        adaptive: false,
    };

    /// Liquid glass that follows what lies behind it and thickens with its
    /// size: the one to reach for.
    pub const REGULAR: Backdrop = Backdrop {
        blur: 6.0,
        saturation: 1.6,
        tint: Rgba {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            // `#ffffff59`, as the DSL's `Backdrop::regular()`.
            a: 89.0 / 255.0,
        },
        refraction: 12.0,
        bevel: 16.0,
        dispersion: 0.08,
        highlight: 0.8,
        adaptive: true,
        ..Backdrop::GLASS
    };

    /// Liquid glass that shows more of what lies behind it, dimmed so the
    /// bright symbols on it read; it does not adapt.
    pub const CLEAR: Backdrop = Backdrop {
        blur: 2.0,
        saturation: 1.3,
        tint: Rgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            // `#00000040`, as the DSL's `Backdrop::clear()`.
            a: 64.0 / 255.0,
        },
        refraction: 12.0,
        bevel: 16.0,
        dispersion: 0.08,
        highlight: 0.9,
        ..Backdrop::GLASS
    };

    /// The frosted surface it draws over a box of `rect` rounded by
    /// `radius`, faded to `opacity`.
    pub(crate) fn surface(
        self,
        rect: Rect,
        radius: f32,
        opacity: f32,
        access: GlassAccess,
    ) -> FrostedMaterial {
        let glass = self.followed(access);
        let mut surface = glass.material(rect, radius, opacity);
        if access.high_contrast {
            // The rim the opposite of the face.
            let dark = glass.tint.r + glass.tint.g + glass.tint.b < 1.5;
            let rim = if dark { 1.0 } else { 0.0 };
            surface.border = viso_render::Border {
                width: 1.0,
                color: Rgba {
                    r: rim,
                    g: rim,
                    b: rim,
                    a: 1.0,
                },
            };
        }
        surface
    }

    /// The glass as the user's settings have it.
    fn followed(self, access: GlassAccess) -> Backdrop {
        let mut glass = self;
        if access.high_contrast {
            // Near black or white, as its tint leans.
            let lum = 0.2126 * self.tint.r + 0.7152 * self.tint.g + 0.0722 * self.tint.b;
            let face = if lum < 0.5 { 0.0 } else { 1.0 };
            glass.tint = Rgba {
                r: face,
                g: face,
                b: face,
                a: self.tint.a.max(0.9),
            };
        }
        if access.reduced_transparency || access.high_contrast {
            glass.blur *= FROSTIER;
            glass.tint.a += (1.0 - glass.tint.a) * DENSER;
            glass.luminosity = 0.0;
            glass.adaptive = false;
            glass.refraction = 0.0;
            glass.dispersion = 0.0;
            glass.highlight = 0.0;
        }
        glass
    }

    fn material(self, rect: Rect, radius: f32, opacity: f32) -> FrostedMaterial {
        // Adaptive glass is thicker the larger it is: a deeper bend and
        // bevel, a softer blur.
        let thick = match self.adaptive {
            true => (rect.w.min(rect.h) / THIN).clamp(0.8, 2.5),
            false => 1.0,
        };
        let mut surface = FrostedMaterial::new(
            rect,
            Corners::uniform(radius),
            self.blur.max(0.0) * 0.5 * thick.sqrt(),
        );
        let saturation = ColorMatrix::saturation(self.saturation.max(0.0));
        surface.color = ColorOp {
            matrix: match self.adaptive {
                // Its tint is laid over by tone, after the color.
                true => saturation,
                false => saturation.then(&ColorMatrix::tint(
                    [self.tint.r, self.tint.g, self.tint.b],
                    self.tint.a,
                )),
            },
            gamma: 1.0,
        };
        surface.adaptive_tint = self.adaptive.then_some(self.tint);
        surface.noise = self.noise.clamp(0.0, 1.0);
        surface.luminosity = Luminosity {
            amount: self.luminosity.clamp(0.0, 1.0),
            luminance: 0.2126 * self.tint.r + 0.7152 * self.tint.g + 0.0722 * self.tint.b,
        };
        surface.lens = Lens {
            refraction: self.refraction.max(0.0) * thick,
            bevel: self.bevel.max(0.0) * thick,
            dispersion: self.dispersion.clamp(0.0, 1.0),
        };
        surface.glint = Glint {
            strength: self.highlight.clamp(0.0, 1.0),
            light: self.light,
            width: GLINT_WIDTH,
        };
        surface.opacity = opacity;
        surface
    }
}

/// The nodes with backdrops, in slot order.
#[derive(Debug, Default)]
pub(crate) struct Backdrops {
    entries: Vec<(u32, Backdrop)>,
    pub(crate) access: GlassAccess,
}

impl Backdrops {
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    /// Forgets the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        if let Ok(at) = self.find(index) {
            self.entries.remove(at);
        }
    }

    fn find(&self, index: u32) -> Result<usize, usize> {
        self.entries.binary_search_by_key(&index, |e| e.0)
    }

    #[inline]
    pub(crate) fn get(&self, index: u32) -> Option<Backdrop> {
        if self.entries.is_empty() {
            return None;
        }
        self.find(index).ok().map(|at| self.entries[at].1)
    }
}

impl NodeStore {
    /// Makes every backdrop follow the user's settings: denser and flat
    /// with `reduced_transparency`, near black or white with a contrasting
    /// rim with `high_contrast`. A change repaints every glass box.
    pub fn set_glass_access(&mut self, reduced_transparency: bool, high_contrast: bool) {
        let access = GlassAccess {
            reduced_transparency,
            high_contrast,
        };
        if self.backdrops.access == access {
            return;
        }
        self.backdrops.access = access;
        let glass: Vec<u32> = self.backdrops.entries.iter().map(|e| e.0).collect();
        for index in glass {
            if let Some(id) = self.arena().live_id(index) {
                self.mark_dirty(id, crate::dirty::DirtyClass::PAINT);
            }
        }
    }

    /// The backdrop of node `id`'s box.
    pub fn backdrop(&self, id: NodeId) -> Option<Backdrop> {
        self.backdrops.get(id.index())
    }

    /// Makes node `id`'s box glass over what lies behind it, or an ordinary
    /// box; a change repaints it. A live-guarded write — a stale handle is a
    /// no-op.
    pub fn set_backdrop(&mut self, id: NodeId, backdrop: Option<Backdrop>) {
        if !self.arena().is_live(id) || self.backdrop(id) == backdrop {
            return;
        }
        let index = id.index();
        match (self.backdrops.find(index), backdrop) {
            (Ok(at), Some(b)) => self.backdrops.entries[at].1 = b,
            (Ok(at), None) => {
                self.backdrops.entries.remove(at);
            }
            (Err(at), Some(b)) => self.backdrops.entries.insert(at, (index, b)),
            (Err(_), None) => {}
        }
        self.mark_dirty(id, crate::dirty::DirtyClass::PAINT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{BuildCx, LeafStyle};
    use crate::layout::Sizing;
    use crate::style::BoxStyle;
    use viso_render::Primitive;

    fn panel(store: &mut NodeStore, opacity: f32) -> NodeId {
        let mut cx = BuildCx::new(store);
        let panel = cx.leaf(LeafStyle {
            size: Sizing::fixed(200.0, 100.0),
            style: BoxStyle::solid(Rgba {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.1,
            })
            .with_radius(12.0),
        });
        cx.backdrop(panel, Backdrop::GLASS);
        cx.opacity(panel, opacity);
        let panel = panel.id();
        store.layout(panel, Rect::new(0.0, 0.0, 400.0, 300.0), &mut Vec::new());
        panel
    }

    fn paint(store: &NodeStore, root: NodeId) -> Vec<Primitive> {
        let mut out = Vec::new();
        crate::paint::paint_tree(store, root, &mut out);
        out
    }

    #[test]
    fn a_glass_box_draws_its_frosted_surface_under_its_fill() {
        let mut store = NodeStore::new();
        let panel = panel(&mut store, 1.0);
        let out = paint(&store, panel);
        let Primitive::Frosted(surface) = &out[0] else {
            panic!("the glass first: {out:?}");
        };
        assert_eq!(surface.rect, store.world(panel), "the box's rect");
        assert_eq!(surface.backdrop_sigma, 15.0);
        assert_eq!(surface.radius, Corners::uniform(12.0), "its corners");
        assert_eq!(surface.opacity, 1.0);
        assert!(matches!(out[1], Primitive::Quad(_)), "then the box's fill");

        store.set_backdrop(panel, None);
        let out = paint(&store, panel);
        assert!(!out.iter().any(|p| matches!(p, Primitive::Frosted(_))));
    }

    #[test]
    fn a_fading_glass_box_fades_its_surface_outside_its_layer() {
        let mut store = NodeStore::new();
        let panel = panel(&mut store, 0.5);
        let out = paint(&store, panel);
        let Primitive::Frosted(surface) = &out[0] else {
            panic!("the glass before the fade: {out:?}");
        };
        assert_eq!(surface.opacity, 0.5);
        assert!(
            matches!(out[1], Primitive::Layer(_)),
            "then the fading layer"
        );
    }

    fn liquid() -> Backdrop {
        Backdrop {
            refraction: 10.0,
            highlight: 0.8,
            luminosity: 0.5,
            ..Backdrop::GLASS
        }
    }

    fn surface_of(store: &NodeStore, panel: NodeId) -> FrostedMaterial {
        match paint(store, panel).into_iter().next() {
            Some(Primitive::Frosted(surface)) => *surface,
            other => panic!("the glass first: {other:?}"),
        }
    }

    #[test]
    fn less_transparency_frosts_denser_and_flat_and_repaints_the_glass() {
        let mut store = NodeStore::new();
        let panel = panel(&mut store, 1.0);
        store.set_backdrop(panel, Some(liquid()));
        let clear = surface_of(&store, panel);
        assert!(clear.lens.is_on() && clear.glint.is_on());

        store.clear_dirty();
        store.set_glass_access(true, false);
        assert!(store.dirty(panel).contains(crate::dirty::DirtyClass::PAINT));
        let dense = surface_of(&store, panel);
        assert!(dense.backdrop_sigma > clear.backdrop_sigma, "frostier");
        assert!(!dense.lens.is_on() && !dense.glint.is_on() && !dense.luminosity.is_on());
        assert_eq!(dense.border, viso_render::Border::NONE);
    }

    #[test]
    fn more_contrast_turns_the_glass_white_or_black_with_the_opposite_rim() {
        let mut store = NodeStore::new();
        let panel = panel(&mut store, 1.0);
        store.set_glass_access(false, true);
        // The standard glass's white tint: a white face, a black rim.
        let light = surface_of(&store, panel);
        assert_eq!(light.border.width, 1.0);
        assert_eq!(light.border.color.r, 0.0);
        let dark = Backdrop {
            tint: Rgba {
                r: 0.1,
                g: 0.1,
                b: 0.1,
                a: 0.5,
            },
            ..Backdrop::GLASS
        };
        store.set_backdrop(panel, Some(dark));
        assert_eq!(surface_of(&store, panel).border.color.r, 1.0);
    }

    #[test]
    fn adaptive_glass_tints_by_tone_and_thickens_with_its_size() {
        let small = Backdrop::REGULAR.material(Rect::new(0.0, 0.0, 44.0, 44.0), 22.0, 1.0);
        let large = Backdrop::REGULAR.material(Rect::new(0.0, 0.0, 300.0, 200.0), 22.0, 1.0);
        assert_eq!(small.adaptive_tint, Some(Backdrop::REGULAR.tint));
        assert_eq!(
            small.lens.refraction,
            Backdrop::REGULAR.refraction,
            "as thick as said"
        );
        assert!(large.lens.refraction > small.lens.refraction, "deeper");
        assert!(large.backdrop_sigma > small.backdrop_sigma, "softer");
        // Its tint is not also in the fused color.
        assert_eq!(
            small.color.matrix,
            ColorMatrix::saturation(Backdrop::REGULAR.saturation)
        );
        let clear = Backdrop::CLEAR.material(Rect::new(0.0, 0.0, 300.0, 200.0), 22.0, 1.0);
        assert_eq!(clear.adaptive_tint, None);
        assert_eq!(
            clear.lens.refraction,
            Backdrop::CLEAR.refraction,
            "clear stays as said"
        );
    }
}
