//! Building a vector tile's mesh from its decoded features and a style.
//!
//! Each fill, line and circle layer of the style, in order, draws the
//! features of its source layer that pass its filter at the build's zoom,
//! into one mesh for the whole tile in tile units of [`EXTENT`] across.
//! Features no layer draws are never decoded past their properties.

use std::collections::HashMap;

use viso_render::MeshVertex;

use crate::mvt::{self, Feature, GeomType, Value, VectorTile};
use crate::style::{Paint, Properties, Style};
use crate::tess::clip::{self, Square};
use crate::tess::{LineStyle, MeshBuilder};

/// The side of a tile in the units its mesh is built in.
pub const EXTENT: f32 = 4096.0;

/// How far past its tile a line is kept, in tile units: lines drawn by the
/// next tile over meet these without a seam.
const LINE_MARGIN: f32 = EXTENT / 16.0;

/// What a build needs besides the tile.
#[derive(Debug, Clone, Copy)]
pub struct BuildParams<'a> {
    pub style: &'a Style,
    /// The zoom the style's values are taken at.
    pub zoom: f32,
    /// The tile's side on screen at that zoom, in points.
    pub tile_points: f32,
}

/// A built tile: its mesh in tile units.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TileMesh {
    pub vertices: Vec<MeshVertex>,
    pub indices: Vec<u32>,
}

impl TileMesh {
    /// The bytes the mesh holds.
    pub fn bytes(&self) -> usize {
        self.vertices.len() * std::mem::size_of::<MeshVertex>() + self.indices.len() * 4
    }

    /// Whether it draws nothing.
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
}

/// A feature of a tile layer, read through its layer's tables.
pub struct FeatureRef<'a> {
    pub layer: &'a mvt::Layer,
    pub feature: &'a Feature,
    keys: &'a HashMap<&'a str, u32>,
}

impl<'a> FeatureRef<'a> {
    /// Feature `feature` of `layer`, whose keys `keys` indexes.
    pub fn new(
        layer: &'a mvt::Layer,
        feature: &'a Feature,
        keys: &'a HashMap<&'a str, u32>,
    ) -> Self {
        Self {
            layer,
            feature,
            keys,
        }
    }
}

impl Properties for FeatureRef<'_> {
    fn get(&self, key: &str) -> Option<&Value> {
        self.layer.value_at(self.feature, *self.keys.get(key)?)
    }

    fn kind(&self) -> GeomType {
        self.feature.kind
    }
}

/// A layer's key table as a map.
pub fn key_index(layer: &mvt::Layer) -> HashMap<&str, u32> {
    layer
        .keys
        .iter()
        .enumerate()
        .map(|(i, k)| (k.as_str(), i as u32))
        .collect()
}

/// A layer's features' geometry, decoded once when first asked for and
/// scaled to [`EXTENT`].
struct Decoded {
    scale: f32,
    geometry: Vec<Option<Option<Shape>>>,
}

/// A geometry's points and the ends of its parts.
type Shape = (Vec<[f32; 2]>, Vec<u32>);

impl Decoded {
    fn new(layer: &mvt::Layer) -> Self {
        Self {
            scale: EXTENT / layer.extent as f32,
            geometry: vec![None; layer.features.len()],
        }
    }

    fn get(&mut self, layer: &mvt::Layer, i: usize) -> Option<&Shape> {
        let scale = self.scale;
        self.geometry[i]
            .get_or_insert_with(|| {
                let g = layer.features[i].geometry().ok()?;
                let points = g
                    .points
                    .iter()
                    .map(|p| [p[0] as f32 * scale, p[1] as f32 * scale])
                    .collect();
                Some((points, g.ends))
            })
            .as_ref()
    }
}

/// The parts of a decoded geometry.
fn parts<'g>(points: &'g [[f32; 2]], ends: &'g [u32]) -> impl Iterator<Item = &'g [[f32; 2]]> + 'g {
    let mut start = 0;
    ends.iter().map(move |&end| {
        let part = &points[start..end as usize];
        start = end as usize;
        part
    })
}

/// Twice a ring's signed area, positive clockwise on screen.
fn area(ring: &[[f32; 2]]) -> f32 {
    let n = ring.len();
    (0..n)
        .map(|i| {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum()
}

/// The polygons of a polygon geometry: each exterior ring with the holes
/// after it.
pub(crate) fn polygons<'g>(
    points: &'g [[f32; 2]],
    ends: &'g [u32],
    mut each: impl FnMut(&[&'g [[f32; 2]]]),
) {
    let mut rings: Vec<&[[f32; 2]]> = Vec::new();
    for ring in parts(points, ends) {
        if ring.len() < 3 {
            continue;
        }
        if area(ring) > 0.0 && !rings.is_empty() {
            each(&rings);
            rings.clear();
        }
        rings.push(ring);
    }
    if !rings.is_empty() {
        each(&rings);
    }
}

/// Build `tile`'s mesh by `params`, with `builder`'s scratch.
pub fn build_vector(
    tile: &VectorTile,
    params: &BuildParams<'_>,
    builder: &mut MeshBuilder,
) -> TileMesh {
    let layers: Vec<&mvt::Layer> = tile.layers.iter().collect();
    build_layers(&layers, params, builder)
}

/// Build the mesh of a tile whose layers are `tile_layers` by `params`.
pub fn build_layers(
    tile_layers: &[&mvt::Layer],
    params: &BuildParams<'_>,
    builder: &mut MeshBuilder,
) -> TileMesh {
    build_layers_in(tile_layers, params, 0..usize::MAX, builder)
}

/// [`build_layers`] of the style's layers in `range` alone.
pub fn build_layers_in(
    tile_layers: &[&mvt::Layer],
    params: &BuildParams<'_>,
    range: std::ops::Range<usize>,
    builder: &mut MeshBuilder,
) -> TileMesh {
    builder.clear();
    let square = Square {
        lo: 0.0,
        hi: EXTENT,
    };
    let lines = Square {
        lo: -LINE_MARGIN,
        hi: EXTENT + LINE_MARGIN,
    };
    let units_per_point = EXTENT / params.tile_points.max(1.0);
    let zoom = params.zoom;
    let mut layers: Vec<Option<(HashMap<&str, u32>, Decoded)>> = Vec::new();
    layers.resize_with(tile_layers.len(), || None);
    for (index, style_layer) in params.style.layers.iter().enumerate() {
        if !range.contains(&index) || !style_layer.source.is_empty() || !style_layer.shows_at(zoom)
        {
            continue;
        }
        if !matches!(
            style_layer.paint,
            Paint::Fill { .. }
                | Paint::Extrusion { .. }
                | Paint::Line { .. }
                | Paint::Circle { .. }
        ) {
            continue;
        }
        let Some(li) = tile_layers
            .iter()
            .position(|l| l.name == style_layer.source_layer)
        else {
            continue;
        };
        let layer = tile_layers[li];
        let (keys, decoded) =
            layers[li].get_or_insert_with(|| (key_index(layer), Decoded::new(layer)));
        for (fi, feature) in layer.features.iter().enumerate() {
            let props = FeatureRef::new(layer, feature, keys);
            if !style_layer.filter.test(&props) {
                continue;
            }
            match &style_layer.paint {
                Paint::Fill { color, opacity } | Paint::Extrusion { color, opacity, .. } => {
                    if feature.kind != GeomType::Polygon {
                        continue;
                    }
                    let c = color.eval(zoom, &props).linear(opacity.eval(zoom, &props));
                    if c[3] <= 0.0 {
                        continue;
                    }
                    let Some((points, ends)) = decoded.get(layer, fi) else {
                        continue;
                    };
                    polygons(points, ends, |rings| {
                        builder.fill(rings.iter().copied(), square, c);
                    });
                }
                Paint::Line {
                    color,
                    width,
                    opacity,
                    dashes,
                    join,
                    cap,
                } => {
                    let c = color.eval(zoom, &props).linear(opacity.eval(zoom, &props));
                    let w = width.eval(zoom, &props);
                    if c[3] <= 0.0 || w <= 0.0 || feature.kind == GeomType::Point {
                        continue;
                    }
                    let style = LineStyle {
                        width: w,
                        color: c,
                        join: *join,
                        cap: *cap,
                        miter_limit: 2.0,
                        dashes,
                        units_per_point,
                    };
                    let Some((points, ends)) = decoded.get(layer, fi) else {
                        continue;
                    };
                    if feature.kind == GeomType::Polygon {
                        outline(points, ends, square, &style, builder);
                    } else {
                        for part in parts(points, ends) {
                            builder.line(part, lines, &style);
                        }
                    }
                }
                Paint::Circle {
                    radius,
                    color,
                    stroke_width,
                    stroke_color,
                } => {
                    if feature.kind != GeomType::Point {
                        continue;
                    }
                    let r = radius.eval(zoom, &props);
                    let c = color.eval(zoom, &props).linear(1.0);
                    let sw = stroke_width.eval(zoom, &props);
                    let stroke =
                        (sw > 0.0).then(|| (sw, stroke_color.eval(zoom, &props).linear(1.0)));
                    let Some((points, _)) = decoded.get(layer, fi) else {
                        continue;
                    };
                    for &p in points {
                        if square.contains(p) {
                            builder.circle(p, r, c, stroke);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    TileMesh {
        vertices: std::mem::take(&mut builder.vertices),
        indices: std::mem::take(&mut builder.indices),
    }
}

/// Stroke a polygon's rings, clipped to `square`, leaving out the edges
/// the clip laid along the square's sides.
fn outline(
    points: &[[f32; 2]],
    ends: &[u32],
    square: Square,
    style: &LineStyle<'_>,
    builder: &mut MeshBuilder,
) {
    let (mut ring, mut scratch, mut run) = (Vec::new(), Vec::new(), Vec::new());
    for part in parts(points, ends) {
        clip::clip_ring(part, square, &mut ring, &mut scratch);
        let n = ring.len();
        if n < 2 {
            continue;
        }
        // Start the walk after an edge on a side, so each run is whole.
        let start = (0..n)
            .find(|&i| square.on_side(ring[i], ring[(i + 1) % n]))
            .map_or(0, |i| i + 1);
        run.clear();
        for k in 0..=n {
            let a = ring[(start + k) % n];
            let b = ring[(start + k + 1) % n];
            if k == n || square.on_side(a, b) {
                if run.len() >= 2 {
                    builder.line(&run, square, style);
                }
                run.clear();
                continue;
            }
            if run.is_empty() {
                run.push(a);
            }
            run.push(b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mvt::TileEncoder;
    use crate::style::{Color, Filter, Layer};

    fn tile() -> VectorTile {
        let mut t = TileEncoder::new();
        let mut water = t.layer("water", 4096);
        let sea: &[[i32; 2]] = &[[-64, -64], [4160, -64], [4160, 2048], [-64, 2048]];
        water.feature(
            None,
            GeomType::Polygon,
            &[sea],
            &[("class", "ocean".into())],
        );
        let lake: &[[i32; 2]] = &[[100, 3000], [400, 3000], [400, 3300], [100, 3300]];
        water.feature(
            None,
            GeomType::Polygon,
            &[lake],
            &[("class", "lake".into())],
        );
        water.finish();
        let mut roads = t.layer("transportation", 4096);
        let road: &[[i32; 2]] = &[[0, 3000], [4096, 3000]];
        roads.feature(
            None,
            GeomType::Line,
            &[road],
            &[("class", "primary".into())],
        );
        roads.finish();
        let mut pois = t.layer("poi", 2048);
        pois.feature(None, GeomType::Point, &[&[[1024, 1024]]], &[]);
        pois.finish();
        mvt::decode(&t.finish()).unwrap()
    }

    fn style() -> Style {
        Style::new("test", Color::WHITE)
            .with(
                Layer::fill("sea", "water", Color::hex(0x0000ff))
                    .filter(Filter::eq("class", "ocean")),
            )
            .with(Layer::line("roads", "transportation", Color::BLACK, 4.0))
            .with(Layer::circle("dots", "poi", 3.0, Color::BLACK))
            .with(Layer::line("hidden", "transportation", Color::BLACK, 4.0).zooms(16.0, 24.0))
    }

    #[test]
    fn layers_draw_their_filtered_features_in_order() {
        let style = style();
        let params = BuildParams {
            style: &style,
            zoom: 12.0,
            tile_points: 512.0,
        };
        let mesh = build_vector(&tile(), &params, &mut MeshBuilder::new());
        assert!(!mesh.is_empty());
        let blue = mesh.vertices.iter().filter(|v| v.color[2] > 0.9).count();
        // The sea's clipped ring and its fringe: the lake is filtered out.
        assert_eq!(blue, 8);
        // The sea comes first, the road over it.
        assert!(mesh.vertices[..blue].iter().all(|v| v.color[2] > 0.9));
        // The sea is clipped to the tile and fringed only on its open side.
        assert!(
            mesh.vertices[..blue]
                .iter()
                .all(|v| v.pos[0] >= 0.0 && v.pos[0] <= EXTENT && v.pos[1] >= 0.0)
        );
        let dots: Vec<_> = mesh
            .vertices
            .iter()
            .filter(|v| v.pos == [2048.0, 2048.0])
            .collect();
        assert!(
            !dots.is_empty(),
            "the 2048-extent layer scales to the build's units"
        );
        assert!(mesh.bytes() > 0);
    }

    #[test]
    fn polygon_outlines_skip_the_cut_sides() {
        let style = Style::new("t", Color::WHITE).with(
            Layer::line("edge", "water", Color::BLACK, 1.0).filter(Filter::eq("class", "ocean")),
        );
        let params = BuildParams {
            style: &style,
            zoom: 12.0,
            tile_points: 512.0,
        };
        let mesh = build_vector(&tile(), &params, &mut MeshBuilder::new());
        // Only the sea's southern edge, at y = 2048, is stroked.
        assert!(!mesh.is_empty());
        assert!(
            mesh.vertices.iter().all(|v| v.pos[1] == 2048.0),
            "{:?}",
            mesh.vertices.iter().map(|v| v.pos).collect::<Vec<_>>()
        );
    }
}
