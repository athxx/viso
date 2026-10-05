//! The input tape: what a game's ticks saw of its input, recorded or written
//! by hand, to replay a run tick for tick (Viso_DSL_1.0.md section 110.5).
//!
//! A tape holds actions and the move vector, never raw keys, so remapping an
//! `InputMap` leaves it valid. Its header names the run it reproduces: the
//! world's seed, the build hash, the determinism tier and the tick rate. Its
//! body is the input each tick sees, stored as the changes between
//! consecutive ticks: an action pressed (held, with a press edge), released
//! (let go, with a release edge) or set quietly (held or let go without an
//! edge, as a remap does), and the move vector set. Changes of one tick apply
//! in order before it runs, so a press and a release in one tick are a tap.
//!
//! The binary form is a versioned `viso-ende` blob. The text form is one
//! directive a line, `#` starting a comment:
//!
//! ```text
//! seed 1234
//! tick_rate 60
//! ticks 600
//! 30: press Jump
//! 31..90: axis move = (1, 0)
//! 95: tap Fire
//! 100..=110: press Crouch
//! ```
//!
//! A range `a..b` applies the change at tick `a` and undoes it at `b`
//! (`a..=b`: after `b`): a ranged press releases, a ranged move vector
//! returns to `(0, 0)`. Header lines are `seed`, `build`, `determinism`
//! (`same_binary` or `cross_platform`), `tick_rate` and `ticks`, the ticks
//! the tape covers (by default, through its last change).

use std::fmt::{self, Write as _};

use viso_ende::{Decode, DecodeError, Decoder, Encode, Encoder, ProtocolTag};

use crate::native::Determinism;
use crate::wire::{malformed, read_list, write_list};

/// One change of a tape's input, by the index of the action in
/// [`InputTape::actions`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TapeChange {
    /// The action goes down, with a press edge.
    Press(u32),
    /// The action goes up, with a release edge.
    Release(u32),
    /// The action is held or let go without an edge.
    Set(u32, bool),
    /// The move vector, until set again.
    Move(f64, f64),
}

/// A change and the tick it applies before.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TapeEvent {
    pub tick: u64,
    pub change: TapeChange,
}

/// A recorded or hand-written run's input.
#[derive(Debug, Clone, PartialEq)]
pub struct InputTape {
    /// The world's random seed.
    pub seed: u64,
    /// The hash of the build it was recorded on; 0 when written by hand.
    pub build: u64,
    /// The tier its snapshot hashes agree under.
    pub determinism: Determinism,
    /// The fixed step's ticks a second.
    pub tick_rate: u32,
    /// The ticks it covers, from tick 0.
    pub ticks: u64,
    /// The action names its changes index.
    pub actions: Vec<String>,
    /// The changes, by ascending tick, each tick's in order.
    pub events: Vec<TapeEvent>,
}

/// Why a text tape did not parse, or a tape does not fit a build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TapeError {
    /// The 1-based line of a text tape, if the error has one.
    pub line: Option<u32>,
    pub message: String,
}

impl fmt::Display for TapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "line {line}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for TapeError {}

/// The tag a tape blob starts with after its protocol header.
const MAGIC: [u8; 4] = *b"GTP1";

impl InputTape {
    /// An empty tape for a run seeded with `seed` at `tick_rate`.
    pub fn new(seed: u64, tick_rate: u32) -> InputTape {
        InputTape {
            seed,
            build: 0,
            determinism: Determinism::SameBinary,
            tick_rate,
            ticks: 0,
            actions: Vec::new(),
            events: Vec::new(),
        }
    }

    /// The index of action `name`, adding it.
    pub fn action(&mut self, name: &str) -> u32 {
        match self.actions.iter().position(|a| a == name) {
            Some(i) => i as u32,
            None => {
                self.actions.push(name.to_owned());
                (self.actions.len() - 1) as u32
            }
        }
    }

    /// Whether `bytes` start like a binary tape rather than text.
    pub fn is_binary(bytes: &[u8]) -> bool {
        let mut dec = Decoder::new(bytes);
        ProtocolTag::decode(&mut dec).is_ok() && dec.read_raw(MAGIC.len()).ok() == Some(&MAGIC[..])
    }

    /// The tape as a blob [`InputTape::decode`] reads back.
    pub fn encode(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        ProtocolTag::current().encode(&mut enc);
        enc.write_raw(&MAGIC);
        enc.write_u64(self.seed);
        enc.write_u64(self.build);
        enc.write_u8(match self.determinism {
            Determinism::None => 0,
            Determinism::SameBinary => 1,
            Determinism::CrossPlatform => 2,
        });
        enc.write_varint(u64::from(self.tick_rate));
        enc.write_varint(self.ticks);
        write_list(&mut enc, &self.actions, |enc, a| enc.write_str(a));
        let mut tick = 0;
        write_list(&mut enc, &self.events, |enc, e| {
            enc.write_varint(e.tick - tick);
            tick = e.tick;
            match e.change {
                TapeChange::Press(a) => {
                    enc.write_u8(0);
                    enc.write_varint(u64::from(a));
                }
                TapeChange::Release(a) => {
                    enc.write_u8(1);
                    enc.write_varint(u64::from(a));
                }
                TapeChange::Set(a, held) => {
                    enc.write_u8(2 + u8::from(held));
                    enc.write_varint(u64::from(a));
                }
                TapeChange::Move(x, y) => {
                    enc.write_u8(4);
                    enc.write_f64(x);
                    enc.write_f64(y);
                }
            }
        });
        enc.into_bytes()
    }

    /// Reads a blob [`InputTape::encode`] wrote.
    ///
    /// # Errors
    ///
    /// A [`DecodeError`] when the bytes are malformed, from another wire
    /// version, not a tape, or name an action or tick out of range.
    pub fn decode(bytes: &[u8]) -> Result<InputTape, DecodeError> {
        let mut dec = Decoder::new(bytes);
        let offset = dec.position();
        if !ProtocolTag::decode(&mut dec)?.is_compatible() || dec.read_raw(MAGIC.len())? != MAGIC {
            return Err(DecodeError::Malformed { offset });
        }
        let seed = dec.read_u64()?;
        let build = dec.read_u64()?;
        let determinism = match dec.read_u8()? {
            0 => Determinism::None,
            1 => Determinism::SameBinary,
            2 => Determinism::CrossPlatform,
            _ => return Err(malformed(&dec)),
        };
        let tick_rate = u32::try_from(dec.read_varint()?).map_err(|_| malformed(&dec))?;
        let ticks = dec.read_varint()?;
        let actions = read_list(&mut dec, |dec| Ok(dec.read_str()?.to_owned()))?;
        let mut tick = 0u64;
        let events = read_list(&mut dec, |dec| {
            tick = tick
                .checked_add(dec.read_varint()?)
                .ok_or_else(|| malformed(dec))?;
            let kind = dec.read_u8()?;
            let change = if kind == 4 {
                TapeChange::Move(dec.read_f64()?, dec.read_f64()?)
            } else {
                let action = u32::try_from(dec.read_varint()?)
                    .ok()
                    .filter(|&a| (a as usize) < actions.len())
                    .ok_or_else(|| malformed(dec))?;
                match kind {
                    0 => TapeChange::Press(action),
                    1 => TapeChange::Release(action),
                    2 | 3 => TapeChange::Set(action, kind == 3),
                    _ => return Err(malformed(dec)),
                }
            };
            Ok(TapeEvent { tick, change })
        })?;
        dec.finish()?;
        if tick_rate == 0 {
            return Err(DecodeError::Malformed { offset });
        }
        Ok(InputTape {
            seed,
            build,
            determinism,
            tick_rate,
            ticks,
            actions,
            events,
        })
    }

    /// Parses the text form, for a run seeded with `seed` at `tick_rate`
    /// unless its header says otherwise.
    ///
    /// # Errors
    ///
    /// The first line that is not a directive, with its number.
    pub fn parse(text: &str, seed: u64, tick_rate: u32) -> Result<InputTape, TapeError> {
        let mut tape = InputTape::new(seed, tick_rate);
        let mut ticks = None;
        let mut events: Vec<(u64, usize, TapeChange)> = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let at = Some(n as u32 + 1);
            let fail = |message: String| TapeError { line: at, message };
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((when, what)) = line.split_once(':') else {
                let (key, value) = line.split_once(char::is_whitespace).ok_or_else(|| {
                    fail(format!(
                        "expected `tick: change` or `key value`, found `{line}`"
                    ))
                })?;
                let value = value.trim();
                let number =
                    |v: &str| parse_u64(v).ok_or_else(|| fail(format!("`{v}` is not a number")));
                match key {
                    "seed" => tape.seed = number(value)?,
                    "build" => tape.build = number(value)?,
                    "ticks" => ticks = Some(number(value)?),
                    "tick_rate" => {
                        tape.tick_rate = u32::try_from(number(value)?)
                            .ok()
                            .filter(|&r| r > 0)
                            .ok_or_else(|| fail(format!("`{value}` is not a tick rate")))?;
                    }
                    "determinism" => {
                        tape.determinism = match value {
                            "same_binary" => Determinism::SameBinary,
                            "cross_platform" => Determinism::CrossPlatform,
                            _ => return Err(fail(format!("unknown determinism tier `{value}`"))),
                        }
                    }
                    _ => return Err(fail(format!("unknown header `{key}`"))),
                }
                continue;
            };
            let (start, end) = parse_ticks(when.trim())
                .ok_or_else(|| fail(format!("`{}` is not a tick or tick range", when.trim())))?;
            let mut words = what.split_whitespace();
            let verb = words.next().unwrap_or("");
            let rest: Vec<&str> = words.collect();
            let mut push =
                |tick: u64, change: TapeChange| events.push((tick, events.len(), change));
            match verb {
                "press" | "release" | "tap" | "hold" | "unhold" => {
                    let [name] = rest[..] else {
                        return Err(fail(format!("`{verb}` takes one action name")));
                    };
                    let action = tape.action(name);
                    match (verb, end) {
                        ("press", None) => push(start, TapeChange::Press(action)),
                        ("press", Some(end)) => {
                            push(start, TapeChange::Press(action));
                            push(end, TapeChange::Release(action));
                        }
                        ("release", None) => push(start, TapeChange::Release(action)),
                        ("tap", None) => {
                            push(start, TapeChange::Press(action));
                            push(start, TapeChange::Release(action));
                        }
                        ("hold", None) => push(start, TapeChange::Set(action, true)),
                        ("unhold", None) => push(start, TapeChange::Set(action, false)),
                        _ => return Err(fail(format!("`{verb}` takes a single tick"))),
                    }
                }
                "axis" => {
                    let spec = rest.join(" ");
                    let (x, y) = parse_move(&spec).ok_or_else(|| {
                        fail(format!(
                            "expected `axis move = (x, y)`, found `axis {spec}`"
                        ))
                    })?;
                    push(start, TapeChange::Move(x, y));
                    if let Some(end) = end {
                        push(end, TapeChange::Move(0.0, 0.0));
                    }
                }
                _ => return Err(fail(format!("unknown change `{verb}`"))),
            }
        }
        // By tick, each tick's changes in the order written.
        events.sort_by_key(|&(tick, order, _)| (tick, order));
        let through = events.last().map_or(0, |e| e.0 + 1);
        tape.ticks = ticks.unwrap_or(through);
        tape.events = events
            .into_iter()
            .map(|(tick, _, change)| TapeEvent { tick, change })
            .collect();
        Ok(tape)
    }

    /// The text form, which [`InputTape::parse`] reads back.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "seed {}", self.seed);
        if self.build != 0 {
            let _ = writeln!(out, "build 0x{:016x}", self.build);
        }
        let tier = match self.determinism {
            Determinism::CrossPlatform => "cross_platform",
            _ => "same_binary",
        };
        let _ = writeln!(out, "determinism {tier}");
        let _ = writeln!(out, "tick_rate {}", self.tick_rate);
        let _ = writeln!(out, "ticks {}", self.ticks);
        let name = |a: u32| self.actions[a as usize].as_str();
        let mut i = 0;
        while i < self.events.len() {
            let TapeEvent { tick, change } = self.events[i];
            let next = self.events.get(i + 1);
            let tap = matches!((change, next), (TapeChange::Press(a), Some(&TapeEvent { tick: t, change: TapeChange::Release(b) })) if t == tick && a == b);
            let _ = match change {
                _ if tap => {
                    i += 1;
                    let TapeChange::Press(a) = change else {
                        unreachable!()
                    };
                    writeln!(out, "{tick}: tap {}", name(a))
                }
                TapeChange::Press(a) => writeln!(out, "{tick}: press {}", name(a)),
                TapeChange::Release(a) => writeln!(out, "{tick}: release {}", name(a)),
                TapeChange::Set(a, true) => writeln!(out, "{tick}: hold {}", name(a)),
                TapeChange::Set(a, false) => writeln!(out, "{tick}: unhold {}", name(a)),
                TapeChange::Move(x, y) => writeln!(out, "{tick}: axis move = ({x:?}, {y:?})"),
            };
            i += 1;
        }
        out
    }
}

/// A decimal or `0x` hexadecimal number, `_` separators allowed.
fn parse_u64(text: &str) -> Option<u64> {
    let digits: String = text.chars().filter(|&c| c != '_').collect();
    match digits.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => digits.parse().ok(),
    }
}

/// `a`, `a..b` or `a..=b`: the first tick and, for a range, the tick its
/// change is undone at.
fn parse_ticks(text: &str) -> Option<(u64, Option<u64>)> {
    let Some((start, end)) = text.split_once("..") else {
        return Some((parse_u64(text)?, None));
    };
    let start = parse_u64(start.trim())?;
    let end = match end.strip_prefix('=') {
        Some(end) => parse_u64(end.trim())?.checked_add(1)?,
        None => parse_u64(end.trim())?,
    };
    (end > start).then_some((start, Some(end)))
}

/// `move = (x, y)`.
fn parse_move(text: &str) -> Option<(f64, f64)> {
    let value = text.strip_prefix("move")?.trim().strip_prefix('=')?.trim();
    let inner = value.strip_prefix('(')?.strip_suffix(')')?;
    let (x, y) = inner.split_once(',')?;
    let (x, y): (f64, f64) = (x.trim().parse().ok()?, y.trim().parse().ok()?);
    (x.is_finite() && y.is_finite()).then_some((x, y))
}

/// A tape a scheduler replays: its changes, each action mapped to the
/// running build's schema by name, and the input it has set so far.
#[derive(Debug)]
pub(super) struct Playback {
    tape: InputTape,
    /// The schema index of each tape action, `None` for one the build lacks.
    map: Vec<Option<u32>>,
    next: usize,
    held: Vec<bool>,
    moved: (f64, f64),
}

impl Playback {
    /// `tape` over a build with the action names `actions`.
    ///
    /// # Errors
    ///
    /// When the tape names an action the build lacks.
    pub(super) fn new(tape: InputTape, actions: &[Box<str>]) -> Result<Playback, TapeError> {
        let map = map_actions(&tape, actions);
        if let Some(i) = map.iter().position(Option::is_none) {
            return Err(TapeError {
                line: None,
                message: format!(
                    "the tape's action `{}` is not an action of the build",
                    tape.actions[i]
                ),
            });
        }
        Ok(Playback {
            held: vec![false; tape.actions.len()],
            map,
            next: 0,
            moved: (0.0, 0.0),
            tape,
        })
    }

    /// `tape` over a build with the action names `actions`, a change of an
    /// action it lacks skipped, as a replay over a reloaded build runs.
    pub(super) fn lenient(tape: InputTape, actions: &[Box<str>]) -> Playback {
        Playback {
            held: vec![false; tape.actions.len()],
            map: map_actions(&tape, actions),
            next: 0,
            moved: (0.0, 0.0),
            tape,
        }
    }

    /// Maps the tape onto a reloaded build's `actions`; a change of an
    /// action it lacks is skipped from now on.
    pub(super) fn rebind(&mut self, actions: &[Box<str>]) {
        self.map = map_actions(&self.tape, actions);
    }

    /// Starts over from the first change, over a build with `actions`.
    pub(super) fn restart(&mut self, actions: &[Box<str>]) {
        self.rebind(actions);
        self.next = 0;
        self.held.fill(false);
        self.moved = (0.0, 0.0);
    }

    /// Puts it where it is when tick `tick` is about to run, as a restored or
    /// rewound game resumes there.
    pub(super) fn seek(&mut self, tick: u64, actions: &[Box<str>]) {
        self.restart(actions);
        if let Some(last) = tick.checked_sub(1) {
            self.changes(last);
        }
    }

    /// The changes that put a fresh latch of the bound build where the tape
    /// has its input, without edges.
    pub(super) fn current(&self) -> impl Iterator<Item = TapeChange> + '_ {
        let held = self.held.iter().enumerate().filter(|h| *h.1);
        let held = held.filter_map(|(t, _)| Some(TapeChange::Set(self.map[t]?, true)));
        held.chain([TapeChange::Move(self.moved.0, self.moved.1)])
    }

    /// The changes of `tick`, mapped, skipping those of earlier ticks.
    pub(super) fn changes(&mut self, tick: u64) -> Vec<TapeChange> {
        let mut out = Vec::new();
        while let Some(event) = self.tape.events.get(self.next) {
            if event.tick > tick {
                break;
            }
            self.next += 1;
            let change = event.change;
            let mapped = match change {
                TapeChange::Press(t) | TapeChange::Release(t) | TapeChange::Set(t, _) => {
                    let on = !matches!(change, TapeChange::Release(_) | TapeChange::Set(_, false));
                    self.held[t as usize] = on;
                    let Some(a) = self.map[t as usize] else {
                        continue;
                    };
                    match change {
                        TapeChange::Press(_) => TapeChange::Press(a),
                        TapeChange::Release(_) => TapeChange::Release(a),
                        _ => TapeChange::Set(a, on),
                    }
                }
                TapeChange::Move(x, y) => {
                    self.moved = (x, y);
                    change
                }
            };
            if event.tick == tick {
                out.push(mapped);
            }
        }
        out
    }

    /// The ticks the tape covers.
    pub(super) fn ticks(&self) -> u64 {
        self.tape.ticks
    }
}

fn map_actions(tape: &InputTape, actions: &[Box<str>]) -> Vec<Option<u32>> {
    tape.actions
        .iter()
        .map(|name| actions.iter().position(|a| **a == **name).map(|i| i as u32))
        .collect()
}

/// A tape a scheduler writes as its ticks read their input.
#[derive(Debug)]
pub(super) struct Recorder {
    tape: InputTape,
    /// The tape index of each action of the running build's schema.
    map: Vec<u32>,
    held: Vec<bool>,
    moved: (f64, f64),
}

impl Recorder {
    pub(super) fn new(seed: u64, tick_rate: u32, actions: &[Box<str>]) -> Recorder {
        let mut recorder = Recorder {
            tape: InputTape::new(seed, tick_rate),
            map: Vec::new(),
            held: Vec::new(),
            moved: (0.0, 0.0),
        };
        recorder.rebind(actions);
        recorder
    }

    /// Records a reloaded build's `actions` under their names.
    pub(super) fn rebind(&mut self, actions: &[Box<str>]) {
        self.map = actions.iter().map(|a| self.tape.action(a)).collect();
        self.held.resize(self.tape.actions.len(), false);
    }

    /// Records what tick `tick` read: `bits` gives each schema action's
    /// held, pressed and released bits, `moved` the move vector.
    pub(super) fn observe(
        &mut self,
        tick: u64,
        bits: impl Fn(u32) -> (bool, bool, bool),
        moved: (f64, f64),
    ) {
        let mut push = |change| self.tape.events.push(TapeEvent { tick, change });
        for (a, &t) in self.map.iter().enumerate() {
            let (held, pressed, released) = bits(a as u32);
            match (pressed, released) {
                (true, true) if held => {
                    push(TapeChange::Release(t));
                    push(TapeChange::Press(t));
                }
                (true, true) => {
                    push(TapeChange::Press(t));
                    push(TapeChange::Release(t));
                }
                (true, false) => push(TapeChange::Press(t)),
                (false, true) => push(TapeChange::Release(t)),
                (false, false) if held != self.held[t as usize] => push(TapeChange::Set(t, held)),
                (false, false) => {}
            }
            self.held[t as usize] = held;
        }
        if moved != self.moved {
            push(TapeChange::Move(moved.0, moved.1));
            self.moved = moved;
        }
        self.tape.ticks = tick + 1;
    }

    /// Forgets what ticks from `tick` on read, as a restored or rewound game
    /// runs them again.
    pub(super) fn rewind(&mut self, tick: u64) {
        self.tape.events.retain(|e| e.tick < tick);
        self.tape.ticks = self.tape.ticks.min(tick);
        self.held.fill(false);
        self.moved = (0.0, 0.0);
        for event in &self.tape.events {
            match event.change {
                TapeChange::Press(t) | TapeChange::Set(t, true) => self.held[t as usize] = true,
                TapeChange::Release(t) | TapeChange::Set(t, false) => {
                    self.held[t as usize] = false;
                }
                TapeChange::Move(x, y) => self.moved = (x, y),
            }
        }
    }

    /// Forgets the changes of the ticks before `tick`, keeping the input
    /// they leave held as changes of the tick before it, so a replay from
    /// `tick` starts with the input it had.
    pub(super) fn forget_before(&mut self, tick: u64) {
        let Some(last) = tick.checked_sub(1) else {
            return;
        };
        let cut = self.tape.events.partition_point(|e| e.tick < tick);
        if cut == 0 || self.tape.events[..cut].iter().all(|e| e.tick == last) {
            return;
        }
        let mut held = vec![false; self.tape.actions.len()];
        let mut moved = None;
        for event in &self.tape.events[..cut] {
            match event.change {
                TapeChange::Press(t) | TapeChange::Set(t, true) => held[t as usize] = true,
                TapeChange::Release(t) | TapeChange::Set(t, false) => held[t as usize] = false,
                TapeChange::Move(x, y) => moved = Some((x, y)),
            }
        }
        let base = held
            .iter()
            .enumerate()
            .filter(|(_, on)| **on)
            .map(|(t, _)| TapeChange::Set(t as u32, true))
            .chain(moved.map(|(x, y)| TapeChange::Move(x, y)))
            .map(|change| TapeEvent { tick: last, change });
        self.tape.events.splice(..cut, base.collect::<Vec<_>>());
    }

    /// The tape so far, of a run on build `build`.
    pub(super) fn tape(&self, build: u64) -> InputTape {
        InputTape {
            build,
            ..self.tape.clone()
        }
    }

    /// The tape so far, of a run on build `build`.
    pub(super) fn finish(self, build: u64) -> InputTape {
        InputTape { build, ..self.tape }
    }
}
