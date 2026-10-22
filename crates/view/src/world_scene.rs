//! A game's World as a 3D scene: what a `GameViewport` draws.
//!
//! Each frame, after the session's ticks and `FrameUpdate`s, every live
//! entity is drawn at the clock's interpolation as its Kit model, turned to
//! its facing and sized by its half extents, seen through the stage's camera
//! or the viewport's own. A model is one built-in shape, or a composite of
//! shapes laid out at the size of the prefab it belongs to and stretched with
//! the body. The stage's particles are drawn as soft sprites over the World,
//! and its debug shapes as thin unlit boxes. The fill compares what it reads
//! with what it read last, so an unchanged World under an unchanged camera
//! produces no new frame.

use viso_behavior::game::kit::{
    Camera as RigCamera, CameraRig, CameraView, DebugShape, Model, Sprite,
};
use viso_behavior::game::{Extracted, Scheduler};
use viso_ui::scene3d::{
    Camera, Lighting, LinearStraight, Projection, SceneFrame, SceneInstance, SceneSprite, Shape,
    Vec3, affine, vec3,
};
use viso_ui::scenes::SceneSlot;
use viso_ui::{NodeStore, SceneTarget};

/// One shape of a composite model, in metres about the model's centre at
/// its reference size.
#[derive(Debug, Clone, Copy)]
struct Part {
    shape: Shape,
    centre: [f32; 3],
    half: [f32; 3],
    /// A quarter turn laying the shape's `y` axis along `x` or `z`.
    lay: Lay,
    color: [f32; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lay {
    Upright,
    /// The shape's `y` along `x`.
    AlongX,
    /// The shape's `y` along `z`.
    AlongZ,
    /// Upside down.
    Flipped,
}

const fn part(shape: Shape, centre: [f32; 3], half: [f32; 3], color: [f32; 3]) -> Part {
    Part {
        shape,
        centre,
        half,
        lay: Lay::Upright,
        color,
    }
}

const fn laid(shape: Shape, centre: [f32; 3], half: [f32; 3], lay: Lay, color: [f32; 3]) -> Part {
    Part {
        shape,
        centre,
        half,
        lay,
        color,
    }
}

const SKIN: [f32; 3] = [0.93, 0.74, 0.6];
const TYRE: [f32; 3] = [0.08, 0.08, 0.09];
const WOOD: [f32; 3] = [0.42, 0.26, 0.13];

const fn wheel(x: f32, y: f32, z: f32) -> Part {
    laid(
        Shape::Cylinder,
        [x, y, z],
        [0.2, 0.09, 0.2],
        Lay::AlongX,
        TYRE,
    )
}

use Shape::{Capsule, Cone, Cube, Cylinder, Sphere};

const BOLT: [f32; 3] = [0.22, 0.22, 0.24];
const HORN: [f32; 3] = [0.95, 0.93, 0.85];
const EYE: [f32; 3] = [1.0, 0.9, 0.2];
const STEEL: [f32; 3] = [0.35, 0.37, 0.4];

const TREE: &[Part] = &[
    part(Cylinder, [0.0, -1.3, 0.0], [0.13, 0.7, 0.13], WOOD),
    part(Cone, [0.0, 0.4, 0.0], [0.9, 1.6, 0.9], [0.16, 0.48, 0.2]),
];
const ROCK: &[Part] = &[
    part(
        Sphere,
        [0.0, -0.05, 0.0],
        [0.7, 0.45, 0.6],
        [0.48, 0.48, 0.5],
    ),
    part(
        Sphere,
        [0.42, -0.25, 0.35],
        [0.26, 0.22, 0.24],
        [0.42, 0.42, 0.44],
    ),
];
const BARREL: &[Part] = &[
    part(
        Cylinder,
        [0.0, 0.0, 0.0],
        [0.4, 0.5, 0.4],
        [0.55, 0.34, 0.16],
    ),
    part(Cylinder, [0.0, 0.3, 0.0], [0.41, 0.04, 0.41], BOLT),
    part(Cylinder, [0.0, -0.3, 0.0], [0.41, 0.04, 0.41], BOLT),
];
const COIN: &[Part] = &[laid(
    Cylinder,
    [0.0; 3],
    [0.3, 0.05, 0.3],
    Lay::AlongZ,
    [1.0, 0.78, 0.18],
)];
const GEM: &[Part] = &[
    part(Cone, [0.0, 0.1, 0.0], [0.25, 0.15, 0.25], [0.3, 0.85, 0.95]),
    laid(
        Cone,
        [0.0, -0.15, 0.0],
        [0.25, 0.1, 0.25],
        Lay::Flipped,
        [0.2, 0.7, 0.9],
    ),
];
const FLAG: &[Part] = &[
    part(
        Cylinder,
        [0.0, -1.45, 0.0],
        [0.3, 0.05, 0.3],
        [0.5, 0.5, 0.52],
    ),
    part(
        Cylinder,
        [0.0, 0.0, 0.0],
        [0.05, 1.5, 0.05],
        [0.9, 0.9, 0.88],
    ),
    part(Cube, [0.42, 1.1, 0.0], [0.4, 0.26, 0.02], [0.9, 0.15, 0.15]),
];
const CAR: &[Part] = &[
    part(
        Cube,
        [0.0, -0.12, 0.0],
        [0.55, 0.26, 1.0],
        [0.85, 0.16, 0.14],
    ),
    part(
        Cube,
        [0.0, 0.34, 0.1],
        [0.48, 0.22, 0.52],
        [0.75, 0.82, 0.9],
    ),
    wheel(0.56, -0.4, -0.62),
    wheel(-0.56, -0.4, -0.62),
    wheel(0.56, -0.4, 0.62),
    wheel(-0.56, -0.4, 0.62),
];
const TRUCK: &[Part] = &[
    part(
        Cube,
        [0.0, -0.12, -0.8],
        [0.62, 0.5, 0.45],
        [0.18, 0.36, 0.78],
    ),
    part(
        Cube,
        [0.0, 0.05, 0.38],
        [0.66, 0.66, 0.9],
        [0.82, 0.82, 0.8],
    ),
    wheel(0.64, -0.8, -0.8),
    wheel(-0.64, -0.8, -0.8),
    wheel(0.64, -0.8, 0.7),
    wheel(-0.64, -0.8, 0.7),
];
const HERO: &[Part] = &[
    part(
        Capsule,
        [0.0, -0.25, 0.0],
        [0.3, 0.65, 0.3],
        [0.2, 0.45, 0.9],
    ),
    part(Sphere, [0.0, 0.62, 0.0], [0.26, 0.26, 0.26], SKIN),
];
const VILLAGER: &[Part] = &[
    part(
        Capsule,
        [0.0, -0.25, 0.0],
        [0.3, 0.6, 0.3],
        [0.55, 0.45, 0.25],
    ),
    part(Sphere, [0.0, 0.55, 0.0], [0.24, 0.24, 0.24], SKIN),
    part(Cone, [0.0, 0.82, 0.0], [0.3, 0.12, 0.3], [0.75, 0.6, 0.3]),
];
const MONSTER: &[Part] = &[
    part(Sphere, [0.0, -0.1, 0.0], [0.6, 0.7, 0.6], [0.55, 0.2, 0.65]),
    part(Cone, [0.3, 0.62, 0.0], [0.08, 0.2, 0.08], HORN),
    part(Cone, [-0.3, 0.62, 0.0], [0.08, 0.2, 0.08], HORN),
    part(Sphere, [0.2, 0.22, -0.52], [0.1, 0.1, 0.1], EYE),
    part(Sphere, [-0.2, 0.22, -0.52], [0.1, 0.1, 0.1], EYE),
];
const ROBOT: &[Part] = &[
    part(
        Cube,
        [0.0, -0.15, 0.0],
        [0.35, 0.42, 0.25],
        [0.6, 0.65, 0.7],
    ),
    part(
        Cube,
        [0.0, 0.55, 0.0],
        [0.25, 0.22, 0.22],
        [0.7, 0.74, 0.78],
    ),
    part(
        Cube,
        [0.0, 0.58, -0.22],
        [0.15, 0.04, 0.02],
        [1.0, 0.2, 0.15],
    ),
    part(Cylinder, [0.15, -0.75, 0.0], [0.08, 0.15, 0.08], STEEL),
    part(Cylinder, [-0.15, -0.75, 0.0], [0.08, 0.15, 0.08], STEEL),
];

/// A model drawn as a composite: its reference half extents (its prefab's)
/// and its parts. The models face `-z`.
fn composite(model: Model) -> Option<([f32; 3], &'static [Part])> {
    Some(match model {
        Model::Tree => ([0.3, 2.0, 0.3], TREE),
        Model::Rock => ([0.7, 0.5, 0.7], ROCK),
        Model::Barrel => ([0.4, 0.5, 0.4], BARREL),
        Model::Coin => ([0.3, 0.3, 0.3], COIN),
        Model::Gem => ([0.25, 0.25, 0.25], GEM),
        Model::Flag => ([1.0, 1.5, 1.0], FLAG),
        Model::Car => ([1.0, 0.6, 1.0], CAR),
        Model::Truck => ([1.3, 1.0, 1.3], TRUCK),
        Model::Hero => ([0.4, 0.9, 0.4], HERO),
        Model::Villager => ([0.4, 0.85, 0.4], VILLAGER),
        Model::Monster => ([0.6, 0.8, 0.6], MONSTER),
        Model::Robot => ([0.4, 0.9, 0.4], ROBOT),
        _ => return None,
    })
}

/// The shape and color of a model drawn as one shape stretched to the body.
fn single(model: Model) -> (Shape, [f32; 3]) {
    match model {
        Model::Capsule => (Shape::Capsule, [0.9, 0.58, 0.3]),
        Model::Sphere => (Shape::Sphere, [0.85, 0.35, 0.3]),
        Model::Cylinder => (Shape::Cylinder, [0.4, 0.6, 0.85]),
        Model::Cone => (Shape::Cone, [0.95, 0.75, 0.3]),
        Model::Wedge => (Shape::Wedge, [0.7, 0.55, 0.4]),
        Model::Ground => (Shape::Cube, [0.36, 0.55, 0.32]),
        _ => (Shape::Cube, [0.74, 0.73, 0.7]),
    }
}

/// The linear part of a body's turn: the model's `-z` along its facing.
fn turn(facing: [f32; 2]) -> [[f32; 3]; 3] {
    let [fx, fz] = facing;
    if fx == 0.0 && fz == 0.0 {
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    }
    // Columns: where the model's x, y and z go.
    let (x, y, z) = ([-fz, 0.0, fx], [0.0, 1.0, 0.0], [-fx, 0.0, -fz]);
    [[x[0], y[0], z[0]], [x[1], y[1], z[1]], [x[2], y[2], z[2]]]
}

/// A part's own linear part: its half extents after its lay.
fn lay(lay: Lay, half: [f32; 3]) -> [[f32; 3]; 3] {
    let [x, y, z] = half;
    match lay {
        Lay::Upright => [[x, 0.0, 0.0], [0.0, y, 0.0], [0.0, 0.0, z]],
        // Columns: the shape's x, y and z axes, scaled.
        Lay::AlongX => [[0.0, y, 0.0], [-x, 0.0, 0.0], [0.0, 0.0, z]],
        Lay::AlongZ => [[x, 0.0, 0.0], [0.0, 0.0, -z], [0.0, y, 0.0]],
        Lay::Flipped => [[x, 0.0, 0.0], [0.0, -y, 0.0], [0.0, 0.0, -z]],
    }
}

fn mul(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    core::array::from_fn(|i| core::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

fn instance(linear: &[[f32; 3]; 3], at: [f32; 3], color: [f32; 3]) -> SceneInstance {
    affine(
        linear,
        at,
        LinearStraight::new(color[0], color[1], color[2], 1.0),
    )
}

/// Pushes `e` into `frame` as its model.
fn push(frame: &mut SceneFrame, e: &Extracted) {
    let model = e.model.resolve(e.kind);
    let turn = turn(e.facing);
    let at = e.position.to_array();
    let half = e.half_extents.to_array();
    match composite(model) {
        Some((reference, parts)) => {
            let stretch: [f32; 3] = core::array::from_fn(|i| {
                if reference[i] > 0.0 {
                    half[i] / reference[i]
                } else {
                    1.0
                }
            });
            let body = mul(
                &turn,
                &[
                    [stretch[0], 0.0, 0.0],
                    [0.0, stretch[1], 0.0],
                    [0.0, 0.0, stretch[2]],
                ],
            );
            for p in parts {
                let linear = mul(&body, &lay(p.lay, p.half));
                let centre: [f32; 3] = core::array::from_fn(|r| {
                    at[r] + (0..3).map(|k| body[r][k] * p.centre[k]).sum::<f32>()
                });
                frame.push(p.shape, instance(&linear, centre, p.color));
            }
        }
        None => {
            let (shape, color) = single(model);
            let linear = mul(&turn, &lay(Lay::Upright, half));
            frame.push(shape, instance(&linear, at, color));
        }
    }
}

/// The color of debug shapes.
const DEBUG: [f32; 3] = [0.25, 1.0, 0.35];
/// How thick a debug line is per metre of distance from the eye: about two
/// pixels at a common field of view and window size.
const DEBUG_THICKNESS: f32 = 0.003;

/// Pushes a debug line from `from` to `to` into `frame`, seen from `eye`.
fn push_line(frame: &mut SceneFrame, eye: Vec3, from: Vec3, to: Vec3) {
    let along = (to - from) * 0.5;
    let centre = from + along;
    let t = (eye.distance(centre) * DEBUG_THICKNESS).max(1e-3);
    let unit = if along.length() > 1e-6 {
        along.normalize()
    } else {
        vec3(1.0, 0.0, 0.0)
    };
    // Any two axes across the line.
    let other = if unit.y.abs() < 0.9 {
        vec3(0.0, 1.0, 0.0)
    } else {
        vec3(1.0, 0.0, 0.0)
    };
    let side = unit.cross(other).normalize();
    let across = side.cross(unit);
    let a = if along.length() > 1e-6 {
        along
    } else {
        unit * t
    };
    let (b, c) = (side * t, across * t);
    let linear = [[a.x, b.x, c.x], [a.y, b.y, c.y], [a.z, b.z, c.z]];
    let color = LinearStraight::new(DEBUG[0], DEBUG[1], DEBUG[2], 1.0);
    frame.push_unlit(Shape::Cube, affine(&linear, centre.to_array(), color));
}

/// Pushes a debug shape into `frame`, seen from `eye`: a line, or a box's
/// twelve edges.
fn push_debug(frame: &mut SceneFrame, eye: Vec3, shape: &DebugShape) {
    match *shape {
        DebugShape::Line { from, to } => push_line(frame, eye, from, to),
        DebugShape::Box {
            center,
            half_extents,
        } => {
            let (c, h) = (center, half_extents);
            let corner = |i: u32| {
                let s = |bit: u32| if i & bit != 0 { 1.0 } else { -1.0 };
                c + vec3(h.x * s(1), h.y * s(2), h.z * s(4))
            };
            for i in 0..8u32 {
                for bit in [1, 2, 4] {
                    if i & bit == 0 {
                        push_line(frame, eye, corner(i), corner(i | bit));
                    }
                }
            }
        }
    }
}

/// The renderer's camera for a stage's view.
pub fn camera_of(view: &CameraView) -> Camera {
    Camera {
        eye: view.eye,
        target: view.target,
        up: view.up,
        projection: Projection::Perspective {
            fov_y: view.fov.clamp(1.0, 179.0).to_radians(),
        },
        near: view.near.max(1e-3),
        far: if view.far > view.near {
            view.far
        } else {
            f32::INFINITY
        },
    }
}

/// The look of a game's scene: a sky that the fog fades into.
fn lighting() -> (Lighting, LinearStraight) {
    let sky = [0.55, 0.68, 0.85];
    let lighting = Lighting {
        fog: sky,
        fog_density: 0.008,
        ..Lighting::default()
    };
    (lighting, LinearStraight::new(sky[0], sky[1], sky[2], 1.0))
}

/// What one viewport last drew of the World, so a fill that reads the same
/// draws nothing new.
#[derive(Debug, Default)]
pub struct WorldScene {
    read: Drawn,
    drawn: Drawn,
    camera: Option<Camera>,
}

/// What a fill reads: the World's bodies, the particles and the debug
/// shapes.
#[derive(Debug, Default, PartialEq)]
struct Drawn {
    bodies: Vec<Extracted>,
    sprites: Vec<Sprite>,
    shapes: Vec<DebugShape>,
}

impl WorldScene {
    /// Fills `frame` with the World `scheduler` runs, seen through `camera`
    /// or the stage's; `false`, leaving `frame` as it was, when neither the
    /// World as drawn nor the camera moved since the last fill.
    pub fn fill(
        &mut self,
        scheduler: &Scheduler,
        camera: Option<Camera>,
        frame: &mut SceneFrame,
    ) -> bool {
        let stage = scheduler.stage();
        let camera = camera.unwrap_or_else(|| camera_of(&stage.view()));
        let read = &mut self.read;
        scheduler
            .world()
            .extract(scheduler.clock().alpha(), &mut read.bodies);
        read.sprites.clear();
        read.sprites.extend(stage.sprites());
        read.shapes.clear();
        read.shapes.extend(stage.debug_shapes().copied());
        if self.camera == Some(camera) && self.read == self.drawn {
            return false;
        }
        core::mem::swap(&mut self.read, &mut self.drawn);
        self.camera = Some(camera);
        frame.clear_instances();
        frame.camera = camera;
        (frame.lighting, frame.background) = lighting();
        for e in &self.drawn.bodies {
            push(frame, e);
        }
        for s in &self.drawn.sprites {
            let [r, g, b, a] = s.color;
            let color = LinearStraight::new(r, g, b, a);
            frame.push_sprite(SceneSprite::new(s.position, s.size * 0.5, color));
        }
        for shape in &self.drawn.shapes {
            push_debug(frame, camera.eye, shape);
        }
        true
    }

    /// Forgets what it drew, so the next fill draws.
    pub fn invalidate(&mut self) {
        self.camera = None;
    }
}

/// What a viewport keeps beside its scene: its own rig, if it has one, the
/// camera easing along it, and what it last drew.
#[derive(Debug, Default)]
struct Viewport {
    rig: Option<CameraRig>,
    camera: RigCamera,
    scene: WorldScene,
}

/// Makes `node` a game viewport: it shows the World of the game its view
/// root runs, seen through `rig`, or the stage's camera when `None`, and a
/// press on it focuses it so the keys reach the game. What a `GameViewport`
/// node is, for a tree built in Rust.
pub fn game_viewport(store: &mut NodeStore, node: viso_ui::NodeId, rig: Option<CameraRig>) {
    if let Some(slot) = store.set_scene(node) {
        set_viewport_rig(store, slot, rig);
        store.set_focusable(node, true);
    }
}

/// Points the viewport showing `slot` at `rig`, or at the stage's camera
/// when `None`.
pub(crate) fn set_viewport_rig(store: &mut NodeStore, slot: SceneSlot, rig: Option<CameraRig>) {
    let scenes = store.scenes_mut();
    let Some(state) = scenes.state_mut(slot) else {
        return;
    };
    let viewport = state
        .get_or_insert_with(|| Box::new(Viewport::default()))
        .downcast_mut::<Viewport>();
    let Some(viewport) = viewport else {
        return;
    };
    if viewport.rig == rig {
        return;
    }
    viewport.rig = rig;
    if let Some(rig) = rig {
        viewport.camera.set(rig);
    }
    scenes.reset(slot);
}

/// Fills a viewport's scene from the World `scheduler` runs, `dt` seconds
/// after the last frame; whether the frame changed.
/// A scene that is not a viewport's is left to whoever fills it.
pub(crate) fn fill_viewport(target: SceneTarget<'_>, scheduler: &Scheduler, dt: f32) -> bool {
    let Some(viewport) = target
        .state
        .as_mut()
        .and_then(|state| state.downcast_mut::<Viewport>())
    else {
        return false;
    };
    if target.fresh {
        viewport.scene.invalidate();
    }
    let camera = viewport.rig.map(|_| {
        let alpha = scheduler.clock().alpha();
        viewport.camera.advance(dt, scheduler.world(), alpha);
        camera_of(&viewport.camera.view())
    });
    viewport.scene.fill(scheduler, camera, target.frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A body facing `+x` turns its model's `-z` there.
    #[test]
    fn a_turn_points_the_model_along_its_facing() {
        let t = turn([1.0, 0.0]);
        let forward: [f32; 3] = core::array::from_fn(|r| -t[r][2]);
        assert_eq!(forward, [1.0, 0.0, 0.0]);
        let det = t[0][0] * (t[1][1] * t[2][2] - t[1][2] * t[2][1])
            - t[0][1] * (t[1][0] * t[2][2] - t[1][2] * t[2][0])
            + t[0][2] * (t[1][0] * t[2][1] - t[1][1] * t[2][0]);
        assert!((det - 1.0).abs() < 1e-6, "a rotation, not a mirror");
    }

    /// Every lay is a rotation of the scaled shape, so faces keep winding.
    #[test]
    fn every_lay_keeps_the_winding() {
        for l in [Lay::Upright, Lay::AlongX, Lay::AlongZ, Lay::Flipped] {
            let m = lay(l, [1.0, 2.0, 3.0]);
            let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
                - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
                + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
            assert!((det - 6.0).abs() < 1e-5, "{l:?}: {det}");
        }
    }

    /// A debug box is its twelve edges, each a thin unlit box spanning the
    /// edge.
    #[test]
    fn a_debug_box_draws_its_edges_as_thin_unlit_boxes() {
        let mut frame = SceneFrame::new();
        let shape = DebugShape::Box {
            center: vec3(1.0, 2.0, 3.0),
            half_extents: vec3(1.0, 0.5, 2.0),
        };
        push_debug(&mut frame, vec3(0.0, 0.0, 10.0), &shape);
        let batches: Vec<_> = frame.batches().collect();
        assert_eq!(batches.len(), 1);
        let (material, mesh, edges) = batches[0];
        assert_eq!(
            (material, mesh),
            (viso_ui::scene3d::SceneMaterial::Unlit, Shape::Cube.into())
        );
        assert_eq!(edges.len(), 12);
        let mut lengths: Vec<f32> = edges
            .iter()
            .map(|e| {
                let r = &e.rows;
                let col =
                    |j: usize| (r[0][j] * r[0][j] + r[1][j] * r[1][j] + r[2][j] * r[2][j]).sqrt();
                let (a, b, c) = (col(0), col(1), col(2));
                assert!(b < 0.1 && c < 0.1, "thin: {b} {c}");
                a * 2.0
            })
            .collect();
        lengths.sort_by(f32::total_cmp);
        let want = [1.0, 1.0, 1.0, 1.0, 2.0, 2.0, 2.0, 2.0, 4.0, 4.0, 4.0, 4.0];
        for (got, want) in lengths.iter().zip(want) {
            assert!((got - want).abs() < 1e-5, "{lengths:?}");
        }
    }

    /// Every composite fits roughly in its reference box.
    #[test]
    fn composites_fit_their_reference_size() {
        let composites = [
            Model::Tree,
            Model::Rock,
            Model::Barrel,
            Model::Coin,
            Model::Gem,
            Model::Flag,
            Model::Car,
            Model::Truck,
            Model::Hero,
            Model::Villager,
            Model::Monster,
            Model::Robot,
        ];
        for model in composites {
            let (reference, parts) = composite(model).expect("a composite");
            for p in parts {
                for (i, (c, r)) in p.centre.iter().zip(reference).enumerate() {
                    assert!(c.abs() <= r * 1.6, "{model:?} {p:?} axis {i}");
                }
            }
        }
    }
}
