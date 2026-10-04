//! The stage: everything the Kit's Presentation commands produce, as the
//! frame reads it. The camera's view, the particles' sprites, the sound cues
//! waiting for the audio thread, and the debug shapes.
//!
//! A tick's commands reach the stage when the tick's commands are delivered;
//! a `FrameUpdate`'s as it calls them. The scheduler advances the stage once
//! a frame, after the frame's hooks: the camera eases, the particles move.
//! Debug shapes a tick drew last until the next tick delivers; those a frame
//! drew, until the next frame.

use super::camera::{Camera, CameraRig, CameraView};
use super::particles::{Particles, Sprite};
use super::synth::{Cue, Sound};
use super::{Particle, Sfx, Wave};
use crate::game::world::{EntityId, GameWorld};
use crate::native::Vec3F32;

/// The most cues waiting; more are dropped until the host drains them.
pub const MAX_CUES: usize = 64;
/// The distance within which a positioned sound plays at full gain, in
/// metres; past it the gain falls off with distance.
const HEARING: f32 = 8.0;
/// How far a positioned sound pans at most.
const PAN: f32 = 0.8;

/// A debug shape: removed from release builds with the calls that draw it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DebugShape {
    Line {
        from: Vec3F32,
        to: Vec3F32,
    },
    Box {
        center: Vec3F32,
        half_extents: Vec3F32,
    },
}

/// What the Kit's Presentation commands produce.
#[derive(Debug, Clone, Default)]
pub struct Stage {
    camera: Camera,
    particles: Particles,
    cues: Vec<Cue>,
    tick_shapes: Vec<DebugShape>,
    frame_shapes: Vec<DebugShape>,
    in_frame: bool,
}

impl Stage {
    /// The camera's view.
    pub fn view(&self) -> CameraView {
        self.camera.view()
    }

    /// The particles as the frame draws them.
    pub fn sprites(&self) -> impl Iterator<Item = Sprite> + '_ {
        self.particles.sprites()
    }

    /// The particles alive.
    pub fn particles(&self) -> usize {
        self.particles.len()
    }

    /// The particle emitters running.
    pub fn emitters(&self) -> usize {
        self.particles.emitters()
    }

    /// The sound cues issued since the last drain.
    pub fn cues(&self) -> &[Cue] {
        &self.cues
    }

    /// Takes the sound cues, in issue order, for the audio thread's
    /// [`Synth`](super::Synth).
    pub fn drain_cues(&mut self) -> std::vec::Drain<'_, Cue> {
        self.cues.drain(..)
    }

    /// The debug shapes to draw this frame.
    pub fn debug_shapes(&self) -> impl Iterator<Item = &DebugShape> {
        self.tick_shapes.iter().chain(&self.frame_shapes)
    }

    pub(super) fn camera(&mut self, rig: CameraRig) {
        self.camera.set(rig);
    }

    pub(super) fn shake(&mut self, strength: f32, seconds: f32) {
        self.camera.shake(strength, seconds);
    }

    pub(super) fn burst(&mut self, kind: Particle, at: Vec3F32, count: i64) {
        self.particles.burst(kind, at.to_array(), count);
    }

    pub(super) fn emit(&mut self, entity: EntityId, kind: Particle) {
        self.particles.emit(entity, kind);
    }

    pub(super) fn stop_emit(&mut self, entity: EntityId) {
        self.particles.stop(entity);
    }

    /// Cues `sfx`, placed at `at` when given: panned and quieted by where
    /// it is from the camera.
    pub(super) fn sound(&mut self, sfx: Sfx, at: Option<Vec3F32>) {
        let (gain, pan) = at.map_or((1.0, 0.0), |at| self.placed(at));
        self.cue(Cue {
            sound: Sound::Sfx(sfx),
            gain,
            pan,
        });
    }

    pub(super) fn tone(&mut self, wave: Wave, from: f32, to: f32, seconds: f32) {
        self.cue(Cue {
            sound: Sound::Tone {
                wave,
                from,
                to,
                seconds,
            },
            gain: 1.0,
            pan: 0.0,
        });
    }

    pub(super) fn debug(&mut self, shape: DebugShape) {
        if self.in_frame {
            self.frame_shapes.push(shape);
        } else {
            self.tick_shapes.push(shape);
        }
    }

    /// Before a tick's commands are delivered.
    pub(crate) fn begin_tick(&mut self) {
        self.tick_shapes.clear();
    }

    /// Before a frame's hooks run.
    pub(crate) fn begin_frame(&mut self) {
        self.frame_shapes.clear();
        self.in_frame = true;
    }

    /// After a frame's hooks ran: the camera eases and the particles move
    /// `dt` seconds, the world drawn `alpha` of the way into the next tick.
    pub(crate) fn end_frame(&mut self, dt: f32, world: &GameWorld, alpha: f32) {
        self.in_frame = false;
        self.camera.advance(dt, world, alpha);
        self.particles.advance(dt, world, alpha);
    }

    /// Starts over, as a rebuilt world does: no rig, particle, cue or
    /// shape.
    pub(crate) fn reset(&mut self) {
        self.camera.reset();
        self.particles.clear();
        self.cues.clear();
        self.tick_shapes.clear();
        self.frame_shapes.clear();
    }

    fn cue(&mut self, cue: Cue) {
        if self.cues.len() < MAX_CUES {
            self.cues.push(cue);
        }
    }

    /// The gain and pan of a sound at `at` heard from the camera.
    fn placed(&self, at: Vec3F32) -> (f32, f32) {
        let view = self.camera.view();
        let sub = |a: Vec3F32, b: Vec3F32| [a.x - b.x, a.y - b.y, a.z - b.z];
        let to = sub(at, view.eye);
        let distance = to.iter().map(|c| c * c).sum::<f32>().sqrt();
        if !distance.is_finite() || distance <= f32::EPSILON {
            return (1.0, 0.0);
        }
        let forward = sub(view.target, view.eye);
        let up = view.up.to_array();
        let right = [
            forward[1] * up[2] - forward[2] * up[1],
            forward[2] * up[0] - forward[0] * up[2],
            forward[0] * up[1] - forward[1] * up[0],
        ];
        let length = right.iter().map(|c| c * c).sum::<f32>().sqrt();
        let pan = if length > 0.0 {
            (0..3).map(|i| to[i] * right[i]).sum::<f32>() / (distance * length)
        } else {
            0.0
        };
        ((HEARING / distance).min(1.0), (pan * PAN).clamp(-1.0, 1.0))
    }
}
