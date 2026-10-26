//! The built-in meshes, generated procedurally.
//!
//! Every shape spans `[-1, 1]` on each axis, so an instance's linear part is
//! the shape's half extents (turned): a body with half extents `h` draws as
//! its shape scaled by `h`. Faces wind counter-clockwise seen from outside.
//! Curved surfaces share their vertices across a ring, so a sphere or a
//! cylinder's side is smooth-shaded; a hard edge (a cap's rim, a box's edge)
//! splits its vertices so each face keeps its own normal.

use core::f32::consts::{FRAC_PI_2, PI, TAU};

use viso_gpu::scene::{SceneVertex, normalize};

/// A built-in mesh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum Shape {
    /// A box.
    Cube,
    /// A UV sphere.
    Sphere,
    /// A cylinder capped by two half-ellipsoids, each half the height of the
    /// straight part: a standing character.
    Capsule,
    /// A capped cylinder along `y`.
    Cylinder,
    /// A cone along `y`, its apex up, on a flat base.
    Cone,
    /// A ramp: a box cut by the plane through its top back edge (`y = 1`,
    /// `z = -1`) and its bottom front edge (`y = -1`, `z = 1`).
    Wedge,
    /// A square in the `xz` plane facing `+y`.
    Plane,
}

impl Shape {
    /// Every shape, in id order.
    pub const ALL: [Shape; 7] = [
        Shape::Cube,
        Shape::Sphere,
        Shape::Capsule,
        Shape::Cylinder,
        Shape::Cone,
        Shape::Wedge,
        Shape::Plane,
    ];

    /// Its vertices and triangle indices.
    pub fn geometry(self) -> (Vec<SceneVertex>, Vec<u16>) {
        let mut b = Builder::default();
        match self {
            Shape::Cube => cube(&mut b),
            Shape::Sphere => {
                let rings: Vec<Ring> = (0..=SPHERE_RINGS)
                    .map(|i| {
                        let phi = -FRAC_PI_2 + PI * i as f32 / SPHERE_RINGS as f32;
                        let (y, r) = phi.sin_cos();
                        Ring::new(r, y, [r, y])
                    })
                    .collect();
                b.revolve(&rings);
            }
            Shape::Capsule => {
                // Each cap is the half-ellipsoid of radius 1 across and
                // `CAP` tall, its normal the ellipse's gradient.
                let cap = |phi: f32, centre: f32| {
                    let (s, c) = phi.sin_cos();
                    Ring::new(c, centre + CAP * s, [c, s / CAP])
                };
                let half = CAPSULE_CAP_RINGS;
                let mut rings: Vec<Ring> = (0..=half)
                    .map(|i| cap(-FRAC_PI_2 + FRAC_PI_2 * i as f32 / half as f32, -CAP))
                    .collect();
                rings.extend((0..=half).map(|i| cap(FRAC_PI_2 * i as f32 / half as f32, CAP)));
                b.revolve(&rings);
            }
            Shape::Cylinder => {
                b.revolve(&[
                    Ring::new(1.0, -1.0, [1.0, 0.0]),
                    Ring::new(1.0, 1.0, [1.0, 0.0]),
                ]);
                b.disc(1.0);
                b.disc(-1.0);
            }
            Shape::Cone => {
                // The side's normal leans up by the slope: the radius falls by
                // 1 over a height of 2.
                let n = [2.0, 1.0];
                b.revolve(&[Ring::new(1.0, -1.0, n), Ring::new(0.0, 1.0, n)]);
                b.disc(-1.0);
            }
            Shape::Wedge => wedge(&mut b),
            Shape::Plane => b.face(
                &[
                    [-1.0, 0.0, -1.0],
                    [-1.0, 0.0, 1.0],
                    [1.0, 0.0, 1.0],
                    [1.0, 0.0, -1.0],
                ],
                [0.0, 1.0, 0.0],
            ),
        }
        let mut vertices = b.vertices;
        let positions: Vec<[f32; 3]> = vertices.iter().map(|v| v.position).collect();
        let normals: Vec<[f32; 3]> = vertices.iter().map(|v| v.normal).collect();
        let uvs: Vec<[f32; 2]> = vertices.iter().map(|v| v.uv).collect();
        let indices: Vec<u32> = b.indices.iter().map(|&i| u32::from(i)).collect();
        let tangents = super::model::geometry::tangents(&positions, &normals, &uvs, &indices);
        for (v, t) in vertices.iter_mut().zip(tangents) {
            v.tangent = t;
        }
        (vertices, b.indices)
    }
}

/// Segments around a surface of revolution.
const SEGMENTS: u32 = 32;
/// Rings from pole to pole of the sphere.
const SPHERE_RINGS: u32 = 16;
/// Rings across each capsule cap.
const CAPSULE_CAP_RINGS: u32 = 8;
/// The height of a capsule cap, and the half height of its straight part.
const CAP: f32 = 0.5;

/// One ring of a surface of revolution: its radius, height and the outward
/// normal's radial and vertical parts.
#[derive(Clone, Copy)]
struct Ring {
    radius: f32,
    y: f32,
    normal: [f32; 2],
}

impl Ring {
    fn new(radius: f32, y: f32, normal: [f32; 2]) -> Ring {
        Ring {
            // A pole's radius computes to a rounding error, either side of 0.
            radius: if radius.abs() < 1e-6 { 0.0 } else { radius },
            y,
            normal,
        }
    }
}

#[derive(Default)]
struct Builder {
    vertices: Vec<SceneVertex>,
    indices: Vec<u16>,
}

impl Builder {
    fn vertex(&mut self, position: [f32; 3], normal: [f32; 3], uv: [f32; 2]) -> u16 {
        let i = self.vertices.len() as u16;
        self.vertices.push(SceneVertex {
            position,
            normal: normalize(normal),
            uv,
            tangent: [0.0; 4],
        });
        i
    }

    /// The triangle `a b c`, unless two of its corners coincide (a pole).
    fn triangle(&mut self, a: u16, b: u16, c: u16) {
        let p = |i: u16| self.vertices[usize::from(i)].position;
        if p(a) == p(b) || p(b) == p(c) || p(c) == p(a) {
            return;
        }
        self.indices.extend([a, b, c]);
    }

    /// The surface swept by `rings`, bottom to top, about `y`: a seam column
    /// is repeated so the texture coordinate wraps.
    fn revolve(&mut self, rings: &[Ring]) {
        let base = self.vertices.len() as u16;
        let columns = SEGMENTS + 1;
        for (i, ring) in rings.iter().enumerate() {
            let v = i as f32 / (rings.len() - 1) as f32;
            for j in 0..columns {
                let theta = TAU * j as f32 / SEGMENTS as f32;
                let (s, c) = theta.sin_cos();
                self.vertex(
                    [ring.radius * s, ring.y, ring.radius * c],
                    [ring.normal[0] * s, ring.normal[1], ring.normal[0] * c],
                    [j as f32 / SEGMENTS as f32, 1.0 - v],
                );
            }
        }
        let at = |i: usize, j: u32| base + (i as u32 * columns + j) as u16;
        for i in 0..rings.len() - 1 {
            for j in 0..SEGMENTS {
                let (a, b, c, d) = (at(i, j), at(i, j + 1), at(i + 1, j + 1), at(i + 1, j));
                self.triangle(a, b, c);
                self.triangle(a, c, d);
            }
        }
    }

    /// The unit disc at height `y`, facing up at the top and down at the
    /// bottom.
    fn disc(&mut self, y: f32) {
        let up = y > 0.0;
        let normal = [0.0, y.signum(), 0.0];
        let centre = self.vertex([0.0, y, 0.0], normal, [0.5, 0.5]);
        let first = self.vertices.len() as u16;
        for j in 0..SEGMENTS {
            let (s, c) = (TAU * j as f32 / SEGMENTS as f32).sin_cos();
            self.vertex([s, y, c], normal, [0.5 + 0.5 * s, 0.5 - 0.5 * c]);
        }
        for j in 0..SEGMENTS as u16 {
            let (a, b) = (first + j, first + (j + 1) % SEGMENTS as u16);
            // The ring runs counter-clockwise seen from above.
            if up {
                self.triangle(centre, a, b);
            } else {
                self.triangle(centre, b, a);
            }
        }
    }

    /// A flat convex polygon of `corners` with `normal`, wound to face it
    /// whichever way the corners run.
    fn face(&mut self, corners: &[[f32; 3]], normal: [f32; 3]) {
        // The two axes across the face: those the normal does not point along.
        let axis = (0..3)
            .max_by(|&a, &b| normal[a].abs().total_cmp(&normal[b].abs()))
            .unwrap();
        let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
        let first = self.vertices.len() as u16;
        for p in corners {
            self.vertex(*p, normal, [p[u] * 0.5 + 0.5, 0.5 - p[v] * 0.5]);
        }
        let sub = |a: [f32; 3], b: [f32; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
        let (e1, e2) = (sub(corners[1], corners[0]), sub(corners[2], corners[0]));
        let cross = [
            e1[1] * e2[2] - e1[2] * e2[1],
            e1[2] * e2[0] - e1[0] * e2[2],
            e1[0] * e2[1] - e1[1] * e2[0],
        ];
        let facing = cross[0] * normal[0] + cross[1] * normal[1] + cross[2] * normal[2] > 0.0;
        for k in 1..corners.len() as u16 - 1 {
            let (b, c) = (first + k, first + k + 1);
            if facing {
                self.triangle(first, b, c);
            } else {
                self.triangle(first, c, b);
            }
        }
    }
}

fn cube(b: &mut Builder) {
    for axis in 0..3 {
        for sign in [-1.0f32, 1.0] {
            let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
            let corner = |su: f32, sv: f32| {
                let mut p = [0.0; 3];
                p[axis] = sign;
                p[u] = su;
                p[v] = sv;
                p
            };
            let mut normal = [0.0; 3];
            normal[axis] = sign;
            b.face(
                &[
                    corner(-1.0, -1.0),
                    corner(1.0, -1.0),
                    corner(1.0, 1.0),
                    corner(-1.0, 1.0),
                ],
                normal,
            );
        }
    }
}

fn wedge(b: &mut Builder) {
    // Bottom, back and the slope, then the two triangular sides.
    b.face(
        &[
            [-1.0, -1.0, -1.0],
            [1.0, -1.0, -1.0],
            [1.0, -1.0, 1.0],
            [-1.0, -1.0, 1.0],
        ],
        [0.0, -1.0, 0.0],
    );
    b.face(
        &[
            [-1.0, -1.0, -1.0],
            [-1.0, 1.0, -1.0],
            [1.0, 1.0, -1.0],
            [1.0, -1.0, -1.0],
        ],
        [0.0, 0.0, -1.0],
    );
    b.face(
        &[
            [-1.0, -1.0, 1.0],
            [1.0, -1.0, 1.0],
            [1.0, 1.0, -1.0],
            [-1.0, 1.0, -1.0],
        ],
        [0.0, 1.0, 1.0],
    );
    for x in [-1.0, 1.0] {
        b.face(
            &[[x, -1.0, -1.0], [x, 1.0, -1.0], [x, -1.0, 1.0]],
            [x, 0.0, 0.0],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }

    fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }

    /// Every triangle faces the way its corners' normals point, which for a
    /// closed convex shape is away from its centre.
    #[test]
    fn every_shape_winds_outward_within_the_unit_box() {
        for shape in Shape::ALL {
            let (vertices, indices) = shape.geometry();
            assert!(!indices.is_empty() && indices.len() % 3 == 0, "{shape:?}");
            for v in &vertices {
                assert!(
                    v.position.iter().all(|c| c.abs() <= 1.0 + 1e-6),
                    "{shape:?} {v:?}"
                );
                let len = dot(v.normal, v.normal).sqrt();
                assert!((len - 1.0).abs() < 1e-5, "{shape:?} unit normal {v:?}");
            }
            for t in indices.as_chunks::<3>().0 {
                let [a, b, c] = t.map(|i| vertices[usize::from(i)]);
                let e1 = sub(b.position, a.position);
                let e2 = sub(c.position, a.position);
                let n = [
                    e1[1] * e2[2] - e1[2] * e2[1],
                    e1[2] * e2[0] - e1[0] * e2[2],
                    e1[0] * e2[1] - e1[1] * e2[0],
                ];
                let mean = [0, 1, 2].map(|i| a.normal[i] + b.normal[i] + c.normal[i]);
                assert!(dot(n, mean) > 0.0, "{shape:?} winds inward: {t:?}");
                if shape != Shape::Plane {
                    let inside = match shape {
                        Shape::Wedge => [0.0, -0.5, -0.5],
                        _ => [0.0; 3],
                    };
                    let centre = [0, 1, 2]
                        .map(|i| (a.position[i] + b.position[i] + c.position[i]) / 3.0 - inside[i]);
                    assert!(dot(n, centre) > 0.0, "{shape:?} faces its centre: {t:?}");
                }
            }
        }
    }

    /// A box's six faces and a sphere's rings reach the unit box's walls.
    #[test]
    fn the_shapes_fill_the_unit_box() {
        for shape in [Shape::Cube, Shape::Sphere, Shape::Capsule, Shape::Cylinder] {
            let (vertices, _) = shape.geometry();
            for axis in 0..3 {
                let max = vertices
                    .iter()
                    .map(|v| v.position[axis])
                    .fold(f32::MIN, f32::max);
                assert!((max - 1.0).abs() < 1e-5, "{shape:?} axis {axis}: {max}");
            }
        }
    }
}
