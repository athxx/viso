//! Camera rigs: how the frame's camera follows the game. A rig is a plain
//! value a game builds and hands to `kit.camera(rig)`; the [`Stage`]'s camera
//! eases toward what the rig asks for every frame and shakes when told to.
//!
//! - third person: behind the target's facing at `distance`, its pivot
//!   `height` above the target, turning to stay behind it;
//! - follow: at `distance` from the target along a fixed `yaw`, looking down
//!   at `pitch`, never turning;
//! - top down: `distance` above the target looking straight down, screen up
//!   along `yaw`;
//! - fixed: at an eye point looking at another.
//!
//! The camera is Presentation: it reads the world drawn at the frame's
//! interpolation and nothing in the Simulation reads it back, so it uses the
//! host's floating-point functions freely.
//!
//! [`Stage`]: super::Stage

use std::rc::Rc;

use crate::game::world::{EntityId, GameWorld};
use crate::native::{NativeValue, SchemaTy, Vec3F32};
use crate::value::{Aggregate, Value};

/// The camera's near plane, in metres.
const NEAR: f32 = 0.1;
/// Its far plane, in metres.
const FAR: f32 = 1000.0;
/// How close to vertical a pitch counts as straight down, in degrees.
const VERTICAL: f32 = 89.9;

/// What a rig follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RigKind {
    ThirdPerson,
    Follow,
    TopDown,
    Fixed,
}

/// A camera rig.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraRig {
    pub kind: RigKind,
    /// The entity it follows; unused by a fixed rig.
    pub target: EntityId,
    /// How far the eye sits from the pivot, in metres.
    pub distance: f32,
    /// How far the pivot sits above the target, in metres.
    pub height: f32,
    /// How far it looks down, in degrees: negative looks down.
    pub pitch: f32,
    /// The heading it looks along, in degrees clockwise from `-z`; a
    /// third-person rig takes its target's.
    pub yaw: f32,
    /// The vertical field of view, in degrees.
    pub fov: f32,
    /// How long it takes to catch up, in seconds: the time constant of its
    /// easing, 0 for none.
    pub lag: f32,
    /// A fixed rig's eye and the point it looks at.
    pub eye: [f32; 3],
    pub look: [f32; 3],
}

impl CameraRig {
    /// The full path of its native value type.
    pub const PATH: &'static str = "viso::game::kit::CameraRig";

    fn rig(kind: RigKind, target: EntityId) -> CameraRig {
        CameraRig {
            kind,
            target,
            distance: 6.0,
            height: 1.6,
            pitch: -15.0,
            yaw: 0.0,
            fov: 60.0,
            lag: 0.15,
            eye: [0.0; 3],
            look: [0.0; 3],
        }
    }

    /// Behind `target`, turning with it.
    pub fn third_person(target: EntityId) -> CameraRig {
        CameraRig::rig(RigKind::ThirdPerson, target)
    }

    /// Following `target` from a fixed direction.
    pub fn follow(target: EntityId) -> CameraRig {
        CameraRig {
            distance: 14.0,
            height: 0.0,
            pitch: -35.0,
            fov: 55.0,
            lag: 0.25,
            ..CameraRig::rig(RigKind::Follow, target)
        }
    }

    /// Straight above `target`.
    pub fn top_down(target: EntityId) -> CameraRig {
        CameraRig {
            distance: 24.0,
            height: 0.0,
            pitch: -90.0,
            fov: 50.0,
            lag: 0.2,
            ..CameraRig::rig(RigKind::TopDown, target)
        }
    }

    /// At `eye` looking at `look`.
    pub fn fixed(eye: Vec3F32, look: Vec3F32) -> CameraRig {
        CameraRig {
            eye: eye.to_array(),
            look: look.to_array(),
            lag: 0.0,
            ..CameraRig::rig(RigKind::Fixed, EntityId::default())
        }
    }
}

/// What the frame draws from: a camera's eye, the point it looks at, its up
/// direction and its projection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraView {
    pub eye: Vec3F32,
    pub target: Vec3F32,
    pub up: Vec3F32,
    /// The vertical field of view, in degrees.
    pub fov: f32,
    pub near: f32,
    pub far: f32,
}

/// The camera a stage draws through: its rig and where it is on the way to
/// what the rig asks for.
#[derive(Debug, Clone, Default)]
pub(super) struct Camera {
    rig: Option<CameraRig>,
    /// The eased pivot, yaw (degrees) and whether they have a value yet.
    pivot: [f32; 3],
    yaw: f32,
    settled: bool,
    /// The shake: its strength in metres, its length and what is left of it
    /// in seconds, and the time it has run.
    shake: [f32; 3],
    clock: f32,
}

impl Camera {
    /// Follows `rig` from now on, easing over from where the camera is.
    pub(super) fn set(&mut self, rig: CameraRig) {
        self.rig = Some(rig);
    }

    /// Shakes the camera by up to `strength` metres for `seconds`, fading
    /// out; a stronger shake replaces a weaker one.
    pub(super) fn shake(&mut self, strength: f32, seconds: f32) {
        let strength = if strength.is_finite() {
            strength.clamp(0.0, 4.0)
        } else {
            0.0
        };
        let seconds = if seconds.is_finite() {
            seconds.clamp(0.0, 5.0)
        } else {
            0.0
        };
        let left = self.shake[0] * (self.shake[2] / self.shake[1].max(f32::EPSILON));
        if strength >= left && seconds > 0.0 {
            self.shake = [strength, seconds, seconds];
        }
    }

    /// Eases toward the rig over `dt` seconds, its target drawn `alpha` of
    /// the way into the next tick.
    pub(super) fn advance(&mut self, dt: f32, world: &GameWorld, alpha: f32) {
        self.clock += dt;
        self.shake[2] = (self.shake[2] - dt).max(0.0);
        let Some(rig) = self.rig else {
            return;
        };
        let (pivot, yaw) = match rig.kind {
            RigKind::Fixed => (rig.look, rig.yaw),
            _ => {
                let Some(at) = world.interpolated(rig.target, alpha) else {
                    return;
                };
                let [x, y, z] = at.to_array();
                let yaw = match rig.kind {
                    RigKind::ThirdPerson => {
                        let [fx, fz] = world.facing(rig.target).unwrap_or([0.0, -1.0]);
                        fx.atan2(-fz).to_degrees()
                    }
                    _ => rig.yaw,
                };
                ([x, y + rig.height, z], yaw)
            }
        };
        let ease = if !self.settled || rig.lag <= 0.0 || !dt.is_finite() {
            1.0
        } else {
            1.0 - (-dt.max(0.0) / rig.lag).exp()
        };
        for (p, to) in self.pivot.iter_mut().zip(pivot) {
            *p += (to - *p) * ease;
        }
        let turn = (yaw - self.yaw + 540.0).rem_euclid(360.0) - 180.0;
        self.yaw += turn * ease;
        self.settled = true;
    }

    /// The view from the camera now: a default overview without a rig.
    pub(super) fn view(&self) -> CameraView {
        let Some(rig) = self.rig.filter(|_| self.settled) else {
            return CameraView {
                eye: Vec3F32::new(0.0, 10.0, 14.0),
                target: Vec3F32::new(0.0, 0.0, 0.0),
                up: Vec3F32::new(0.0, 1.0, 0.0),
                fov: 60.0,
                near: NEAR,
                far: FAR,
            };
        };
        let (yaw, pitch) = (self.yaw.to_radians(), rig.pitch.clamp(-90.0, 90.0));
        let along = [yaw.sin(), -yaw.cos()];
        let (eye, look, up) = match rig.kind {
            RigKind::Fixed => (rig.eye, rig.look, [0.0, 1.0, 0.0]),
            _ => {
                let p = pitch.to_radians();
                let forward = [along[0] * p.cos(), p.sin(), along[1] * p.cos()];
                let eye = std::array::from_fn(|i| self.pivot[i] - forward[i] * rig.distance);
                let up = if pitch.abs() >= VERTICAL {
                    [along[0], 0.0, along[1]]
                } else {
                    [0.0, 1.0, 0.0]
                };
                (eye, self.pivot, up)
            }
        };
        let offset = self.shake_offset();
        let moved =
            |v: [f32; 3]| Vec3F32::new(v[0] + offset[0], v[1] + offset[1], v[2] + offset[2]);
        CameraView {
            eye: moved(eye),
            target: moved(look),
            up: Vec3F32::from_array(up),
            fov: rig.fov.clamp(20.0, 120.0),
            near: NEAR,
            far: FAR,
        }
    }

    /// Where the shake moves the camera now: incommensurate sines, fading
    /// with the square of the time left.
    fn shake_offset(&self) -> [f32; 3] {
        let [strength, length, left] = self.shake;
        if left <= 0.0 || length <= 0.0 {
            return [0.0; 3];
        }
        let fade = left / length;
        let amount = strength * fade * fade;
        let t = self.clock;
        [
            amount * (t * 47.0).sin(),
            amount * (t * 61.0 + 1.3).sin() * 0.6,
            amount * (t * 53.0 + 2.1).sin(),
        ]
    }

    /// Forgets the rig, as a rebuilt world starts over.
    pub(super) fn reset(&mut self) {
        *self = Camera::default();
    }
}

impl NativeValue for CameraRig {
    const TY: SchemaTy = SchemaTy::Value(CameraRig::PATH);

    fn from_value(value: &Value) -> Option<CameraRig> {
        let Value::Agg(agg) = value else {
            return None;
        };
        let [kind, index, generation, rest @ ..] = &agg.fields[..] else {
            return None;
        };
        let f = |v: &Value| v.as_float().map(|v| v as f32);
        let [
            distance,
            height,
            pitch,
            yaw,
            fov,
            lag,
            ex,
            ey,
            ez,
            lx,
            ly,
            lz,
        ] = rest
        else {
            return None;
        };
        let kind = [
            RigKind::ThirdPerson,
            RigKind::Follow,
            RigKind::TopDown,
            RigKind::Fixed,
        ]
        .get(usize::try_from(kind.as_int()?).ok()?)
        .copied()?;
        Some(CameraRig {
            kind,
            target: EntityId::new(
                u32::try_from(index.as_int()?).ok()?,
                u32::try_from(generation.as_int()?).ok()?,
            ),
            distance: f(distance)?,
            height: f(height)?,
            pitch: f(pitch)?,
            yaw: f(yaw)?,
            fov: f(fov)?,
            lag: f(lag)?,
            eye: [f(ex)?, f(ey)?, f(ez)?],
            look: [f(lx)?, f(ly)?, f(lz)?],
        })
    }

    fn into_value(self) -> Value {
        let f = |v: f32| Value::Float(f64::from(v));
        let [ex, ey, ez] = self.eye;
        let [lx, ly, lz] = self.look;
        Value::Agg(Rc::new(Aggregate {
            tag: 0,
            fields: Box::new([
                Value::Int(self.kind as i64),
                Value::Int(i64::from(self.target.index())),
                Value::Int(i64::from(self.target.generation())),
                f(self.distance),
                f(self.height),
                f(self.pitch),
                f(self.yaw),
                f(self.fov),
                f(self.lag),
                f(ex),
                f(ey),
                f(ez),
                f(lx),
                f(ly),
                f(lz),
            ]),
        }))
    }
}
