//! Background layers: the colors, gradients and images a node's box paints
//! over its fill, bottom to top, each rounded by the box's corners.
//!
//! The box's base color stays its fill, the fast path every box takes and
//! the one a background transition moves; layers are for the boxes that ask
//! for more. A gradient spans the whole box: a linear one along a CSS angle,
//! a radial one from a point of the box out to its farthest corner. An image
//! is placed by its fit and faded by its opacity, and draws nothing until it
//! is resident. The table is cold: one entry for each node with layers, kept
//! in slot order so paint finds a node's in a binary search and a tree with
//! none checks an empty table.

use crate::asset::{AssetDraw, AssetFit};
use crate::component::NodeStore;
use crate::layout::Vec2;
use crate::node::NodeId;
use viso_render::asset::ResourceId;
use viso_render::{
    Corners, ExtendMode, Gradient, GradientKind, GradientStop, InterpolationSpace, LayerClip,
    Point, Primitive, Quad, Rect, Rgba, RoundMask,
};

/// One layer of a box's background.
#[derive(Debug, Clone, PartialEq)]
pub enum FillLayer {
    /// A flat color.
    Color(Rgba),
    /// A gradient along `angle` (radians, CSS: zero up, clockwise) across
    /// the whole box.
    Linear {
        angle: f32,
        stops: Box<[GradientStop]>,
    },
    /// A circular gradient from `center` (a fraction of the box on each
    /// axis) out to the box's farthest corner.
    Radial {
        center: Vec2,
        stops: Box<[GradientStop]>,
    },
    /// An image placed by `fit` and faded by `opacity`.
    Image {
        resource: ResourceId,
        fit: AssetFit,
        opacity: f32,
    },
}

impl FillLayer {
    /// The stops of `colors` at `at` (evenly spread when `at` is empty or
    /// not one per color), in ascending order.
    pub fn stops(colors: &[Rgba], at: &[f32]) -> Box<[GradientStop]> {
        let last = colors.len().saturating_sub(1).max(1) as f32;
        let mut stops: Vec<GradientStop> = colors
            .iter()
            .enumerate()
            .map(|(i, &color)| GradientStop {
                offset: match at.len() == colors.len() {
                    true => at[i].clamp(0.0, 1.0),
                    false => i as f32 / last,
                },
                color,
            })
            .collect();
        stops.sort_by(|a, b| a.offset.total_cmp(&b.offset));
        stops.into()
    }

    fn resource(&self) -> Option<ResourceId> {
        match self {
            FillLayer::Image { resource, .. } => Some(*resource),
            _ => None,
        }
    }
}

/// The nodes whose boxes paint layers, in slot order.
#[derive(Debug, Default)]
pub(crate) struct Backgrounds {
    entries: Vec<(u32, Box<[FillLayer]>)>,
}

impl Backgrounds {
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    fn find(&self, index: u32) -> Result<usize, usize> {
        self.entries.binary_search_by_key(&index, |e| e.0)
    }

    #[inline]
    pub(crate) fn get(&self, index: u32) -> &[FillLayer] {
        if self.entries.is_empty() {
            return &[];
        }
        self.find(index).map_or(&[], |at| &self.entries[at].1)
    }

    /// Takes the layers of slot `index`, freed or replaced.
    fn take(&mut self, index: u32) -> Option<Box<[FillLayer]>> {
        let at = self.find(index).ok()?;
        Some(self.entries.remove(at).1)
    }
}

impl NodeStore {
    /// The layers node `id`'s box paints over its fill, bottom to top.
    pub fn background_layers(&self, id: NodeId) -> &[FillLayer] {
        self.backgrounds.get(id.index())
    }

    /// Makes node `id`'s box paint `layers` over its fill, bottom to top;
    /// the images among them are wanted until the node lets them go. A
    /// change repaints it. A live-guarded write — a stale handle is a no-op.
    pub fn set_background_layers(&mut self, id: NodeId, layers: Vec<FillLayer>) {
        if !self.arena().is_live(id) || self.background_layers(id) == &layers[..] {
            return;
        }
        let index = id.index();
        for resource in layers.iter().filter_map(FillLayer::resource) {
            self.assets.hold(resource, id);
        }
        self.leave_background(id);
        if !layers.is_empty() {
            let at = self.backgrounds.find(index).unwrap_err();
            self.backgrounds.entries.insert(at, (index, layers.into()));
        }
        self.mark_dirty(id, crate::dirty::DirtyClass::PAINT);
    }

    /// Drops node `id`'s layers, letting go of their images.
    pub(crate) fn leave_background(&mut self, id: NodeId) {
        let Some(layers) = self.backgrounds.take(id.index()) else {
            return;
        };
        for resource in layers.iter().filter_map(FillLayer::resource) {
            self.assets.release(resource, id);
        }
    }
}

/// The layers of a box at `world` rounded by `radius`, bottom to top.
#[cold]
pub(crate) fn paint_layers(
    store: &NodeStore,
    layers: &[FillLayer],
    world: Rect,
    radius: f32,
    out: &mut Vec<Primitive>,
) {
    if world.w <= 0.0 || world.h <= 0.0 {
        return;
    }
    let mask = (radius > 0.0).then_some(RoundMask {
        rect: world,
        radius: Corners::uniform(radius),
    });
    let center = Point::new(world.x + world.w * 0.5, world.y + world.h * 0.5);
    for layer in layers {
        match layer {
            FillLayer::Color(color) => out.push(Primitive::Quad(Quad {
                rect: world,
                color: *color,
                radius,
                border: viso_render::Border::NONE,
            })),
            FillLayer::Linear { angle, stops } => {
                // The CSS gradient line: through the center along the angle,
                // as long as the box's extent along it, so the corners take
                // the end colors.
                let (sin, cos) = angle.sin_cos();
                let half = (world.w * sin.abs() + world.h * cos.abs()) * 0.5;
                let (dx, dy) = (sin * half, -cos * half);
                gradient(
                    out,
                    world,
                    GradientKind::Linear,
                    Point::new(center.x - dx, center.y - dy),
                    Point::new(center.x + dx, center.y + dy),
                    stops,
                    mask,
                );
            }
            FillLayer::Radial { center: at, stops } => {
                let origin = Point::new(world.x + world.w * at.x, world.y + world.h * at.y);
                let reach_x = (origin.x - world.x).max(world.x + world.w - origin.x);
                let reach_y = (origin.y - world.y).max(world.y + world.h - origin.y);
                let reach = reach_x.hypot(reach_y);
                gradient(
                    out,
                    world,
                    GradientKind::Radial,
                    origin,
                    Point::new(reach, 0.0),
                    stops,
                    mask,
                );
            }
            FillLayer::Image {
                resource,
                fit,
                opacity,
            } => {
                let (draw, [w, h]) = store.assets.resolve(*resource);
                let opacity = opacity.clamp(0.0, 1.0);
                if opacity <= 0.0 {
                    continue;
                }
                let natural = Vec2::new(w, h);
                match draw {
                    // A raster fades by its tint; a vector's paths keep their
                    // colors, so it fades as a layer.
                    AssetDraw::Raster(_) => {
                        let tint = Rgba {
                            r: 1.0,
                            g: 1.0,
                            b: 1.0,
                            a: opacity,
                        };
                        crate::paint::paint_asset(
                            &draw,
                            natural,
                            *fit,
                            Some(tint),
                            false,
                            world,
                            mask,
                            out,
                        );
                    }
                    AssetDraw::Vector(_) => {
                        out.push(Primitive::Layer(LayerClip {
                            clip: world,
                            opacity,
                            blur_sigma: 0.0,
                            backdrop_sigma: 0.0,
                        }));
                        if let Some(radius) = mask.map(|m| m.radius) {
                            out.push(Primitive::ClipCorners(radius));
                        }
                        crate::paint::paint_asset(
                            &draw, natural, *fit, None, false, world, None, out,
                        );
                        out.push(Primitive::LayerEnd);
                    }
                    AssetDraw::Nothing | AssetDraw::Failed => {}
                }
            }
        }
    }
}

/// A gradient over `rect` from `p0` by `p1`, masked to the box's corners.
fn gradient(
    out: &mut Vec<Primitive>,
    rect: Rect,
    kind: GradientKind,
    p0: Point,
    p1: Point,
    stops: &[GradientStop],
    mask: Option<RoundMask>,
) {
    match stops {
        [] => {}
        // One color is that color flat.
        [only] => out.push(Primitive::Quad(Quad {
            rect,
            color: only.color,
            radius: mask.map_or(0.0, |m| m.radius.left_top),
            border: viso_render::Border::NONE,
        })),
        stops => out.push(Primitive::Gradient(Gradient {
            rect,
            kind,
            extend: ExtendMode::Clamp,
            p0,
            p1,
            stops: stops.to_vec(),
            interp: InterpolationSpace::LinearRgb,
            mask,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{BuildCx, LeafStyle};
    use crate::layout::Sizing;
    use crate::style::BoxStyle;

    fn red() -> Rgba {
        Rgba {
            r: 1.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        }
    }

    fn blue() -> Rgba {
        Rgba {
            r: 0.0,
            g: 0.0,
            b: 1.0,
            a: 1.0,
        }
    }

    fn painted(layers: Vec<FillLayer>, radius: f32) -> Vec<Primitive> {
        let mut store = NodeStore::new();
        let card = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle {
                size: Sizing::fixed(100.0, 40.0),
                style: BoxStyle::solid(Rgba::TRANSPARENT).with_radius(radius),
            })
            .id()
        };
        store.set_background_layers(card, layers);
        store.layout(card, Rect::new(0.0, 0.0, 100.0, 40.0), &mut Vec::new());
        let mut out = Vec::new();
        crate::paint::paint_tree(&store, card, &mut out);
        out
    }

    #[test]
    fn stops_spread_evenly_unless_placed_one_per_color() {
        let even = FillLayer::stops(&[red(), blue(), red()], &[]);
        let at: Vec<f32> = even.iter().map(|s| s.offset).collect();
        assert_eq!(at, [0.0, 0.5, 1.0]);
        let placed = FillLayer::stops(&[red(), blue()], &[0.8, 0.2]);
        assert_eq!(placed[0].offset, 0.2, "sorted");
        assert_eq!(placed[0].color, blue());
        let odd = FillLayer::stops(&[red(), blue()], &[0.3]);
        assert_eq!(odd[1].offset, 1.0, "not one per color: even");
    }

    #[test]
    fn a_linear_layer_runs_the_css_line_and_rounds_with_the_box() {
        let stops = FillLayer::stops(&[red(), blue()], &[]);
        // `90deg` runs left to right across the whole width.
        let out = painted(
            vec![FillLayer::Linear {
                angle: core::f32::consts::FRAC_PI_2,
                stops: stops.clone(),
            }],
            8.0,
        );
        let [Primitive::Gradient(g)] = &out[..] else {
            panic!("one gradient: {out:?}");
        };
        assert!(
            (g.p0.x - 0.0).abs() < 1e-3 && (g.p0.y - 20.0).abs() < 1e-3,
            "{:?}",
            g.p0
        );
        assert!(
            (g.p1.x - 100.0).abs() < 1e-3 && (g.p1.y - 20.0).abs() < 1e-3,
            "{:?}",
            g.p1
        );
        assert_eq!(g.mask.unwrap().radius, Corners::uniform(8.0));
        // `180deg`, the default, runs top to bottom.
        let out = painted(
            vec![FillLayer::Linear {
                angle: core::f32::consts::PI,
                stops,
            }],
            0.0,
        );
        let [Primitive::Gradient(g)] = &out[..] else {
            panic!("one gradient: {out:?}");
        };
        assert!((g.p0.y - 0.0).abs() < 1e-3 && (g.p1.y - 40.0).abs() < 1e-3);
        assert_eq!(g.mask, None, "a sharp box masks nothing");
    }

    #[test]
    fn a_radial_layer_reaches_the_farthest_corner_and_layers_paint_in_order() {
        let out = painted(
            vec![
                FillLayer::Color(blue()),
                FillLayer::Radial {
                    center: Vec2::new(0.0, 0.0),
                    stops: FillLayer::stops(&[red(), Rgba::TRANSPARENT], &[]),
                },
            ],
            0.0,
        );
        let [Primitive::Quad(q), Primitive::Gradient(g)] = &out[..] else {
            panic!("a color, then the gradient: {out:?}");
        };
        assert_eq!(q.color, blue());
        assert_eq!((g.p0.x, g.p0.y), (0.0, 0.0));
        assert!((g.p1.x - 100.0f32.hypot(40.0)).abs() < 1e-3);
    }
}
