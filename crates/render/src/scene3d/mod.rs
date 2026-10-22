//! The 3D scene lane (ADR 0047): lit, depth-tested meshes drawn into a
//! scene's own target, which the 2D scene composites like an image.
//!
//! A scene is retained render data, not UI nodes. Its owner fills a
//! [`SceneFrame`] — a camera, the lighting, a background and one batch of
//! [`SceneInstance`]s per mesh — and hands it to the renderer with
//! [`Renderer::swap_scene`] under a [`SceneSlot`]. A
//! [`Primitive::Scene`] draw of that slot then places the scene in the 2D
//! stream: the renderer sizes the slot's color and depth targets to the
//! draw's device pixels (in 64-pixel steps, so a resize rarely reallocates),
//! culls each instance against the camera's frustum, and renders the scene
//! ahead of the frame's 2D passes — only when the frame or the size changed
//! since it last rendered. The 2D pass composites the target as an image, so
//! clip, transform, opacity and z-order are the 2D scene's.
//!
//! Meshes are the built-in [`Shape`]s, generated once on first use, or ones
//! uploaded with [`Renderer::create_scene_mesh`].
//!
//! [`Renderer::swap_scene`]: crate::Renderer::swap_scene
//! [`Renderer::create_scene_mesh`]: crate::Renderer::create_scene_mesh
//! [`Primitive::Scene`]: crate::Primitive::Scene

mod lane;
mod shapes;

pub(crate) use lane::SceneLane;
pub use lane::SceneStats;
pub use shapes::Shape;
pub use viso_gpu::scene::{SceneInstance, SceneMaterial, SceneUniforms, SceneVertex};

pub use viso_math::{LinearStraight, Mat4, Quat, Vec3, vec3};

use viso_gpu::scene::{encode, normalize};

/// A mesh the scene lane draws: a built-in [`Shape`] or an uploaded mesh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MeshId(u32);

impl MeshId {
    /// The id's dense index.
    #[inline]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    pub(crate) const fn new(index: u32) -> MeshId {
        MeshId(index)
    }
}

impl From<Shape> for MeshId {
    #[inline]
    fn from(shape: Shape) -> MeshId {
        MeshId(shape as u32)
    }
}

/// How a camera projects.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Projection {
    /// Perspective, with this vertical field of view in radians.
    Perspective { fov_y: f32 },
    /// Orthographic, viewing this many world units from the bottom of the
    /// view to its top.
    Orthographic { height: f32 },
}

/// Where a scene is seen from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    pub eye: Vec3,
    pub target: Vec3,
    pub up: Vec3,
    pub projection: Projection,
    /// The distance to the near plane, greater than zero.
    pub near: f32,
    /// The distance to the far plane; infinite (the default) for a
    /// perspective camera that never clips the distance.
    pub far: f32,
}

impl Default for Camera {
    fn default() -> Camera {
        Camera {
            eye: vec3(0.0, 6.0, 12.0),
            target: Vec3::ZERO,
            up: vec3(0.0, 1.0, 0.0),
            projection: Projection::Perspective {
                fov_y: 50f32.to_radians(),
            },
            near: 0.1,
            far: f32::INFINITY,
        }
    }
}

/// The far plane an orthographic camera with an infinite `far` uses.
const ORTHO_FAR: f32 = 1.0e4;

impl Camera {
    /// World to clip space for a view `aspect` wide per unit of height, with
    /// reversed depth (near at 1, far at 0).
    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let near = self.near.max(1e-4);
        let proj = match self.projection {
            Projection::Perspective { fov_y } => {
                Mat4::perspective_reversed(fov_y, aspect, near, self.far)
            }
            Projection::Orthographic { height } => {
                let far = if self.far.is_finite() {
                    self.far
                } else {
                    ORTHO_FAR
                };
                Mat4::ortho_reversed(height * aspect, height, near, far)
            }
        };
        proj.mul(&Mat4::look_at(self.eye, self.target, self.up))
    }
}

/// How a scene is lit: one directional light, a hemispheric ambient, and
/// exponential fog. Colors are linear RGB.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lighting {
    /// The direction toward the light.
    pub direction: Vec3,
    pub color: [f32; 3],
    pub intensity: f32,
    /// The ambient light from above and from below.
    pub sky: [f32; 3],
    pub ground: [f32; 3],
    pub fog: [f32; 3],
    /// The fog's density per unit of distance; `0.0` for none.
    pub fog_density: f32,
}

impl Default for Lighting {
    fn default() -> Lighting {
        Lighting {
            direction: vec3(0.4, 0.85, 0.35),
            color: [1.0, 0.96, 0.9],
            intensity: 1.0,
            sky: [0.32, 0.36, 0.45],
            ground: [0.1, 0.09, 0.08],
            fog: [0.62, 0.7, 0.8],
            fog_density: 0.0,
        }
    }
}

/// One frame of a scene: what [`Renderer::swap_scene`] hands the renderer.
///
/// Its instances are grouped by material and mesh, one batch each, so the
/// lane draws each in one instanced draw: the lit meshes, then the unlit
/// ones, then the sprites, blended back to front over them. Filling reuses
/// the batches' storage, so a steady scene refills without allocating.
///
/// [`Renderer::swap_scene`]: crate::Renderer::swap_scene
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SceneFrame {
    pub camera: Camera,
    pub lighting: Lighting,
    /// What shows where nothing is drawn, straight linear RGBA. A transparent
    /// background lets the 2D content behind the scene show through.
    pub background: LinearStraight,
    lit: Batches,
    unlit: Batches,
    sprites: Vec<SceneSprite>,
}

/// One material's instances, by mesh.
#[derive(Debug, Clone, Default, PartialEq)]
struct Batches {
    /// Each mesh's instances, by [`MeshId::index`].
    by_mesh: Vec<Vec<SceneInstance>>,
    /// The meshes with instances, in the order they were first pushed.
    meshes: Vec<MeshId>,
}

impl Batches {
    fn clear(&mut self) {
        for mesh in self.meshes.drain(..) {
            self.by_mesh[mesh.index()].clear();
        }
    }

    fn push(&mut self, mesh: MeshId, instance: SceneInstance) {
        if self.by_mesh.len() <= mesh.index() {
            self.by_mesh.resize_with(mesh.index() + 1, Vec::new);
        }
        let batch = &mut self.by_mesh[mesh.index()];
        if batch.is_empty() {
            self.meshes.push(mesh);
        }
        batch.push(instance);
    }

    fn iter(&self) -> impl Iterator<Item = (MeshId, &[SceneInstance])> {
        self.meshes
            .iter()
            .map(|&mesh| (mesh, self.by_mesh[mesh.index()].as_slice()))
    }
}

/// A particle: a soft disc of `radius` at `position`, always facing the
/// camera, in a color the frame encodes as it is pushed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SceneSprite {
    pub position: Vec3,
    pub radius: f32,
    color: [f32; 4],
}

impl SceneSprite {
    /// A sprite of `radius` at `position` in `color`, its alpha the disc's
    /// opacity at its centre.
    pub fn new(position: Vec3, radius: f32, color: LinearStraight) -> SceneSprite {
        SceneSprite {
            position,
            radius,
            color: encoded(color),
        }
    }

    /// The sprite on a [`Shape::Plane`] turned to face along `-forward`,
    /// spanned by `right` and `up`, at least `least` across. A sprite
    /// smaller than that is drawn at it, its opacity lowered by the ratio of
    /// their areas, so a particle under a pixel still shows as faintly as
    /// its size warrants rather than vanishing between the pixel centres.
    pub(crate) fn facing(
        &self,
        (right, up, forward): (Vec3, Vec3, Vec3),
        least: f32,
    ) -> SceneInstance {
        let (r, color) = if self.radius < least && least > 0.0 {
            let k = (self.radius / least).powi(2);
            (least, self.color.map(|c| c * k))
        } else {
            (self.radius, self.color)
        };
        let (x, y, z) = (right * r, forward * -1.0, up * r);
        let p = self.position;
        SceneInstance {
            rows: [
                [x.x, y.x, z.x, p.x],
                [x.y, y.y, z.y, p.y],
                [x.z, y.z, z.z, p.z],
            ],
            color,
        }
    }
}

impl SceneFrame {
    /// An empty frame with the default camera and lighting.
    pub fn new() -> SceneFrame {
        SceneFrame::default()
    }

    /// Drop every instance and sprite, keeping the storage.
    pub fn clear_instances(&mut self) {
        self.lit.clear();
        self.unlit.clear();
        self.sprites.clear();
    }

    /// Add `instance` of `mesh`, lit by the scene's light.
    pub fn push(&mut self, mesh: impl Into<MeshId>, instance: SceneInstance) {
        self.lit.push(mesh.into(), instance);
    }

    /// Add `instance` of `mesh` in its color as is: debug geometry, or a
    /// surface that glows.
    pub fn push_unlit(&mut self, mesh: impl Into<MeshId>, instance: SceneInstance) {
        self.unlit.push(mesh.into(), instance);
    }

    /// Add `sprite`, drawn over the meshes; the lane sorts the sprites back
    /// to front.
    pub fn push_sprite(&mut self, sprite: SceneSprite) {
        self.sprites.push(sprite);
    }

    /// The meshes with instances, each with its material and instances: the
    /// lit ones, then the unlit.
    pub fn batches(&self) -> impl Iterator<Item = (SceneMaterial, MeshId, &[SceneInstance])> {
        let lit = self.lit.iter().map(|(m, b)| (SceneMaterial::Lit, m, b));
        let unlit = self.unlit.iter().map(|(m, b)| (SceneMaterial::Unlit, m, b));
        lit.chain(unlit)
    }

    /// The sprites, in the order they were pushed.
    pub fn sprites(&self) -> &[SceneSprite] {
        &self.sprites
    }

    /// How many instances and sprites the frame holds.
    pub fn instance_count(&self) -> usize {
        self.batches().map(|(_, _, b)| b.len()).sum::<usize>() + self.sprites.len()
    }

    /// The uniform block of the frame projected by `view_proj`, drawing
    /// `material`.
    pub(crate) fn uniforms(&self, view_proj: &Mat4, material: SceneMaterial) -> SceneUniforms {
        let l = &self.lighting;
        let d = normalize([l.direction.x, l.direction.y, l.direction.z]);
        let c = &self.camera.eye;
        SceneUniforms {
            view_proj: view_proj.v,
            camera: [c.x, c.y, c.z, material.code()],
            light_dir: [d[0], d[1], d[2], l.intensity],
            light_color: [l.color[0], l.color[1], l.color[2], 0.0],
            sky: [l.sky[0], l.sky[1], l.sky[2], 0.0],
            ground: [l.ground[0], l.ground[1], l.ground[2], 0.0],
            fog: [l.fog[0], l.fog[1], l.fog[2], l.fog_density],
        }
    }

    /// The background as the target stores it: encoded, premultiplied.
    pub(crate) fn clear_color(&self) -> [f32; 4] {
        encoded(self.background)
    }
}

impl Camera {
    /// How large one pixel of a view `pixels` tall is at `depth` along the
    /// view, in world units.
    pub(crate) fn pixel_at(&self, depth: f32, pixels: u32) -> f32 {
        let span = match self.projection {
            Projection::Perspective { fov_y } => 2.0 * depth.max(0.0) * (fov_y * 0.5).tan(),
            Projection::Orthographic { height } => height,
        };
        span / pixels.max(1) as f32
    }

    /// The camera's unit right, up and forward axes.
    pub(crate) fn basis(&self) -> (Vec3, Vec3, Vec3) {
        let unit = |v: Vec3, or: Vec3| if v.length() > 1e-6 { v.normalize() } else { or };
        let forward = unit(self.target - self.eye, vec3(0.0, 0.0, -1.0));
        let right = unit(forward.cross(self.up), vec3(1.0, 0.0, 0.0));
        (right, right.cross(forward), forward)
    }
}

/// `color` as the scene programs read and write it: encoded for the target,
/// premultiplied.
fn encoded(color: LinearStraight) -> [f32; 4] {
    let a = color.a.clamp(0.0, 1.0);
    [
        encode(color.r) * a,
        encode(color.g) * a,
        encode(color.b) * a,
        a,
    ]
}

/// An instance of a mesh at `position`, turned by `rotation` and scaled by
/// `scale` (for a built-in shape, its half extents), in `color`.
pub fn instance(
    position: Vec3,
    rotation: Quat,
    scale: Vec3,
    color: LinearStraight,
) -> SceneInstance {
    let r = rotation.to_mat4().v;
    let s = [scale.x, scale.y, scale.z];
    let p = [position.x, position.y, position.z];
    SceneInstance {
        rows: core::array::from_fn(|i| [r[i] * s[0], r[4 + i] * s[1], r[8 + i] * s[2], p[i]]),
        color: encoded(color),
    }
}

/// An instance of a mesh placed by the affine `linear` then `translation`,
/// in `color`: `linear[r]` is row `r` of its 3×3 part.
pub fn affine(
    linear: &[[f32; 3]; 3],
    translation: [f32; 3],
    color: LinearStraight,
) -> SceneInstance {
    SceneInstance {
        rows: core::array::from_fn(|r| [linear[r][0], linear[r][1], linear[r][2], translation[r]]),
        color: encoded(color),
    }
}

/// `view_proj` moved so the view fills the top-left `used` of a target
/// `allocated` pixels large: clip `[-1, 1]` maps onto those pixels only.
pub(crate) fn fit_view(view_proj: &Mat4, used: [u32; 2], allocated: [u32; 2]) -> Mat4 {
    let sx = used[0] as f32 / allocated[0] as f32;
    let sy = used[1] as f32 / allocated[1] as f32;
    let fit = Mat4 {
        v: [
            sx,
            0.0,
            0.0,
            0.0,
            0.0,
            sy,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
            0.0,
            sx - 1.0,
            1.0 - sy,
            0.0,
            1.0,
        ],
    };
    fit.mul(view_proj)
}

/// The six planes of a clip volume, each `[a, b, c, d]` with the inside
/// where `a x + b y + c z + d >= 0`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Frustum {
    planes: [[f32; 4]; 6],
}

impl Frustum {
    /// The planes bounding `view_proj`'s clip volume, `-w <= x, y <= w` and
    /// `0 <= z <= w`.
    pub(crate) fn of(view_proj: &Mat4) -> Frustum {
        let m = &view_proj.v;
        let row = |i: usize| [m[i], m[4 + i], m[8 + i], m[12 + i]];
        let (x, y, z, w) = (row(0), row(1), row(2), row(3));
        let add = |a: [f32; 4], b: [f32; 4]| core::array::from_fn(|i| a[i] + b[i]);
        let sub = |a: [f32; 4], b: [f32; 4]| core::array::from_fn(|i| a[i] - b[i]);
        Frustum {
            planes: [add(w, x), sub(w, x), add(w, y), sub(w, y), z, sub(w, z)],
        }
    }

    /// Whether the box centred at `centre` with half extents `half` reaches
    /// inside.
    #[inline]
    pub(crate) fn sees(&self, centre: [f32; 3], half: [f32; 3]) -> bool {
        self.planes.iter().all(|p| {
            let reach = p[0].abs() * half[0] + p[1].abs() * half[1] + p[2].abs() * half[2];
            p[0] * centre[0] + p[1] * centre[1] + p[2] * centre[2] + p[3] + reach >= 0.0
        })
    }
}

/// A mesh's bounding box in its own space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Bounds {
    pub centre: [f32; 3],
    pub half: [f32; 3],
}

impl Bounds {
    pub(crate) const UNIT: Bounds = Bounds {
        centre: [0.0; 3],
        half: [1.0; 3],
    };

    pub(crate) fn of(vertices: &[SceneVertex]) -> Bounds {
        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        for v in vertices {
            for i in 0..3 {
                lo[i] = lo[i].min(v.position[i]);
                hi[i] = hi[i].max(v.position[i]);
            }
        }
        if vertices.is_empty() {
            return Bounds {
                centre: [0.0; 3],
                half: [0.0; 3],
            };
        }
        Bounds {
            centre: core::array::from_fn(|i| (lo[i] + hi[i]) * 0.5),
            half: core::array::from_fn(|i| (hi[i] - lo[i]) * 0.5),
        }
    }

    /// The world box of these bounds placed by `instance`.
    #[inline]
    pub(crate) fn placed(&self, instance: &SceneInstance) -> ([f32; 3], [f32; 3]) {
        let r = &instance.rows;
        let centre = instance.place(self.centre);
        let half = core::array::from_fn(|i| {
            r[i][0].abs() * self.half[0]
                + r[i][1].abs() * self.half[1]
                + r[i][2].abs() * self.half[2]
        });
        (centre, half)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam(eye: Vec3) -> Camera {
        Camera {
            eye,
            target: Vec3::ZERO,
            ..Camera::default()
        }
    }

    #[test]
    fn the_frustum_keeps_what_the_camera_sees() {
        let f = Frustum::of(&cam(vec3(0.0, 0.0, 10.0)).view_proj(1.0));
        assert!(f.sees([0.0; 3], [1.0; 3]), "in front");
        assert!(!f.sees([0.0, 0.0, 20.0], [1.0; 3]), "behind the camera");
        assert!(!f.sees([40.0, 0.0, 0.0], [1.0; 3]), "far to the side");
        assert!(
            f.sees([0.0, 0.0, -1.0e5], [1.0; 3]),
            "an infinite far plane"
        );
        // A box straddling the side plane is kept.
        let edge = 10.0 * (25f32.to_radians()).tan();
        assert!(f.sees([edge + 0.5, 0.0, 0.0], [1.0; 3]));
    }

    #[test]
    fn a_placed_box_bounds_its_turned_extent() {
        let i = instance(
            vec3(1.0, 2.0, 3.0),
            Quat::from_axis_angle(vec3(0.0, 1.0, 0.0), core::f32::consts::FRAC_PI_4),
            vec3(2.0, 1.0, 2.0),
            LinearStraight::new(1.0, 1.0, 1.0, 1.0),
        );
        let (centre, half) = Bounds::UNIT.placed(&i);
        assert_eq!(centre, [1.0, 2.0, 3.0]);
        let diagonal = 2.0 * 2f32.sqrt();
        assert!((half[0] - diagonal).abs() < 1e-5 && (half[1] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn a_fitted_view_maps_onto_the_used_pixels() {
        let vp = Mat4::IDENTITY;
        let m = fit_view(&vp, [100, 50], [128, 64]);
        let px = |x: f32, y: f32| {
            let c = m.transform_vec4(viso_math::vec4(x, y, 0.5, 1.0));
            [(c.x + 1.0) * 0.5 * 128.0, (1.0 - c.y) * 0.5 * 64.0]
        };
        assert_eq!(px(-1.0, 1.0), [0.0, 0.0]);
        let corner = px(1.0, -1.0);
        assert!((corner[0] - 100.0).abs() < 1e-4 && (corner[1] - 50.0).abs() < 1e-4);
    }

    #[test]
    fn refilling_a_frame_reuses_its_batches() {
        let mut frame = SceneFrame::new();
        let one = instance(
            Vec3::ZERO,
            Quat::IDENTITY,
            vec3(1.0, 1.0, 1.0),
            LinearStraight::new(1.0, 0.0, 0.0, 1.0),
        );
        frame.push(Shape::Sphere, one);
        frame.push(Shape::Cube, one);
        frame.push(Shape::Sphere, one);
        let order: Vec<_> = frame.batches().map(|(_, m, b)| (m, b.len())).collect();
        assert_eq!(
            order,
            [
                (MeshId::from(Shape::Sphere), 2),
                (MeshId::from(Shape::Cube), 1)
            ]
        );
        frame.clear_instances();
        assert_eq!(frame.instance_count(), 0);
        frame.push(Shape::Cube, one);
        assert_eq!(frame.batches().count(), 1);
    }
}
