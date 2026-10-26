//! Tiles raised for a tilted camera.
//!
//! A tile becomes one 3D model in its own points ([`SIDE`] across at its
//! zoom; x east, y up, z south): an opaque base of the background's color,
//! its flat drawing laid just above the base and blended in its own order,
//! and the polygons of its extrusion layers raised into lit solids. Over
//! relief the base is a lattice of planar triangles on the elevations, and
//! every triangle of the drawing is cut along the lattice so that each
//! piece lies in one of its planes, shaded by how the slope under it faces
//! the light.
//!
//! Colors come from a small palette texture: two texels across, from clear
//! to opaque, so that a vertex's coverage weight still softens its edge, and
//! two rows for each color, from shaded to fully lit.

use std::collections::HashMap;

use viso_render::scene3d::model::{
    AlphaMode, ImageKind, ModelAsset, ModelImage, ModelMaterial, ModelMesh, ModelNode,
    ModelPrimitive, ModelRig, ModelTexture, TextureRef, Trs, Wrap,
};
use viso_render::{Mesh, MeshVertex};

use crate::build::{EXTENT, FeatureRef, key_index, polygons};
use crate::camera::TILE_SIZE;
use crate::geo::tile_square;
use crate::mvt::{self, GeomType};
use crate::raster::Dem;
use crate::source::TileId;
use crate::style::{Paint, Properties, Style};
use crate::tess::clip::{Square, clip_ring};
use crate::tess::earcut::Earcut;

/// A tile's side in its model's units.
pub const SIDE: f32 = TILE_SIZE as f32;

/// How far above the base the drawing lies, in points of the level drawn.
pub const LIFT: f32 = 0.15;

/// The lattice cells along a tile's side over relief.
const CELLS: usize = 32;

/// How far the base's skirts reach down, in model units: they close the
/// cracks where neighbouring tiles' relief does not quite meet.
const SKIRT: f32 = SIDE / 48.0;

/// The darkest a slope facing away from the light shades a color to.
const SHADED: f32 = 0.45;

/// The direction toward the light that shades relief: from the northwest,
/// halfway up the sky.
const LIGHT: [f32; 3] = [-0.5, std::f32::consts::FRAC_1_SQRT_2, -0.5];

/// Elevations under a tile: the part of a terrain tile that covers it.
#[derive(Debug, Clone, Copy)]
pub struct Relief<'a> {
    pub dem: &'a Dem,
    /// Where the tile starts in the terrain tile, `0..1` across and down,
    /// and how much of it the tile spans.
    pub origin: [f64; 2],
    pub span: f64,
    /// Model units per meter of elevation, any exaggeration included.
    pub scale: f32,
}

impl Relief<'_> {
    /// The elevation, in model units, at model point `p`.
    pub fn height(&self, p: [f32; 2]) -> f32 {
        let u = self.origin[0] + self.span * f64::from(p[0] / SIDE);
        let v = self.origin[1] + self.span * f64::from(p[1] / SIDE);
        self.dem.sample(u.clamp(0.0, 1.0), v.clamp(0.0, 1.0)) * self.scale
    }
}

/// What a tile's model is built from.
pub struct Raise<'a> {
    pub tile: TileId,
    /// The zoom level its drawing was built for.
    pub band: u8,
    /// Its drawing's meshes, in paint order, in tile units of [`EXTENT`]:
    /// those beneath the style's image layers, and those over them.
    pub meshes: &'a [Mesh],
    pub over: &'a [Mesh],
    /// How many image layers the style draws between the two.
    pub images: usize,
    /// Its decoded layers, for the extrusion layers.
    pub layers: &'a [&'a mvt::Layer],
    pub style: &'a Style,
    /// The background's straight linear color.
    pub background: [f32; 4],
    pub relief: Option<Relief<'a>>,
}

/// Model units per meter on the ground of `tile`.
pub fn units_per_meter(tile: TileId) -> f32 {
    let (nw, side) = tile_square(tile);
    let middle = crate::geo::WorldPoint::new(nw.x + side / 2.0, nw.y + side / 2.0);
    (f64::from(SIDE) / (side * middle.meters_per_unit())) as f32
}

/// Distinct colors, each a pair of palette rows.
#[derive(Default)]
struct Palette {
    index: HashMap<[u32; 4], u32>,
    colors: Vec<[f32; 4]>,
}

/// The palette rows' limit: the texture stays within the smallest device
/// limit.
const MAX_COLORS: usize = 2048;

impl Palette {
    fn id(&mut self, c: [f32; 4]) -> u32 {
        let key = c.map(f32::to_bits);
        if let Some(&i) = self.index.get(&key) {
            return i;
        }
        if self.colors.len() >= MAX_COLORS {
            return 0;
        }
        let i = self.colors.len() as u32;
        self.colors.push(c);
        self.index.insert(key, i);
        i
    }

    /// The texture coordinate of color `i` at coverage `edge` and light
    /// `shade` (`0` shaded, `1` lit).
    fn uv(&self, i: u32, edge: f32, shade: f32) -> [f32; 2] {
        let rows = (self.colors.len().max(1) * 2) as f32;
        [
            0.25 + 0.5 * edge.clamp(0.0, 1.0),
            (2.0 * i as f32 + 0.5 + shade.clamp(0.0, 1.0)) / rows,
        ]
    }

    fn image(&self) -> ModelImage {
        let n = self.colors.len().max(1);
        let mut rgba = Vec::with_capacity(n * 16);
        for i in 0..n {
            let c = self.colors.get(i).copied().unwrap_or([0.0; 4]);
            for light in [SHADED, 1.0] {
                let rgb = [c[0] * light, c[1] * light, c[2] * light].map(srgb);
                for clear in [true, false] {
                    rgba.extend_from_slice(&rgb);
                    let a = if clear { 0.0 } else { c[3] };
                    rgba.push((a.clamp(0.0, 1.0) * 255.0).round() as u8);
                }
            }
        }
        ModelImage {
            width: 2,
            height: (n * 2) as u32,
            rgba,
            kind: ImageKind::Color,
            mips: Vec::new(),
        }
    }
}

/// A linear channel sRGB-encoded into a byte.
fn srgb(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}

/// One primitive being built: positions, normals, palette entries and
/// indices.
#[derive(Default)]
struct Part {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    /// Color, coverage and light, mapped to texture coordinates once the
    /// palette is complete.
    looks: Vec<(u32, f32, f32)>,
    indices: Vec<u32>,
}

impl Part {
    fn push(&mut self, p: [f32; 3], n: [f32; 3], look: (u32, f32, f32)) -> u32 {
        self.positions.push(p);
        self.normals.push(n);
        self.looks.push(look);
        (self.positions.len() - 1) as u32
    }

    fn primitive(self, palette: &Palette, material: u32) -> Option<ModelPrimitive> {
        if self.indices.is_empty() {
            return None;
        }
        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        for p in &self.positions {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        let uvs = self
            .looks
            .iter()
            .map(|&(c, e, s)| palette.uv(c, e, s))
            .collect();
        Some(ModelPrimitive {
            positions: self.positions,
            normals: self.normals,
            uvs,
            indices: self.indices,
            material: Some(material),
            bounds: (lo, hi),
            ..ModelPrimitive::default()
        })
    }
}

const UP: [f32; 3] = [0.0, 1.0, 0.0];

/// A vertex of the drawing in model units: where, which color, how covered.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Flat {
    p: [f32; 2],
    color: u32,
    edge: f32,
}

fn mix(a: Flat, b: Flat, t: f32) -> Flat {
    Flat {
        p: [
            a.p[0] + (b.p[0] - a.p[0]) * t,
            a.p[1] + (b.p[1] - a.p[1]) * t,
        ],
        color: a.color,
        edge: a.edge + (b.edge - a.edge) * t,
    }
}

/// The relief sampled at the corners of the lattice's cells, and the light
/// each corner's slope takes.
struct Lattice {
    cell: f32,
    heights: Vec<f32>,
    shades: Vec<f32>,
}

impl Lattice {
    fn new(relief: &Relief<'_>) -> Self {
        let n = CELLS + 1;
        let cell = SIDE / CELLS as f32;
        let mut heights = Vec::with_capacity(n * n);
        for j in 0..n {
            for i in 0..n {
                heights.push(relief.height([i as f32 * cell, j as f32 * cell]));
            }
        }
        let at = |i: usize, j: usize| heights[j.min(CELLS) * n + i.min(CELLS)];
        let mut shades = Vec::with_capacity(n * n);
        let flat = LIGHT[1];
        for j in 0..n {
            for i in 0..n {
                let (i0, i1) = (i.saturating_sub(1), (i + 1).min(CELLS));
                let (j0, j1) = (j.saturating_sub(1), (j + 1).min(CELLS));
                let dx = (at(i1, j) - at(i0, j)) / ((i1 - i0) as f32 * cell);
                let dz = (at(i, j1) - at(i, j0)) / ((j1 - j0) as f32 * cell);
                let len = (dx * dx + dz * dz + 1.0).sqrt();
                let facing = (-dx * LIGHT[0] + LIGHT[1] - dz * LIGHT[2]) / len;
                // Lit as flat ground or better: the color as it is; turned
                // away, darker the further.
                let dark = ((flat - facing) / flat * 2.0).clamp(0.0, 1.0);
                shades.push(1.0 - dark);
            }
        }
        Self {
            cell,
            heights,
            shades,
        }
    }

    /// The lattice triangle holding `p`: its corners' indices.
    fn triangle(&self, p: [f32; 2]) -> [usize; 3] {
        let n = CELLS + 1;
        let x = (p[0] / self.cell).clamp(0.0, CELLS as f32);
        let z = (p[1] / self.cell).clamp(0.0, CELLS as f32);
        let i = (x.floor() as usize).min(CELLS - 1);
        let j = (z.floor() as usize).min(CELLS - 1);
        let (fx, fz) = (x - i as f32, z - j as f32);
        let c = |di: usize, dj: usize| (j + dj) * n + i + di;
        if fx + fz <= 1.0 {
            [c(0, 0), c(1, 0), c(0, 1)]
        } else {
            [c(1, 1), c(0, 1), c(1, 0)]
        }
    }

    /// The height and light at `p` on the plane of lattice triangle `t`.
    fn on(&self, t: [usize; 3], p: [f32; 2]) -> (f32, f32) {
        let n = CELLS + 1;
        let corner = |k: usize| {
            let (i, j) = (t[k] % n, t[k] / n);
            [i as f32 * self.cell, j as f32 * self.cell]
        };
        let (a, b, c) = (corner(0), corner(1), corner(2));
        // Barycentric weights of `p` in the right triangle a, b, c.
        let wb = (p[0] - a[0]) / (b[0] - a[0]);
        let wc = (p[1] - a[1]) / (c[1] - a[1]);
        let wa = 1.0 - wb - wc;
        let h = wa * self.heights[t[0]] + wb * self.heights[t[1]] + wc * self.heights[t[2]];
        let s = wa * self.shades[t[0]] + wb * self.shades[t[1]] + wc * self.shades[t[2]];
        (h, s)
    }

    /// The height at `p`.
    fn height(&self, p: [f32; 2]) -> f32 {
        self.on(self.triangle(p), p).0
    }
}

/// Splits convex polygon `poly` at the line where `f` reaches `at`.
fn split(poly: &[Flat], f: &dyn Fn([f32; 2]) -> f32, at: f32) -> (Vec<Flat>, Vec<Flat>) {
    let (mut below, mut above) = (Vec::new(), Vec::new());
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        let (fa, fb) = (f(a.p) - at, f(b.p) - at);
        if fa <= 0.0 {
            below.push(a);
        }
        if fa >= 0.0 {
            above.push(a);
        }
        if (fa < 0.0 && fb > 0.0) || (fa > 0.0 && fb < 0.0) {
            let m = mix(a, b, fa / (fa - fb));
            below.push(m);
            above.push(m);
        }
    }
    (below, above)
}

/// Cuts convex polygon `poly` along the lattice's columns, rows and
/// diagonals of `cell`, from family `family` on, handing each piece to
/// `emit`.
fn cut(poly: Vec<Flat>, family: usize, cell: f32, emit: &mut dyn FnMut(&[Flat])) {
    if poly.len() < 3 {
        return;
    }
    if family == 3 {
        emit(&poly);
        return;
    }
    let f = move |p: [f32; 2]| {
        (match family {
            0 => p[0],
            1 => p[1],
            _ => p[0] + p[1],
        }) / cell
    };
    let (lo, hi) = poly.iter().fold((f32::MAX, f32::MIN), |(lo, hi), v| {
        let x = f(v.p);
        (lo.min(x), hi.max(x))
    });
    let (first, last) = (lo.floor() as i64 + 1, hi.ceil() as i64);
    let mut rest = poly;
    for k in first..last {
        let (below, above) = split(&rest, &f, k as f32);
        cut(below, family + 1, cell, emit);
        rest = above;
        if rest.len() < 3 {
            return;
        }
    }
    cut(rest, family + 1, cell, emit);
}

/// The drawing's vertex `v` in model units, its points `units` model units
/// each and its screen extrusion taken at `k` points per model unit.
fn flat(v: &MeshVertex, units: f32, k: f32, palette: &mut Palette) -> Flat {
    Flat {
        p: [
            v.pos[0] * units + v.extrude[0] / k,
            v.pos[1] * units + v.extrude[1] / k,
        ],
        color: palette.id(v.color),
        edge: v.edge,
    }
}

/// Lays `meshes` (points of `units` model units each, extruded at `k`
/// points per model unit) into `part`: flat at `lift`, or over `lattice`
/// `lift` above it, pieces past the tile left out.
fn lay(
    meshes: &[Mesh],
    units: f32,
    k: f32,
    lift: f32,
    lattice: Option<&Lattice>,
    palette: &mut Palette,
    part: &mut Part,
) {
    for mesh in meshes {
        let at = |v: &MeshVertex, palette: &mut Palette| flat(v, units, k, palette);
        match lattice {
            None => {
                let base = part.positions.len() as u32;
                for v in mesh.vertices.iter() {
                    let f = at(v, palette);
                    part.push([f.p[0], lift, f.p[1]], UP, (f.color, f.edge, 1.0));
                }
                part.indices.extend(mesh.indices.iter().map(|&i| base + i));
            }
            Some(lattice) => {
                let inside =
                    |p: [f32; 2]| (0.0..=SIDE).contains(&p[0]) && (0.0..=SIDE).contains(&p[1]);
                for tri in mesh.indices.as_chunks::<3>().0 {
                    let poly: Vec<Flat> = tri
                        .iter()
                        .map(|&i| at(&mesh.vertices[i as usize], palette))
                        .collect();
                    cut(poly, 0, lattice.cell, &mut |piece| {
                        let n = piece.len() as f32;
                        let mid = piece
                            .iter()
                            .fold([0.0, 0.0], |m, v| [m[0] + v.p[0] / n, m[1] + v.p[1] / n]);
                        if !inside(mid) {
                            return;
                        }
                        let t = lattice.triangle(mid);
                        let base = part.positions.len() as u32;
                        for v in piece {
                            let (h, s) = lattice.on(t, v.p);
                            part.push([v.p[0], h + lift, v.p[1]], UP, (v.color, v.edge, s));
                        }
                        for i in 1..piece.len() as u32 - 1 {
                            part.indices.extend([base, base + i, base + i + 1]);
                        }
                    });
                }
            }
        }
    }
}

/// The base under a tile: a square of `color`, or the lattice's triangles
/// with skirts hanging from its edges.
fn base(lattice: Option<&Lattice>, color: u32, part: &mut Part) {
    let Some(l) = lattice else {
        let b = part.positions.len() as u32;
        for p in [[0.0, 0.0], [SIDE, 0.0], [SIDE, SIDE], [0.0, SIDE]] {
            part.push([p[0], 0.0, p[1]], UP, (color, 1.0, 1.0));
        }
        part.indices.extend([b, b + 1, b + 2, b, b + 2, b + 3]);
        return;
    };
    let n = CELLS + 1;
    let b = part.positions.len() as u32;
    for j in 0..n {
        for i in 0..n {
            let k = j * n + i;
            let p = [i as f32 * l.cell, l.heights[k], j as f32 * l.cell];
            part.push(p, UP, (color, 1.0, l.shades[k]));
        }
    }
    let at = |i: usize, j: usize| b + (j * n + i) as u32;
    for j in 0..CELLS {
        for i in 0..CELLS {
            part.indices.extend([at(i, j), at(i + 1, j), at(i, j + 1)]);
            part.indices
                .extend([at(i + 1, j + 1), at(i, j + 1), at(i + 1, j)]);
        }
    }
    // The skirts, round the four edges.
    let edges: [Vec<(usize, usize)>; 4] = [
        (0..n).map(|i| (i, 0)).collect(),
        (0..n).map(|j| (CELLS, j)).collect(),
        (0..n).rev().map(|i| (i, CELLS)).collect(),
        (0..n).rev().map(|j| (0, j)).collect(),
    ];
    for edge in edges {
        for w in edge.windows(2) {
            let (a, c) = (w[0], w[1]);
            let (ka, kc) = (a.1 * n + a.0, c.1 * n + c.0);
            let pa = [a.0 as f32 * l.cell, a.1 as f32 * l.cell];
            let pc = [c.0 as f32 * l.cell, c.1 as f32 * l.cell];
            let s = part.positions.len() as u32;
            let look = |k: usize| (color, 1.0, l.shades[k]);
            part.push([pa[0], l.heights[ka], pa[1]], UP, look(ka));
            part.push([pc[0], l.heights[kc], pc[1]], UP, look(kc));
            part.push([pc[0], l.heights[kc] - SKIRT, pc[1]], UP, look(kc));
            part.push([pa[0], l.heights[ka] - SKIRT, pa[1]], UP, look(ka));
            part.indices.extend([s, s + 1, s + 2, s, s + 2, s + 3]);
        }
    }
}

/// A numeric property, also when written as text ("12", "12.5 m").
fn number(props: &dyn Properties, key: &str) -> Option<f32> {
    let v = props.get(key)?;
    let n = match v.as_f64() {
        Some(n) => n,
        None => {
            let s = v.as_str()?.trim();
            let end = s
                .find(|c: char| !(c.is_ascii_digit() || c == '.'))
                .unwrap_or(s.len());
            s[..end].parse::<f64>().ok()?
        }
    };
    (n.is_finite() && n >= 0.0).then_some(n as f32)
}

/// How tall a feature says it is, in meters.
fn feature_height(props: &dyn Properties) -> Option<f32> {
    number(props, "height")
        .or_else(|| number(props, "render_height"))
        .or_else(|| {
            number(props, "building:levels")
                .or_else(|| number(props, "levels"))
                .map(|n| n * 3.0)
        })
        .map(|h| h.min(1000.0))
}

/// How high a feature's base is, in meters.
fn feature_base(props: &dyn Properties) -> f32 {
    number(props, "min_height")
        .or_else(|| number(props, "render_min_height"))
        .unwrap_or(0.0)
        .min(1000.0)
}

/// The tile's extrusion layers raised into `part`.
fn extrude(r: &Raise<'_>, lattice: Option<&Lattice>, palette: &mut Palette, part: &mut Part) {
    let zoom = f32::from(r.band);
    let units = SIDE / EXTENT;
    let meters = units_per_meter(r.tile);
    let square = Square {
        lo: 0.0,
        hi: EXTENT,
    };
    let mut earcut = Earcut::default();
    let (mut ring, mut scratch) = (Vec::new(), Vec::new());
    let (mut points, mut holes, mut tris) = (Vec::new(), Vec::new(), Vec::new());
    let mut rings: Vec<(usize, usize)> = Vec::new();
    for layer in &r.style.layers {
        let Paint::Extrusion {
            color,
            opacity,
            height,
        } = &layer.paint
        else {
            continue;
        };
        if !layer.source.is_empty() || !layer.shows_at(zoom) {
            continue;
        }
        let Some(tl) = r.layers.iter().find(|l| l.name == layer.source_layer) else {
            continue;
        };
        let keys = key_index(tl);
        let scale = EXTENT / tl.extent as f32;
        for feature in &tl.features {
            if feature.kind != GeomType::Polygon {
                continue;
            }
            let props = FeatureRef::new(tl, feature, &keys);
            if !layer.filter.test(&props) {
                continue;
            }
            let c = color.eval(zoom, &props).linear(opacity.eval(zoom, &props));
            if c[3] <= 0.0 {
                continue;
            }
            let tall = feature_height(&props).unwrap_or_else(|| height.eval(zoom, &props));
            let low = feature_base(&props);
            if tall <= low {
                continue;
            }
            let Ok(g) = feature.geometry() else {
                continue;
            };
            let pts: Vec<[f32; 2]> = g
                .points
                .iter()
                .map(|p| [p[0] as f32 * scale, p[1] as f32 * scale])
                .collect();
            let look = (palette.id([c[0], c[1], c[2], 1.0]), 1.0, 1.0);
            polygons(&pts, &g.ends, |parts| {
                points.clear();
                holes.clear();
                rings.clear();
                for (k, part_ring) in parts.iter().enumerate() {
                    clip_ring(part_ring, square, &mut ring, &mut scratch);
                    ring.dedup();
                    while ring.len() > 1 && ring.first() == ring.last() {
                        ring.pop();
                    }
                    if ring.len() < 3 {
                        if k == 0 {
                            return;
                        }
                        continue;
                    }
                    if k > 0 {
                        holes.push(points.len() as u32);
                    }
                    let start = points.len();
                    points.extend(ring.iter().map(|p| [p[0] * units, p[1] * units]));
                    rings.push((start, points.len()));
                }
                if points.len() < 3 {
                    return;
                }
                let ground = lattice.map_or(0.0, |l| {
                    points.iter().map(|&p| l.height(p)).fold(f32::MAX, f32::min)
                });
                let (bottom, top) = (ground + low * meters, ground + tall * meters);
                // The roof.
                tris.clear();
                earcut.triangulate(&points, &holes, 0, &mut tris);
                let b = part.positions.len() as u32;
                for p in &points {
                    part.push([p[0], top, p[1]], UP, look);
                }
                part.indices.extend(tris.iter().map(|&i| b + i));
                // The walls, but not along the tile's edges: the solid goes
                // on in the next tile.
                let side = Square { lo: 0.0, hi: SIDE };
                for &(start, end) in &rings {
                    let ring = &points[start..end];
                    for i in 0..ring.len() {
                        let (a, c) = (ring[i], ring[(i + 1) % ring.len()]);
                        if side.on_side(a, c) {
                            continue;
                        }
                        let (dx, dz) = (c[0] - a[0], c[1] - a[1]);
                        let len = dx.hypot(dz);
                        if len <= 0.0 {
                            continue;
                        }
                        let n = [dz / len, 0.0, -dx / len];
                        let s = part.positions.len() as u32;
                        part.push([a[0], bottom, a[1]], n, look);
                        part.push([c[0], bottom, c[1]], n, look);
                        part.push([c[0], top, c[1]], n, look);
                        part.push([a[0], top, a[1]], n, look);
                        part.indices.extend([s, s + 1, s + 2, s, s + 2, s + 3]);
                    }
                }
            });
        }
    }
}

/// The materials a model draws with, all reading the palette.
fn material(unlit: bool, alpha: AlphaMode) -> ModelMaterial {
    ModelMaterial {
        base_color_texture: Some(TextureRef {
            texture: 0,
            uv_set: 0,
        }),
        metallic: 0.0,
        roughness: 0.9,
        alpha,
        double_sided: true,
        unlit,
        ..ModelMaterial::default()
    }
}

/// The model of `parts`, each with the material at its index in
/// `materials`.
fn assemble(
    parts: Vec<Part>,
    materials: Vec<ModelMaterial>,
    palette: &Palette,
) -> Option<ModelAsset> {
    let primitives: Vec<ModelPrimitive> = parts
        .into_iter()
        .enumerate()
        .filter_map(|(m, p)| p.primitive(palette, m as u32))
        .collect();
    if primitives.is_empty() {
        return None;
    }
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for p in &primitives {
        for k in 0..3 {
            lo[k] = lo[k].min(p.bounds.0[k]);
            hi[k] = hi[k].max(p.bounds.1[k]);
        }
    }
    let rig = ModelRig {
        nodes: vec![ModelNode {
            name: String::new(),
            parent: None,
            children: Vec::new(),
            rest: Trs::default(),
            mesh: Some(0),
            skin: None,
        }],
        roots: vec![0],
        order: vec![0],
        mesh_bounds: vec![(lo, hi)],
        ..ModelRig::default()
    };
    Some(ModelAsset {
        rig: std::sync::Arc::new(rig),
        meshes: vec![ModelMesh {
            name: String::new(),
            primitives,
        }],
        materials,
        textures: vec![ModelTexture {
            image: Some(0),
            wrap: Wrap::Clamp,
            nearest: false,
        }],
        images: vec![palette.image()],
    })
}

/// How far apart the boxes of a tile's flat parts rise, in model units.
/// The scene blends its parts from the farthest box middle to the nearest;
/// every flat part of a tile spans the tile's square, so the one whose box
/// rises higher draws over the others.
const ORDER: f32 = 0.25;

/// The order among a tile's flat parts of image layer `layer`'s tiles of
/// zoom `zoom`: over the drawing beneath the image layers, finer tiles over
/// coarser ones, each layer over the one before.
pub fn image_order(layer: usize, zoom: u8) -> u32 {
    1 + layer as u32 * 32 + u32::from(zoom.min(31))
}

/// The order of a tile's drawing over its style's `images` image layers.
fn over_order(images: usize) -> u32 {
    1 + images as u32 * 32
}

/// The heights a tile's flat parts lie between, in model units: its ground
/// and the lift over it.
fn ground_span(lattice: Option<&Lattice>, lift: f32) -> (f32, f32) {
    match lattice {
        None => (0.0, lift),
        Some(l) => l.heights.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &h| {
            (lo.min(h), hi.max(h + lift))
        }),
    }
}

/// The two corners of a flat part's box, the tile's square over `span`
/// risen for `order`: vertices no triangle uses, so the part sorts by the
/// tile's middle.
fn box_corners(span: (f32, f32), order: u32) -> [[f32; 3]; 2] {
    let rise = order as f32 * ORDER * 2.0;
    [[0.0, span.0, 0.0], [SIDE, span.1 + rise, SIDE]]
}

/// A tile's model, `None` when it draws nothing.
pub fn raise(r: &Raise<'_>) -> Option<ModelAsset> {
    let k = (f32::from(r.band) - f32::from(r.tile.z)).exp2();
    let lattice = r.relief.as_ref().map(Lattice::new);
    let mut palette = Palette::default();
    let background = palette.id(r.background);
    let (mut floor, mut ground, mut solids) = (Part::default(), Part::default(), Part::default());
    let mut over = Part::default();
    base(lattice.as_ref(), background, &mut floor);
    let units = SIDE / EXTENT;
    let lift = LIFT / k;
    lay(
        r.meshes,
        units,
        k,
        lift,
        lattice.as_ref(),
        &mut palette,
        &mut ground,
    );
    // Without image layers the whole drawing is one part; with them, the
    // part over the images is a second, ordered over them.
    let split = if r.images == 0 {
        &mut ground
    } else {
        &mut over
    };
    lay(
        r.over,
        units,
        k,
        lift,
        lattice.as_ref(),
        &mut palette,
        split,
    );
    if r.images > 0 {
        let span = ground_span(lattice.as_ref(), lift);
        for (part, order) in [(&mut ground, 0), (&mut over, over_order(r.images))] {
            if !part.indices.is_empty() {
                for p in box_corners(span, order) {
                    part.push(p, UP, (background, 0.0, 1.0));
                }
            }
        }
    }
    extrude(r, lattice.as_ref(), &mut palette, &mut solids);
    assemble(
        vec![floor, ground, over, solids],
        vec![
            material(true, AlphaMode::Opaque),
            material(true, AlphaMode::Blend),
            material(true, AlphaMode::Blend),
            material(false, AlphaMode::Opaque),
        ],
        &palette,
    )
}

/// An image layer's tile laid on a tile: what [`raise_image`] builds from.
pub struct Imagery<'a> {
    /// The tile it lies on, and the zoom level that tile's drawing was
    /// built for.
    pub tile: TileId,
    pub band: u8,
    /// The image's tile and its straight sRGB RGBA8 pixels.
    pub image_tile: TileId,
    pub width: u32,
    pub height: u32,
    pub rgba: &'a [u8],
    /// Its order among the tile's flat parts: [`image_order`].
    pub order: u32,
    pub relief: Option<Relief<'a>>,
}

/// The part of an image tile over `tile`, as a model in `tile`'s units:
/// laid over its ground as its drawing is, the image cut to the part it
/// covers. `None` when the two do not meet.
pub fn raise_image(r: &Imagery<'_>) -> Option<ModelAsset> {
    let (w, h) = (r.width as usize, r.height as usize);
    if w == 0 || h == 0 || r.rgba.len() < w * h * 4 {
        return None;
    }
    let (t, it) = (r.tile, r.image_tile);
    // Where it lies on the tile, in model units, and the part of the image
    // that lies there, in its pixels.
    let (rect, crop) = if it.z >= t.z {
        let d = it.z - t.z;
        if d > 24 || it.x >> d != t.x || it.y >> d != t.y {
            return None;
        }
        let side = SIDE / (1u32 << d) as f32;
        let x0 = (it.x - (t.x << d)) as f32 * side;
        let y0 = (it.y - (t.y << d)) as f32 * side;
        ([x0, y0, side], [0.0, 0.0, w as f64, h as f64])
    } else {
        let d = t.z - it.z;
        if d > 24 || t.x >> d != it.x || t.y >> d != it.y {
            return None;
        }
        let s = f64::from(1u32 << d);
        let u = f64::from(t.x - (it.x << d)) / s;
        let v = f64::from(t.y - (it.y << d)) / s;
        (
            [0.0, 0.0, SIDE],
            [u * w as f64, v * h as f64, w as f64 / s, h as f64 / s],
        )
    };
    // The texels it reads, whole, and where the part lies among them.
    let (x0, y0) = (crop[0].floor() as usize, crop[1].floor() as usize);
    let x1 = ((crop[0] + crop[2]).ceil() as usize).clamp(x0 + 1, w);
    let y1 = ((crop[1] + crop[3]).ceil() as usize).clamp(y0 + 1, h);
    let (cw, ch) = (x1 - x0, y1 - y0);
    let mut rgba = Vec::with_capacity(cw * ch * 4);
    for row in y0..y1 {
        rgba.extend_from_slice(&r.rgba[(row * w + x0) * 4..(row * w + x1) * 4]);
    }
    let uv0 = [
        ((crop[0] - x0 as f64) / cw as f64) as f32,
        ((crop[1] - y0 as f64) / ch as f64) as f32,
    ];
    let uv1 = [
        ((crop[0] + crop[2] - x0 as f64) / cw as f64) as f32,
        ((crop[1] + crop[3] - y0 as f64) / ch as f64) as f32,
    ];
    let k = (f32::from(r.band) - f32::from(t.z)).exp2();
    let lift = LIFT / k;
    let lattice = r.relief.as_ref().map(Lattice::new);
    // Over relief, a lattice of the ground's own cells; flat, one quad.
    let n = lattice
        .as_ref()
        .map_or(1, |l| ((rect[2] / l.cell).round() as usize).max(1));
    let step = rect[2] / n as f32;
    let mut prim = ModelPrimitive::default();
    for j in 0..=n {
        for i in 0..=n {
            let p = [rect[0] + i as f32 * step, rect[1] + j as f32 * step];
            let y = lattice.as_ref().map_or(0.0, |l| l.height(p)) + lift;
            let (fu, fv) = (i as f32 / n as f32, j as f32 / n as f32);
            prim.positions.push([p[0], y, p[1]]);
            prim.normals.push(UP);
            prim.uvs.push([
                uv0[0] + (uv1[0] - uv0[0]) * fu,
                uv0[1] + (uv1[1] - uv0[1]) * fv,
            ]);
        }
    }
    let at = |i: usize, j: usize| (j * (n + 1) + i) as u32;
    for j in 0..n {
        for i in 0..n {
            prim.indices.extend([at(i, j), at(i + 1, j), at(i, j + 1)]);
            prim.indices
                .extend([at(i + 1, j + 1), at(i, j + 1), at(i + 1, j)]);
        }
    }
    for p in box_corners(ground_span(lattice.as_ref(), lift), r.order) {
        prim.positions.push(p);
        prim.normals.push(UP);
        prim.uvs.push(uv0);
    }
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for p in &prim.positions {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    prim.bounds = (lo, hi);
    prim.material = Some(0);
    let rig = ModelRig {
        nodes: vec![ModelNode {
            name: String::new(),
            parent: None,
            children: Vec::new(),
            rest: Trs::default(),
            mesh: Some(0),
            skin: None,
        }],
        roots: vec![0],
        order: vec![0],
        mesh_bounds: vec![(lo, hi)],
        ..ModelRig::default()
    };
    Some(ModelAsset {
        rig: std::sync::Arc::new(rig),
        meshes: vec![ModelMesh {
            name: String::new(),
            primitives: vec![prim],
        }],
        materials: vec![material(true, AlphaMode::Blend)],
        textures: vec![ModelTexture {
            image: Some(0),
            wrap: Wrap::Clamp,
            nearest: false,
        }],
        images: vec![ModelImage::mipped(
            cw as u32,
            ch as u32,
            rgba,
            ImageKind::Color,
        )],
    })
}

/// A shape's drawing as a model: `meshes` in the points of its frame,
/// extruded at `k` frame points per point, laid `lift` frame points over
/// the ground, which `ground` gives the height of at a frame point when the
/// map has relief. Its edges are cut rather than blended, so that it draws
/// with the solids, over every tile's drawing; each color in the order it
/// first appears lies `step` higher than the one before, so a line draws
/// over its casing.
pub fn raise_shape(
    meshes: &[Mesh],
    k: f32,
    lift: f32,
    step: f32,
    ground: Option<&dyn Fn([f32; 2]) -> f32>,
) -> Option<ModelAsset> {
    let mut palette = Palette::default();
    let mut part = Part::default();
    for mesh in meshes {
        let base = part.positions.len() as u32;
        for v in mesh.vertices.iter() {
            let p = [v.pos[0] + v.extrude[0] / k, v.pos[1] + v.extrude[1] / k];
            let h = ground.map_or(0.0, |g| g(p));
            let color = palette.id([v.color[0], v.color[1], v.color[2], 1.0]);
            let edge = v.edge * v.color[3];
            let y = h + lift + step * color as f32;
            part.push([p[0], y, p[1]], UP, (color, edge, 1.0));
        }
        part.indices.extend(mesh.indices.iter().map(|&i| base + i));
    }
    assemble(
        vec![part],
        vec![material(true, AlphaMode::Mask(0.5))],
        &palette,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mvt::TileEncoder;
    use crate::style::{Color, Layer};

    fn features() -> mvt::VectorTile {
        let mut t = TileEncoder::new();
        let mut b = t.layer("building", 4096);
        let sq: &[[i32; 2]] = &[[1000, 1000], [2000, 1000], [2000, 2000], [1000, 2000]];
        b.feature(None, GeomType::Polygon, &[sq], &[("height", 30.0.into())]);
        // Running off the tile's east edge: no wall there.
        let cut: &[[i32; 2]] = &[[3000, 3000], [4200, 3000], [4200, 3500], [3000, 3500]];
        b.feature(None, GeomType::Polygon, &[cut], &[]);
        b.finish();
        mvt::decode(&t.finish()).unwrap()
    }

    fn style() -> Style {
        Style::new("t", Color::WHITE).with(Layer::extrusion(
            "b",
            "building",
            Color::hex(0x808080),
            10.0,
        ))
    }

    fn quad(color: [f32; 4]) -> Mesh {
        let v = |x: f32, y: f32| MeshVertex::new([x, y], color, 1.0);
        Mesh::new(
            vec![
                v(0.0, 0.0),
                v(4096.0, 0.0),
                v(4096.0, 4096.0),
                v(0.0, 4096.0),
            ],
            vec![0, 1, 2, 0, 2, 3],
        )
    }

    fn raised(relief: Option<Relief<'_>>) -> ModelAsset {
        let tile = TileId::new(14, 8469, 5582);
        let decoded = features();
        let layers: Vec<&mvt::Layer> = decoded.layers.iter().collect();
        let style = style();
        raise(&Raise {
            tile,
            band: 14,
            meshes: &[quad([0.2, 0.4, 0.8, 1.0])],
            over: &[],
            images: 0,
            layers: &layers,
            style: &style,
            background: [1.0; 4],
            relief,
        })
        .expect("draws")
    }

    #[test]
    fn a_tile_is_a_base_its_drawing_over_it_and_its_buildings_raised() {
        let m = raised(None);
        let prims = &m.meshes[0].primitives;
        assert_eq!(prims.len(), 3);
        // The base, then the drawing lifted over it.
        assert_eq!(prims[0].bounds.1[1], 0.0);
        assert!((prims[1].bounds.0[1] - LIFT).abs() < 1e-6);
        assert_eq!(prims[1].indices.len(), 6);
        // The solids: 30 m and 10 m tall, roofs and walls.
        let meters = units_per_meter(TileId::new(14, 8469, 5582));
        let top = prims[2].bounds.1[1];
        assert!((top - 30.0 * meters).abs() < 1e-3, "{top}");
        let walls = prims[2].normals.iter().filter(|n| n[1] == 0.0).count() / 4;
        // Four walls round the square, three round the cut one.
        assert_eq!(walls, 7);
        // Outward: the walls facing west stand at the buildings' west ends.
        let west: Vec<f32> = prims[2]
            .positions
            .iter()
            .zip(&prims[2].normals)
            .filter(|(_, n)| **n == [-1.0, 0.0, 0.0])
            .map(|(p, _)| p[0])
            .collect();
        assert_eq!(
            west,
            [125.0; 4].into_iter().chain([375.0; 4]).collect::<Vec<_>>()
        );
        assert!(!m.materials[3].unlit && m.materials[1].alpha == AlphaMode::Blend);
        let image = &m.images[0];
        assert_eq!(image.width, 2);
        assert_eq!(image.height, 2 * 3);
    }

    /// The middle of a primitive's box, its two unused corners included,
    /// as the scene sorts it.
    fn middle(p: &ModelPrimitive) -> [f32; 3] {
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for q in &p.positions {
            for k in 0..3 {
                lo[k] = lo[k].min(q[k]);
                hi[k] = hi[k].max(q[k]);
            }
        }
        core::array::from_fn(|k| (lo[k] + hi[k]) / 2.0)
    }

    #[test]
    fn images_sort_between_the_drawing_beneath_them_and_the_one_over() {
        let tile = TileId::new(14, 8469, 5582);
        let decoded = features();
        let layers: Vec<&mvt::Layer> = decoded.layers.iter().collect();
        let style = style();
        let m = raise(&Raise {
            tile,
            band: 14,
            meshes: &[quad([0.2, 0.4, 0.8, 1.0])],
            over: &[quad([0.9, 0.1, 0.1, 1.0])],
            images: 1,
            layers: &layers,
            style: &style,
            background: [1.0; 4],
            relief: None,
        })
        .expect("draws");
        let prims = &m.meshes[0].primitives;
        assert_eq!(prims.len(), 4);
        let (under, over) = (middle(&prims[1]), middle(&prims[2]));
        // Both span the tile, the drawing over the images higher.
        assert_eq!([under[0], under[2]], [SIDE / 2.0; 2]);
        assert_eq!([over[0], over[2]], [SIDE / 2.0; 2]);
        // An image a level finer, in the tile's north-east quarter.
        let rgba: Vec<u8> = (0..256 * 256).flat_map(|i| [i as u8, 0, 0, 255]).collect();
        let image = |image_tile: TileId, order: u32| {
            raise_image(&Imagery {
                tile,
                band: 14,
                image_tile,
                width: 256,
                height: 256,
                rgba: &rgba,
                order,
                relief: None,
            })
        };
        let fine = TileId::new(15, 8469 * 2 + 1, 5582 * 2);
        let im = image(fine, image_order(0, 15)).expect("meets");
        let p = &im.meshes[0].primitives[0];
        let laid: Vec<[f32; 3]> = p.positions[..4].to_vec();
        assert_eq!(laid[0], [SIDE / 2.0, LIFT, 0.0]);
        assert_eq!(laid[3], [SIDE, LIFT, SIDE / 2.0]);
        assert_eq!((p.uvs[0], p.uvs[3]), ([0.0, 0.0], [1.0, 1.0]));
        assert_eq!(im.images[0].width, 256);
        assert!(!im.images[0].mips.is_empty());
        let mid = middle(p);
        assert_eq!([mid[0], mid[2]], [SIDE / 2.0; 2]);
        assert!(
            under[1] < mid[1] && mid[1] < over[1],
            "{under:?} {mid:?} {over:?}"
        );
        // A coarser one covers the tile with the quarter over it.
        let coarse = TileId::new(13, 8469 / 2, 5582 / 2);
        let im = image(coarse, image_order(0, 13)).expect("meets");
        assert_eq!((im.images[0].width, im.images[0].height), (128, 128));
        let coarse_mid = middle(&im.meshes[0].primitives[0]);
        assert!(coarse_mid[1] < mid[1]);
        // Elsewhere, nothing.
        assert!(image(TileId::new(15, 0, 0), 1).is_none());
    }

    #[test]
    fn over_relief_the_drawing_is_cut_onto_the_lattice() {
        // A ramp rising eastward 1000 m across the tile.
        let dem = Dem {
            width: 64,
            height: 64,
            meters: (0..64 * 64)
                .map(|i| (i % 64) as f32 / 63.0 * 1000.0)
                .collect(),
        };
        let relief = Relief {
            dem: &dem,
            origin: [0.0, 0.0],
            span: 1.0,
            scale: 0.1,
        };
        let m = raised(Some(relief));
        let prims = &m.meshes[0].primitives;
        let ground = &prims[1];
        // Two triangles become two per cell at least.
        assert!(
            ground.indices.len() / 3 >= 2 * CELLS * CELLS,
            "{}",
            ground.indices.len()
        );
        // Every vertex lies on the relief, just over it.
        for p in &ground.positions {
            let h = relief.height([p[0], p[2]]);
            assert!((p[1] - h - LIFT).abs() < 0.6, "{p:?} {h}");
        }
        // The east rises 100 units over the west.
        assert!(ground.bounds.1[1] - ground.bounds.0[1] > 90.0);
        // Buildings stand on the ground beneath them.
        let solids = &prims[2];
        let low = solids
            .positions
            .iter()
            .map(|p| p[1])
            .fold(f32::MAX, f32::min);
        assert!(low > 10.0, "{low}");
    }

    #[test]
    fn heights_read_numbers_and_text() {
        struct P(Vec<(&'static str, mvt::Value)>);
        impl Properties for P {
            fn get(&self, key: &str) -> Option<&mvt::Value> {
                self.0.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
            }
            fn kind(&self) -> GeomType {
                GeomType::Polygon
            }
        }
        assert_eq!(
            feature_height(&P(vec![("height", "12.5 m".into())])),
            Some(12.5)
        );
        assert_eq!(
            feature_height(&P(vec![("building:levels", 4.0.into())])),
            Some(12.0)
        );
        assert_eq!(feature_height(&P(vec![])), None);
        assert_eq!(feature_base(&P(vec![("min_height", 3.0.into())])), 3.0);
    }
}
