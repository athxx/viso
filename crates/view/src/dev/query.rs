//! A question about a running game (`Viso_CLI.md` §22.3): what `viso game
//! peek` and `viso game record` ask a dev runtime through `viso run`, and its
//! answer.
//!
//! The host names the game by the file whose mounts run it, and the runtime
//! asks the first session running it, in mount order. A peek is answered at
//! the next frame boundary with the state's value in its wire form, which the
//! host writes as JSON by the state's type. A recording restarts the World, so
//! the tape replays from tick 0 on a game started with its seed, and is
//! answered with the tape once the game ran the ticks asked for.

use viso_ende::{Decoder, Encoder};

use super::wire::{
    FileId, MAX_LOG, MAX_NAME, WireError, malformed, read_file, read_string, read_u32, write_string,
};

/// The most ticks one recording runs: over three days at 60 Hz.
pub const MAX_RECORD_TICKS: u64 = 1 << 24;

/// What the host asks of the game the mounts of `file` run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameQuery {
    /// The host's handle on the question, which the answer repeats.
    pub id: u32,
    pub file: FileId,
    pub ask: GameAsk,
}

/// The question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameAsk {
    /// The value of state `state` of system `system`.
    Peek { system: String, state: String },
    /// A tape of the next `span` of play, from a restarted World.
    Record { span: RecordSpan },
}

/// How long a recording runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordSpan {
    /// Fixed ticks, at most [`MAX_RECORD_TICKS`].
    Ticks(u64),
    /// Milliseconds of game time, in the game's ticks rounded up, at most
    /// [`MAX_RECORD_TICKS`].
    Millis(u64),
}

/// The runtime's answer to the query `id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameAnswer {
    pub id: u32,
    pub reply: Result<GameReply, GameRefusal>,
}

/// What a query found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameReply {
    /// The state's value in its wire form, the ticks the game ran, and how
    /// many sessions run the file.
    State {
        value: Vec<u8>,
        tick: u64,
        sessions: u32,
    },
    /// The tape, in its binary form.
    Tape { tape: Vec<u8> },
}

/// Why a query has no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameRefusal {
    /// No mount of the file runs a game.
    NotRunning,
    /// The game has no such system, or the system no such state.
    UnknownState,
    /// The game replays a tape: its input is not play to record.
    Replaying,
    /// The game is being recorded already.
    Recording,
    /// The game ended before the recording did.
    Ended,
    /// The World did not restart: the fault, naming the system.
    Fault(String),
    /// The tape outgrew a dev frame.
    TooLarge,
}

impl std::fmt::Display for GameRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GameRefusal::NotRunning => f.write_str("no mount of the file runs a game"),
            GameRefusal::UnknownState => f.write_str("the game has no such state"),
            GameRefusal::Replaying => f.write_str("the game replays a tape"),
            GameRefusal::Recording => f.write_str("the game is being recorded already"),
            GameRefusal::Ended => f.write_str("the game ended before the recording did"),
            GameRefusal::Fault(line) => write!(f, "the World did not restart: {line}"),
            GameRefusal::TooLarge => f.write_str("the tape outgrew a dev frame"),
        }
    }
}

pub(super) fn write_query(enc: &mut Encoder, query: &GameQuery) {
    enc.write_varint(u64::from(query.id));
    enc.write_varint(u64::from(query.file.0));
    match &query.ask {
        GameAsk::Peek { system, state } => {
            enc.write_u8(0);
            write_string(enc, system);
            write_string(enc, state);
        }
        GameAsk::Record { span } => {
            let (tag, n) = match *span {
                RecordSpan::Ticks(n) => (1, n),
                RecordSpan::Millis(n) => (2, n),
            };
            enc.write_u8(tag);
            enc.write_varint(n);
        }
    }
}

pub(super) fn read_query(dec: &mut Decoder<'_>) -> Result<GameQuery, WireError> {
    let id = read_u32(dec)?;
    let file = read_file(dec)?;
    let ask = match dec.read_u8()? {
        0 => GameAsk::Peek {
            system: read_string(dec, MAX_NAME, "system name")?,
            state: read_string(dec, MAX_NAME, "state name")?,
        },
        tag @ (1 | 2) => {
            let n = dec.read_varint()?;
            let span = if tag == 1 {
                if n > MAX_RECORD_TICKS {
                    return Err(WireError::Bound {
                        what: "recorded ticks",
                    });
                }
                RecordSpan::Ticks(n)
            } else {
                RecordSpan::Millis(n)
            };
            GameAsk::Record { span }
        }
        _ => return Err(malformed(dec)),
    };
    Ok(GameQuery { id, file, ask })
}

pub(super) fn write_answer(enc: &mut Encoder, answer: &GameAnswer) {
    enc.write_varint(u64::from(answer.id));
    match &answer.reply {
        Ok(GameReply::State {
            value,
            tick,
            sessions,
        }) => {
            enc.write_u8(0);
            enc.write_bytes(value);
            enc.write_varint(*tick);
            enc.write_varint(u64::from(*sessions));
        }
        Ok(GameReply::Tape { tape }) => {
            enc.write_u8(1);
            enc.write_bytes(tape);
        }
        Err(refusal) => {
            enc.write_u8(2);
            match refusal {
                GameRefusal::NotRunning => enc.write_u8(0),
                GameRefusal::UnknownState => enc.write_u8(1),
                GameRefusal::Replaying => enc.write_u8(2),
                GameRefusal::Recording => enc.write_u8(3),
                GameRefusal::Ended => enc.write_u8(4),
                GameRefusal::Fault(line) => {
                    enc.write_u8(5);
                    write_string(enc, line);
                }
                GameRefusal::TooLarge => enc.write_u8(6),
            }
        }
    }
}

pub(super) fn read_answer(dec: &mut Decoder<'_>) -> Result<GameAnswer, WireError> {
    let id = read_u32(dec)?;
    // A byte string is bounded by the frame it is in.
    let bytes = |dec: &mut Decoder<'_>| dec.read_bytes().map(<[u8]>::to_vec);
    let reply = match dec.read_u8()? {
        0 => Ok(GameReply::State {
            value: bytes(dec)?,
            tick: dec.read_varint()?,
            sessions: read_u32(dec)?,
        }),
        1 => Ok(GameReply::Tape { tape: bytes(dec)? }),
        2 => Err(match dec.read_u8()? {
            0 => GameRefusal::NotRunning,
            1 => GameRefusal::UnknownState,
            2 => GameRefusal::Replaying,
            3 => GameRefusal::Recording,
            4 => GameRefusal::Ended,
            5 => GameRefusal::Fault(read_string(dec, MAX_LOG, "fault line")?),
            6 => GameRefusal::TooLarge,
            _ => return Err(malformed(dec)),
        }),
        _ => return Err(malformed(dec)),
    };
    Ok(GameAnswer { id, reply })
}
