//! The Native World of `viso::game`: entities, their bodies and tags, the
//! command buffer Simulation hooks write through, the physics step and the
//! seeded random source.
//!
//! A hook reads the world as it was committed and writes commands: `spawn`,
//! `walk`, `jump`, `teleport` and `remove` are buffered under the issuing
//! system, and a faulting hook's commands are discarded with its state writes.
//! The scheduler commits the buffer at fixed points of a tick, merged in
//! `(system, sequence)` order, so the result depends on system order alone,
//! never on which hook ran first:
//!
//! - `walk` and `jump` add up;
//! - `teleport` sets the position, the last one winning, and puts the body at
//!   rest; the frame does not interpolate across it;
//! - `remove` ends the entity, and every later command naming it is skipped;
//! - `spawn` returns the entity's [`EntityId`] at once; the body exists from
//!   the commit, so reads before it see nothing.
//!
//! An `EntityId` is a slot and a generation: a removed entity's slot is reused
//! with the next generation, so a stale id never names a new entity. Queries
//! list entities in allocation (commit) order.
//!
//! The physics step moves characters: their horizontal velocity is the tick's
//! `walk`, `jump` adds to their vertical velocity, gravity pulls them down,
//! and blocks stop them axis by axis, a block below setting `on_floor`. Each
//! step reports the contacts that began, between a character and a sensor or
//! another character, earlier allocation first. All of it is single-precision
//! IEEE arithmetic without fused operations, reproducible on every target.
//!
//! The committed state is shared with snapshots and copied only when a commit
//! or step changes it.

use std::cell::{Cell, RefCell};
use std::fmt;
use std::rc::Rc;

use super::input::schema_enum;
use crate::native::{
    Determinism, NativeError, NativeFunction, NativeObject, NativeValue, Obj, SchemaTy, Vec3F32,
};
use crate::value::{Aggregate, Value};
use crate::wire::{malformed, read_list, read_u32_varint, write_list};
use viso_ende::{DecodeError, Decoder, Encoder};

/// Gravity, in metres per second squared.
pub const GRAVITY: f32 = 9.81;

/// How far apart two touching bodies may sit and still count as touching
/// across the other axes, absorbing rounding at a resolved contact.
const SKIN: f32 = 1e-3;

/// An entity: its slot and the generation of the slot's current occupant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct EntityId {
    index: u32,
    generation: u32,
}

impl EntityId {
    /// The full path of its native value type.
    pub const PATH: &'static str = "viso::game::EntityId";

    /// The entity in slot `index` of generation `generation`, for a host that
    /// names entities, as a host-side simulation reporting contacts does.
    pub const fn new(index: u32, generation: u32) -> EntityId {
        EntityId { index, generation }
    }

    /// Its slot.
    pub fn index(self) -> u32 {
        self.index
    }

    /// The generation of its slot.
    pub fn generation(self) -> u32 {
        self.generation
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.index, self.generation)
    }
}

impl NativeValue for EntityId {
    const TY: SchemaTy = SchemaTy::Value(EntityId::PATH);

    fn from_value(value: &Value) -> Option<EntityId> {
        let Value::Agg(agg) = value else {
            return None;
        };
        let [index, generation] = &agg.fields[..] else {
            return None;
        };
        Some(EntityId {
            index: u32::try_from(index.as_int()?).ok()?,
            generation: u32::try_from(generation.as_int()?).ok()?,
        })
    }

    fn into_value(self) -> Value {
        Value::Agg(Rc::new(Aggregate {
            tag: 0,
            fields: Box::new([
                Value::Int(i64::from(self.index)),
                Value::Int(i64::from(self.generation)),
            ]),
        }))
    }
}

/// A game tag: a variant index of the package's tag enum, below 64.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Tag(pub u32);

impl Tag {
    /// The most variants a tag enum has.
    pub const MAX: u32 = 64;

    fn bit(self) -> u64 {
        1 << self.0
    }
}

impl NativeValue for Tag {
    const TY: SchemaTy = SchemaTy::Tag;

    fn from_value(value: &Value) -> Option<Tag> {
        value
            .as_int()
            .and_then(|i| u32::try_from(i).ok())
            .filter(|&i| i < Tag::MAX)
            .map(Tag)
    }

    fn into_value(self) -> Value {
        Value::Int(i64::from(self.0))
    }
}

schema_enum! {
    /// The default tags, used when a package derives no `GameTag` enum.
    GameTag = "viso::game::GameTag" {
        Player = "player", Enemy = "enemy", Ally = "ally", Coin = "coin",
        Pickup = "pickup", Hazard = "hazard", Goal = "goal",
        Projectile = "projectile", Platform = "platform", Trigger = "trigger",
    }
}

/// The derive that makes a unit-only enum the package's tags.
pub const GAME_TAG_DERIVE: &str = "GameTag";

/// What a body is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum BodyKind {
    /// A moving body that walks, jumps, falls and stands on blocks.
    #[default]
    Character,
    /// A solid that stops characters and never moves by itself.
    Block,
    /// A volume that stops nothing and reports characters entering it.
    Sensor,
}

impl BodyKind {
    fn from_index(index: i64) -> Option<BodyKind> {
        [BodyKind::Character, BodyKind::Block, BodyKind::Sensor]
            .get(usize::try_from(index).ok()?)
            .copied()
    }
}

/// What to spawn: a body's kind, place, half extents and tags.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpawnDesc {
    kind: BodyKind,
    at: [f32; 3],
    half: [f32; 3],
    tags: u64,
}

impl SpawnDesc {
    /// The full path of its native value type.
    pub const PATH: &'static str = "viso::game::SpawnDesc";

    /// A body of `kind` and full `size` at the origin, without tags; a
    /// negative size counts as its magnitude.
    pub fn new(kind: BodyKind, size: Vec3F32) -> SpawnDesc {
        let half = |v: f32| v.abs() * 0.5;
        SpawnDesc {
            kind,
            at: [0.0; 3],
            half: [half(size.x), half(size.y), half(size.z)],
            tags: 0,
        }
    }

    /// The player-sized character: 0.8 × 1.8 × 0.8.
    pub fn player() -> SpawnDesc {
        SpawnDesc::new(BodyKind::Character, Vec3F32::new(0.8, 1.8, 0.8))
    }

    /// Placed with its centre at `at`.
    pub fn at(self, at: Vec3F32) -> SpawnDesc {
        SpawnDesc {
            at: at.to_array(),
            ..self
        }
    }

    /// Carrying `tag` as well.
    pub fn tag(self, tag: Tag) -> SpawnDesc {
        SpawnDesc {
            tags: self.tags | tag.bit(),
            ..self
        }
    }
}

impl NativeValue for SpawnDesc {
    const TY: SchemaTy = SchemaTy::Value(SpawnDesc::PATH);

    fn from_value(value: &Value) -> Option<SpawnDesc> {
        let Value::Agg(agg) = value else {
            return None;
        };
        let [kind, fields @ .., tags] = &agg.fields[..] else {
            return None;
        };
        let [ax, ay, az, hx, hy, hz] = fields else {
            return None;
        };
        let f = |v: &Value| v.as_float().map(|v| v as f32);
        Some(SpawnDesc {
            kind: BodyKind::from_index(kind.as_int()?)?,
            at: [f(ax)?, f(ay)?, f(az)?],
            half: [f(hx)?, f(hy)?, f(hz)?],
            tags: tags.as_int()? as u64,
        })
    }

    fn into_value(self) -> Value {
        let f = |v: f32| Value::Float(f64::from(v));
        let [ax, ay, az] = self.at;
        let [hx, hy, hz] = self.half;
        Value::Agg(Rc::new(Aggregate {
            tag: 0,
            fields: Box::new([
                Value::Int(self.kind as i64),
                f(ax),
                f(ay),
                f(az),
                f(hx),
                f(hy),
                f(hz),
                Value::Int(self.tags as i64),
            ]),
        }))
    }
}

/// The committed state of a world, column by slot.
#[derive(Debug, Clone, PartialEq, Default)]
pub(super) struct Bodies {
    pub(super) generation: Vec<u32>,
    pub(super) alive: Vec<bool>,
    pub(super) kind: Vec<BodyKind>,
    pub(super) tags: Vec<u64>,
    pub(super) half: Vec<[f32; 3]>,
    pub(super) pos: Vec<[f32; 3]>,
    pub(super) prev: Vec<[f32; 3]>,
    pub(super) vel: Vec<[f32; 3]>,
    /// The committed `walk` (x, z) and `jump` (y) the next step consumes.
    pub(super) push: Vec<[f32; 3]>,
    pub(super) floor: Vec<bool>,
    /// Free slots; the last is reused first.
    pub(super) free: Vec<u32>,
    /// Live slots in allocation order.
    pub(super) order: Vec<u32>,
    /// The pairs in contact after the last step, ascending.
    pub(super) contacts: Vec<(EntityId, EntityId)>,
}

impl Bodies {
    fn slots(&self) -> usize {
        self.generation.len()
    }

    /// The slot of `id` if it names a live entity.
    fn live(&self, id: EntityId) -> Option<usize> {
        let slot = id.index as usize;
        (slot < self.slots() && self.alive[slot] && self.generation[slot] == id.generation)
            .then_some(slot)
    }

    fn id(&self, slot: usize) -> EntityId {
        EntityId {
            index: slot as u32,
            generation: self.generation[slot],
        }
    }

    /// Whether the slots overlap by more than nothing on `axis` and more than
    /// [`SKIN`] on the others.
    fn overlap(&self, a: usize, b: usize, axis: usize) -> bool {
        (0..3).all(|i| {
            let reach = self.half[a][i] + self.half[b][i];
            let reach = if i == axis { reach } else { reach - SKIN };
            (self.pos[a][i] - self.pos[b][i]).abs() < reach
        })
    }

    /// Whether the slots overlap at all.
    fn touch(&self, a: usize, b: usize) -> bool {
        (0..3).all(|i| (self.pos[a][i] - self.pos[b][i]).abs() < self.half[a][i] + self.half[b][i])
    }

    /// Checks the columns agree: equal lengths, live slots in order exactly
    /// once and free slots exactly once, every other slot neither.
    pub(super) fn is_consistent(&self) -> bool {
        let n = self.slots();
        let lengths = [
            self.alive.len(),
            self.kind.len(),
            self.tags.len(),
            self.half.len(),
            self.pos.len(),
            self.prev.len(),
            self.vel.len(),
            self.push.len(),
            self.floor.len(),
        ];
        if lengths.iter().any(|&l| l != n) {
            return false;
        }
        let mut seen = vec![false; n];
        for &slot in self.order.iter().chain(&self.free) {
            let slot = slot as usize;
            if slot >= n || seen[slot] {
                return false;
            }
            seen[slot] = true;
        }
        seen.iter().all(|&s| s)
            && self.order.iter().all(|&s| self.alive[s as usize])
            && self.free.iter().all(|&s| !self.alive[s as usize])
            && self.contacts.windows(2).all(|w| w[0] < w[1])
    }
}

impl Bodies {
    /// Writes the bodies canonically: slot by slot, floats by their bits.
    pub(super) fn encode(&self, enc: &mut Encoder) {
        let f = |enc: &mut Encoder, v: &[f32; 3]| {
            for c in v {
                enc.write_u32(c.to_bits());
            }
        };
        enc.write_varint(self.slots() as u64);
        for slot in 0..self.slots() {
            enc.write_varint(u64::from(self.generation[slot]));
            enc.write_bool(self.alive[slot]);
            enc.write_u8(self.kind[slot] as u8);
            enc.write_u64(self.tags[slot]);
            f(enc, &self.half[slot]);
            f(enc, &self.pos[slot]);
            f(enc, &self.prev[slot]);
            f(enc, &self.vel[slot]);
            f(enc, &self.push[slot]);
            enc.write_bool(self.floor[slot]);
        }
        let id = |enc: &mut Encoder, id: &EntityId| {
            enc.write_varint(u64::from(id.index));
            enc.write_varint(u64::from(id.generation));
        };
        write_list(enc, &self.free, |enc, &s| enc.write_varint(u64::from(s)));
        write_list(enc, &self.order, |enc, &s| enc.write_varint(u64::from(s)));
        write_list(enc, &self.contacts, |enc, (a, b)| {
            id(enc, a);
            id(enc, b);
        });
    }

    /// Reads bodies [`Bodies::encode`] wrote, checking they agree.
    pub(super) fn decode(dec: &mut Decoder<'_>) -> Result<Bodies, DecodeError> {
        fn f(dec: &mut Decoder<'_>) -> Result<[f32; 3], DecodeError> {
            let mut v = [0.0; 3];
            for c in &mut v {
                *c = f32::from_bits(dec.read_u32()?);
            }
            Ok(v)
        }
        fn id(dec: &mut Decoder<'_>) -> Result<EntityId, DecodeError> {
            Ok(EntityId {
                index: read_u32_varint(dec)?,
                generation: read_u32_varint(dec)?,
            })
        }
        let mut b = Bodies::default();
        for _ in 0..dec.read_varint()? {
            b.generation.push(read_u32_varint(dec)?);
            b.alive.push(dec.read_bool()?);
            let kind = BodyKind::from_index(i64::from(dec.read_u8()?));
            b.kind.push(kind.ok_or_else(|| malformed(dec))?);
            b.tags.push(dec.read_u64()?);
            b.half.push(f(dec)?);
            b.pos.push(f(dec)?);
            b.prev.push(f(dec)?);
            b.vel.push(f(dec)?);
            b.push.push(f(dec)?);
            b.floor.push(dec.read_bool()?);
        }
        b.free = read_list(dec, read_u32_varint)?;
        b.order = read_list(dec, read_u32_varint)?;
        b.contacts = read_list(dec, |dec| Ok((id(dec)?, id(dec)?)))?;
        if !b.is_consistent() {
            return Err(malformed(dec));
        }
        Ok(b)
    }
}

/// A buffered world command.
#[derive(Debug, Clone, Copy)]
pub(super) enum Command {
    Spawn(EntityId, SpawnDesc),
    Walk(EntityId, f32, f32),
    Jump(EntityId, f32),
    Teleport(EntityId, [f32; 3]),
    Remove(EntityId),
}

/// Where a hook's commands start, to discard them when it faults.
#[derive(Debug, Clone, Copy)]
pub(super) struct Mark {
    commands: usize,
    reserved: u32,
    rng: u64,
}

/// A live entity as the frame draws it: [`GameWorld::extract`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Extracted {
    /// The entity.
    pub id: EntityId,
    /// Its body.
    pub kind: BodyKind,
    /// Its tags, one bit per tag variant.
    pub tags: u64,
    /// Its centre, interpolated between the last two ticks.
    pub position: Vec3F32,
    /// Its half extents.
    pub half_extents: Vec3F32,
}

/// A game world, behind a `viso::game::GameWorld` handle.
#[derive(Debug, Default)]
pub struct GameWorld {
    bodies: RefCell<Rc<Bodies>>,
    commands: RefCell<Vec<(usize, Command)>>,
    /// Slots `spawn` reserved since the last commit.
    reserved: Cell<u32>,
    rng: Cell<u64>,
    /// The system whose hook is running, while one may write.
    writer: Cell<Option<usize>>,
    skipped: Cell<u64>,
    /// Scratch for a step's contacts, by allocation order and by identity.
    pairs: RefCell<Vec<(usize, usize)>>,
    contacts: RefCell<Vec<(EntityId, EntityId)>>,
}

impl NativeObject for GameWorld {
    const PATH: &'static str = "viso::game::GameWorld";
}

impl GameWorld {
    /// An empty world drawing random numbers from `seed`.
    pub fn new(seed: u64) -> GameWorld {
        GameWorld {
            rng: Cell::new(seed),
            ..GameWorld::default()
        }
    }

    /// Whether `id` names a live entity.
    pub fn is_alive(&self, id: EntityId) -> bool {
        self.bodies.borrow().live(id).is_some()
    }

    /// The position of `id` at the last tick, if it is alive.
    pub fn position(&self, id: EntityId) -> Option<Vec3F32> {
        let bodies = self.bodies.borrow();
        bodies
            .live(id)
            .map(|slot| Vec3F32::from_array(bodies.pos[slot]))
    }

    /// The position of `id` drawn `alpha` of the way from the previous tick
    /// to the last, if it is alive.
    pub fn interpolated(&self, id: EntityId, alpha: f32) -> Option<Vec3F32> {
        let bodies = self.bodies.borrow();
        bodies.live(id).map(|slot| {
            Vec3F32::from_array(bodies.prev[slot])
                .lerp(Vec3F32::from_array(bodies.pos[slot]), alpha)
        })
    }

    /// The live entities in allocation order.
    pub fn entities(&self) -> Vec<EntityId> {
        let bodies = self.bodies.borrow();
        bodies
            .order
            .iter()
            .map(|&s| bodies.id(s as usize))
            .collect()
    }

    /// Every live entity, in allocation order, as drawn `alpha` of the way
    /// from the previous tick to the last, into `out`, which it clears.
    pub fn extract(&self, alpha: f32, out: &mut Vec<Extracted>) {
        out.clear();
        let bodies = self.bodies.borrow();
        out.extend(bodies.order.iter().map(|&slot| {
            let slot = slot as usize;
            Extracted {
                id: bodies.id(slot),
                kind: bodies.kind[slot],
                tags: bodies.tags[slot],
                position: Vec3F32::from_array(bodies.prev[slot])
                    .lerp(Vec3F32::from_array(bodies.pos[slot]), alpha),
                half_extents: Vec3F32::from_array(bodies.half[slot]),
            }
        }));
    }

    /// Commands skipped because the entity they named was not alive at their
    /// commit.
    pub fn skipped_commands(&self) -> u64 {
        self.skipped.get()
    }

    /// Lets `system`'s hook write.
    pub(super) fn open(&self, system: usize) {
        self.writer.set(Some(system));
    }

    /// Ends the running hook's writes.
    pub(super) fn close(&self) {
        self.writer.set(None);
    }

    pub(super) fn mark(&self) -> Mark {
        Mark {
            commands: self.commands.borrow().len(),
            reserved: self.reserved.get(),
            rng: self.rng.get(),
        }
    }

    /// Discards the commands and draws since `mark`.
    pub(super) fn rollback(&self, mark: Mark) {
        self.commands.borrow_mut().truncate(mark.commands);
        self.reserved.set(mark.reserved);
        self.rng.set(mark.rng);
    }

    pub(super) fn rng(&self) -> u64 {
        self.rng.get()
    }

    /// The committed state, shared.
    pub(super) fn bodies(&self) -> Rc<Bodies> {
        self.bodies.borrow().clone()
    }

    /// Replaces the committed state and the random state, dropping any
    /// buffered command.
    pub(super) fn restore(&self, bodies: Rc<Bodies>, rng: u64) {
        *self.bodies.borrow_mut() = bodies;
        self.rng.set(rng);
        self.commands.borrow_mut().clear();
        self.reserved.set(0);
    }

    fn writer(&self) -> Result<usize, NativeError> {
        self.writer
            .get()
            .ok_or_else(|| NativeError::new("the world changes only in a Simulation hook"))
    }

    pub(super) fn push(&self, command: Command) -> Result<(), NativeError> {
        let system = self.writer()?;
        self.commands.borrow_mut().push((system, command));
        Ok(())
    }

    /// Reserves the next free slot and buffers its spawn.
    pub(super) fn spawn(&self, desc: SpawnDesc) -> Result<EntityId, NativeError> {
        let system = self.writer()?;
        let bodies = self.bodies.borrow();
        let k = self.reserved.get();
        let free = &bodies.free;
        let id = match free.len().checked_sub(1 + k as usize) {
            Some(i) => {
                let slot = free[i];
                EntityId {
                    index: slot,
                    generation: bodies.generation[slot as usize].wrapping_add(1),
                }
            }
            None => {
                let index = u32::try_from(bodies.slots() + k as usize - free.len())
                    .map_err(|_| NativeError::new("the world has no free entity slot"))?;
                EntityId {
                    index,
                    generation: 0,
                }
            }
        };
        self.reserved.set(k + 1);
        self.commands
            .borrow_mut()
            .push((system, Command::Spawn(id, desc)));
        Ok(id)
    }

    /// The next random 64 bits (SplitMix64).
    fn next(&self) -> Result<u64, NativeError> {
        self.writer()?;
        let state = self.rng.get().wrapping_add(0x9e37_79b9_7f4a_7c15);
        self.rng.set(state);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        Ok(z ^ (z >> 31))
    }

    /// A uniform `F64` in `[0, 1)`.
    pub(super) fn random(&self) -> Result<f64, NativeError> {
        Ok((self.next()? >> 11) as f64 * (1.0 / (1u64 << 53) as f64))
    }

    /// A uniform integer in `[low, high)`, without bias.
    pub(super) fn random_range(&self, low: i64, high: i64) -> Result<i64, NativeError> {
        if low >= high {
            return Err(NativeError::new(format!(
                "the range {low}..{high} is empty"
            )));
        }
        let span = (i128::from(high) - i128::from(low)) as u64;
        let floor = span.wrapping_neg() % span;
        loop {
            let wide = u128::from(self.next()?) * u128::from(span);
            if (wide as u64) >= floor {
                return Ok((i128::from(low) + (wide >> 64) as i128) as i64);
            }
        }
    }

    /// Starts a tick: every body's previous position becomes its current one.
    pub(super) fn begin_tick(&self) {
        let mut bodies = self.bodies.borrow_mut();
        if bodies.prev != bodies.pos {
            let bodies = Rc::make_mut(&mut bodies);
            bodies.prev.copy_from_slice(&bodies.pos);
        }
    }

    /// Commits the buffered commands in `(system, sequence)` order.
    pub(super) fn commit(&self) {
        let mut commands = self.commands.borrow_mut();
        let reserved = self.reserved.replace(0);
        if commands.is_empty() {
            return;
        }
        // A stable sort keeps each system's issue order.
        commands.sort_by_key(|&(system, _)| system);
        let mut bodies = self.bodies.borrow_mut();
        let b = Rc::make_mut(&mut bodies);
        for _ in 0..reserved {
            match b.free.pop() {
                Some(slot) => {
                    let slot = slot as usize;
                    b.generation[slot] = b.generation[slot].wrapping_add(1);
                }
                None => {
                    b.generation.push(0);
                    b.alive.push(false);
                    b.kind.push(BodyKind::Character);
                    b.tags.push(0);
                    b.half.push([0.0; 3]);
                    b.pos.push([0.0; 3]);
                    b.prev.push([0.0; 3]);
                    b.vel.push([0.0; 3]);
                    b.push.push([0.0; 3]);
                    b.floor.push(false);
                }
            }
        }
        let mut removed = false;
        let mut skipped = 0;
        for &(_, command) in commands.iter() {
            let target = match command {
                Command::Spawn(id, desc) => {
                    let slot = id.index as usize;
                    debug_assert_eq!(b.generation[slot], id.generation);
                    b.alive[slot] = true;
                    b.kind[slot] = desc.kind;
                    b.tags[slot] = desc.tags;
                    b.half[slot] = desc.half;
                    b.pos[slot] = desc.at;
                    b.prev[slot] = desc.at;
                    b.vel[slot] = [0.0; 3];
                    b.push[slot] = [0.0; 3];
                    b.floor[slot] = false;
                    b.order.push(id.index);
                    continue;
                }
                Command::Walk(id, ..)
                | Command::Jump(id, _)
                | Command::Teleport(id, _)
                | Command::Remove(id) => id,
            };
            let Some(slot) = b.live(target) else {
                skipped += 1;
                continue;
            };
            match command {
                Command::Spawn(..) => unreachable!("spawns commit above"),
                Command::Walk(_, x, z) => {
                    b.push[slot][0] += x;
                    b.push[slot][2] += z;
                }
                Command::Jump(_, speed) => b.push[slot][1] += speed,
                Command::Teleport(_, to) => {
                    b.pos[slot] = to;
                    b.prev[slot] = to;
                    b.vel[slot] = [0.0; 3];
                    b.floor[slot] = false;
                }
                Command::Remove(_) => {
                    b.alive[slot] = false;
                    b.free.push(target.index);
                    removed = true;
                }
            }
        }
        if removed {
            let Bodies { order, alive, .. } = b;
            order.retain(|&s| alive[s as usize]);
        }
        commands.clear();
        self.skipped.set(self.skipped.get() + skipped);
    }

    /// Steps the bodies `dt` seconds and appends the contacts that began to
    /// `began`, earlier allocation first in each pair and pairs in allocation
    /// order.
    pub(super) fn step(&self, dt: f32, began: &mut Vec<(EntityId, EntityId)>) {
        let mut bodies = self.bodies.borrow_mut();
        if bodies.order.is_empty() {
            return;
        }
        let b = Rc::make_mut(&mut bodies);
        let order = std::mem::take(&mut b.order);
        for &slot in &order {
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
            for axis in [1, 0, 2] {
                let speed = b.vel[slot][axis];
                if speed == 0.0 {
                    continue;
                }
                b.pos[slot][axis] += speed * dt;
                for &other in &order {
                    let other = other as usize;
                    if b.kind[other] != BodyKind::Block || !b.overlap(slot, other, axis) {
                        continue;
                    }
                    let reach = b.half[slot][axis] + b.half[other][axis];
                    if speed > 0.0 {
                        b.pos[slot][axis] = b.pos[other][axis] - reach;
                    } else {
                        b.pos[slot][axis] = b.pos[other][axis] + reach;
                        if axis == 1 {
                            b.floor[slot] = true;
                        }
                    }
                    b.vel[slot][axis] = 0.0;
                }
            }
        }
        let mut now = self.pairs.borrow_mut();
        now.clear();
        for (i, &a) in order.iter().enumerate() {
            let a = a as usize;
            if b.kind[a] != BodyKind::Character {
                continue;
            }
            for (j, &o) in order.iter().enumerate() {
                let o = o as usize;
                let pairs = match b.kind[o] {
                    BodyKind::Sensor => true,
                    BodyKind::Character => j > i,
                    BodyKind::Block => false,
                };
                if pairs && b.touch(a, o) {
                    now.push((i.min(j), i.max(j)));
                }
            }
        }
        now.sort_unstable();
        let mut contacts = self.contacts.borrow_mut();
        contacts.clear();
        contacts.extend(
            now.iter()
                .map(|&(i, j)| (b.id(order[i] as usize), b.id(order[j] as usize))),
        );
        for &pair in contacts.iter() {
            if b.contacts.binary_search(&pair).is_err() {
                began.push(pair);
            }
        }
        contacts.sort_unstable();
        std::mem::swap(&mut b.contacts, &mut *contacts);
        b.order = order;
    }
}

fn dead(id: EntityId) -> NativeError {
    NativeError::new(format!("entity {id} is not alive"))
}

fn live(this: &GameWorld, id: EntityId) -> Result<usize, NativeError> {
    this.bodies.borrow().live(id).ok_or_else(|| dead(id))
}

/// A world native: it reads or writes the world, so it is not
/// `deterministic`, yet it reproduces on every target.
const fn world(f: NativeFunction) -> NativeFunction {
    f.reproducible(Determinism::CrossPlatform)
}

pub(super) static GAME_WORLD_METHODS: [NativeFunction; 14] = [
    world(
        crate::native!(fn "is_alive" |_cx, this: Obj<GameWorld>, id: EntityId| -> bool {
            Ok(this.is_alive(id))
        }),
    ),
    world(
        crate::native!(fn "position" |_cx, this: Obj<GameWorld>, id: EntityId| -> Vec3F32 {
            this.position(id).ok_or_else(|| dead(id))
        }),
    ),
    world(
        crate::native!(fn "velocity" |_cx, this: Obj<GameWorld>, id: EntityId| -> Vec3F32 {
            let slot = live(&this, id)?;
            Ok(Vec3F32::from_array(this.bodies.borrow().vel[slot]))
        }),
    ),
    world(
        crate::native!(fn "on_floor" |_cx, this: Obj<GameWorld>, id: EntityId| -> bool {
            let bodies = this.bodies.borrow();
            Ok(bodies.live(id).is_some_and(|slot| bodies.floor[slot]))
        }),
    ),
    world(
        crate::native!(fn "has_tag" |_cx, this: Obj<GameWorld>, id: EntityId, tag: Tag| -> bool {
            let bodies = this.bodies.borrow();
            Ok(bodies.live(id).is_some_and(|slot| bodies.tags[slot] & tag.bit() != 0))
        }),
    ),
    world(
        crate::native!(fn "query" |_cx, this: Obj<GameWorld>, tag: Tag| -> Vec<EntityId> {
            let bodies = this.bodies.borrow();
            Ok(bodies
                .order
                .iter()
                .map(|&s| s as usize)
                .filter(|&s| bodies.tags[s] & tag.bit() != 0)
                .map(|s| bodies.id(s))
                .collect())
        }),
    ),
    world(
        crate::native!(fn "entities" |_cx, this: Obj<GameWorld>| -> Vec<EntityId> {
            Ok(this.entities())
        }),
    ),
    world(
        crate::native!(action "spawn" |_cx, this: Obj<GameWorld>, desc: SpawnDesc| -> EntityId {
            this.spawn(desc)
        }),
    ),
    world(
        crate::native!(action "walk" |_cx, this: Obj<GameWorld>, id: EntityId, x: f64, z: f64| -> () {
            this.push(Command::Walk(id, x as f32, z as f32))
        }),
    ),
    world(
        crate::native!(action "jump" |_cx, this: Obj<GameWorld>, id: EntityId, speed: f64| -> () {
            this.push(Command::Jump(id, speed as f32))
        }),
    ),
    world(
        crate::native!(action "teleport" |_cx, this: Obj<GameWorld>, id: EntityId, to: Vec3F32| -> () {
            this.push(Command::Teleport(id, to.to_array()))
        }),
    ),
    world(
        crate::native!(action "remove" |_cx, this: Obj<GameWorld>, id: EntityId| -> () {
            this.push(Command::Remove(id))
        }),
    ),
    world(
        crate::native!(action "random" |_cx, this: Obj<GameWorld>| -> f64 {
            this.random()
        }),
    ),
    world(
        crate::native!(action "random_range" |_cx, this: Obj<GameWorld>, low: i64, high: i64| -> i64 {
            this.random_range(low, high)
        }),
    ),
];

pub(super) static SPAWN_DESC_METHODS: [NativeFunction; 6] = [
    crate::native!(fn "player" |_cx| -> SpawnDesc { Ok(SpawnDesc::player()) }).constant(),
    crate::native!(fn "character" |_cx, size: Vec3F32| -> SpawnDesc {
        Ok(SpawnDesc::new(BodyKind::Character, size))
    })
    .constant(),
    crate::native!(fn "block" |_cx, size: Vec3F32| -> SpawnDesc {
        Ok(SpawnDesc::new(BodyKind::Block, size))
    })
    .constant(),
    crate::native!(fn "sensor" |_cx, size: Vec3F32| -> SpawnDesc {
        Ok(SpawnDesc::new(BodyKind::Sensor, size))
    })
    .constant(),
    crate::native!(fn "at" |_cx, this: SpawnDesc, at: Vec3F32| -> SpawnDesc { Ok(this.at(at)) })
        .deterministic(),
    crate::native!(fn "tag" |_cx, this: SpawnDesc, tag: Tag| -> SpawnDesc { Ok(this.tag(tag)) })
        .deterministic(),
];
