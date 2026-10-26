//! Mapbox Vector Tiles: the protobuf encoding of a tile's layers of
//! features, each a point, line or polygon geometry in tile units with a
//! table of properties.
//!
//! [`decode`] reads a tile — gzipped, zlib-wrapped or bare — into layers
//! whose features keep their geometry encoded until asked for it, so
//! features a style filters out cost no geometry decode. Malformed input is
//! an error, never a panic. [`TileEncoder`] writes tiles, for tests and for
//! apps that generate data.

use std::borrow::Cow;
use std::fmt;

/// Why a tile could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MvtError {
    /// The compressed stream is corrupt or inflates past the size limit.
    Inflate,
    /// The protobuf ends early or holds an impossible field.
    Truncated,
    /// A geometry command or a property reference is out of range.
    Malformed(&'static str),
}

impl fmt::Display for MvtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inflate => f.write_str("the tile does not inflate"),
            Self::Truncated => f.write_str("the tile ends early"),
            Self::Malformed(what) => write!(f, "malformed tile: {what}"),
        }
    }
}

impl std::error::Error for MvtError {}

/// The most a tile may inflate to: well past any real tile, short of a
/// decompression bomb.
const MAX_INFLATED: usize = 64 << 20;

/// `bytes` inflated when gzipped or zlib-wrapped, else as they are.
pub fn inflate(bytes: &[u8]) -> Result<Cow<'_, [u8]>, MvtError> {
    use miniz_oxide::inflate::{
        decompress_to_vec_with_limit as raw, decompress_to_vec_zlib_with_limit as zlib,
    };
    match bytes {
        [0x1f, 0x8b, ..] => {
            let body = gzip_body(bytes).ok_or(MvtError::Inflate)?;
            raw(body, MAX_INFLATED)
                .map(Cow::Owned)
                .map_err(|_| MvtError::Inflate)
        }
        // A zlib header: deflate (`8`) in the low nibble, and a check that
        // makes the pair a multiple of 31.
        [cmf, flg, ..] if cmf & 0x0f == 8 && (u16::from(*cmf) << 8 | u16::from(*flg)) % 31 == 0 => {
            zlib(bytes, MAX_INFLATED)
                .map(Cow::Owned)
                .map_err(|_| MvtError::Inflate)
        }
        _ => Ok(Cow::Borrowed(bytes)),
    }
}

/// The deflate stream inside a gzip member, past its header.
fn gzip_body(bytes: &[u8]) -> Option<&[u8]> {
    const FHCRC: u8 = 2;
    const FEXTRA: u8 = 4;
    const FNAME: u8 = 8;
    const FCOMMENT: u8 = 16;
    let flags = *bytes.get(3)?;
    let mut at = 10;
    if flags & FEXTRA != 0 {
        let len = u16::from_le_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]) as usize;
        at += 2 + len;
    }
    for flag in [FNAME, FCOMMENT] {
        if flags & flag != 0 {
            at += bytes.get(at..)?.iter().position(|&b| b == 0)? + 1;
        }
    }
    if flags & FHCRC != 0 {
        at += 2;
    }
    bytes.get(at..)
}

/// A property value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    String(String),
    /// Every numeric kind — float, double, signed and unsigned — as a
    /// double.
    Number(f64),
    Bool(bool),
}

impl Value {
    /// The value as text, if it is text.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    /// The value as a number, if it is one.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            _ => None,
        }
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Self::String(s.to_owned())
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Self::Number(n)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

/// A feature's kind of geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GeomType {
    Unknown,
    Point,
    Line,
    Polygon,
}

impl GeomType {
    fn from_wire(v: u64) -> Self {
        match v {
            1 => Self::Point,
            2 => Self::Line,
            3 => Self::Polygon,
            _ => Self::Unknown,
        }
    }

    fn wire(self) -> u64 {
        match self {
            Self::Unknown => 0,
            Self::Point => 1,
            Self::Line => 2,
            Self::Polygon => 3,
        }
    }
}

/// A decoded tile: its layers, in the order the tile lists them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VectorTile {
    pub layers: Vec<Layer>,
}

impl VectorTile {
    /// The layer called `name`.
    pub fn layer(&self, name: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.name == name)
    }
}

/// A tile layer: features sharing a key and value table, in tile units of
/// `extent` across.
#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    pub name: String,
    pub extent: u32,
    pub keys: Vec<String>,
    pub values: Vec<Value>,
    pub features: Vec<Feature>,
}

impl Layer {
    /// The index of property key `key` in the layer's table.
    pub fn key(&self, key: &str) -> Option<u32> {
        self.keys.iter().position(|k| k == key).map(|i| i as u32)
    }

    /// Feature `feature`'s value for key index `key`.
    pub fn value_at(&self, feature: &Feature, key: u32) -> Option<&Value> {
        feature
            .tags
            .as_chunks::<2>()
            .0
            .iter()
            .find(|pair| pair[0] == key)
            .and_then(|pair| self.values.get(pair[1] as usize))
    }

    /// Feature `feature`'s value for `key`.
    pub fn value(&self, feature: &Feature, key: &str) -> Option<&Value> {
        self.value_at(feature, self.key(key)?)
    }

    /// Feature `feature`'s properties, key by key.
    pub fn properties<'a>(
        &'a self,
        feature: &'a Feature,
    ) -> impl Iterator<Item = (&'a str, &'a Value)> + 'a {
        feature.tags.as_chunks::<2>().0.iter().filter_map(|pair| {
            Some((
                self.keys.get(pair[0] as usize)?.as_str(),
                self.values.get(pair[1] as usize)?,
            ))
        })
    }
}

/// A feature: its id, kind, property tags and encoded geometry.
#[derive(Debug, Clone, PartialEq)]
pub struct Feature {
    pub id: Option<u64>,
    pub kind: GeomType,
    /// Key and value indices into the layer's tables, in pairs.
    pub tags: Vec<u32>,
    commands: Vec<u32>,
}

/// A feature's geometry in tile units: its points in order, cut into parts
/// — the points of a multipoint, the lines of a multiline, the rings of a
/// polygon (each closed implicitly, its first point not repeated).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Geometry {
    pub points: Vec<[i32; 2]>,
    /// The end of each part in `points`.
    pub ends: Vec<u32>,
}

impl Geometry {
    /// The parts, each a run of points.
    pub fn parts(&self) -> impl Iterator<Item = &[[i32; 2]]> + '_ {
        let mut start = 0;
        self.ends.iter().map(move |&end| {
            let part = &self.points[start..end as usize];
            start = end as usize;
            part
        })
    }
}

/// Twice a ring's signed area, positive for a ring that winds clockwise on
/// screen (`y` down): an exterior ring.
pub fn ring_area(ring: &[[i32; 2]]) -> f64 {
    let n = ring.len();
    (0..n)
        .map(|i| {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            a[0] as f64 * b[1] as f64 - b[0] as f64 * a[1] as f64
        })
        .sum()
}

impl Feature {
    /// Decode the feature's geometry.
    pub fn geometry(&self) -> Result<Geometry, MvtError> {
        let mut g = Geometry::default();
        let (mut x, mut y) = (0i32, 0i32);
        let mut at = 0;
        let cmds = &self.commands;
        let polygon = self.kind == GeomType::Polygon;
        while at < cmds.len() {
            let c = cmds[at];
            at += 1;
            let (id, count) = (c & 7, (c >> 3) as usize);
            match id {
                1 | 2 => {
                    let pairs = cmds
                        .get(at..at + count * 2)
                        .ok_or(MvtError::Malformed("geometry runs past its end"))?;
                    at += count * 2;
                    for pair in pairs.as_chunks::<2>().0 {
                        x = x.wrapping_add(zigzag32(pair[0]));
                        y = y.wrapping_add(zigzag32(pair[1]));
                        if id == 1 && !g.points.is_empty() && self.kind != GeomType::Point {
                            g.ends.push(g.points.len() as u32);
                        }
                        g.points.push([x, y]);
                        if id == 1 && self.kind == GeomType::Point {
                            g.ends.push(g.points.len() as u32);
                        }
                    }
                }
                7 => {
                    if !polygon {
                        return Err(MvtError::Malformed("close path outside a polygon"));
                    }
                }
                _ => return Err(MvtError::Malformed("unknown geometry command")),
            }
        }
        if self.kind != GeomType::Point
            && g.ends.last().copied() != Some(g.points.len() as u32)
            && !g.points.is_empty()
        {
            g.ends.push(g.points.len() as u32);
        }
        Ok(g)
    }
}

fn zigzag32(v: u32) -> i32 {
    ((v >> 1) as i32) ^ -((v & 1) as i32)
}

fn zigzag64(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

/// A protobuf reader over a byte slice.
struct Pbf<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Pbf<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }

    fn done(&self) -> bool {
        self.at >= self.buf.len()
    }

    fn varint(&mut self) -> Result<u64, MvtError> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = *self.buf.get(self.at).ok_or(MvtError::Truncated)?;
            self.at += 1;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(MvtError::Truncated)
    }

    /// The next field's number and wire type.
    fn key(&mut self) -> Result<(u32, u8), MvtError> {
        let k = self.varint()?;
        Ok(((k >> 3) as u32, (k & 7) as u8))
    }

    fn bytes(&mut self) -> Result<&'a [u8], MvtError> {
        let len = self.varint()? as usize;
        let end = self.at.checked_add(len).ok_or(MvtError::Truncated)?;
        let out = self.buf.get(self.at..end).ok_or(MvtError::Truncated)?;
        self.at = end;
        Ok(out)
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N], MvtError> {
        let out = self
            .buf
            .get(self.at..self.at + N)
            .ok_or(MvtError::Truncated)?;
        self.at += N;
        Ok(out.try_into().expect("N bytes"))
    }

    fn string(&mut self) -> Result<String, MvtError> {
        Ok(String::from_utf8_lossy(self.bytes()?).into_owned())
    }

    fn skip(&mut self, wire: u8) -> Result<(), MvtError> {
        match wire {
            0 => {
                self.varint()?;
            }
            1 => {
                self.fixed::<8>()?;
            }
            2 => {
                self.bytes()?;
            }
            5 => {
                self.fixed::<4>()?;
            }
            _ => return Err(MvtError::Malformed("unknown wire type")),
        }
        Ok(())
    }

    /// A packed run of `u32` varints, or one unpacked.
    fn packed(&mut self, wire: u8, out: &mut Vec<u32>) -> Result<(), MvtError> {
        if wire == 0 {
            out.push(self.varint()? as u32);
            return Ok(());
        }
        let mut inner = Pbf::new(self.bytes()?);
        while !inner.done() {
            out.push(inner.varint()? as u32);
        }
        Ok(())
    }
}

/// Read a tile, inflating it first when it is compressed.
pub fn decode(bytes: &[u8]) -> Result<VectorTile, MvtError> {
    let bytes = inflate(bytes)?;
    let mut pbf = Pbf::new(&bytes);
    let mut tile = VectorTile::default();
    while !pbf.done() {
        match pbf.key()? {
            (3, 2) => tile.layers.push(layer(pbf.bytes()?)?),
            (_, wire) => pbf.skip(wire)?,
        }
    }
    Ok(tile)
}

fn layer(bytes: &[u8]) -> Result<Layer, MvtError> {
    let mut pbf = Pbf::new(bytes);
    let mut layer = Layer {
        name: String::new(),
        extent: 4096,
        keys: Vec::new(),
        values: Vec::new(),
        features: Vec::new(),
    };
    while !pbf.done() {
        match pbf.key()? {
            (1, 2) => layer.name = pbf.string()?,
            (2, 2) => layer.features.push(feature(pbf.bytes()?)?),
            (3, 2) => layer.keys.push(pbf.string()?),
            (4, 2) => layer.values.push(value(pbf.bytes()?)?),
            (5, 0) => layer.extent = (pbf.varint()? as u32).max(1),
            (_, wire) => pbf.skip(wire)?,
        }
    }
    Ok(layer)
}

fn feature(bytes: &[u8]) -> Result<Feature, MvtError> {
    let mut pbf = Pbf::new(bytes);
    let mut f = Feature {
        id: None,
        kind: GeomType::Unknown,
        tags: Vec::new(),
        commands: Vec::new(),
    };
    while !pbf.done() {
        match pbf.key()? {
            (1, 0) => f.id = Some(pbf.varint()?),
            (2, wire @ (0 | 2)) => pbf.packed(wire, &mut f.tags)?,
            (3, 0) => f.kind = GeomType::from_wire(pbf.varint()?),
            (4, wire @ (0 | 2)) => pbf.packed(wire, &mut f.commands)?,
            (_, wire) => pbf.skip(wire)?,
        }
    }
    if !f.tags.len().is_multiple_of(2) {
        return Err(MvtError::Malformed("odd property tags"));
    }
    Ok(f)
}

fn value(bytes: &[u8]) -> Result<Value, MvtError> {
    let mut pbf = Pbf::new(bytes);
    let mut v = Value::Bool(false);
    while !pbf.done() {
        v = match pbf.key()? {
            (1, 2) => Value::String(pbf.string()?),
            (2, 5) => Value::Number(f32::from_le_bytes(pbf.fixed()?) as f64),
            (3, 1) => Value::Number(f64::from_le_bytes(pbf.fixed()?)),
            (4, 0) => Value::Number(pbf.varint()? as i64 as f64),
            (5, 0) => Value::Number(pbf.varint()? as f64),
            (6, 0) => Value::Number(zigzag64(pbf.varint()?) as f64),
            (7, 0) => Value::Bool(pbf.varint()? != 0),
            (_, wire) => {
                pbf.skip(wire)?;
                continue;
            }
        };
    }
    Ok(v)
}

/// A protobuf writer.
#[derive(Default)]
struct Out(Vec<u8>);

impl Out {
    fn varint(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.0.push(v as u8 | 0x80);
            v >>= 7;
        }
        self.0.push(v as u8);
    }

    fn key(&mut self, field: u32, wire: u8) {
        self.varint(u64::from(field) << 3 | u64::from(wire));
    }

    fn bytes(&mut self, field: u32, bytes: &[u8]) {
        self.key(field, 2);
        self.varint(bytes.len() as u64);
        self.0.extend_from_slice(bytes);
    }

    fn packed(&mut self, field: u32, values: &[u32]) {
        let mut inner = Out::default();
        for &v in values {
            inner.varint(u64::from(v));
        }
        self.bytes(field, &inner.0);
    }
}

/// Writes vector tiles: layers of features with their geometry and
/// properties.
#[derive(Default)]
pub struct TileEncoder {
    out: Out,
}

/// One layer being written; [`finish`](LayerEncoder::finish) adds it to
/// the tile.
pub struct LayerEncoder<'a> {
    tile: &'a mut TileEncoder,
    name: String,
    extent: u32,
    keys: Vec<String>,
    values: Vec<Value>,
    features: Vec<Vec<u8>>,
}

impl TileEncoder {
    /// A tile with no layers.
    pub fn new() -> Self {
        Self::default()
    }

    /// Start layer `name`, `extent` tile units across.
    pub fn layer(&mut self, name: &str, extent: u32) -> LayerEncoder<'_> {
        LayerEncoder {
            tile: self,
            name: name.to_owned(),
            extent,
            keys: Vec::new(),
            values: Vec::new(),
            features: Vec::new(),
        }
    }

    /// The tile's bytes.
    pub fn finish(self) -> Vec<u8> {
        self.out.0
    }

    /// The tile's bytes, gzipped.
    pub fn finish_gzip(self) -> Vec<u8> {
        let body = miniz_oxide::deflate::compress_to_vec(&self.out.0, 6);
        let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255];
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&self.out.0).to_le_bytes());
        out.extend_from_slice(&(self.out.0.len() as u32).to_le_bytes());
        out
    }
}

/// The CRC-32 a gzip trailer carries.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (!(crc & 1)).wrapping_add(1));
        }
    }
    !crc
}

impl LayerEncoder<'_> {
    /// Add a feature of `kind` whose geometry is `parts` (the points of a
    /// multipoint, the lines of a multiline, the rings of a polygon — each
    /// ring unclosed, exteriors clockwise on screen), with `properties`.
    pub fn feature(
        &mut self,
        id: Option<u64>,
        kind: GeomType,
        parts: &[&[[i32; 2]]],
        properties: &[(&str, Value)],
    ) -> &mut Self {
        let mut tags = Vec::with_capacity(properties.len() * 2);
        for (key, value) in properties {
            let k = match self.keys.iter().position(|k| k == key) {
                Some(i) => i,
                None => {
                    self.keys.push((*key).to_owned());
                    self.keys.len() - 1
                }
            };
            let v = match self.values.iter().position(|v| v == value) {
                Some(i) => i,
                None => {
                    self.values.push(value.clone());
                    self.values.len() - 1
                }
            };
            tags.extend([k as u32, v as u32]);
        }
        let mut cmds = Vec::new();
        let (mut x, mut y) = (0i32, 0i32);
        let zz = |v: i32| ((v << 1) ^ (v >> 31)) as u32;
        let mut delta = |p: [i32; 2], cmds: &mut Vec<u32>| {
            cmds.push(zz(p[0] - x));
            cmds.push(zz(p[1] - y));
            (x, y) = (p[0], p[1]);
        };
        if kind == GeomType::Point {
            cmds.push(1 | (parts.len() as u32) << 3);
            for part in parts {
                if let Some(&p) = part.first() {
                    delta(p, &mut cmds);
                }
            }
        } else {
            for part in parts {
                let Some((&first, rest)) = part.split_first() else {
                    continue;
                };
                cmds.push(1 | 1 << 3);
                delta(first, &mut cmds);
                cmds.push(2 | (rest.len() as u32) << 3);
                for &p in rest {
                    delta(p, &mut cmds);
                }
                if kind == GeomType::Polygon {
                    cmds.push(7 | 1 << 3);
                }
            }
        }
        let mut f = Out::default();
        if let Some(id) = id {
            f.key(1, 0);
            f.varint(id);
        }
        f.packed(2, &tags);
        f.key(3, 0);
        f.varint(kind.wire());
        f.packed(4, &cmds);
        self.features.push(f.0);
        self
    }

    /// Add the layer to its tile.
    pub fn finish(self) {
        let mut l = Out::default();
        l.key(15, 0);
        l.varint(2);
        l.bytes(1, self.name.as_bytes());
        for f in &self.features {
            l.bytes(2, f);
        }
        for k in &self.keys {
            l.bytes(3, k.as_bytes());
        }
        for v in &self.values {
            let mut o = Out::default();
            match v {
                Value::String(s) => o.bytes(1, s.as_bytes()),
                Value::Number(n) if n.fract() == 0.0 && n.abs() < 9e15 => {
                    if *n < 0.0 {
                        o.key(6, 0);
                        o.varint(((*n as i64) << 1 ^ (*n as i64) >> 63) as u64);
                    } else {
                        o.key(5, 0);
                        o.varint(*n as u64);
                    }
                }
                Value::Number(n) => {
                    o.key(3, 1);
                    o.0.extend_from_slice(&n.to_le_bytes());
                }
                Value::Bool(b) => {
                    o.key(7, 0);
                    o.varint(u64::from(*b));
                }
            }
            l.bytes(4, &o.0);
        }
        l.key(5, 0);
        l.varint(u64::from(self.extent));
        self.tile.out.bytes(3, &l.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> TileEncoder {
        let mut tile = TileEncoder::new();
        let mut water = tile.layer("water", 4096);
        let outer: &[[i32; 2]] = &[[0, 0], [4096, 0], [4096, 4096], [0, 4096]];
        let hole: &[[i32; 2]] = &[[1000, 1000], [1000, 2000], [2000, 2000], [2000, 1000]];
        water.feature(
            Some(7),
            GeomType::Polygon,
            &[outer, hole],
            &[("class", "lake".into())],
        );
        water.finish();
        let mut roads = tile.layer("transportation", 4096);
        let a: &[[i32; 2]] = &[[-64, 100], [500, 120], [4200, 3000]];
        let b: &[[i32; 2]] = &[[10, 10], [20, 20]];
        roads.feature(
            None,
            GeomType::Line,
            &[a, b],
            &[
                ("class", "primary".into()),
                ("layer", Value::Number(-1.0)),
                ("oneway", true.into()),
                ("width", 7.5.into()),
            ],
        );
        roads.feature(
            None,
            GeomType::Point,
            &[&[[5, 6]], &[[7, 8]]],
            &[("rank", 3.0.into())],
        );
        roads.finish();
        tile
    }

    #[test]
    fn tiles_round_trip() {
        let tile = decode(&sample().finish()).unwrap();
        assert_eq!(tile.layers.len(), 2);
        let water = tile.layer("water").unwrap();
        assert_eq!(water.extent, 4096);
        let lake = &water.features[0];
        assert_eq!((lake.id, lake.kind), (Some(7), GeomType::Polygon));
        assert_eq!(
            water.value(lake, "class").and_then(Value::as_str),
            Some("lake")
        );
        let g = lake.geometry().unwrap();
        assert_eq!(g.ends, vec![4, 8]);
        let rings: Vec<_> = g.parts().collect();
        assert!(ring_area(rings[0]) > 0.0 && ring_area(rings[1]) < 0.0);

        let roads = tile.layer("transportation").unwrap();
        let road = &roads.features[0];
        let g = road.geometry().unwrap();
        assert_eq!(g.points[0], [-64, 100]);
        assert_eq!(g.ends, vec![3, 5]);
        assert_eq!(roads.value(road, "layer"), Some(&Value::Number(-1.0)));
        assert_eq!(roads.value(road, "oneway"), Some(&Value::Bool(true)));
        assert_eq!(roads.value(road, "width"), Some(&Value::Number(7.5)));
        assert_eq!(roads.properties(road).count(), 4);
        let points = roads.features[1].geometry().unwrap();
        assert_eq!(points.points, vec![[5, 6], [7, 8]]);
        assert_eq!(points.ends, vec![1, 2]);
    }

    #[test]
    fn compressed_tiles_inflate() {
        let plain = sample().finish();
        let gz = sample().finish_gzip();
        assert_eq!(decode(&gz).unwrap(), decode(&plain).unwrap());
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&plain, 6);
        assert_eq!(decode(&z).unwrap(), decode(&plain).unwrap());
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn malformed_tiles_fail_without_panicking() {
        let bytes = sample().finish();
        for cut in 1..bytes.len() {
            let _ = decode(&bytes[..cut]).map(|t| {
                for l in &t.layers {
                    for f in &l.features {
                        let _ = f.geometry();
                    }
                }
            });
        }
        assert_eq!(decode(&[0x1f, 0x8b, 8, 0]), Err(MvtError::Inflate));
        assert!(decode(&[0x1a, 0xff]).is_err());
        let bad = Feature {
            id: None,
            kind: GeomType::Line,
            tags: Vec::new(),
            commands: vec![2 | 5 << 3, 1],
        };
        assert!(bad.geometry().is_err());
    }
}
