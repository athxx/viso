//! What steers a character between ticks: a behavior (wander, chase,
//! patrol) or a driver (a vehicle). Each lowers to an ordinary `walk`: the
//! step adds the steering of every controlled character, computed from the
//! committed positions before anything moves, in allocation order, to the
//! `walk` its systems committed.
//!
//! All of it is single-precision IEEE arithmetic without fused operations and
//! without host transcendentals (a vehicle turns through a polynomial), and a
//! wandering character draws its goals from the world's seeded random
//! source, so steering replays bit for bit on every target.

use std::cell::Cell;
use std::rc::Rc;

use super::super::world::{Bodies, BodyKind};
use crate::wire::{malformed, read_list, read_u32_varint, write_list};
use viso_ende::{DecodeError, Decoder, Encoder};

/// How near its goal a steered character counts as there, in metres.
pub(in crate::game) const ARRIVE: f32 = 0.1;

/// A vehicle's acceleration under full throttle, in m/s².
const ACCEL: f32 = 14.0;
/// Its deceleration braking against its motion, in m/s².
const BRAKE: f32 = 28.0;
/// Its deceleration rolling without throttle, in m/s².
const COAST: f32 = 5.0;
/// Its top speed forward, in m/s.
const TOP_SPEED: f32 = 18.0;
/// Its top speed in reverse, in m/s.
const REVERSE_SPEED: f32 = 6.0;
/// Its yaw rate at full lock, in radians per second.
const STEER_RATE: f32 = 2.4;
/// The speed below which steering loses authority, so a parked vehicle
/// does not spin, in m/s.
const STEER_PEAK: f32 = 4.0;
/// The largest turn one tick makes, in radians: the rotation polynomial is
/// exact to single precision below it.
const MAX_TURN: f32 = 0.5;

/// What steers a body.
#[derive(Debug, Clone, PartialEq, Default)]
pub(in crate::game) enum Control {
    /// Nothing: it moves by its systems' `walk` alone.
    #[default]
    None,
    Wander(Wander),
    Chase(Chase),
    Patrol(Patrol),
    Vehicle(Vehicle),
}

/// Wandering around a home point: walk to a random point within `range`,
/// pause `pause` ticks, pick another.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::game) struct Wander {
    /// Where it wanders around: where it stood when the behavior committed.
    pub(in crate::game) home: [f32; 3],
    pub(in crate::game) range: f32,
    pub(in crate::game) speed: f32,
    pub(in crate::game) pause: u32,
    pub(in crate::game) goal: [f32; 3],
    /// The ticks of pause left at the goal.
    pub(in crate::game) wait: u32,
}

/// Chasing the nearest body with one of `tags` within `range`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::game) struct Chase {
    pub(in crate::game) tags: u64,
    pub(in crate::game) range: f32,
    pub(in crate::game) speed: f32,
}

/// Walking a closed route of waypoints, in order.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::game) struct Patrol {
    pub(in crate::game) route: Rc<[[f32; 3]]>,
    pub(in crate::game) speed: f32,
    /// The waypoint it walks to.
    pub(in crate::game) next: u32,
}

/// A driven vehicle: its speed along its facing, and the throttle and steer
/// committed for the next step, each summed and then clamped to `[-1, 1]`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(in crate::game) struct Vehicle {
    pub(in crate::game) speed: f32,
    pub(in crate::game) throttle: f32,
    pub(in crate::game) steer: f32,
}

impl Control {
    /// Takes it on at `at`, where the body stands when the command commits:
    /// a wanderer's home and first goal.
    pub(in crate::game) fn anchored(self, at: [f32; 3]) -> Control {
        match self {
            Control::Wander(w) => Control::Wander(Wander {
                home: at,
                goal: at,
                wait: 0,
                ..w
            }),
            other => other,
        }
    }

    /// Stops it, as a teleport puts its body at rest.
    pub(in crate::game) fn halt(&mut self) {
        if let Control::Vehicle(v) = self {
            v.speed = 0.0;
        }
    }

    pub(in crate::game) fn encode(&self, enc: &mut Encoder) {
        let f = |enc: &mut Encoder, v: f32| enc.write_u32(v.to_bits());
        let v3 = |enc: &mut Encoder, v: &[f32; 3]| v.iter().for_each(|&c| f(enc, c));
        match self {
            Control::None => enc.write_u8(0),
            Control::Wander(w) => {
                enc.write_u8(1);
                v3(enc, &w.home);
                f(enc, w.range);
                f(enc, w.speed);
                enc.write_varint(u64::from(w.pause));
                v3(enc, &w.goal);
                enc.write_varint(u64::from(w.wait));
            }
            Control::Chase(c) => {
                enc.write_u8(2);
                enc.write_u64(c.tags);
                f(enc, c.range);
                f(enc, c.speed);
            }
            Control::Patrol(p) => {
                enc.write_u8(3);
                write_list(enc, &p.route, |enc, v| v3(enc, v));
                f(enc, p.speed);
                enc.write_varint(u64::from(p.next));
            }
            Control::Vehicle(v) => {
                enc.write_u8(4);
                f(enc, v.speed);
                f(enc, v.throttle);
                f(enc, v.steer);
            }
        }
    }

    pub(in crate::game) fn decode(dec: &mut Decoder<'_>) -> Result<Control, DecodeError> {
        fn f(dec: &mut Decoder<'_>) -> Result<f32, DecodeError> {
            Ok(f32::from_bits(dec.read_u32()?))
        }
        fn v3(dec: &mut Decoder<'_>) -> Result<[f32; 3], DecodeError> {
            Ok([f(dec)?, f(dec)?, f(dec)?])
        }
        Ok(match dec.read_u8()? {
            0 => Control::None,
            1 => Control::Wander(Wander {
                home: v3(dec)?,
                range: f(dec)?,
                speed: f(dec)?,
                pause: read_u32_varint(dec)?,
                goal: v3(dec)?,
                wait: read_u32_varint(dec)?,
            }),
            2 => Control::Chase(Chase {
                tags: dec.read_u64()?,
                range: f(dec)?,
                speed: f(dec)?,
            }),
            3 => {
                let route: Vec<[f32; 3]> = read_list(dec, v3)?;
                let speed = f(dec)?;
                let next = read_u32_varint(dec)?;
                if next as usize >= route.len().max(1) {
                    return Err(malformed(dec));
                }
                Control::Patrol(Patrol {
                    route: route.into(),
                    speed,
                    next,
                })
            }
            4 => Control::Vehicle(Vehicle {
                speed: f(dec)?,
                throttle: f(dec)?,
                steer: f(dec)?,
            }),
            _ => return Err(malformed(dec)),
        })
    }
}

/// Adds the steering of every controlled character in `order` to its
/// committed `walk`, for a step of `dt` seconds, drawing from `rng`. Every
/// steering reads the positions before the step moved anything.
pub(in crate::game) fn steer(b: &mut Bodies, order: &[u32], dt: f32, rng: &Cell<u64>) {
    for &slot in order {
        let s = slot as usize;
        if b.kind[s] != BodyKind::Character || b.control[s] == Control::None {
            continue;
        }
        let mut control = std::mem::take(&mut b.control[s]);
        let at = b.pos[s];
        let walk = match &mut control {
            Control::None => [0.0; 2],
            Control::Wander(w) => {
                if planar(at, w.goal) > ARRIVE {
                    toward(at, w.goal, w.speed, dt)
                } else if w.wait > 0 {
                    w.wait -= 1;
                    [0.0; 2]
                } else {
                    w.goal = pick(w.home, w.range, rng);
                    w.wait = w.pause;
                    toward(at, w.goal, w.speed, dt)
                }
            }
            Control::Chase(c) => {
                let mut nearest: Option<(f32, [f32; 3])> = None;
                for &other in order {
                    let o = other as usize;
                    if o == s || b.tags[o] & c.tags == 0 {
                        continue;
                    }
                    let d = planar(at, b.pos[o]);
                    if d <= c.range && nearest.is_none_or(|(best, _)| d < best) {
                        nearest = Some((d, b.pos[o]));
                    }
                }
                nearest.map_or([0.0; 2], |(_, to)| toward(at, to, c.speed, dt))
            }
            Control::Patrol(p) => {
                if p.route.is_empty() {
                    [0.0; 2]
                } else {
                    if planar(at, p.route[p.next as usize]) <= ARRIVE {
                        p.next = (p.next + 1) % p.route.len() as u32;
                    }
                    toward(at, p.route[p.next as usize], p.speed, dt)
                }
            }
            Control::Vehicle(v) => drive(v, &mut b.facing[s], dt),
        };
        b.control[s] = control;
        b.push[s][0] += walk[0];
        b.push[s][2] += walk[1];
    }
}

/// After the step moved every character: a walking one faces where it
/// moved, and a vehicle keeps the part of its speed along its facing that
/// the blocks let through.
pub(in crate::game) fn settle(b: &mut Bodies, order: &[u32]) {
    for &slot in order {
        let s = slot as usize;
        if b.kind[s] != BodyKind::Character {
            continue;
        }
        let [x, _, z] = b.vel[s];
        match &mut b.control[s] {
            Control::Vehicle(v) => {
                let [fx, fz] = b.facing[s];
                v.speed = x * fx + z * fz;
            }
            _ => {
                let square = x * x + z * z;
                if square > 1e-8 {
                    let length = square.sqrt();
                    b.facing[s] = [x / length, z / length];
                }
            }
        }
    }
}

/// Integrates a vehicle's throttle and steer over `dt`, turning `facing`;
/// returns its `walk`.
fn drive(v: &mut Vehicle, facing: &mut [f32; 2], dt: f32) -> [f32; 2] {
    let throttle = clamp_unit(std::mem::take(&mut v.throttle));
    let steer = clamp_unit(std::mem::take(&mut v.steer));
    if throttle != 0.0 {
        let rate = if throttle * v.speed < 0.0 {
            BRAKE
        } else {
            ACCEL
        };
        v.speed += throttle * rate * dt;
    } else {
        let drop = COAST * dt;
        v.speed = if v.speed > drop {
            v.speed - drop
        } else if v.speed < -drop {
            v.speed + drop
        } else {
            0.0
        };
    }
    v.speed = v.speed.clamp(-REVERSE_SPEED, TOP_SPEED);
    if steer != 0.0 && v.speed != 0.0 {
        let authority = (v.speed.abs() / STEER_PEAK).min(1.0);
        let turn = steer * STEER_RATE * authority * dt;
        let turn = if v.speed < 0.0 { -turn } else { turn };
        *facing = rotate(*facing, turn.clamp(-MAX_TURN, MAX_TURN));
    }
    [facing[0] * v.speed, facing[1] * v.speed]
}

/// `[-1, 1]`, a non-finite input counting as 0.
fn clamp_unit(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// `facing` turned `angle` radians clockwise seen from above (toward `+x`
/// from `-z`), renormalized.
pub(in crate::game) fn rotate([x, z]: [f32; 2], angle: f32) -> [f32; 2] {
    let (sin, cos) = sin_cos(angle);
    let (rx, rz) = (x * cos - z * sin, z * cos + x * sin);
    let length = (rx * rx + rz * rz).sqrt();
    if length > 0.0 {
        [rx / length, rz / length]
    } else {
        [0.0, -1.0]
    }
}

/// The sine and cosine of `x`, `|x| <= MAX_TURN`, by their Taylor series:
/// the first omitted terms are below single-precision rounding there.
fn sin_cos(x: f32) -> (f32, f32) {
    let x2 = x * x;
    let sin = x * (1.0 - x2 / 6.0 * (1.0 - x2 / 20.0 * (1.0 - x2 / 42.0 * (1.0 - x2 / 72.0))));
    let cos = 1.0 - x2 / 2.0 * (1.0 - x2 / 12.0 * (1.0 - x2 / 30.0 * (1.0 - x2 / 56.0)));
    (sin, cos)
}

/// The horizontal distance from `a` to `b`.
fn planar(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (dx, dz) = (b[0] - a[0], b[2] - a[2]);
    (dx * dx + dz * dz).sqrt()
}

/// The `walk` from `at` toward `to` at `speed`, slowing to land on it.
fn toward(at: [f32; 3], to: [f32; 3], speed: f32, dt: f32) -> [f32; 2] {
    let (dx, dz) = (to[0] - at[0], to[2] - at[2]);
    let distance = (dx * dx + dz * dz).sqrt();
    if distance <= 0.0 || dt <= 0.0 {
        return [0.0; 2];
    }
    let speed = speed.min(distance / dt);
    [dx / distance * speed, dz / distance * speed]
}

/// A uniform point within `range` of `home` on the ground plane, by
/// rejection: no transcendental, so every target draws the same point.
fn pick(home: [f32; 3], range: f32, rng: &Cell<u64>) -> [f32; 3] {
    let unit =
        |rng: &Cell<u64>| (super::super::world::draw(rng) >> 40) as f32 / (1u32 << 24) as f32;
    for _ in 0..16 {
        let (x, z) = (unit(rng) * 2.0 - 1.0, unit(rng) * 2.0 - 1.0);
        if x * x + z * z <= 1.0 {
            return [home[0] + x * range, home[1], home[2] + z * range];
        }
    }
    home
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rotation_polynomial_matches_the_host_within_rounding() {
        for i in -50..=50 {
            let x = i as f32 * MAX_TURN / 50.0;
            let (sin, cos) = sin_cos(x);
            assert!((sin - x.sin()).abs() <= 2.0 * f32::EPSILON, "sin {x}");
            assert!((cos - x.cos()).abs() <= 2.0 * f32::EPSILON, "cos {x}");
        }
    }

    #[test]
    fn steering_right_turns_toward_positive_x() {
        let [x, z] = rotate([0.0, -1.0], 0.3);
        assert!(x > 0.0 && z < 0.0);
        assert!((x * x + z * z - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_vehicle_accelerates_brakes_and_coasts_to_rest() {
        let (mut v, mut facing) = (Vehicle::default(), [0.0, -1.0]);
        for _ in 0..60 {
            v.throttle = 1.0;
            drive(&mut v, &mut facing, 1.0 / 60.0);
        }
        assert!((v.speed - 14.0).abs() < 1e-3, "{}", v.speed);
        v.throttle = -1.0;
        drive(&mut v, &mut facing, 1.0 / 60.0);
        assert!(v.speed < 14.0 - 28.0 / 60.0 + 1e-3);
        for _ in 0..240 {
            drive(&mut v, &mut facing, 1.0 / 60.0);
        }
        assert_eq!(v.speed, 0.0);
        assert_eq!(facing, [0.0, -1.0]);
    }

    #[test]
    fn every_control_round_trips_its_wire_form() {
        let controls = [
            Control::None,
            Control::Wander(Wander {
                home: [1.0, 2.0, 3.0],
                range: 4.0,
                speed: 2.0,
                pause: 30,
                goal: [0.5, 2.0, -1.0],
                wait: 7,
            }),
            Control::Chase(Chase {
                tags: 0b101,
                range: 9.0,
                speed: 3.5,
            }),
            Control::Patrol(Patrol {
                route: vec![[0.0; 3], [4.0, 0.0, 0.0]].into(),
                speed: 2.0,
                next: 1,
            }),
            Control::Vehicle(Vehicle {
                speed: -2.0,
                throttle: 0.5,
                steer: -1.0,
            }),
        ];
        for control in controls {
            let mut enc = Encoder::new();
            control.encode(&mut enc);
            let bytes = enc.into_bytes();
            let mut dec = Decoder::new(&bytes);
            assert_eq!(Control::decode(&mut dec), Ok(control.clone()));
            for len in 0..bytes.len() {
                assert!(Control::decode(&mut Decoder::new(&bytes[..len])).is_err());
            }
        }
    }
}
