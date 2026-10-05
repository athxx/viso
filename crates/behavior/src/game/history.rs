//! The dev runtime's recent past: a ring of snapshots a few ticks apart over
//! a window of ticks, and the input every tick in it read, so that after a
//! logic reload the game can go back and run that stretch again on the new
//! code ([`Scheduler::replay_from`](super::Scheduler::replay_from)).

use std::collections::VecDeque;
use std::fmt;

use super::snapshot::{GameSnapshot, Restored};
use super::tape::{InputTape, Recorder};

/// The window a dev scheduler keeps by default: the last 10 seconds.
pub const DEFAULT_HISTORY_SECONDS: u32 = 10;

/// The snapshots a scheduler keeps of its recent ticks and their input.
#[derive(Debug)]
pub(super) struct History {
    /// The ticks of the past it can go back over.
    window: u64,
    /// The ticks between snapshots.
    every: u64,
    /// By ascending tick, each at a tick boundary.
    snapshots: VecDeque<GameSnapshot>,
    /// The input of every tick from the oldest snapshot on.
    input: Recorder,
}

impl History {
    /// A history of `window` ticks, a snapshot every `every`, recording
    /// input into `input`.
    pub(super) fn new(window: u64, every: u64, input: Recorder) -> History {
        History {
            window,
            every: every.max(1),
            snapshots: VecDeque::new(),
            input,
        }
    }

    /// Whether the boundary before tick `tick` takes a snapshot.
    pub(super) fn due(&self, tick: u64) -> bool {
        tick.is_multiple_of(self.every) && self.snapshots.back().is_none_or(|s| s.tick < tick)
    }

    /// Keeps `snapshot`, taken before tick `tick`, and forgets what lies
    /// before the window.
    pub(super) fn push(&mut self, snapshot: GameSnapshot) {
        let tick = snapshot.tick;
        self.snapshots.push_back(snapshot);
        let edge = tick.saturating_sub(self.window);
        // The oldest kept snapshot is the last at or before the window's edge.
        while self.snapshots.get(1).is_some_and(|s| s.tick <= edge) {
            self.snapshots.pop_front();
        }
        if let Some(oldest) = self.snapshots.front() {
            self.input.forget_before(oldest.tick);
        }
    }

    /// The input recorder.
    pub(super) fn input_mut(&mut self) -> &mut Recorder {
        &mut self.input
    }

    /// Forgets the snapshots and input from tick `tick` on, as a rewound or
    /// restored game runs them again.
    pub(super) fn rewind(&mut self, tick: u64) {
        while self.snapshots.back().is_some_and(|s| s.tick >= tick) {
            self.snapshots.pop_back();
        }
        self.input.rewind(tick);
    }

    /// Forgets everything, recording into `input` from now on, as a new run
    /// starts.
    pub(super) fn restart(&mut self, input: Recorder) {
        self.snapshots.clear();
        self.input = input;
    }

    /// The latest snapshot at or before tick `tick`.
    pub(super) fn before(&self, tick: u64) -> Option<&GameSnapshot> {
        self.snapshots.iter().rev().find(|s| s.tick <= tick)
    }

    /// The tick of the oldest snapshot, the earliest a replay can start at.
    pub(super) fn oldest(&self) -> Option<u64> {
        self.snapshots.front().map(|s| s.tick)
    }

    /// The snapshots kept.
    pub(super) fn len(&self) -> usize {
        self.snapshots.len()
    }

    /// The input recorded so far, of a run on build `build`.
    pub(super) fn tape(&self, build: u64) -> InputTape {
        self.input.tape(build)
    }
}

/// What [`Scheduler::replay_from`](super::Scheduler::replay_from) did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Replayed {
    /// The tick of the snapshot it went back to, at or before the asked one.
    pub from: u64,
    /// The ticks it ran again, back up to the tick it left.
    pub ticks: u64,
    /// What restoring the snapshot did with its states.
    pub restored: Restored,
}

/// Why a replay did not run; the game is left as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayError {
    /// The scheduler keeps no history ([`keep_history`](super::Scheduler::keep_history)).
    NoHistory,
    /// The tick lies before the oldest snapshot kept, which is `oldest`
    /// (`None` before the first tick boundary).
    Forgotten { oldest: Option<u64> },
    /// The tick has not run yet.
    Ahead,
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReplayError::NoHistory => f.write_str("the game keeps no history"),
            ReplayError::Forgotten { oldest: Some(t) } => {
                write!(f, "the history goes back to tick {t}")
            }
            ReplayError::Forgotten { oldest: None } => f.write_str("the history is empty"),
            ReplayError::Ahead => f.write_str("the tick has not run yet"),
        }
    }
}

impl std::error::Error for ReplayError {}
