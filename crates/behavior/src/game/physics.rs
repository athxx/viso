//! The physics a game world steps its bodies with (§106, §108): the
//! [`Physics`] contract an engine implements, and [`Kinematic`], the
//! built-in engine.
//!
//! The world owns the entities, their commands and the contacts' events; an
//! engine owns the motion. Each tick, after the world commits its commands
//! and its behaviors steer, the engine moves the bodies over the step and
//! reports which overlap; the world then reports the contacts that began. An
//! engine declares the determinism tier its step reaches, which a game whose
//! profile demands more rejects (`E9104`), and whether it can save and
//! restore what it keeps between steps: without that, a snapshot of its
//! world is degraded, and restoring it may diverge from an uninterrupted run.

use std::fmt;

use super::grid::{Bounds, Grid, bounds};
use super::world::{Bodies, BodyKind, EntityId, GRAVITY};
use crate::native::Determinism;

/// A physics engine, stepping a world's bodies.
///
/// Every body is a box: a [`BodyKind::Character`] moves, a
/// [`BodyKind::Block`] stands still and stops characters, and a
/// [`BodyKind::Sensor`] stands still and stops nothing. Each step, a
/// character's horizontal velocity is the `walk` its systems committed this
/// tick and its `jump` adds to its vertical velocity
/// ([`StepBodies::take_push`]); the engine moves it, sets whether a block
/// holds it up, and reports the pairs that overlap.
pub trait Physics: fmt::Debug {
    /// The engine's name, for diagnostics.
    fn name(&self) -> &str;

    /// How far its step reproduces: on one binary, on every Tier-1 target,
    /// or not at all.
    fn determinism(&self) -> Determinism;

    /// Steps `bodies` `dt` seconds and appends each pair of bodies that
    /// overlaps afterwards, a character with a sensor or another character,
    /// by their places in [`StepBodies::order`], once each.
    fn step(&mut self, bodies: &mut StepBodies<'_>, dt: f32, contacts: &mut Vec<(u32, u32)>);

    /// Forgets what it derived from the bodies that do not move: a block
    /// was spawned, moved or removed, or the bodies were replaced.
    fn statics_changed(&mut self) {}

    /// What it keeps between steps, for a snapshot to restore; `None` when it
    /// cannot be saved, which makes the snapshot degraded.
    fn save(&self) -> Option<Vec<u8>> {
        None
    }

    /// Puts back what [`save`](Self::save) wrote.
    ///
    /// # Errors
    ///
    /// State it cannot read, which leaves it as after
    /// [`statics_changed`](Self::statics_changed).
    fn load(&mut self, state: &[u8]) -> Result<(), PhysicsError> {
        let _ = state;
        Err(PhysicsError::new("this engine keeps no state to restore"))
    }

    /// A new engine of the same kind and settings, keeping nothing: the one
    /// a rebuilt game steps with.
    fn fork(&self) -> Box<dyn Physics>;
}

/// A physics engine's state that would not load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicsError {
    message: String,
}

impl PhysicsError {
    /// An error reading `message`.
    pub fn new(message: impl Into<String>) -> PhysicsError {
        PhysicsError {
            message: message.into(),
        }
    }
}

impl fmt::Display for PhysicsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PhysicsError {}

/// A physics engine whose tier is below the one the game's profile demands
/// (`E9104`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicsTier {
    /// The engine.
    pub engine: String,
    /// The tier it reaches.
    pub has: Determinism,
    /// The tier the game demands.
    pub needs: Determinism,
}

impl PhysicsTier {
    /// The stable diagnostic code.
    pub fn code(&self) -> &'static str {
        "E9104"
    }

    /// Whether `engine` reaches `needs`, else why not.
    pub(super) fn check(engine: &dyn Physics, needs: Determinism) -> Result<(), PhysicsTier> {
        let has = engine.determinism();
        if has >= needs {
            return Ok(());
        }
        Err(PhysicsTier {
            engine: engine.name().to_owned(),
            has,
            needs,
        })
    }
}

impl fmt::Display for PhysicsTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the physics engine `{}` is reproducible at `{}`, below the `{}` the game's \
             Simulation needs",
            self.engine,
            self.has.name(),
            self.needs.name()
        )
    }
}

impl std::error::Error for PhysicsTier {}

/// The bodies of a world as an engine steps them, by slot.
pub struct StepBodies<'a> {
    pub(super) b: &'a mut Bodies,
    pub(super) order: &'a [u32],
}

impl<'a> StepBodies<'a> {
    /// The live slots, in allocation order; the slice outlives a borrow of
    /// the bodies, so an engine can walk it while it moves them.
    #[inline]
    pub fn order(&self) -> &'a [u32] {
        self.order
    }

    /// How many slots there are, live or free.
    #[inline]
    pub fn slots(&self) -> usize {
        self.b.slots()
    }

    /// The entity at `slot`.
    #[inline]
    pub fn id(&self, slot: usize) -> EntityId {
        self.b.id(slot)
    }

    /// The body at `slot`.
    #[inline]
    pub fn kind(&self, slot: usize) -> BodyKind {
        self.b.kind[slot]
    }

    /// Its tags, one bit per tag variant.
    #[inline]
    pub fn tags(&self, slot: usize) -> u64 {
        self.b.tags[slot]
    }

    /// Its half extents.
    #[inline]
    pub fn half_extents(&self, slot: usize) -> [f32; 3] {
        self.b.half[slot]
    }

    /// Its centre.
    #[inline]
    pub fn position(&self, slot: usize) -> [f32; 3] {
        self.b.pos[slot]
    }

    /// Moves its centre to `to`.
    #[inline]
    pub fn set_position(&mut self, slot: usize, to: [f32; 3]) {
        self.b.pos[slot] = to;
    }

    /// Its velocity.
    #[inline]
    pub fn velocity(&self, slot: usize) -> [f32; 3] {
        self.b.vel[slot]
    }

    /// Sets its velocity.
    #[inline]
    pub fn set_velocity(&mut self, slot: usize, to: [f32; 3]) {
        self.b.vel[slot] = to;
    }

    /// The `walk` (x, z) and `jump` (y) its systems committed for this step,
    /// leaving none for the next.
    #[inline]
    pub fn take_push(&mut self, slot: usize) -> [f32; 3] {
        std::mem::take(&mut self.b.push[slot])
    }

    /// Whether a block held it up at the last step.
    #[inline]
    pub fn on_floor(&self, slot: usize) -> bool {
        self.b.floor[slot]
    }

    /// Sets whether a block holds it up.
    #[inline]
    pub fn set_on_floor(&mut self, slot: usize, on: bool) {
        self.b.floor[slot] = on;
    }
}

/// The built-in engine: characters fall at [`GRAVITY`] and blocks stop them
/// axis by axis, vertical first, each against the blocks in allocation order;
/// a block below sets `on_floor`. Every operation is single-precision IEEE
/// without fused operations, so it reproduces on every Tier-1 target.
///
/// A uniform grid finds each body's neighbours: the blocks a character's
/// sweep may meet, regridded only when a block changes, and the sensors and
/// characters it may touch, regridded every step. The outcome is bit for bit
/// that of testing every pair in allocation order. What it keeps between
/// steps derives from the bodies, so it saves as nothing.
#[derive(Debug, Default)]
pub struct Kinematic {
    bp: Broadphase,
}

impl Physics for Kinematic {
    fn name(&self) -> &str {
        "viso::game::Kinematic"
    }

    fn determinism(&self) -> Determinism {
        Determinism::CrossPlatform
    }

    fn statics_changed(&mut self) {
        self.bp.fresh = false;
    }

    fn save(&self) -> Option<Vec<u8>> {
        Some(Vec::new())
    }

    fn load(&mut self, state: &[u8]) -> Result<(), PhysicsError> {
        self.bp.fresh = false;
        if state.is_empty() {
            Ok(())
        } else {
            Err(PhysicsError::new("the built-in engine saves no state"))
        }
    }

    fn fork(&self) -> Box<dyn Physics> {
        Box::new(Kinematic::default())
    }

    /// A character meets only the blocks the grid finds near its sweep along
    /// an axis, in allocation order; should a block push it out of that
    /// sweep, every later block is tested, so the outcome is that of testing
    /// every block in allocation order.
    fn step(&mut self, bodies: &mut StepBodies<'_>, dt: f32, contacts: &mut Vec<(u32, u32)>) {
        let (b, order) = (&mut *bodies.b, bodies.order);
        let bp = &mut self.bp;
        bp.prepare(b, order);
        for &slot in order {
            let slot = slot as usize;
            if b.kind[slot] != BodyKind::Character {
                continue;
            }
            let push = std::mem::take(&mut b.push[slot]);
            let vel = &mut b.vel[slot];
            vel[0] = push[0];
            vel[2] = push[2];
            vel[1] += push[1] - GRAVITY * dt;
            b.floor[slot] = false;
            let vel = b.vel[slot];
            if vel == [0.0; 3] {
                continue;
            }
            // One query covers the whole sweep: each axis moves within its
            // own range unless a block pushes it out, after which the axes
            // left test every block.
            let (half, from) = (b.half[slot], b.pos[slot]);
            let to: [f32; 3] = std::array::from_fn(|i| from[i] + vel[i] * dt);
            let (start, end) = (bounds(from, half), bounds(to, half));
            let region = (
                std::array::from_fn(|i| start.0[i].min(end.0[i])),
                std::array::from_fn(|i| start.1[i].max(end.1[i])),
            );
            bp.blocks_near(b, order, region);
            let mut swept = true;
            for axis in [1, 0, 2] {
                let speed = vel[axis];
                if speed == 0.0 {
                    continue;
                }
                b.pos[slot][axis] += speed * dt;
                let (low, high) = (from[axis].min(to[axis]), from[axis].max(to[axis]));
                let mut rest = 0;
                if swept {
                    rest = order.len();
                    for &rank in &bp.found {
                        let other = order[rank as usize] as usize;
                        if b.overlap(slot, other, axis) {
                            b.resolve(slot, other, axis, speed);
                            if !(low..=high).contains(&b.pos[slot][axis]) {
                                swept = false;
                                rest = rank as usize + 1;
                                break;
                            }
                        }
                    }
                }
                for &other in &order[rest..] {
                    let other = other as usize;
                    if b.kind[other] == BodyKind::Block && b.overlap(slot, other, axis) {
                        b.resolve(slot, other, axis, speed);
                    }
                }
            }
        }
        bp.movers.clear(bp.cell);
        bp.low.resize(b.slots(), [0; 3]);
        for &slot in order {
            let s = slot as usize;
            if b.kind[s] != BodyKind::Block {
                let bounds = bounds(b.pos[s], b.half[s]);
                bp.low[s] = bp.movers.cell(bounds.0);
                bp.movers.insert(slot, bounds);
            }
        }
        bp.movers.finish();
        let kinds = |a: usize, o: usize| {
            matches!(
                (b.kind[a], b.kind[o]),
                (BodyKind::Character, BodyKind::Character | BodyKind::Sensor)
                    | (BodyKind::Sensor, BodyKind::Character)
            )
        };
        let rank = |slot: usize| bp.rank[slot];
        let ordered = |a: u32, o: u32| (a.min(o), a.max(o));
        for (cell, run) in bp.movers.cells() {
            for (i, &(_, a)) in run.iter().enumerate() {
                let a = a as usize;
                for &(_, o) in &run[i + 1..] {
                    let o = o as usize;
                    if !kinds(a, o) {
                        continue;
                    }
                    // Counted in the cell of the overlap's lowest corner only.
                    let (la, lo) = (bp.low[a], bp.low[o]);
                    if (0..3).all(|k| la[k].max(lo[k]) == cell[k]) && b.touch(a, o) {
                        contacts.push(ordered(rank(a), rank(o)));
                    }
                }
            }
        }
        let wide = bp.movers.wide();
        for (i, &w) in wide.iter().enumerate() {
            let w = w as usize;
            for &o in order {
                let o = o as usize;
                let counted = wide[..=i].contains(&(o as u32));
                if !counted && kinds(w, o) && b.touch(w, o) {
                    contacts.push(ordered(rank(w), rank(o)));
                }
            }
        }
    }
}

/// What the step keeps between ticks to find a body's neighbours: derived
/// from the bodies, never snapshotted, rebuilt when stale.
#[derive(Debug, Default)]
struct Broadphase {
    /// The cell size the grids are built at.
    cell: f32,
    /// The blocks, valid while `fresh`.
    blocks: Grid,
    fresh: bool,
    /// The sensors and characters, regridded every step.
    movers: Grid,
    /// Each live slot's place in allocation order.
    rank: Vec<u32>,
    /// Scratch: a query's slots, then their ranks.
    found: Vec<u32>,
    /// Each mover's lowest cell.
    low: Vec<[i32; 3]>,
}

impl Broadphase {
    /// Ranks the live slots and, when the blocks moved or the cell size
    /// changed, regrids the blocks. The cell is the smallest power of two
    /// as wide as the widest character, so a character covers at most two
    /// cells an axis.
    fn prepare(&mut self, b: &Bodies, order: &[u32]) {
        self.rank.resize(b.slots(), 0);
        let mut widest: f32 = 0.0;
        for (i, &slot) in order.iter().enumerate() {
            let slot = slot as usize;
            self.rank[slot] = i as u32;
            if b.kind[slot] == BodyKind::Character {
                widest = b.half[slot].iter().fold(widest, |w, &h| w.max(2.0 * h));
            }
        }
        let cell = if widest.is_finite() {
            widest.clamp(0.25, 4096.0).log2().ceil().exp2()
        } else {
            4096.0
        };
        if self.fresh && self.cell == cell {
            return;
        }
        self.cell = cell;
        self.blocks.clear(cell);
        for &slot in order {
            let s = slot as usize;
            if b.kind[s] == BodyKind::Block {
                self.blocks.insert(slot, bounds(b.pos[s], b.half[s]));
            }
        }
        self.blocks.finish();
        self.fresh = true;
    }

    /// The ranks of the blocks near `region`, every block when the region is
    /// too large to query, ascending and once each, into `found`.
    fn blocks_near(&mut self, b: &Bodies, order: &[u32], region: Bounds) {
        self.found.clear();
        if self.blocks.query(region, &mut self.found) {
            for slot in &mut self.found {
                *slot = self.rank[*slot as usize];
            }
        } else {
            let all = order
                .iter()
                .enumerate()
                .filter(|&(_, &s)| b.kind[s as usize] == BodyKind::Block);
            self.found.extend(all.map(|(i, _)| i as u32));
        }
        self.found.sort_unstable();
        self.found.dedup();
    }
}
