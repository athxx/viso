//! Rollback netcode on the Simulation tier: every peer runs the whole game,
//! each feeding its own player's input and predicting the others', and
//! re-simulates from the last snapshot that is still right when a remote
//! input turns out to differ from its prediction.
//!
//! A [`RollbackSession`] wraps a [`Scheduler`]. Each tick it samples the
//! local device input for the tick `input_delay` ahead, runs the tick with
//! every player's input it knows and a prediction for the rest (the latest
//! known input of that player, still held, without edges), and keeps the
//! snapshot before it. Peers exchange [`packets`](RollbackSession::packet_for)
//! over any transport, lossy and reordering included: each packet repeats
//! every local input the peer has not acknowledged, acknowledges the peer's
//! inputs received, and carries recent confirmed snapshot hashes. A tick is
//! confirmed once every player's input of it is known; the snapshot at the
//! confirmed boundary is the last confirmed snapshot, and a peer whose hash of
//! a confirmed tick differs is a [`Desync`]. The game stalls rather than
//! predict more than `max_prediction` ticks past it.
//!
//! Re-simulation restores a snapshot and runs the ticks again on the same
//! scheduler, so a Presentation command of a tick already delivered is not
//! delivered again; only Simulation state is rolled back.

use std::collections::VecDeque;
use std::fmt;

use viso_ende::{Decode, DecodeError, Decoder, Encode, Encoder, ProtocolTag};

use super::{GameSnapshot, MAX_PLAYERS, Scheduler, TickInput};
use crate::wire::{malformed, read_list, write_list};

/// How a [`RollbackSession`] plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionConfig {
    /// The players in the game, every peer one of them.
    pub players: u32,
    /// The player this peer is.
    pub local: u32,
    /// The ticks a local input waits before its tick runs: the time it has
    /// to reach the other peers before they would predict it.
    pub input_delay: u32,
    /// The most ticks the game runs past the last confirmed tick before it
    /// stalls for the remote inputs.
    pub max_prediction: u32,
    /// Every how many ticks a confirmed snapshot's hash is exchanged.
    pub hash_interval: u32,
}

impl SessionConfig {
    /// `players` players, this peer `local`, two ticks of input delay, at
    /// most eight ticks of prediction, a hash every eight ticks.
    pub fn new(players: u32, local: u32) -> SessionConfig {
        SessionConfig {
            players,
            local,
            input_delay: 2,
            max_prediction: 8,
            hash_interval: 8,
        }
    }
}

/// Why a session could not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// Fewer than 2 or more than [`MAX_PLAYERS`] players, or no tick of
    /// prediction or hash interval.
    Config(String),
    /// The game's input schema has more actions than a [`TickInput`] holds.
    Actions(usize),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::Config(message) => f.write_str(message),
            SessionError::Actions(n) => write!(
                f,
                "the game's input schema has {n} actions; a rollback session sends at most 64"
            ),
        }
    }
}

impl std::error::Error for SessionError {}

/// Why a packet was not taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PacketError {
    /// The bytes are not a packet.
    Decode(DecodeError),
    /// The packet came from a peer running another build.
    Build {
        /// This peer's build.
        expected: u64,
        /// The sender's.
        found: u64,
    },
    /// The sender is not a remote player of this game.
    Peer(u32),
}

impl fmt::Display for PacketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PacketError::Decode(error) => write!(f, "not a session packet: {error}"),
            PacketError::Build { expected, found } => write!(
                f,
                "the packet is from build {found:#018x}, not this game's {expected:#018x}"
            ),
            PacketError::Peer(peer) => write!(f, "player {peer} is no remote player of this game"),
        }
    }
}

impl std::error::Error for PacketError {}

impl From<DecodeError> for PacketError {
    fn from(error: DecodeError) -> PacketError {
        PacketError::Decode(error)
    }
}

/// A confirmed tick whose snapshot hash differs between this peer and
/// another: the two games diverged by then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Desync {
    /// The tick boundary the hashes are of.
    pub tick: u64,
    /// The other peer.
    pub peer: u32,
    /// This peer's hash.
    pub local: u64,
    /// The other peer's.
    pub remote: u64,
}

/// What a session did so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionStats {
    /// Rollbacks: restores to an earlier tick.
    pub rollbacks: u64,
    /// Ticks run again after a rollback.
    pub resimulated: u64,
    /// Ticks owed that waited because the game ran `max_prediction` past
    /// the last confirmed tick.
    pub stalls: u64,
    /// Player-ticks run on a predicted input.
    pub predicted: u64,
}

/// One player's inputs known, a contiguous run of ticks.
#[derive(Debug, Clone)]
struct Track {
    /// The tick of `inputs[0]`.
    start: u64,
    inputs: VecDeque<TickInput>,
    /// For a remote player: the first tick of the local input it lacks.
    acked: u64,
}

impl Track {
    /// The first tick whose input is not known.
    fn end(&self) -> u64 {
        self.start + self.inputs.len() as u64
    }

    fn get(&self, tick: u64) -> Option<TickInput> {
        let i = usize::try_from(tick.checked_sub(self.start)?).ok()?;
        self.inputs.get(i).copied()
    }

    /// The input of `tick`, or the prediction of it.
    fn best(&self, tick: u64) -> (TickInput, bool) {
        match self.get(tick) {
            Some(input) => (input, false),
            None => (
                self.inputs
                    .back()
                    .copied()
                    .map(TickInput::held_on)
                    .unwrap_or_default(),
                true,
            ),
        }
    }
}

/// The tag a packet starts with after its protocol header.
const MAGIC: [u8; 4] = *b"GNP1";
/// The most inputs one packet repeats.
const PACKET_INPUTS: usize = 128;
/// The confirmed hashes kept, and how many recent ones a packet carries.
const HASHES_KEPT: usize = 64;
const PACKET_HASHES: usize = 4;

/// A game played by several peers with rollback, this peer one player.
pub struct RollbackSession {
    game: Scheduler,
    config: SessionConfig,
    build: u64,
    tracks: Vec<Track>,
    /// The snapshot at each tick boundary from the last confirmed one up to
    /// the current tick, exclusive.
    saved: VecDeque<GameSnapshot>,
    /// The inputs each of those ticks ran on.
    used: VecDeque<Box<[TickInput]>>,
    /// The last confirmed tick boundary.
    confirmed: u64,
    owed: u64,
    /// This peer's confirmed hashes, by tick, ascending.
    hashes: VecDeque<(u64, u64)>,
    /// Other peers' hashes of ticks not confirmed here yet.
    pending: Vec<(u32, u64, u64)>,
    desyncs: Vec<Desync>,
    stats: SessionStats,
}

impl RollbackSession {
    /// A session over `game`, from its current tick on: every peer starts
    /// its own copy of the same build and seed at the same tick, the first
    /// `input_delay` ticks of every player empty.
    ///
    /// # Errors
    ///
    /// A [`SessionError`] for a configuration out of range or a game whose
    /// input schema has more than 64 actions.
    pub fn new(
        mut game: Scheduler,
        config: SessionConfig,
    ) -> Result<RollbackSession, SessionError> {
        if !(2..=MAX_PLAYERS).contains(&config.players) || config.local >= config.players {
            return Err(SessionError::Config(format!(
                "a session has 2 to {MAX_PLAYERS} players and is one of them, not player {} of {}",
                config.local, config.players
            )));
        }
        if config.max_prediction == 0 || config.hash_interval == 0 {
            return Err(SessionError::Config(
                "a session predicts at least one tick and exchanges hashes".to_owned(),
            ));
        }
        let actions = super::scheduler::actions(&game);
        if actions > 64 {
            return Err(SessionError::Actions(actions));
        }
        game.set_players(config.players);
        let start = game.clock().tick();
        let track = Track {
            start,
            inputs: vec![TickInput::default(); config.input_delay as usize].into(),
            acked: start,
        };
        Ok(RollbackSession {
            build: game.build(),
            game,
            tracks: vec![track; config.players as usize],
            saved: VecDeque::new(),
            used: VecDeque::new(),
            confirmed: start,
            owed: 0,
            hashes: VecDeque::new(),
            pending: Vec::new(),
            desyncs: Vec::new(),
            stats: SessionStats::default(),
            config,
        })
    }

    /// The game. Its device input is the local player's; the session runs
    /// its ticks, so the host does not step it.
    pub fn game(&self) -> &Scheduler {
        &self.game
    }

    /// The game, to report the local player's device input to.
    pub fn game_mut(&mut self) -> &mut Scheduler {
        &mut self.game
    }

    /// How it plays.
    pub fn config(&self) -> SessionConfig {
        self.config
    }

    /// The last tick boundary every player's input before which is known.
    pub fn confirmed_tick(&self) -> u64 {
        self.confirmed
    }

    /// The snapshot at the last confirmed tick boundary.
    pub fn confirmed_snapshot(&self) -> GameSnapshot {
        match self.saved.front() {
            Some(snapshot) => snapshot.clone(),
            None => self.game.snapshot(),
        }
    }

    /// This peer's hash of the confirmed tick boundary `tick`, while kept.
    pub fn confirmed_hash(&self, tick: u64) -> Option<u64> {
        self.hashes.iter().find(|h| h.0 == tick).map(|h| h.1)
    }

    /// The desyncs found so far.
    pub fn desyncs(&self) -> &[Desync] {
        &self.desyncs
    }

    /// What it did so far.
    pub fn stats(&self) -> SessionStats {
        self.stats
    }

    /// Runs one frame of `wall_dt` seconds: the ticks the clock owes, as
    /// many as prediction allows, then the presentation.
    pub fn frame(&mut self, wall_dt: f64) -> u32 {
        let owed = self.game.clock_mut().advance(wall_dt);
        let ran = self.advance(owed);
        self.game.present(wall_dt);
        ran
    }

    /// Owes `ticks` more ticks and runs as many owed as prediction allows;
    /// returns those run. What waits runs once inputs arrive, at most
    /// `max_prediction` ticks of it.
    pub fn advance(&mut self, ticks: u32) -> u32 {
        self.owed = (self.owed + u64::from(ticks)).min(u64::from(self.config.max_prediction));
        let mut ran = 0;
        while self.owed > 0 {
            self.confirm();
            if self.game.clock().tick() - self.confirmed >= u64::from(self.config.max_prediction) {
                self.stats.stalls += self.owed;
                break;
            }
            let input = self.game.sample_input();
            self.tracks[self.config.local as usize]
                .inputs
                .push_back(input);
            self.run_tick();
            self.owed -= 1;
            ran += 1;
        }
        self.confirm();
        ran
    }

    /// Runs the current tick on the best inputs known, keeping the snapshot
    /// before it and the inputs it ran on.
    fn run_tick(&mut self) {
        let tick = self.game.clock().tick();
        let inputs: Box<[TickInput]> = self
            .tracks
            .iter()
            .map(|track| {
                let (input, predicted) = track.best(tick);
                self.stats.predicted += u64::from(predicted);
                input
            })
            .collect();
        self.saved.push_back(self.game.snapshot());
        self.game.step_with(&inputs);
        self.used.push_back(inputs);
    }

    /// The packet for `peer`: every local input it has not acknowledged, the
    /// acknowledgement of its inputs, and recent confirmed hashes.
    ///
    /// # Panics
    ///
    /// If `peer` is this peer or no player of the game.
    pub fn packet_for(&self, peer: u32) -> Vec<u8> {
        assert!(
            peer != self.config.local && peer < self.config.players,
            "player {peer} is no remote player"
        );
        let local = &self.tracks[self.config.local as usize];
        let from = self.tracks[peer as usize].acked.max(local.start);
        let skip = (from - local.start) as usize;
        let inputs: Vec<TickInput> = local
            .inputs
            .iter()
            .skip(skip)
            .take(PACKET_INPUTS)
            .copied()
            .collect();
        let hashes: Vec<(u64, u64)> = self
            .hashes
            .iter()
            .rev()
            .take(PACKET_HASHES)
            .copied()
            .collect();
        let mut enc = Encoder::new();
        ProtocolTag::current().encode(&mut enc);
        enc.write_raw(&MAGIC);
        enc.write_u64(self.build);
        enc.write_varint(u64::from(self.config.local));
        enc.write_varint(self.tracks[peer as usize].end());
        enc.write_varint(from);
        write_list(&mut enc, &inputs, |enc, input| {
            enc.write_u64(input.held);
            enc.write_u64(input.pressed);
            enc.write_u64(input.released);
            enc.write_u64(input.axes[0]);
            enc.write_u64(input.axes[1]);
        });
        write_list(&mut enc, &hashes, |enc, (tick, hash)| {
            enc.write_varint(*tick);
            enc.write_u64(*hash);
        });
        enc.into_bytes()
    }

    /// Takes a packet from another peer: its new inputs, rolling back and
    /// re-simulating from the first tick one changes; its acknowledgement;
    /// and its hashes, checked against this peer's.
    ///
    /// # Errors
    ///
    /// A [`PacketError`] for bytes that are no packet of this game: the
    /// session is left as it was.
    pub fn receive(&mut self, bytes: &[u8]) -> Result<(), PacketError> {
        let mut dec = Decoder::new(bytes);
        let offset = dec.position();
        if !ProtocolTag::decode(&mut dec)?.is_compatible() || dec.read_raw(MAGIC.len())? != MAGIC {
            return Err(DecodeError::Malformed { offset }.into());
        }
        let build = dec.read_u64()?;
        let from = dec.read_varint()?;
        let ack = dec.read_varint()?;
        let first = dec.read_varint()?;
        let inputs = read_list(&mut dec, |dec| {
            Ok(TickInput {
                held: dec.read_u64()?,
                pressed: dec.read_u64()?,
                released: dec.read_u64()?,
                axes: [dec.read_u64()?, dec.read_u64()?],
            })
        })?;
        let hashes = read_list(&mut dec, |dec| Ok((dec.read_varint()?, dec.read_u64()?)))?;
        if inputs.len() > PACKET_INPUTS {
            return Err(malformed(&dec).into());
        }
        dec.finish()?;
        if build != self.build {
            return Err(PacketError::Build {
                expected: self.build,
                found: build,
            });
        }
        let peer = u32::try_from(from)
            .ok()
            .filter(|&p| p < self.config.players && p != self.config.local)
            .ok_or(PacketError::Peer(from.min(u64::from(u32::MAX)) as u32))?;
        let local_end = self.tracks[self.config.local as usize].end();
        let track = &mut self.tracks[peer as usize];
        track.acked = track.acked.max(ack.min(local_end));
        let known = track.end();
        for (i, input) in inputs.into_iter().enumerate() {
            let tick = first + i as u64;
            if tick == track.end() {
                track.inputs.push_back(input);
            }
        }
        if track.end() > known {
            self.reconcile(peer as usize, known);
        }
        for (tick, hash) in hashes {
            self.check_hash(peer, tick, hash);
        }
        self.confirm();
        Ok(())
    }

    /// Re-simulates from the first tick from `known` on that ran on an input
    /// of `player` other than the best known now.
    fn reconcile(&mut self, player: usize, known: u64) {
        let now = self.game.clock().tick();
        let track = &self.tracks[player];
        let wrong = (known.max(self.confirmed)..now).find(|&tick| {
            let used = &self.used[(tick - self.confirmed) as usize];
            used[player] != track.best(tick).0
        });
        let Some(tick) = wrong else {
            return;
        };
        let at = (tick - self.confirmed) as usize;
        let snapshot = self.saved[at].clone();
        self.saved.truncate(at);
        self.used.truncate(at);
        self.game.restore(&snapshot);
        self.stats.rollbacks += 1;
        for _ in tick..now {
            self.run_tick();
            self.stats.resimulated += 1;
        }
    }

    /// Moves the confirmed boundary up to the first tick some player's input
    /// of is unknown, hashing the confirmed boundaries on the interval.
    fn confirm(&mut self) {
        let now = self.game.clock().tick();
        let known = self.tracks.iter().map(Track::end).min().unwrap_or(now);
        let target = known.min(now);
        while self.confirmed < target {
            self.saved.pop_front();
            self.used.pop_front();
            self.confirmed += 1;
            if self.confirmed.is_multiple_of(u64::from(self.config.hash_interval)) {
                let hash = self.confirmed_snapshot().hash();
                self.hashes.push_back((self.confirmed, hash));
                if self.hashes.len() > HASHES_KEPT {
                    self.hashes.pop_front();
                }
                let tick = self.confirmed;
                let (due, rest): (Vec<_>, Vec<_>) =
                    self.pending.drain(..).partition(|p| p.1 == tick);
                self.pending = rest;
                for (peer, _, remote) in due {
                    self.compare(peer, tick, hash, remote);
                }
            }
        }
        // What re-simulation and the peers still need.
        let confirmed = self.confirmed;
        let local = self.config.local as usize;
        let acked = (0..self.tracks.len())
            .filter(|&p| p != local)
            .map(|p| self.tracks[p].acked)
            .min()
            .unwrap_or(confirmed);
        for (p, track) in self.tracks.iter_mut().enumerate() {
            let keep = if p == local {
                confirmed.min(acked)
            } else {
                confirmed
            };
            while track.start < keep && track.inputs.len() > 1 {
                track.inputs.pop_front();
                track.start += 1;
            }
        }
        self.pending.retain(|p| p.1 > confirmed);
    }

    /// Checks `peer`'s `hash` of `tick` against this peer's, now or once
    /// confirmed here.
    fn check_hash(&mut self, peer: u32, tick: u64, hash: u64) {
        if let Some(local) = self.confirmed_hash(tick) {
            self.compare(peer, tick, local, hash);
        } else if tick > self.confirmed && !self.pending.iter().any(|p| p.0 == peer && p.1 == tick)
        {
            self.pending.push((peer, tick, hash));
        }
    }

    fn compare(&mut self, peer: u32, tick: u64, local: u64, remote: u64) {
        if local != remote
            && !self
                .desyncs
                .iter()
                .any(|d| d.peer == peer && d.tick == tick)
        {
            self.desyncs.push(Desync {
                tick,
                peer,
                local,
                remote,
            });
        }
    }
}

impl fmt::Debug for RollbackSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RollbackSession")
            .field("config", &self.config)
            .field("tick", &self.game.clock().tick())
            .field("confirmed", &self.confirmed)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}
