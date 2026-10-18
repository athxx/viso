//! What an attached inspector follows as it happens: the input events the
//! app routed and the nodes they reached, focus moving, and each node marked
//! dirty with why.
//!
//! A runtime keeps the events of a window in a bounded journal while an
//! inspector asks for them, numbering each; an [`InspectAsk::Events`]
//! (super::InspectAsk::Events) gets those after the last number the tool
//! saw. A headless run that is asked to keeps its journal from its start and
//! writes it at its end.

use viso_ende::{Decode, DecodeError, Decoder, Encode, Encoder};

use super::census::{count, malformed};

/// The events of a window after a number.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EventLog {
    pub window: u32,
    /// The number the next event takes: what to ask after next.
    pub next: u64,
    /// The events between the number asked after and the first here that
    /// the journal no longer holds, or never recorded.
    pub dropped: u64,
    pub events: Vec<InspectEvent>,
}

/// One event, numbered in the order the runtime recorded it.
#[derive(Debug, Clone, PartialEq)]
pub struct InspectEvent {
    pub seq: u64,
    /// The frame it happened in, counted from when the journal started.
    pub frame: u64,
    pub kind: EventKind,
}

/// What happened.
#[derive(Debug, Clone, PartialEq)]
pub enum EventKind {
    /// An input event: what it was (`pointer_down`, `key_down Enter`, …),
    /// where in the window for a pointer, and the nodes it was routed
    /// through, root first, by index and generation.
    Input {
        input: String,
        at: Option<[f32; 2]>,
        route: Vec<(u32, u32)>,
    },
    /// Focus moved, from one node or none to another or none.
    Focus {
        from: Option<(u32, u32)>,
        to: Option<(u32, u32)>,
    },
    /// A node was marked dirty for `class`, as
    /// [`DirtyClass`](viso_ui::DirtyClass) bits, and why.
    Dirty {
        node: (u32, u32),
        class: u8,
        cause: Cause,
    },
}

/// Why a node was marked dirty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cause {
    Other,
    Input,
    /// A state cell changed: by index and generation, its durable identity
    /// when the compiler declared it, and its value after the change.
    State {
        cell: (u32, u32),
        key: Option<(u64, u64)>,
        value: String,
    },
    Computed,
    Animation,
    Task,
    Reload,
    Text,
    Asset,
    Window,
    Layout,
}

impl Cause {
    /// Its name, as a tool shows it.
    pub fn name(&self) -> &'static str {
        match self {
            Cause::Other => "other",
            Cause::Input => "input",
            Cause::State { .. } => "state",
            Cause::Computed => "computed",
            Cause::Animation => "animation",
            Cause::Task => "task",
            Cause::Reload => "reload",
            Cause::Text => "text",
            Cause::Asset => "asset",
            Cause::Window => "window",
            Cause::Layout => "layout",
        }
    }

    fn tag(&self) -> u8 {
        match self {
            Cause::Other => 0,
            Cause::Input => 1,
            Cause::State { .. } => 2,
            Cause::Computed => 3,
            Cause::Animation => 4,
            Cause::Task => 5,
            Cause::Reload => 6,
            Cause::Text => 7,
            Cause::Asset => 8,
            Cause::Window => 9,
            Cause::Layout => 10,
        }
    }
}

impl EventLog {
    /// Its wire form.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }

    /// The log `bytes` spell.
    ///
    /// # Errors
    ///
    /// Bytes that are not a log.
    pub fn from_bytes(bytes: &[u8]) -> Result<EventLog, DecodeError> {
        EventLog::decode_from_slice(bytes)
    }
}

fn write_id(enc: &mut Encoder, (index, generation): (u32, u32)) {
    enc.write_varint(u64::from(index));
    enc.write_varint(u64::from(generation));
}

fn read_u32(dec: &mut Decoder<'_>) -> Result<u32, DecodeError> {
    u32::try_from(dec.read_varint()?).map_err(|_| malformed(dec))
}

fn read_id(dec: &mut Decoder<'_>) -> Result<(u32, u32), DecodeError> {
    Ok((read_u32(dec)?, read_u32(dec)?))
}

fn write_option_id(enc: &mut Encoder, id: Option<(u32, u32)>) {
    match id {
        Some(id) => {
            enc.write_u8(1);
            write_id(enc, id);
        }
        None => enc.write_u8(0),
    }
}

fn read_option_id(dec: &mut Decoder<'_>) -> Result<Option<(u32, u32)>, DecodeError> {
    match dec.read_u8()? {
        0 => Ok(None),
        1 => Ok(Some(read_id(dec)?)),
        _ => Err(malformed(dec)),
    }
}

impl Encode for EventLog {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_varint(u64::from(self.window));
        enc.write_varint(self.next);
        enc.write_varint(self.dropped);
        enc.write_varint(self.events.len() as u64);
        for event in &self.events {
            enc.write_varint(event.seq);
            enc.write_varint(event.frame);
            match &event.kind {
                EventKind::Input { input, at, route } => {
                    enc.write_u8(0);
                    enc.write_str(input);
                    match at {
                        Some([x, y]) => {
                            enc.write_u8(1);
                            enc.write_f32(*x);
                            enc.write_f32(*y);
                        }
                        None => enc.write_u8(0),
                    }
                    enc.write_varint(route.len() as u64);
                    for &id in route {
                        write_id(enc, id);
                    }
                }
                EventKind::Focus { from, to } => {
                    enc.write_u8(1);
                    write_option_id(enc, *from);
                    write_option_id(enc, *to);
                }
                EventKind::Dirty { node, class, cause } => {
                    enc.write_u8(2);
                    write_id(enc, *node);
                    enc.write_u8(*class);
                    enc.write_u8(cause.tag());
                    if let Cause::State { cell, key, value } = cause {
                        write_id(enc, *cell);
                        match key {
                            Some((hi, lo)) => {
                                enc.write_u8(1);
                                enc.write_u64(*hi);
                                enc.write_u64(*lo);
                            }
                            None => enc.write_u8(0),
                        }
                        enc.write_str(value);
                    }
                }
            }
        }
    }
}

impl Decode for EventLog {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let window = read_u32(dec)?;
        let next = dec.read_varint()?;
        let dropped = dec.read_varint()?;
        let events = (0..count(dec)?)
            .map(|_| {
                let seq = dec.read_varint()?;
                let frame = dec.read_varint()?;
                let kind = match dec.read_u8()? {
                    0 => {
                        let input = dec.read_str()?.to_owned();
                        let at = match dec.read_u8()? {
                            0 => None,
                            1 => Some([dec.read_f32()?, dec.read_f32()?]),
                            _ => return Err(malformed(dec)),
                        };
                        let route = (0..count(dec)?)
                            .map(|_| read_id(dec))
                            .collect::<Result<_, _>>()?;
                        EventKind::Input { input, at, route }
                    }
                    1 => EventKind::Focus {
                        from: read_option_id(dec)?,
                        to: read_option_id(dec)?,
                    },
                    2 => {
                        let node = read_id(dec)?;
                        let class = dec.read_u8()?;
                        let cause = match dec.read_u8()? {
                            0 => Cause::Other,
                            1 => Cause::Input,
                            2 => {
                                let cell = read_id(dec)?;
                                let key = match dec.read_u8()? {
                                    0 => None,
                                    1 => Some((dec.read_u64()?, dec.read_u64()?)),
                                    _ => return Err(malformed(dec)),
                                };
                                let value = dec.read_str()?.to_owned();
                                Cause::State { cell, key, value }
                            }
                            3 => Cause::Computed,
                            4 => Cause::Animation,
                            5 => Cause::Task,
                            6 => Cause::Reload,
                            7 => Cause::Text,
                            8 => Cause::Asset,
                            9 => Cause::Window,
                            10 => Cause::Layout,
                            _ => return Err(malformed(dec)),
                        };
                        EventKind::Dirty { node, class, cause }
                    }
                    _ => return Err(malformed(dec)),
                };
                if seq >= next {
                    return Err(malformed(dec));
                }
                Ok(InspectEvent { seq, frame, kind })
            })
            .collect::<Result<Vec<_>, DecodeError>>()?;
        if !events.is_sorted_by(|a, b| a.seq < b.seq) {
            return Err(malformed(dec));
        }
        Ok(EventLog {
            window,
            next,
            dropped,
            events,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_round_trips_and_a_misordered_one_is_refused() {
        let log = EventLog {
            window: 1,
            next: 9,
            dropped: 2,
            events: vec![
                InspectEvent {
                    seq: 3,
                    frame: 1,
                    kind: EventKind::Input {
                        input: "pointer_down".into(),
                        at: Some([4.0, 5.5]),
                        route: vec![(0, 0), (6, 1)],
                    },
                },
                InspectEvent {
                    seq: 4,
                    frame: 1,
                    kind: EventKind::Focus {
                        from: None,
                        to: Some((6, 1)),
                    },
                },
                InspectEvent {
                    seq: 8,
                    frame: 2,
                    kind: EventKind::Dirty {
                        node: (7, 0),
                        class: 0b0010_0000,
                        cause: Cause::State {
                            cell: (2, 0),
                            key: Some((11, 12)),
                            value: "3".into(),
                        },
                    },
                },
            ],
        };
        assert_eq!(EventLog::from_bytes(&log.to_bytes()).unwrap(), log);

        let mut misordered = log.clone();
        misordered.events.swap(0, 1);
        assert!(EventLog::from_bytes(&misordered.to_bytes()).is_err());
        let mut past = log;
        past.next = 8;
        assert!(EventLog::from_bytes(&past.to_bytes()).is_err());
    }
}
