//! Which drawn features lie under a point: the style's layers tested from
//! the top down against a tile's decoded features, by what each draws — a
//! fill by its area, a line by its width, a circle by its radius.

use std::collections::HashSet;

use crate::build::{FeatureRef, key_index};
use crate::mvt::{self, GeomType, Value};
use crate::style::{Paint, Style};

/// A feature drawn under a queried point.
#[derive(Debug, Clone, PartialEq)]
pub struct QueriedFeature {
    /// The style layer that drew it.
    pub layer: String,
    /// The tile layer it came from.
    pub source_layer: String,
    pub id: Option<u64>,
    pub kind: GeomType,
    pub properties: Vec<(String, Value)>,
}

/// How far `p` is from the segment `a`–`b`.
fn to_segment(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let d = [b[0] - a[0], b[1] - a[1]];
    let len2 = d[0] * d[0] + d[1] * d[1];
    let t = if len2 > 0.0 {
        (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p[0] - a[0] - d[0] * t).hypot(p[1] - a[1] - d[1] * t)
}

/// Whether `p` lies inside the rings `parts` by the even-odd rule.
fn inside(p: [f64; 2], parts: &[Vec<[f64; 2]>]) -> bool {
    let mut odd = false;
    for ring in parts {
        let n = ring.len();
        for i in 0..n {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            if (a[1] > p[1]) != (b[1] > p[1])
                && p[0] < a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0])
            {
                odd = !odd;
            }
        }
    }
    odd
}

/// The features of a tile's `layers` that `style` draws at `zoom` under
/// `at`, given in tile units of `extent` across, within `slop` tile units;
/// `units_per_point` converts the style's point sizes. Topmost first, each
/// feature once, into `out`.
pub fn query_tile(
    layers: &[&mvt::Layer],
    style: &Style,
    zoom: f32,
    at: [f64; 2],
    slop: f64,
    units_per_point: f64,
    out: &mut Vec<QueriedFeature>,
) {
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    for style_layer in style.layers.iter().rev() {
        if !style_layer.source.is_empty() || !style_layer.shows_at(zoom) {
            continue;
        }
        let Some(li) = layers
            .iter()
            .position(|l| l.name == style_layer.source_layer)
        else {
            continue;
        };
        let layer = layers[li];
        let keys = key_index(layer);
        let scale = f64::from(crate::build::EXTENT) / f64::from(layer.extent.max(1));
        for (fi, feature) in layer.features.iter().enumerate() {
            if seen.contains(&(li, fi)) {
                continue;
            }
            let props = FeatureRef::new(layer, feature, &keys);
            if !style_layer.filter.test(&props) {
                continue;
            }
            let reach = match &style_layer.paint {
                Paint::Fill { .. } | Paint::Extrusion { .. }
                    if feature.kind == GeomType::Polygon =>
                {
                    Some(0.0)
                }
                Paint::Line { width, .. } if feature.kind != GeomType::Point => {
                    Some(f64::from(width.eval(zoom, &props)) / 2.0 * units_per_point)
                }
                Paint::Circle {
                    radius,
                    stroke_width,
                    ..
                } => Some(
                    f64::from(radius.eval(zoom, &props) + stroke_width.eval(zoom, &props))
                        * units_per_point,
                ),
                _ => None,
            };
            let Some(reach) = reach else {
                continue;
            };
            let Ok(geometry) = feature.geometry() else {
                continue;
            };
            let parts: Vec<Vec<[f64; 2]>> = geometry
                .parts()
                .map(|p| {
                    p.iter()
                        .map(|q| [f64::from(q[0]) * scale, f64::from(q[1]) * scale])
                        .collect()
                })
                .collect();
            let near = |closed: bool| {
                parts.iter().any(|part| {
                    let n = part.len();
                    if n == 1 {
                        return (part[0][0] - at[0]).hypot(part[0][1] - at[1]) <= reach + slop;
                    }
                    let segments = if closed { n } else { n - 1 };
                    (0..segments)
                        .any(|i| to_segment(at, part[i], part[(i + 1) % n]) <= reach + slop)
                })
            };
            let hit = match (&style_layer.paint, feature.kind) {
                (Paint::Fill { .. } | Paint::Extrusion { .. }, _) => {
                    inside(at, &parts) || near(true)
                }
                (Paint::Line { .. }, GeomType::Polygon) => near(true),
                _ => near(false),
            };
            if hit {
                seen.insert((li, fi));
                out.push(QueriedFeature {
                    layer: style_layer.id.clone(),
                    source_layer: layer.name.clone(),
                    id: feature.id,
                    kind: feature.kind,
                    properties: layer
                        .properties(feature)
                        .map(|(k, v)| (k.to_owned(), v.clone()))
                        .collect(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mvt::TileEncoder;
    use crate::style::{Color, Layer, Prop};

    fn tile() -> mvt::VectorTile {
        let mut t = TileEncoder::new();
        let mut l = t.layer("park", 4096);
        let sq: &[[i32; 2]] = &[[1000, 1000], [3000, 1000], [3000, 3000], [1000, 3000]];
        let hole: &[[i32; 2]] = &[[1800, 1800], [1800, 2200], [2200, 2200], [2200, 1800]];
        l.feature(
            Some(1),
            GeomType::Polygon,
            &[sq, hole],
            &[("name", "Tiergarten".into())],
        );
        l.finish();
        let mut l = t.layer("road", 4096);
        let line: &[[i32; 2]] = &[[0, 500], [4096, 500]];
        l.feature(
            Some(2),
            GeomType::Line,
            &[line],
            &[("class", "primary".into())],
        );
        l.finish();
        mvt::decode(&t.finish()).unwrap()
    }

    fn style() -> Style {
        Style::new("q", Color::WHITE)
            .with(Layer::fill("park", "park", Color::BLACK))
            .with(Layer::line(
                "road-casing",
                "road",
                Color::BLACK,
                Prop::Const(10.0),
            ))
            .with(Layer::line("road", "road", Color::WHITE, Prop::Const(6.0)))
    }

    fn ids(at: [f64; 2], slop: f64) -> Vec<(String, Option<u64>)> {
        let t = tile();
        let layers: Vec<&mvt::Layer> = t.layers.iter().collect();
        let mut out = Vec::new();
        // Eight tile units a point.
        query_tile(&layers, &style(), 14.0, at, slop, 8.0, &mut out);
        out.into_iter().map(|f| (f.layer, f.id)).collect()
    }

    #[test]
    fn a_point_finds_the_area_under_it_but_not_in_its_hole() {
        assert_eq!(ids([1500.0, 1500.0], 0.0), [("park".to_owned(), Some(1))]);
        assert!(ids([2000.0, 2000.0], 0.0).is_empty());
        assert!(ids([3500.0, 3500.0], 0.0).is_empty());
        assert_eq!(ids([3500.0, 3500.0], 720.0).len(), 1);
    }

    #[test]
    fn a_line_is_found_within_its_width_and_once_for_its_layers() {
        // The casing reaches 40 units each side, the line 24: the topmost
        // layer under the point reports the road, once.
        assert_eq!(ids([100.0, 520.0], 0.0), [("road".to_owned(), Some(2))]);
        assert_eq!(
            ids([100.0, 535.0], 0.0),
            [("road-casing".to_owned(), Some(2))]
        );
        assert!(ids([100.0, 545.0], 0.0).is_empty());
        let t = tile();
        let layers: Vec<&mvt::Layer> = t.layers.iter().collect();
        let mut out = Vec::new();
        query_tile(&layers, &style(), 14.0, [100.0, 500.0], 0.0, 8.0, &mut out);
        assert_eq!(
            out[0].properties,
            vec![("class".to_owned(), Value::from("primary"))]
        );
    }
}
