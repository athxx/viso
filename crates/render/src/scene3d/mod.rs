//! The 3D scene lane: lit, depth-tested meshes drawn into a scene's own target,
//! which the 2D scene composites like an image.
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
//! Meshes are the built-in [`Shape`]s, generated once on first use, ones
//! uploaded with [`Renderer::create_scene_mesh`], or a glTF
//! [`SceneModel`]'s. Each instance is drawn in a [`Material`]: the built-in
//! [`MaterialId::LIT`] and [`MaterialId::UNLIT`], one made with
//! [`Renderer::create_scene_material`], or a model's own. Lit surfaces are
//! shaded metallic-roughness by the frame's light and its [`Environment`],
//! then tone mapped.
//!
//! [`Renderer::swap_scene`]: crate::Renderer::swap_scene
//! [`Renderer::create_scene_mesh`]: crate::Renderer::create_scene_mesh
//! [`Renderer::create_scene_material`]: crate::Renderer::create_scene_material
//! [`Primitive::Scene`]: crate::Primitive::Scene

mod environment;
mod lane;
pub mod model;
mod occlusion;
mod resident;
mod shadow;
mod shapes;
mod sky;

use std::sync::Arc;

pub use environment::Environment;
pub(crate) use lane::SceneLane;
pub use lane::SceneStats;
pub use model::AlphaMode;
pub use occlusion::AmbientOcclusion;
pub use resident::SceneModel;
pub use shadow::Shadows;
pub use shapes::Shape;
pub use sky::Sky;
pub use viso_gpu::scene::{
    FrameUniforms, MAX_JOINTS, MaterialUniforms, SceneInstance, SceneVertex, SkinnedVertex,
};

pub use viso_math::{LinearStraight, Mat4, Quat, Vec3, vec3};

use viso_gpu::scene::normalize;

/// A material a scene draws instances in: one of the built-ins, one made
/// with [`Renderer::create_scene_material`](crate::Renderer::create_scene_material),
/// or a [`SceneModel`]'s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MaterialId(pub(crate) u32);

impl MaterialId {
    /// A white, rough dielectric lit by the scene's light: the material of
    /// [`SceneFrame::push`], its color the instance's.
    pub const LIT: MaterialId = MaterialId(0);
    /// The instance's color as is, fogged: debug geometry and glowing
    /// surfaces, [`SceneFrame::push_unlit`]'s.
    pub const UNLIT: MaterialId = MaterialId(1);
    /// A sprite's soft disc, blended.
    pub(crate) const SPRITE: MaterialId = MaterialId(2);
    /// How many ids the built-ins take.
    pub(crate) const BUILT_IN: u32 = 3;

    /// The id's dense index.
    #[inline]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// A metallic-roughness material, its colors linear: what
/// [`Renderer::create_scene_material`](crate::Renderer::create_scene_material)
/// makes. An instance's color multiplies its base color.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Material {
    /// Straight alpha; how it covers what is behind it is `alpha`'s.
    pub base_color: LinearStraight,
    /// 0 for a dielectric, 1 for a metal.
    pub metallic: f32,
    /// 0 mirror-smooth, 1 fully rough.
    pub roughness: f32,
    /// The light it emits, linear RGB.
    pub emissive: [f32; 3],
    pub alpha: AlphaMode,
    /// Its back faces are drawn too, lit by the flipped normal.
    pub double_sided: bool,
    /// Its base color as is, unlit.
    pub unlit: bool,
}

impl Default for Material {
    /// [`MaterialId::LIT`]'s: white, dielectric, half rough.
    fn default() -> Material {
        Material {
            base_color: LinearStraight::new(1.0, 1.0, 1.0, 1.0),
            metallic: 0.0,
            roughness: 0.5,
            emissive: [0.0; 3],
            alpha: AlphaMode::Opaque,
            double_sided: false,
            unlit: false,
        }
    }
}

/// A mesh the scene lane draws: a built-in [`Shape`] or an uploaded mesh.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
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

/// How a scene is lit: one directional light, the environment around it,
/// and exponential fog. Colors are linear RGB.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lighting {
    /// The direction toward the light.
    pub direction: Vec3,
    pub color: [f32; 3],
    /// A white diffuse surface facing a light of intensity 1 reflects its
    /// color.
    pub intensity: f32,
    /// The environment's light from above and from below, when the frame
    /// names no [`Environment`] of its own: a sky that brightens from their
    /// mean at the horizon.
    pub sky: [f32; 3],
    pub ground: [f32; 3],
    /// How strongly the environment lights diffusely, and how strongly it is
    /// reflected.
    pub ambient: f32,
    pub reflections: f32,
    /// What lit colors are scaled by before they are tone mapped into the
    /// target's range.
    pub exposure: f32,
    pub fog: [f32; 3],
    /// The fog's density per unit of distance; `0.0` for none.
    pub fog_density: f32,
    /// How far from the eye the fog begins: nearer, the air is clear.
    pub fog_start: f32,
    /// The shadows the light casts.
    pub shadows: Shadows,
    /// How the scene's surfaces occlude the environment's light from each
    /// other.
    pub occlusion: AmbientOcclusion,
}

impl Default for Lighting {
    fn default() -> Lighting {
        Lighting {
            direction: vec3(0.4, 0.85, 0.35),
            color: [1.0, 0.96, 0.9],
            intensity: 1.0,
            sky: [0.32, 0.36, 0.45],
            ground: [0.1, 0.09, 0.08],
            ambient: 1.0,
            reflections: 1.0,
            exposure: 1.0,
            fog: [0.62, 0.7, 0.8],
            fog_density: 0.0,
            fog_start: 0.0,
            shadows: Shadows::default(),
            occlusion: AmbientOcclusion::default(),
        }
    }
}

impl Lighting {
    /// The environment of a frame that names none: [`Environment::gradient`]
    /// of its sky and ground.
    pub fn sky_environment(&self) -> Environment {
        let horizon = core::array::from_fn(|i| (self.sky[i] + self.ground[i]) * 0.5);
        Environment::gradient(self.sky, horizon, self.ground)
    }
}

/// One frame of a scene: what [`Renderer::swap_scene`] hands the renderer.
///
/// Its instances are grouped by material and mesh, one batch each, so the
/// lane draws each in one instanced draw: the opaque batches first, then
/// the blended materials' instances and the sprites, sorted back to front
/// over them. Filling reuses the batches' storage, so a steady scene
/// refills without allocating.
///
/// [`Renderer::swap_scene`]: crate::Renderer::swap_scene
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SceneFrame {
    pub camera: Camera,
    pub lighting: Lighting,
    /// The environment lighting the scene; `None` for its sky's
    /// ([`Environment::from_sky`]), or without one its lighting's
    /// [`sky_environment`](Lighting::sky_environment).
    pub environment: Option<Arc<Environment>>,
    /// The sky drawn where no opaque mesh is, behind the blended ones and
    /// the sprites, exposed and tone mapped as the meshes are; `None` shows
    /// the background.
    pub sky: Option<Sky>,
    /// What shows where nothing is drawn and there is no sky, straight
    /// linear RGBA. A transparent background lets the 2D content behind the
    /// scene show through.
    pub background: LinearStraight,
    batches: Batches,
    sprites: Vec<SceneSprite>,
    skinned: Vec<SkinnedDraw>,
    /// The skinned draws' palettes: three rows of each joint's affine.
    joints: Vec<[f32; 4]>,
}

/// One instance of a skinned mesh, posed by its own palette.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SkinnedDraw {
    pub mesh: MeshId,
    pub material: MaterialId,
    pub instance: SceneInstance,
    /// Its palette's rows in [`SceneFrame::joints`].
    pub joints: core::ops::Range<u32>,
    /// The posed mesh's box in its own space.
    pub bounds: Bounds,
}

/// The instances of one mesh in one material.
#[derive(Debug, Clone, Default, PartialEq)]
struct Batch {
    material: MaterialId,
    mesh: MeshId,
    instances: Vec<SceneInstance>,
}

impl Default for MaterialId {
    fn default() -> MaterialId {
        MaterialId::LIT
    }
}

/// A frame's batches, in the order they were first pushed; the storage of
/// the ones a clear let go of is kept for the next fill.
#[derive(Debug, Clone, Default, PartialEq)]
struct Batches {
    /// Each mesh's live batches, by [`MeshId::index`]: indices into
    /// `batches`.
    by_mesh: Vec<Vec<u32>>,
    batches: Vec<Batch>,
    /// How many of `batches` are live.
    live: usize,
}

impl Batches {
    fn clear(&mut self) {
        for b in &mut self.batches[..self.live] {
            self.by_mesh[b.mesh.index()].clear();
            b.instances.clear();
        }
        self.live = 0;
    }

    fn push(&mut self, mesh: MeshId, material: MaterialId, instance: SceneInstance) {
        if self.by_mesh.len() <= mesh.index() {
            self.by_mesh.resize_with(mesh.index() + 1, Vec::new);
        }
        let found = self.by_mesh[mesh.index()]
            .iter()
            .copied()
            .find(|&b| self.batches[b as usize].material == material);
        let b = match found {
            Some(b) => b as usize,
            None => {
                if self.live == self.batches.len() {
                    self.batches.push(Batch::default());
                }
                let b = self.live;
                self.live += 1;
                let batch = &mut self.batches[b];
                batch.material = material;
                batch.mesh = mesh;
                self.by_mesh[mesh.index()].push(b as u32);
                b
            }
        };
        self.batches[b].instances.push(instance);
    }

    fn iter(&self) -> impl Iterator<Item = (MaterialId, MeshId, &[SceneInstance])> {
        self.batches[..self.live]
            .iter()
            .map(|b| (b.material, b.mesh, b.instances.as_slice()))
    }
}

/// A particle: a soft disc of `radius` at `position`, always facing the
/// camera, in a color the frame premultiplies as it is pushed.
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
            color: premultiplied(color),
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
        self.batches.clear();
        self.sprites.clear();
        self.skinned.clear();
        self.joints.clear();
    }

    /// Add `instance` of `mesh` in [`MaterialId::LIT`], lit by the scene's
    /// light.
    pub fn push(&mut self, mesh: impl Into<MeshId>, instance: SceneInstance) {
        self.batches.push(mesh.into(), MaterialId::LIT, instance);
    }

    /// Add `instance` of `mesh` in [`MaterialId::UNLIT`], its color as is:
    /// debug geometry, or a surface that glows.
    pub fn push_unlit(&mut self, mesh: impl Into<MeshId>, instance: SceneInstance) {
        self.batches.push(mesh.into(), MaterialId::UNLIT, instance);
    }

    /// Add `instance` of `mesh` in `material`.
    pub fn push_with(
        &mut self,
        mesh: impl Into<MeshId>,
        material: MaterialId,
        instance: SceneInstance,
    ) {
        self.batches.push(mesh.into(), material, instance);
    }

    /// Add `instance` of skinned mesh `mesh` (one
    /// [`Renderer::create_scene_skinned_mesh`] made) in `material`, each of
    /// its vertices moved by `joints` — every joint's affine from the mesh's
    /// bind pose to its pose, in the mesh's space; the first
    /// [`MAX_JOINTS`] — which keep it inside the box `bounds` (its least and
    /// greatest corners, in the mesh's space). Each skinned instance is a
    /// draw of its own.
    ///
    /// [`Renderer::create_scene_skinned_mesh`]: crate::Renderer::create_scene_skinned_mesh
    pub fn push_skinned(
        &mut self,
        mesh: MeshId,
        material: MaterialId,
        instance: SceneInstance,
        joints: &[Mat4],
        bounds: ([f32; 3], [f32; 3]),
    ) {
        self.push_posed_joints(mesh, material, instance, joints.iter().copied(), bounds);
    }

    /// [`push_skinned`](Self::push_skinned) of the joints `joints` yields.
    pub(crate) fn push_posed_joints(
        &mut self,
        mesh: MeshId,
        material: MaterialId,
        instance: SceneInstance,
        joints: impl Iterator<Item = Mat4>,
        bounds: ([f32; 3], [f32; 3]),
    ) {
        let start = self.joints.len() as u32;
        for m in joints.take(MAX_JOINTS) {
            let v = &m.v;
            self.joints
                .extend((0..3).map(|r| [v[r], v[4 + r], v[8 + r], v[12 + r]]));
        }
        let (lo, hi) = bounds;
        self.skinned.push(SkinnedDraw {
            mesh,
            material,
            instance,
            joints: start..self.joints.len() as u32,
            bounds: Bounds {
                centre: core::array::from_fn(|i| (lo[i] + hi[i]) * 0.5),
                half: core::array::from_fn(|i| ((hi[i] - lo[i]) * 0.5).max(0.0)),
            },
        });
    }

    /// The skinned draws, each with its palette's rows.
    pub(crate) fn skinned(&self) -> impl Iterator<Item = (&SkinnedDraw, &[[f32; 4]])> {
        self.skinned.iter().map(|d| {
            let rows = &self.joints[d.joints.start as usize..d.joints.end as usize];
            (d, rows)
        })
    }

    /// Add `sprite`, drawn over the meshes; the lane sorts the sprites back
    /// to front.
    pub fn push_sprite(&mut self, sprite: SceneSprite) {
        self.sprites.push(sprite);
    }

    /// Each material and mesh with instances, with its instances, in the
    /// order they were first pushed.
    pub fn batches(&self) -> impl Iterator<Item = (MaterialId, MeshId, &[SceneInstance])> {
        self.batches.iter()
    }

    /// The sprites, in the order they were pushed.
    pub fn sprites(&self) -> &[SceneSprite] {
        &self.sprites
    }

    /// How many instances and sprites the frame holds.
    pub fn instance_count(&self) -> usize {
        self.batches().map(|(_, _, b)| b.len()).sum::<usize>()
            + self.sprites.len()
            + self.skinned.len()
    }

    /// The frame's uniform block, projected by `view_proj`, lit by an
    /// environment of irradiance `sh`.
    pub(crate) fn uniforms(&self, view_proj: &Mat4, sh: &[[f32; 4]; 9]) -> FrameUniforms {
        let l = &self.lighting;
        let d = normalize([l.direction.x, l.direction.y, l.direction.z]);
        let c = &self.camera.eye;
        FrameUniforms {
            view_proj: view_proj.v,
            camera: [c.x, c.y, c.z, l.exposure],
            light_dir: [d[0], d[1], d[2], l.intensity],
            light_color: [l.color[0], l.color[1], l.color[2], 0.0],
            fog: [l.fog[0], l.fog[1], l.fog[2], l.fog_density],
            ambient: [l.ambient, l.reflections, l.fog_start, 0.0],
            sh: *sh,
            ..FrameUniforms::default()
        }
    }

    /// The background the target clears to: premultiplied linear light.
    pub(crate) fn clear_color(&self) -> [f32; 4] {
        premultiplied(self.background)
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

/// `color` as the scene programs read and write it: premultiplied linear
/// light.
fn premultiplied(color: LinearStraight) -> [f32; 4] {
    let a = color.a.clamp(0.0, 1.0);
    [color.r * a, color.g * a, color.b * a, a]
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
        color: premultiplied(color),
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
        color: premultiplied(color),
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

    /// The box around `positions`; an empty one at the origin for none.
    pub(crate) fn around(positions: impl Iterator<Item = [f32; 3]>) -> Bounds {
        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        let mut any = false;
        for p in positions {
            any = true;
            for i in 0..3 {
                lo[i] = lo[i].min(p[i]);
                hi[i] = hi[i].max(p[i]);
            }
        }
        if !any {
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
        frame.push_unlit(Shape::Sphere, one);
        let order: Vec<_> = frame.batches().map(|(t, m, b)| (t, m, b.len())).collect();
        assert_eq!(
            order,
            [
                (MaterialId::LIT, MeshId::from(Shape::Sphere), 2),
                (MaterialId::LIT, MeshId::from(Shape::Cube), 1),
                (MaterialId::UNLIT, MeshId::from(Shape::Sphere), 1),
            ]
        );
        frame.clear_instances();
        assert_eq!(frame.instance_count(), 0);
        frame.push(Shape::Cube, one);
        assert_eq!(frame.batches().count(), 1);
        assert_eq!(frame.batches.batches.len(), 3, "the storage is kept");
    }
}
