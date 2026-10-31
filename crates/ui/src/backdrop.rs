//! Backdrops: a box of glass, which blurs, saturates and tints what lies
//! behind it, and may even its light, bend it at the rim and catch a glint.
//!
//! A backdrop follows the user's settings: with less transparency asked
//! for it frosts denser and drops its bend and glint; with more contrast
//! it goes near black or white with a contrasting rim; where the system
//! saves power, or the device draws on the CPU, it is its solid fallback
//! and reads nothing behind it.
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
    Border, ColorMatrix, ColorOp, Corners, FrostedMaterial, GlassUnion, Glint, Glow, Lens,
    Luminosity, Primitive, Quad, Rect, Rgba,
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
    /// What it is where no glass is drawn, solid; `None` for its tint made
    /// all but opaque.
    pub fallback: Option<Rgba>,
    /// Whether a press lights it from within, from where it lands.
    pub interactive: bool,
}

/// How long glass takes to come or go, by its bend, blur and tint (seconds).
const MATERIALIZE: f32 = 0.35;

/// A backdrop coming or going: drawn by `amount` of its glass, moving to
/// `to`; one going is drawn from `glass` after the node lets it go.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Materialize {
    node: NodeId,
    glass: Backdrop,
    amount: f32,
    to: f32,
}

/// The glint's width in dp; a faint bleed reaches four times as deep.
const GLINT_WIDTH: f32 = 1.0;

/// The shorter side, in dp, at which adaptive glass is as thick as its
/// values say: a toolbar button's.
const THIN: f32 = 44.0;

/// The settings every backdrop follows: the user's and the device's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GlassAccess {
    /// Less transparency: a denser frost, no bend, glint or luminosity.
    pub reduced_transparency: bool,
    /// More contrast: near black or white, with a contrasting rim.
    pub high_contrast: bool,
    /// No glass at all: each backdrop its solid fallback, no capture —
    /// where the system saves power or the device cannot afford one.
    pub solid: bool,
}

/// How much of what lies behind a solid fallback made from the tint still
/// shows: next to none.
const SOLID: f32 = 0.9;

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
        fallback: None,
        interactive: false,
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

    /// The glass `amount` of the way in: its bend, glint, blur and tint
    /// grown by it, its opacity left alone — how glass comes and goes.
    fn materialized(self, amount: f32) -> Backdrop {
        if amount >= 1.0 {
            return self;
        }
        // Eased out: it settles in.
        let m = 1.0 - (1.0 - amount.clamp(0.0, 1.0)).powi(3);
        Backdrop {
            blur: self.blur * m,
            tint: Rgba {
                a: self.tint.a * m,
                ..self.tint
            },
            luminosity: self.luminosity * m,
            refraction: self.refraction * m,
            highlight: self.highlight * m,
            ..self
        }
    }

    /// What it draws over a box of `rect` rounded by `radius`, faded to
    /// `opacity`, lit by `glow`: its frosted surface, or its solid fallback.
    pub(crate) fn paint(
        self,
        rect: Rect,
        radius: f32,
        opacity: f32,
        access: GlassAccess,
        glow: Option<Glow>,
    ) -> Primitive {
        if !access.solid {
            let mut surface = self.surface(rect, radius, opacity, access);
            surface.glow = glow.filter(|_| self.interactive);
            return Primitive::Frosted(Box::new(surface));
        }
        // Its fallback, or its tint made all but opaque.
        let tint = self.followed(access).tint;
        let color = self.fallback.unwrap_or(Rgba {
            a: tint.a + (1.0 - tint.a) * SOLID,
            ..tint
        });
        Primitive::Quad(Quad {
            rect,
            color: Rgba {
                a: color.a * opacity,
                ..color
            },
            radius,
            border: Border::NONE,
        })
    }

    /// The frosted surface it draws over a box of `rect` rounded by
    /// `radius`, faded to `opacity`.
    fn surface(
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
            surface.border = Border {
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
    /// The nodes whose glass children melt, and how near, in slot order.
    merges: Vec<(u32, f32)>,
    /// The backdrops coming or going.
    coming: Vec<Materialize>,
}

impl Backdrops {
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.merges.clear();
        self.coming.clear();
    }

    /// Forgets the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        if let Ok(at) = self.find(index) {
            self.entries.remove(at);
        }
        if let Ok(at) = self.merges.binary_search_by_key(&index, |e| e.0) {
            self.merges.remove(at);
        }
        self.coming.retain(|c| c.node.index() != index);
    }

    /// How near the glass children of slot `index` come before they melt,
    /// `0` for apart.
    #[inline]
    pub(crate) fn merge(&self, index: u32) -> f32 {
        if self.merges.is_empty() {
            return 0.0;
        }
        self.merges
            .binary_search_by_key(&index, |e| e.0)
            .map_or(0.0, |at| self.merges[at].1)
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
    /// The settings every backdrop follows.
    pub fn glass_access(&self) -> GlassAccess {
        self.backdrops.access
    }

    /// Makes every backdrop follow `access`: denser and flat with less
    /// transparency, near black or white with a contrasting rim with more
    /// contrast, its solid fallback where there is no glass. A change
    /// repaints every glass box.
    pub fn set_glass_access(&mut self, access: GlassAccess) {
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
        let was = self.backdrop(id);
        if !self.arena().is_live(id) || was == backdrop {
            return;
        }
        // Glass a laid-out box turns on or off comes or goes by its bend,
        // unless motion is reduced.
        let laid_out = self.bounds(id).w > 0.0 && self.bounds(id).h > 0.0;
        let going = self.backdrops.coming.iter().position(|c| c.node == id);
        match (was, backdrop, going) {
            (_, Some(glass), Some(at)) => {
                let coming = &mut self.backdrops.coming[at];
                (coming.glass, coming.to) = (glass, 1.0);
            }
            (None, Some(glass), None) if laid_out && !self.reduced_motion() => {
                self.backdrops.coming.push(Materialize {
                    node: id,
                    glass,
                    amount: 0.0,
                    to: 1.0,
                });
            }
            (Some(glass), None, None) if laid_out && !self.reduced_motion() => {
                self.backdrops.coming.push(Materialize {
                    node: id,
                    glass,
                    amount: 1.0,
                    to: 0.0,
                });
            }
            (Some(_), None, Some(at)) => self.backdrops.coming[at].to = 0.0,
            _ => {}
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
        self.repaint_glass(id);
    }

    /// Repaints node `id`'s glass: the node, and the node its glass melts
    /// into.
    fn repaint_glass(&mut self, id: NodeId) {
        self.mark_dirty(id, crate::dirty::DirtyClass::PAINT);
        if let Some(parent) = self.parent(id)
            && self.backdrops.merge(parent.index()) > 0.0
        {
            self.mark_dirty(parent, crate::dirty::DirtyClass::PAINT);
        }
    }

    /// How near node `id`'s glass children come before they melt into one.
    pub fn glass_merge(&self, id: NodeId) -> f32 {
        self.backdrops.merge(id.index())
    }

    /// Makes node `id`'s glass children melt into one surface where they
    /// come within `spacing` of each other — sharing one capture, a press
    /// on one lighting all — or, at `0`, stand apart. A live-guarded write.
    pub fn set_glass_merge(&mut self, id: NodeId, spacing: f32) {
        let spacing = spacing.max(0.0);
        if !self.arena().is_live(id) || self.glass_merge(id) == spacing {
            return;
        }
        let index = id.index();
        let merges = &mut self.backdrops.merges;
        match (merges.binary_search_by_key(&index, |e| e.0), spacing > 0.0) {
            (Ok(at), true) => merges[at].1 = spacing,
            (Ok(at), false) => {
                merges.remove(at);
            }
            (Err(at), true) => merges.insert(at, (index, spacing)),
            (Err(_), false) => {}
        }
        self.mark_dirty(id, crate::dirty::DirtyClass::PAINT);
        let children: Vec<NodeId> = self.arena().children(id).collect();
        for child in children {
            self.mark_dirty(child, crate::dirty::DirtyClass::PAINT);
        }
    }

    /// Whether any node is glass.
    #[inline]
    pub(crate) fn has_glass(&self) -> bool {
        !self.backdrops.entries.is_empty()
    }

    /// Whether node `id`'s glass melts into its parent's.
    #[inline]
    pub(crate) fn melts(&self, id: NodeId) -> bool {
        !self.backdrops.merges.is_empty()
            && self
                .parent(id)
                .is_some_and(|p| self.backdrops.merge(p.index()) > 0.0)
    }

    /// The glass node `id` draws and how far in it is: its backdrop, or one
    /// still going.
    pub(crate) fn drawn_glass(&self, id: NodeId) -> Option<(Backdrop, f32)> {
        let coming = &self.backdrops.coming;
        if !coming.is_empty()
            && let Some(c) = coming.iter().find(|c| c.node == id)
        {
            return Some((c.glass, c.amount));
        }
        self.backdrop(id).map(|b| (b, 1.0))
    }

    /// Whether glass is coming or going: the frame loop beats while it is.
    #[inline]
    pub fn is_materializing(&self) -> bool {
        !self.backdrops.coming.is_empty()
    }

    /// Runs the glass coming and going on by a frame of `delta`.
    pub fn tick_glass(&mut self, delta: std::time::Duration) {
        let step = delta.as_secs_f32().min(0.1) / MATERIALIZE;
        let mut i = self.backdrops.coming.len();
        while i > 0 {
            i -= 1;
            let c = &mut self.backdrops.coming[i];
            c.amount = match c.to > c.amount {
                true => (c.amount + step).min(c.to),
                false => (c.amount - step).max(c.to),
            };
            let (node, done) = (c.node, c.amount == c.to);
            if done || self.reduced_motion() {
                self.backdrops.coming.swap_remove(i);
            }
            if self.arena().is_live(node) {
                self.repaint_glass(node);
            }
        }
    }

    /// What node `id`'s glass, `amount` of the way in, draws over its box
    /// `world` rounded by `radius`.
    pub(crate) fn paint_glass(
        &self,
        id: NodeId,
        glass: Backdrop,
        amount: f32,
        world: Rect,
        radius: f32,
        opacity: f32,
    ) -> Primitive {
        let glow = self.press_glow(id, world).filter(|_| glass.interactive);
        glass
            .materialized(amount)
            .paint(world, radius, opacity, self.backdrops.access, glow)
    }

    /// The glass of node `id`'s melting children, over their boxes in
    /// `space` (each child's box and corner radius), in runs of the same
    /// glass, at most [`GlassUnion::MAX`] a surface.
    pub(crate) fn paint_melted(
        &self,
        members: &[(NodeId, Backdrop, f32, Rect, f32)],
        opacity: f32,
        out: &mut Vec<Primitive>,
    ) {
        let spacing = members
            .first()
            .and_then(|m| self.parent(m.0))
            .map_or(0.0, |p| self.backdrops.merge(p.index()));
        let access = self.backdrops.access;
        let mut start = 0;
        while start < members.len() {
            let (_, glass, amount, ..) = members[start];
            let mut end = start + 1;
            while end < members.len()
                && end - start < GlassUnion::MAX
                && members[end].1 == glass
                && members[end].2 == amount
            {
                end += 1;
            }
            let run = &members[start..end];
            start = end;
            if access.solid || run.len() == 1 {
                for &(id, glass, amount, world, radius) in run {
                    out.push(self.paint_glass(id, glass, amount, world, radius, opacity));
                }
                continue;
            }
            let shapes: Vec<(Rect, f32)> = run.iter().map(|m| (m.3, m.4)).collect();
            let Some(union) = GlassUnion::of(&shapes, spacing) else {
                continue;
            };
            let glow = run.iter().find_map(|m| self.press_glow(m.0, m.3));
            let mut surface =
                glass
                    .materialized(amount)
                    .surface(union.bounds(), 0.0, opacity, access);
            surface.merged = Some(union);
            surface.border = Border::NONE;
            surface.glow = glow.filter(|_| glass.interactive);
            out.push(Primitive::Frosted(Box::new(surface)));
        }
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

    /// Lets the glass coming and going settle.
    fn settle(store: &mut NodeStore) {
        while store.is_materializing() {
            store.tick_glass(std::time::Duration::from_millis(100));
        }
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
        settle(&mut store);
        let out = paint(&store, panel);
        assert!(!out.iter().any(|p| matches!(p, Primitive::Frosted(_))));
    }

    #[test]
    fn a_resized_glass_box_splices_its_surface_onto_its_new_box() {
        use crate::component::{Painted, StackStyle};
        use crate::dirty::DirtyClass;
        let mut store = NodeStore::new();
        let mut panel = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.stack(
                StackStyle {
                    size: Sizing::fill(),
                    ..StackStyle::default()
                },
                |cx| {
                    let leaf = cx.leaf(LeafStyle {
                        size: Sizing::fill(),
                        style: BoxStyle::solid(Rgba::TRANSPARENT).with_radius(12.0),
                    });
                    cx.backdrop(leaf, Backdrop::GLASS);
                    panel = Some(leaf.id());
                },
            )
            .id()
        };
        let panel = panel.unwrap();
        let mut scratch = Vec::new();
        store.layout(root, Rect::new(0.0, 0.0, 400.0, 300.0), &mut scratch);
        let mut out = Vec::new();
        store.repaint(root, &mut out);
        store.clear_dirty();
        // The window resizes, again and again.
        for (w, h) in [(500.0, 320.0), (360.0, 280.0), (420.0, 260.0)] {
            store.mark_dirty(
                root,
                DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
            );
            store.layout(root, Rect::new(0.0, 0.0, w, h), &mut scratch);
            let painted = store.repaint_dirty(root, &mut out);
            assert!(
                !matches!(painted, Painted::Whole(_)),
                "spliced: {painted:?}"
            );
            store.clear_dirty();
            let Some(Primitive::Frosted(surface)) =
                out.iter().find(|p| matches!(p, Primitive::Frosted(_)))
            else {
                panic!("the glass: {out:?}");
            };
            assert_eq!(
                surface.rect,
                store.world(panel),
                "the glass follows its box"
            );
            assert_eq!(out, paint(&store, root), "as a whole paint draws it");
        }
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
        store.set_glass_access(GlassAccess {
            reduced_transparency: true,
            ..GlassAccess::default()
        });
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
        store.set_glass_access(GlassAccess {
            high_contrast: true,
            ..GlassAccess::default()
        });
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

    #[test]
    fn with_no_glass_a_backdrop_is_its_solid_fallback_and_reads_nothing() {
        let mut store = NodeStore::new();
        let panel = panel(&mut store, 0.5);
        store.set_glass_access(GlassAccess {
            solid: true,
            ..GlassAccess::default()
        });
        let out = paint(&store, panel);
        assert!(!out.iter().any(|p| matches!(p, Primitive::Frosted(_))));
        let Primitive::Quad(solid) = &out[0] else {
            panic!("the solid glass first: {out:?}");
        };
        // The standard glass's white tint all but opaque, faded by the node.
        assert_eq!((solid.color.r, solid.radius), (1.0, 12.0));
        let a = (64.0 / 255.0 + (1.0 - 64.0 / 255.0) * SOLID) * 0.5;
        assert!((solid.color.a - a).abs() < 1e-5, "{}", solid.color.a);

        let given = Rgba {
            r: 0.2,
            g: 0.3,
            b: 0.4,
            a: 1.0,
        };
        store.set_backdrop(
            panel,
            Some(Backdrop {
                fallback: Some(given),
                ..Backdrop::GLASS
            }),
        );
        let Primitive::Quad(solid) = &paint(&store, panel)[0] else {
            panic!("solid");
        };
        assert_eq!((solid.color.r, solid.color.a), (0.2, 0.5), "its fallback");
    }

    #[test]
    fn a_press_lights_interactive_glass_from_where_it_landed() {
        let mut store = NodeStore::new();
        let panel = panel(&mut store, 1.0);
        store.set_backdrop(
            panel,
            Some(Backdrop {
                interactive: true,
                ..liquid()
            }),
        );
        settle(&mut store);
        assert!(surface_of(&store, panel).glow.is_none(), "unpressed: dark");
        store.start_ripple(panel, 40.0, 30.0);
        store.tick_ripples(std::time::Duration::from_millis(120));
        let out = paint(&store, panel);
        let Primitive::Frosted(lit) = &out[0] else {
            panic!("the glass first: {out:?}");
        };
        let glow = lit.glow.expect("a press lights it");
        assert_eq!((glow.at.x, glow.at.y), (40.0, 30.0), "where it landed");
        assert!(glow.strength > 0.0 && glow.radius > 0.0);
        assert!(
            !out.iter()
                .any(|p| matches!(p, Primitive::AnalyticEllipse(_))),
            "no ink over glass: {out:?}"
        );
        // Plain glass a press reaches does not light.
        store.set_backdrop(panel, Some(liquid()));
        store.start_ripple(panel, 40.0, 30.0);
        assert!(surface_of(&store, panel).glow.is_none());
    }

    #[test]
    fn glass_turned_on_comes_in_by_its_bend_and_goes_the_same_way() {
        let mut store = NodeStore::new();
        let panel = panel(&mut store, 1.0);
        let glass = liquid();
        store.set_backdrop(panel, None);
        settle(&mut store);
        store.set_backdrop(panel, Some(glass));
        assert!(store.is_materializing());
        assert_eq!(store.drawn_glass(panel), Some((glass, 0.0)));
        let start = surface_of(&store, panel);
        assert!(!start.lens.is_on() && start.backdrop_sigma < 1.0, "flat");
        store.tick_glass(std::time::Duration::from_millis(100));
        let mid = surface_of(&store, panel);
        assert!(mid.lens.is_on() && mid.backdrop_sigma < 15.0, "{mid:?}");
        for _ in 0..5 {
            store.tick_glass(std::time::Duration::from_millis(100));
        }
        assert!(!store.is_materializing(), "settled");
        assert_eq!(surface_of(&store, panel).backdrop_sigma, 15.0);
        // Off, it still draws as it goes, then nothing.
        store.set_backdrop(panel, None);
        assert_eq!(store.drawn_glass(panel), Some((glass, 1.0)));
        for _ in 0..4 {
            store.tick_glass(std::time::Duration::from_millis(100));
        }
        assert_eq!(store.drawn_glass(panel), None);
        // Reduced motion: at once.
        store.set_reduced_motion(true);
        store.set_backdrop(panel, Some(glass));
        assert!(!store.is_materializing());
    }

    #[test]
    fn a_merging_group_melts_its_glass_children_into_one_surface() {
        use crate::component::FlexStyle;
        let mut store = NodeStore::new();
        let (row, pills) = {
            let mut cx = BuildCx::new(&mut store);
            let mut pills = Vec::new();
            cx.flex(
                FlexStyle {
                    gap: 6.0,
                    ..FlexStyle::default()
                },
                |cx| {
                    for _ in 0..3 {
                        let pill = cx.leaf(LeafStyle {
                            size: Sizing::fixed(40.0, 24.0),
                            style: BoxStyle::NONE.with_radius(12.0),
                        });
                        cx.backdrop(pill, liquid());
                        pills.push(pill.id());
                    }
                },
            );
            (cx.root().unwrap(), pills)
        };
        store.layout(row, Rect::new(0.0, 0.0, 300.0, 40.0), &mut Vec::new());
        let surfaces = |store: &NodeStore| -> Vec<FrostedMaterial> {
            paint(store, row)
                .into_iter()
                .filter_map(|p| match p {
                    Primitive::Frosted(s) => Some(*s),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(surfaces(&store).len(), 3, "apart: each its own");
        store.set_glass_merge(row, 12.0);
        let melted = surfaces(&store);
        let [one] = &melted[..] else {
            panic!("one surface: {melted:?}");
        };
        let union = one.merged.expect("the shapes melted");
        assert_eq!(union.count, 3);
        assert_eq!(union.spacing, 12.0);
        assert_eq!(union.shapes[1].0, store.world(pills[1]));
        // Different glass melts in runs of the same.
        store.set_backdrop(pills[2], Some(Backdrop::REGULAR));
        settle(&mut store);
        let runs = surfaces(&store);
        assert_eq!(runs.len(), 2, "{runs:?}");
        assert_eq!(runs[0].merged.map(|u| u.count), Some(2));
        assert!(runs[1].merged.is_none(), "a run of one is plain glass");
    }
}
