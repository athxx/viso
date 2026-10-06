//! Game snapshots: the world, the random state and the Simulation state of
//! every system at a tick boundary.
//!
//! A [`GameSnapshot`] holds the values themselves, which are immutable and
//! shared, the world's committed state among them, so taking one in memory
//! (for rollback or rewind debugging) copies no state, only references.
//! [`GameSnapshot::encode`] writes it as a canonical Ende blob (the world
//! slot by slot, the physics engine's own state, systems and states in
//! stable-identity order, floats by their bits) for saves, tapes and
//! snapshot hashes. A snapshot of an engine that cannot save its state is
//! degraded: it restores the bodies, and the engine starts afresh over them.

use std::rc::Rc;

use viso_ende::{Decode, DecodeError, Decoder, Encode, Encoder, ProtocolTag};

use super::world::Bodies;

use crate::module::StableId;
use crate::value::Value;
use crate::wire::{
    malformed, read_list, read_stable_id, read_value, write_list, write_stable_id, write_value,
};

/// The Simulation state of a game at a tick boundary.
///
/// Systems and their states are keyed by stable identity, each state with
/// the hash of its type's schema, so a snapshot taken by one build restores
/// into another that kept them, as across a logic-only reload. The world and
/// the random state restore whole.
#[derive(Debug, Clone, PartialEq)]
pub struct GameSnapshot {
    pub(super) build: u64,
    pub(super) tick: u64,
    pub(super) rng: u64,
    pub(super) world: Rc<Bodies>,
    /// What the physics engine keeps between steps; `None` when it cannot
    /// save it, which makes the snapshot degraded.
    pub(super) physics: Option<Rc<[u8]>>,
    pub(super) systems: Box<[SystemState]>,
}

/// One system's Simulation states, by ascending identity.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SystemState {
    pub(super) id: StableId,
    pub(super) states: Box<[(StableId, u64, Value)]>,
}

/// What [`Scheduler::restore`](super::Scheduler::restore) did with a
/// snapshot's states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Restored {
    /// Simulation states set from the snapshot.
    pub states: u32,
    /// Simulation states the snapshot holds under another schema, which kept
    /// their current value.
    pub mismatched: u32,
    /// Simulation states the snapshot does not hold, which kept their current
    /// value.
    pub missing: u32,
    /// `@local` states a reload carried; a restore keeps them all.
    pub locals: u32,
    /// Whether the physics engine's state was not restored, the snapshot
    /// being degraded or the state not loading: the engine started afresh
    /// over the restored bodies, so the ticks that follow may differ from
    /// the run the snapshot was taken in.
    pub degraded: bool,
}

/// The tag a snapshot blob starts with after its protocol header.
const MAGIC: [u8; 4] = *b"GSN3";

impl GameSnapshot {
    /// A hash of the build that took it.
    pub fn build(&self) -> u64 {
        self.build
    }

    /// The tick it was taken before.
    pub fn tick(&self) -> u64 {
        self.tick
    }

    /// The state of its random source.
    pub fn rng_state(&self) -> u64 {
        self.rng
    }

    /// The number of live entities in its world.
    pub fn entities(&self) -> usize {
        self.world.order.len()
    }

    /// Whether it lacks the physics engine's state, the engine being one that
    /// cannot save it: restoring it is then only approximate.
    pub fn degraded(&self) -> bool {
        self.physics.is_none()
    }

    /// The number of Simulation states it holds.
    pub fn states(&self) -> usize {
        self.systems.iter().map(|s| s.states.len()).sum()
    }

    /// The snapshot as a canonical blob [`GameSnapshot::decode`] reads back.
    pub fn encode(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        ProtocolTag::current().encode(&mut enc);
        enc.write_raw(&MAGIC);
        enc.write_u64(self.build);
        enc.write_varint(self.tick);
        enc.write_u64(self.rng);
        self.world.encode(&mut enc);
        enc.write_bool(self.physics.is_some());
        if let Some(physics) = &self.physics {
            enc.write_bytes(physics);
        }
        write_list(&mut enc, &self.systems, |enc, system| {
            write_stable_id(enc, system.id);
            write_list(enc, &system.states, |enc, (id, schema, value)| {
                write_stable_id(enc, *id);
                enc.write_u64(*schema);
                debug_assert!(
                    !matches!(value, Value::Handle(_) | Value::Closure(_)),
                    "a Simulation state holds plain data"
                );
                write_value(enc, value);
            });
        });
        enc.into_bytes()
    }

    /// Reads a blob [`GameSnapshot::encode`] wrote.
    ///
    /// # Errors
    ///
    /// A [`DecodeError`] when the bytes are malformed, from another wire
    /// version, or not a snapshot.
    pub fn decode(bytes: &[u8]) -> Result<GameSnapshot, DecodeError> {
        let mut dec = Decoder::new(bytes);
        let offset = dec.position();
        if !ProtocolTag::decode(&mut dec)?.is_compatible() || dec.read_raw(MAGIC.len())? != MAGIC {
            return Err(DecodeError::Malformed { offset });
        }
        let build = dec.read_u64()?;
        let tick = dec.read_varint()?;
        let rng = dec.read_u64()?;
        let world = Rc::new(Bodies::decode(&mut dec)?);
        let physics = if dec.read_bool()? {
            Some(Rc::from(dec.read_bytes()?))
        } else {
            None
        };
        let systems = read_list(&mut dec, |dec| {
            let id = read_stable_id(dec)?;
            let states = read_list(dec, |dec| {
                Ok((read_stable_id(dec)?, dec.read_u64()?, read_value(dec)?))
            })?;
            if states.windows(2).any(|w| w[0].0 >= w[1].0) {
                return Err(malformed(dec));
            }
            Ok(SystemState {
                id,
                states: states.into(),
            })
        })?;
        if systems.windows(2).any(|w| w[0].id >= w[1].id) {
            return Err(malformed(&dec));
        }
        dec.finish()?;
        Ok(GameSnapshot {
            build,
            tick,
            rng,
            world,
            physics,
            systems: systems.into(),
        })
    }

    /// The snapshot hash: 64-bit FNV-1a of its canonical blob. Two runs of
    /// one build agree on it exactly when their worlds, random states and
    /// Simulation states are bit for bit equal.
    pub fn hash(&self) -> u64 {
        fnv(&self.encode())
    }
}

/// 64-bit FNV-1a.
pub(super) fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}
