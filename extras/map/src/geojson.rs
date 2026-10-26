//! GeoJSON an app shows like tile data: its features become a layer of each
//! tile they cross, so the style's layers draw them.
//!
//! [`GeoJson::parse`] reads a FeatureCollection, a Feature or a bare
//! geometry (RFC 7946); [`DataLayer`] holds the features in world units with
//! their bounds and cuts out what crosses one tile — clipped to the tile
//! and a margin, in the tile's units — when that tile builds.

use viso_ende::JsonValue;

use crate::build::EXTENT;
use crate::geo::{LngLat, WorldPoint, tile_square};
use crate::mvt::{self, GeomType, TileEncoder, Value, ring_area};
use crate::source::TileId;

/// How far past its tile a feature is kept, as a fraction of the tile.
const MARGIN: f64 = 1.0 / 8.0;

/// A feature's geometry; the multi kinds hold several parts.
#[derive(Debug, Clone, PartialEq)]
pub enum Geometry {
    Points(Vec<LngLat>),
    Lines(Vec<Vec<LngLat>>),
    /// Polygons, each its outer ring and then its holes.
    Polygons(Vec<Vec<Vec<LngLat>>>),
}

/// One feature: its geometry and properties.
#[derive(Debug, Clone, PartialEq)]
pub struct GeoFeature {
    pub id: Option<u64>,
    pub geometry: Geometry,
    pub properties: Vec<(String, Value)>,
}

impl GeoFeature {
    /// A feature of `geometry` with no properties.
    pub fn new(geometry: Geometry) -> Self {
        Self {
            id: None,
            geometry,
            properties: Vec::new(),
        }
    }

    /// With property `key` set to `value`.
    pub fn with(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.properties.push((key.to_owned(), value.into()));
        self
    }

    /// With id `id`.
    pub fn id(mut self, id: u64) -> Self {
        self.id = Some(id);
        self
    }
}

/// Features read from GeoJSON or made in code.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GeoJson {
    pub features: Vec<GeoFeature>,
}

/// Why GeoJSON did not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeoJsonError(pub String);

impl std::fmt::Display for GeoJsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GeoJSON: {}", self.0)
    }
}

impl std::error::Error for GeoJsonError {}

fn bad(what: &str) -> GeoJsonError {
    GeoJsonError(what.to_owned())
}

fn position(v: &JsonValue) -> Result<LngLat, GeoJsonError> {
    let a = v
        .as_array()
        .ok_or_else(|| bad("a position is not an array"))?;
    match (
        a.first().and_then(JsonValue::as_f64),
        a.get(1).and_then(JsonValue::as_f64),
    ) {
        (Some(lon), Some(lat)) => Ok(LngLat::new(lon, lat)),
        _ => Err(bad("a position lacks its longitude or latitude")),
    }
}

fn positions(v: &JsonValue) -> Result<Vec<LngLat>, GeoJsonError> {
    v.as_array()
        .ok_or_else(|| bad("coordinates are not an array"))?
        .iter()
        .map(position)
        .collect()
}

fn rings(v: &JsonValue) -> Result<Vec<Vec<LngLat>>, GeoJsonError> {
    v.as_array()
        .ok_or_else(|| bad("rings are not an array"))?
        .iter()
        .map(|r| {
            let mut ring = positions(r)?;
            // GeoJSON closes rings; tile rings are open.
            if ring.len() > 1 && ring.first() == ring.last() {
                ring.pop();
            }
            Ok(ring)
        })
        .collect()
}

fn geometry(v: &JsonValue, out: &mut Vec<Geometry>) -> Result<(), GeoJsonError> {
    let kind = v
        .get("type")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| bad("a geometry has no type"))?;
    if kind == "GeometryCollection" {
        for g in v
            .get("geometries")
            .and_then(JsonValue::as_array)
            .unwrap_or(&[])
        {
            geometry(g, out)?;
        }
        return Ok(());
    }
    let c = v
        .get("coordinates")
        .ok_or_else(|| bad("a geometry has no coordinates"))?;
    fn each(c: &JsonValue) -> Result<&[JsonValue], GeoJsonError> {
        c.as_array()
            .ok_or_else(|| bad("coordinates are not an array"))
    }
    out.push(match kind {
        "Point" => Geometry::Points(vec![position(c)?]),
        "MultiPoint" => Geometry::Points(positions(c)?),
        "LineString" => Geometry::Lines(vec![positions(c)?]),
        "MultiLineString" => {
            Geometry::Lines(each(c)?.iter().map(positions).collect::<Result<_, _>>()?)
        }
        "Polygon" => Geometry::Polygons(vec![rings(c)?]),
        "MultiPolygon" => Geometry::Polygons(each(c)?.iter().map(rings).collect::<Result<_, _>>()?),
        other => return Err(GeoJsonError(format!("unknown geometry type {other}"))),
    });
    Ok(())
}

fn value(v: &JsonValue) -> Option<Value> {
    match v {
        JsonValue::String(s) => Some(Value::String(s.clone())),
        JsonValue::Number(n) => Some(Value::Number(*n)),
        JsonValue::Bool(b) => Some(Value::Bool(*b)),
        _ => None,
    }
}

fn feature(v: &JsonValue, out: &mut Vec<GeoFeature>) -> Result<(), GeoJsonError> {
    let id = v
        .get("id")
        .and_then(JsonValue::as_f64)
        .and_then(|n| (n >= 0.0 && n.fract() == 0.0).then_some(n as u64));
    let properties: Vec<(String, Value)> = match v.get("properties") {
        Some(JsonValue::Object(fields)) => fields
            .iter()
            .filter_map(|(k, v)| Some((k.clone(), value(v)?)))
            .collect(),
        _ => Vec::new(),
    };
    let Some(g) = v.get("geometry").filter(|g| !g.is_null()) else {
        return Ok(());
    };
    let mut parts = Vec::new();
    geometry(g, &mut parts)?;
    for geometry in parts {
        out.push(GeoFeature {
            id,
            geometry,
            properties: properties.clone(),
        });
    }
    Ok(())
}

impl GeoJson {
    /// Features made in code.
    pub fn new(features: impl Into<Vec<GeoFeature>>) -> Self {
        Self {
            features: features.into(),
        }
    }

    /// Reads `text`: a FeatureCollection, a Feature or a geometry.
    pub fn parse(text: &str) -> Result<Self, GeoJsonError> {
        let v = JsonValue::parse(text).map_err(|e| GeoJsonError(format!("{e:?}")))?;
        let mut features = Vec::new();
        match v.get("type").and_then(JsonValue::as_str) {
            Some("FeatureCollection") => {
                for f in v
                    .get("features")
                    .and_then(JsonValue::as_array)
                    .ok_or_else(|| bad("a FeatureCollection has no features"))?
                {
                    feature(f, &mut features)?;
                }
            }
            Some("Feature") => feature(&v, &mut features)?,
            Some(_) => {
                let mut parts = Vec::new();
                geometry(&v, &mut parts)?;
                features.extend(parts.into_iter().map(GeoFeature::new));
            }
            None => return Err(bad("no type")),
        }
        Ok(Self { features })
    }
}

/// One feature held in world units.
#[derive(Debug)]
struct Held {
    id: Option<u64>,
    kind: GeomType,
    /// Polygons start a new polygon at each part flagged true.
    parts: Vec<(bool, Vec<WorldPoint>)>,
    lo: WorldPoint,
    hi: WorldPoint,
    properties: Vec<(String, Value)>,
}

/// GeoJSON features ready to be cut into tiles, as a layer named `name`.
#[derive(Debug)]
pub struct DataLayer {
    pub name: String,
    features: Vec<Held>,
}

/// Clips the ring `ring` (tile units) to the square `lo..hi` on both axes.
fn clip_ring(ring: &[[f64; 2]], lo: f64, hi: f64) -> Vec<[f64; 2]> {
    let mut out: Vec<[f64; 2]> = ring.to_vec();
    for side in 0..4 {
        let axis = side % 2;
        let keep = |p: &[f64; 2]| {
            if side < 2 {
                p[axis] >= lo
            } else {
                p[axis] <= hi
            }
        };
        let edge = if side < 2 { lo } else { hi };
        let input = std::mem::take(&mut out);
        let n = input.len();
        for i in 0..n {
            let (a, b) = (input[i], input[(i + 1) % n]);
            let (ka, kb) = (keep(&a), keep(&b));
            if ka {
                out.push(a);
            }
            if ka != kb {
                let t = (edge - a[axis]) / (b[axis] - a[axis]);
                out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
            }
        }
        if out.is_empty() {
            break;
        }
    }
    out
}

/// The runs of `line` (tile units) inside the square `lo..hi`.
fn clip_line(line: &[[f64; 2]], lo: f64, hi: f64, mut run: impl FnMut(&[[f64; 2]])) {
    let mut cur: Vec<[f64; 2]> = Vec::new();
    for w in line.windows(2) {
        let (a, b) = (w[0], w[1]);
        let d = [b[0] - a[0], b[1] - a[1]];
        let (mut t0, mut t1) = (0.0f64, 1.0f64);
        let mut inside = true;
        for axis in 0..2 {
            for (p, q) in [(-d[axis], a[axis] - lo), (d[axis], hi - a[axis])] {
                if p == 0.0 {
                    if q < 0.0 {
                        inside = false;
                    }
                } else {
                    let r = q / p;
                    if p < 0.0 {
                        t0 = t0.max(r);
                    } else {
                        t1 = t1.min(r);
                    }
                }
            }
        }
        if !inside || t0 > t1 {
            if cur.len() >= 2 {
                run(&cur);
            }
            cur.clear();
            continue;
        }
        let at = |t: f64| [a[0] + d[0] * t, a[1] + d[1] * t];
        if cur.is_empty() || t0 > 0.0 {
            if cur.len() >= 2 {
                run(&cur);
            }
            cur.clear();
            cur.push(at(t0));
        }
        cur.push(at(t1));
        if t1 < 1.0 {
            run(&cur);
            cur.clear();
        }
    }
    if cur.len() >= 2 {
        run(&cur);
    }
}

fn rounded(points: &[[f64; 2]]) -> Vec<[i32; 2]> {
    let mut out: Vec<[i32; 2]> = Vec::with_capacity(points.len());
    for p in points {
        let q = [p[0].round() as i32, p[1].round() as i32];
        if out.last() != Some(&q) {
            out.push(q);
        }
    }
    out
}

impl DataLayer {
    /// `data`'s features as the layer `name`.
    pub fn new(name: &str, data: &GeoJson) -> Self {
        let features = data
            .features
            .iter()
            .filter_map(|f| {
                let (kind, parts): (GeomType, Vec<(bool, &[LngLat])>) = match &f.geometry {
                    Geometry::Points(p) => (GeomType::Point, vec![(false, p.as_slice())]),
                    Geometry::Lines(l) => (
                        GeomType::Line,
                        l.iter().map(|l| (false, l.as_slice())).collect(),
                    ),
                    Geometry::Polygons(p) => (
                        GeomType::Polygon,
                        p.iter()
                            .flat_map(|rings| {
                                rings
                                    .iter()
                                    .enumerate()
                                    .map(|(i, r)| (i == 0, r.as_slice()))
                            })
                            .collect(),
                    ),
                };
                let mut lo = WorldPoint::new(f64::INFINITY, f64::INFINITY);
                let mut hi = WorldPoint::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
                let parts: Vec<(bool, Vec<WorldPoint>)> = parts
                    .into_iter()
                    .map(|(outer, p)| {
                        let w: Vec<WorldPoint> = p.iter().map(|p| p.to_world()).collect();
                        for p in &w {
                            lo = WorldPoint::new(lo.x.min(p.x), lo.y.min(p.y));
                            hi = WorldPoint::new(hi.x.max(p.x), hi.y.max(p.y));
                        }
                        (outer, w)
                    })
                    .collect();
                lo.x.is_finite().then(|| Held {
                    id: f.id,
                    kind,
                    parts,
                    lo,
                    hi,
                    properties: f.properties.clone(),
                })
            })
            .collect();
        Self {
            name: name.to_owned(),
            features,
        }
    }

    /// What of the layer crosses `tile`, as that tile's layer; `None` when
    /// nothing does.
    pub fn slice(&self, tile: TileId) -> Option<mvt::Layer> {
        let (nw, side) = tile_square(tile);
        let pad = side * MARGIN;
        let extent = f64::from(EXTENT);
        let (lo, hi) = (-extent * MARGIN, extent * (1.0 + MARGIN));
        let k = extent / side;
        let mut enc = TileEncoder::new();
        let mut layer = enc.layer(&self.name, EXTENT as u32);
        let mut any = false;
        let mut local: Vec<[f64; 2]> = Vec::new();
        for f in &self.features {
            if f.hi.x < nw.x - pad
                || f.lo.x > nw.x + side + pad
                || f.hi.y < nw.y - pad
                || f.lo.y > nw.y + side + pad
            {
                continue;
            }
            let mut out: Vec<Vec<[i32; 2]>> = Vec::new();
            for (outer, part) in &f.parts {
                local.clear();
                local.extend(part.iter().map(|p| [(p.x - nw.x) * k, (p.y - nw.y) * k]));
                match f.kind {
                    GeomType::Point => {
                        for p in &local {
                            if p[0] >= 0.0 && p[0] < extent && p[1] >= 0.0 && p[1] < extent {
                                out.push(rounded(&[*p]));
                            }
                        }
                    }
                    GeomType::Line => {
                        clip_line(&local, lo, hi, |run| {
                            let r = rounded(run);
                            if r.len() >= 2 {
                                out.push(r);
                            }
                        });
                    }
                    _ => {
                        let mut r = rounded(&clip_ring(&local, lo, hi));
                        while r.len() > 1 && r.first() == r.last() {
                            r.pop();
                        }
                        if r.len() < 3 {
                            continue;
                        }
                        let a = ring_area(&r);
                        if (*outer && a < 0.0) || (!*outer && a > 0.0) {
                            r.reverse();
                        }
                        out.push(r);
                    }
                }
            }
            if out.is_empty() {
                continue;
            }
            let parts: Vec<&[[i32; 2]]> = out.iter().map(Vec::as_slice).collect();
            let props: Vec<(&str, Value)> = f
                .properties
                .iter()
                .map(|(k, v)| (k.as_str(), v.clone()))
                .collect();
            layer.feature(f.id, f.kind, &parts, &props);
            any = true;
        }
        if !any {
            return None;
        }
        layer.finish();
        mvt::decode(&enc.finish()).ok()?.layers.pop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"{
      "type": "FeatureCollection",
      "features": [
        {"type": "Feature", "id": 7, "properties": {"name": "Spree", "kind": "river", "n": 3},
         "geometry": {"type": "LineString", "coordinates": [[13.30, 52.51], [13.50, 52.52]]}},
        {"type": "Feature", "properties": {"name": "Park"},
         "geometry": {"type": "Polygon", "coordinates": [[[13.38, 52.50], [13.42, 52.50], [13.42, 52.53], [13.38, 52.53], [13.38, 52.50]]]}},
        {"type": "Feature", "properties": null,
         "geometry": {"type": "MultiPoint", "coordinates": [[13.40, 52.52], [2.35, 48.85]]}}
      ]
    }"#;

    #[test]
    fn a_feature_collection_reads_with_its_properties() {
        let g = GeoJson::parse(DOC).unwrap();
        assert_eq!(g.features.len(), 3);
        assert_eq!(g.features[0].id, Some(7));
        assert_eq!(
            g.features[0].properties,
            vec![
                ("name".to_owned(), Value::from("Spree")),
                ("kind".to_owned(), Value::from("river")),
                ("n".to_owned(), Value::Number(3.0)),
            ]
        );
        let Geometry::Polygons(p) = &g.features[1].geometry else {
            panic!()
        };
        assert_eq!(p[0][0].len(), 4, "the closing point is dropped");
        assert!(GeoJson::parse(r#"{"type": "Point", "coordinates": [1, 2]}"#).is_ok());
        assert!(GeoJson::parse(r#"{"type": "Blob"}"#).is_err());
        assert!(GeoJson::parse("[").is_err());
    }

    #[test]
    fn a_tile_gets_what_crosses_it_clipped_with_exteriors_clockwise() {
        let g = GeoJson::parse(DOC).unwrap();
        let data = DataLayer::new("mine", &g);
        let berlin = LngLat::new(13.40, 52.52).to_world().tile(12);
        let layer = data.slice(berlin).expect("Berlin's tile");
        assert_eq!(layer.name, "mine");
        assert_eq!(layer.features.len(), 3);
        let kinds: Vec<_> = layer.features.iter().map(|f| f.kind).collect();
        assert_eq!(kinds, [GeomType::Line, GeomType::Polygon, GeomType::Point]);
        let poly = layer.features[1].geometry().unwrap();
        let ring = poly.parts().next().unwrap();
        assert!(ring_area(ring) > 0.0);
        let margin = (f64::from(EXTENT) * MARGIN) as i32 + 1;
        for f in &layer.features {
            for p in &f.geometry().unwrap().points {
                assert!(p[0] >= -margin && p[0] <= 4096 + margin, "{p:?}");
            }
        }
        assert_eq!(
            layer.value(&layer.features[0], "name"),
            Some(&Value::from("Spree"))
        );
        let paris = LngLat::new(2.35, 48.85).to_world().tile(12);
        let layer = data.slice(paris).expect("Paris's tile");
        assert_eq!(layer.features.len(), 1);
        let tokyo = LngLat::new(139.7, 35.7).to_world().tile(12);
        assert!(data.slice(tokyo).is_none());
    }

    #[test]
    fn lines_clip_into_the_runs_inside() {
        let mut runs = Vec::new();
        clip_line(
            &[
                [-10.0, 5.0],
                [5.0, 5.0],
                [5.0, 20.0],
                [5.0, 30.0],
                [8.0, 5.0],
            ],
            0.0,
            10.0,
            |r| runs.push(r.to_vec()),
        );
        assert_eq!(
            runs,
            vec![
                vec![[0.0, 5.0], [5.0, 5.0], [5.0, 10.0]],
                vec![[7.4, 10.0], [8.0, 5.0]]
            ]
        );
        let ring = clip_ring(
            &[[-5.0, -5.0], [5.0, -5.0], [5.0, 5.0], [-5.0, 5.0]],
            0.0,
            10.0,
        );
        assert_eq!(ring.len(), 4);
        assert!(ring.iter().all(|p| p[0] >= 0.0 && p[1] >= 0.0));
    }
}
