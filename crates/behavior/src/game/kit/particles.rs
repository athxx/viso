//! Particles: bursts and emitters of short-lived points the frame draws.
//! They are Presentation, local to the device: nothing in the Simulation
//! reads them, so they draw from their own random source and step with the
//! frame, not the tick.
//!
//! A fixed pool holds every particle: a full pool recycles its particles in
//! turn, so a storm of bursts costs a bounded amount of memory and time.

use super::Particle;
use crate::game::world::{EntityId, GameWorld};
use crate::native::Vec3F32;

/// The most particles alive at once.
pub const MAX_PARTICLES: usize = 4096;
/// The most emitters at once; another replaces the oldest.
pub const MAX_EMITTERS: usize = 64;
/// The most particles one burst spawns.
const MAX_BURST: u32 = 256;

/// How a kind of particle looks and moves.
#[derive(Debug, Clone, Copy)]
struct Look {
    /// Seconds it lives.
    life: f32,
    /// Its size at birth and at death, in metres.
    size: [f32; 2],
    /// Its launch speed, in m/s.
    speed: f32,
    /// How strongly gravity pulls it: negative rises.
    gravity: f32,
    /// How quickly the air slows it, per second.
    drag: f32,
    /// Its colour, linear RGBA.
    color: [f32; 4],
    /// Per second an emitter spawns.
    rate: f32,
    /// How much of its launch is straight up rather than any direction.
    lift: f32,
}

const fn look(kind: Particle) -> Look {
    match kind {
        Particle::Spark => Look {
            life: 0.5,
            size: [0.08, 0.02],
            speed: 6.0,
            gravity: 1.0,
            drag: 0.2,
            color: [1.0, 0.85, 0.5, 1.0],
            rate: 40.0,
            lift: 0.3,
        },
        Particle::Smoke => Look {
            life: 1.4,
            size: [0.15, 0.6],
            speed: 1.2,
            gravity: -0.15,
            drag: 1.4,
            color: [0.55, 0.55, 0.58, 0.55],
            rate: 16.0,
            lift: 0.7,
        },
        Particle::Dust => Look {
            life: 0.9,
            size: [0.12, 0.3],
            speed: 1.6,
            gravity: 0.25,
            drag: 2.0,
            color: [0.72, 0.62, 0.46, 0.7],
            rate: 20.0,
            lift: 0.2,
        },
        Particle::Trail => Look {
            life: 0.6,
            size: [0.09, 0.0],
            speed: 0.2,
            gravity: 0.0,
            drag: 3.0,
            color: [0.8, 0.9, 1.0, 0.8],
            rate: 60.0,
            lift: 0.0,
        },
        Particle::Fire => Look {
            life: 0.7,
            size: [0.22, 0.05],
            speed: 1.8,
            gravity: -0.4,
            drag: 1.0,
            color: [1.0, 0.48, 0.12, 0.9],
            rate: 48.0,
            lift: 0.8,
        },
        Particle::Confetti => Look {
            life: 1.8,
            size: [0.1, 0.1],
            speed: 5.0,
            gravity: 0.35,
            drag: 1.2,
            color: [1.0, 1.0, 1.0, 1.0],
            rate: 30.0,
            lift: 0.6,
        },
    }
}

/// The colours confetti picks from.
const CONFETTI: [[f32; 4]; 6] = [
    [0.95, 0.26, 0.21, 1.0],
    [1.0, 0.76, 0.03, 1.0],
    [0.3, 0.69, 0.31, 1.0],
    [0.13, 0.59, 0.95, 1.0],
    [0.61, 0.15, 0.69, 1.0],
    [1.0, 0.6, 0.0, 1.0],
];

/// A particle as the frame draws it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sprite {
    pub position: Vec3F32,
    /// Its diameter, in metres.
    pub size: f32,
    /// Linear RGBA, faded with age.
    pub color: [f32; 4],
}

#[derive(Debug, Clone, Copy)]
struct Point {
    pos: [f32; 3],
    vel: [f32; 3],
    age: f32,
    kind: Particle,
    color: [f32; 4],
}

#[derive(Debug, Clone, Copy)]
struct Emitter {
    entity: EntityId,
    kind: Particle,
    /// Particles owed but not yet spawned.
    owed: f32,
}

/// Every particle and emitter.
#[derive(Debug, Clone)]
pub(super) struct Particles {
    points: Vec<Point>,
    /// The particle a full pool recycles next.
    oldest: usize,
    emitters: Vec<Emitter>,
    random: u32,
}

impl Default for Particles {
    fn default() -> Particles {
        Particles {
            points: Vec::with_capacity(MAX_PARTICLES),
            oldest: 0,
            emitters: Vec::with_capacity(MAX_EMITTERS),
            random: 0x9e37_79b9,
        }
    }
}

impl Particles {
    /// Spawns `count` particles of `kind` at `at`.
    pub(super) fn burst(&mut self, kind: Particle, at: [f32; 3], count: i64) {
        let count = count.clamp(0, i64::from(MAX_BURST)) as u32;
        if at.iter().all(|c| c.is_finite()) {
            for _ in 0..count {
                self.spawn(kind, at);
            }
        }
    }

    /// Emits `kind` from `entity` while it lives; the same kind from the
    /// same entity again does nothing.
    pub(super) fn emit(&mut self, entity: EntityId, kind: Particle) {
        if self
            .emitters
            .iter()
            .any(|e| e.entity == entity && e.kind == kind)
        {
            return;
        }
        if self.emitters.len() == MAX_EMITTERS {
            self.emitters.remove(0);
        }
        self.emitters.push(Emitter {
            entity,
            kind,
            owed: 0.0,
        });
    }

    /// Stops every emitter on `entity`; its particles live out their lives.
    pub(super) fn stop(&mut self, entity: EntityId) {
        self.emitters.retain(|e| e.entity != entity);
    }

    /// Steps every particle `dt` seconds and lets the emitters spawn,
    /// entities drawn `alpha` of the way into the next tick; an emitter
    /// whose entity is gone stops.
    pub(super) fn advance(&mut self, dt: f32, world: &GameWorld, alpha: f32) {
        if !(dt.is_finite() && dt > 0.0) {
            return;
        }
        let dt = dt.min(0.1);
        self.points.retain_mut(|p| {
            let look = look(p.kind);
            p.age += dt;
            if p.age >= look.life {
                return false;
            }
            p.vel[1] -= look.gravity * crate::game::GRAVITY * dt;
            let slow = 1.0 / (1.0 + look.drag * dt);
            for i in 0..3 {
                p.vel[i] *= slow;
                p.pos[i] += p.vel[i] * dt;
            }
            true
        });
        self.oldest = 0;
        let mut emitters = std::mem::take(&mut self.emitters);
        emitters.retain_mut(|e| {
            let Some(at) = world.interpolated(e.entity, alpha) else {
                return false;
            };
            e.owed += look(e.kind).rate * dt;
            while e.owed >= 1.0 {
                e.owed -= 1.0;
                self.spawn(e.kind, at.to_array());
            }
            true
        });
        self.emitters = emitters;
    }

    /// The particles as the frame draws them.
    pub(super) fn sprites(&self) -> impl Iterator<Item = Sprite> + '_ {
        self.points.iter().map(|p| {
            let look = look(p.kind);
            let t = p.age / look.life;
            let [r, g, b, a] = p.color;
            Sprite {
                position: Vec3F32::from_array(p.pos),
                size: look.size[0] + (look.size[1] - look.size[0]) * t,
                color: [r, g, b, a * (1.0 - t)],
            }
        })
    }

    /// The particles alive.
    pub(super) fn len(&self) -> usize {
        self.points.len()
    }

    /// The emitters running.
    pub(super) fn emitters(&self) -> usize {
        self.emitters.len()
    }

    /// Drops every particle and emitter.
    pub(super) fn clear(&mut self) {
        self.points.clear();
        self.emitters.clear();
        self.oldest = 0;
    }

    fn spawn(&mut self, kind: Particle, at: [f32; 3]) {
        let look = look(kind);
        // A uniform direction in the unit ball, by rejection.
        let mut dir = [0.0, 1.0, 0.0];
        for _ in 0..8 {
            let v = [self.unit(), self.unit(), self.unit()];
            if v.iter().map(|c| c * c).sum::<f32>() <= 1.0 {
                dir = v;
                break;
            }
        }
        let speed = look.speed * (0.5 + 0.5 * self.unit().abs());
        let vel = [
            dir[0] * speed * (1.0 - look.lift),
            (dir[1] * (1.0 - look.lift) + look.lift) * speed,
            dir[2] * speed * (1.0 - look.lift),
        ];
        let color = match kind {
            Particle::Confetti => CONFETTI[(self.next() % CONFETTI.len() as u32) as usize],
            _ => look.color,
        };
        let point = Point {
            pos: at,
            vel,
            age: 0.0,
            kind,
            color,
        };
        if self.points.len() < MAX_PARTICLES {
            self.points.push(point);
        } else {
            self.points[self.oldest] = point;
            self.oldest = (self.oldest + 1) % MAX_PARTICLES;
        }
    }

    /// xorshift32.
    fn next(&mut self) -> u32 {
        let mut x = self.random;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.random = x;
        x
    }

    /// A uniform value in `[-1, 1)`.
    fn unit(&mut self) -> f32 {
        (self.next() >> 8) as f32 / (1u32 << 23) as f32 - 1.0
    }
}
