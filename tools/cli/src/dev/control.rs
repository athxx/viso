//! The control endpoint of `viso run`: a loopback
//! listener, named with its token in the dev session's lock, through which
//! `viso game peek` and `viso game record` ask the running app about a game,
//! `viso inspect` for what it shows, `viso test` on a device to play a UI
//! scenario, and `viso profile` for the frames it draws.
//!
//! A control connection sends one [`ControlRequest`] — the session's control
//! token and the question: about a game, named by its `.vs` file, or for the
//! app's census — and reads one [`ControlReply`]. A wrong token is dropped
//! unanswered. The session relays the question to the runtime over the dev
//! channel and answers the connection when the runtime does: a peek and a
//! census at the next frame, a recording once the game ran the ticks asked
//! for, a scenario once it is played, a profile once its frames are drawn or
//! its time is up.

use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use viso_dev_protocol::inspect::{InspectAsk, InspectorCaps};
use viso_dev_protocol::wire::{
    DEV_PROTOCOL_VERSION, GameAsk, MAX_RECORD_TICKS, Message, RecordSpan, WireError, read_frame,
    same_token, write_frame,
};
use viso_ende::{Decode, Decoder, Encode, Encoder};

use crate::error;

/// How long a control connection has to send its request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a client waits to connect.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// The longest token, game name and state name a request carries.
const MAX_FIELD: usize = 4096;

/// The longest scenario a request carries, and report a reply.
const MAX_SCENARIO: usize = 1 << 20;

// The codes a refusal carries over the wire, by name; their exits are the
// registry's.
pub const ENV_NO_DEV_SESSION: &str = error::ENV_NO_DEV_SESSION.name;
pub const GAME_NOT_RUNNING: &str = error::GAME_NOT_RUNNING.name;
pub const GAME_AMBIGUOUS: &str = error::GAME_AMBIGUOUS.name;
pub const GAME_UNKNOWN_STATE: &str = error::GAME_UNKNOWN_STATE.name;
pub const GAME_BUSY: &str = error::GAME_BUSY.name;
pub const GAME_ENDED: &str = error::GAME_ENDED.name;
pub const GAME_FAULT: &str = error::GAME_FAULT.name;
pub const GAME_ANSWER_INVALID: &str = error::GAME_ANSWER_INVALID.name;
pub const INSPECT_ANSWER_INVALID: &str = error::INSPECT_ANSWER_INVALID.name;
pub const PROFILE_APP_GONE: &str = error::PROFILE_APP_GONE.name;

/// What a control connection asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlRequest {
    pub token: String,
    pub ask: ControlAsk,
}

/// The question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlAsk {
    /// About the game of a `.vs` file: a path from the project root, with or
    /// without the extension, or the file's stem.
    Game { game: String, ask: GameAsk },
    /// For what the app's inspector answers `ask` with.
    Inspect(InspectAsk),
    /// To play the UI scenario `script` against the app's first window.
    Scenario { script: String },
    /// For the samples of the next `frames` frames the app draws, or of
    /// those it draws in the next `millis`, whichever ends first; 0 bounds
    /// neither.
    Profile { frames: u32, millis: u64 },
}

/// The session's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlReply {
    /// The state as JSON, the ticks the game ran and the sessions running
    /// the file, named as the project root sees it.
    State {
        file: String,
        json: String,
        tick: u64,
        sessions: u32,
    },
    /// The recorded tape, binary.
    Tape { file: String, tape: Vec<u8> },
    /// The inspector's answer, the table of the views the app mounts, each
    /// in its wire form, and what the inspector answers.
    Inspected {
        answer: Vec<u8>,
        sources: Vec<u8>,
        inspector: InspectorCaps,
    },
    /// The report of the scenario played, a line per step and its end, as
    /// a headless run writes it.
    Played { lines: Vec<String> },
    /// The capture, as its trace file holds it.
    Profiled { trace: Vec<u8> },
    /// Why there is no answer: a code of this module and what to tell the
    /// developer.
    Refused {
        code: String,
        message: String,
        notes: Vec<String>,
    },
}

impl ControlReply {
    /// A refusal under `code`.
    pub fn refused(code: &str, message: impl Into<String>) -> ControlReply {
        ControlReply::Refused {
            code: code.to_owned(),
            message: message.into(),
            notes: Vec::new(),
        }
    }
}

/// A request a control connection delivered, and where its answer goes.
#[derive(Debug)]
pub struct ControlCall {
    pub ask: ControlAsk,
    pub reply: Sender<ControlReply>,
}

/// Where the control threads deliver; `false` once nobody listens.
pub type ControlDeliver = Arc<dyn Fn(ControlCall) -> bool + Send + Sync>;

/// The listener, and the token a request must present.
pub struct Control {
    listener: TcpListener,
    token: String,
}

impl Control {
    /// Binds the endpoint on a free loopback port, taking requests that
    /// present `token`.
    ///
    /// # Errors
    ///
    /// When the loopback interface refuses.
    pub fn bind(token: String) -> io::Result<Control> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        Ok(Control { listener, token })
    }

    /// The endpoint as the session lock names it: `<address> <token>`.
    ///
    /// # Errors
    ///
    /// When the listener's address cannot be read.
    pub fn endpoint(&self) -> io::Result<String> {
        Ok(format!("{} {}", self.listener.local_addr()?, self.token))
    }

    /// Accepts every connection waiting, each read and answered by its own
    /// thread.
    pub fn accept(&self, deliver: &ControlDeliver, threads: &mut Vec<JoinHandle<()>>) {
        while let Ok((stream, _)) = self.listener.accept() {
            let token = self.token.clone();
            let deliver = Arc::clone(deliver);
            threads.push(thread::spawn(move || serve(stream, &token, &*deliver)));
        }
    }
}

/// Reads one request, hands it to the session and writes its answer.
fn serve(mut stream: TcpStream, token: &str, deliver: &dyn Fn(ControlCall) -> bool) {
    if stream.set_nonblocking(false).is_err()
        || stream.set_read_timeout(Some(REQUEST_TIMEOUT)).is_err()
    {
        return;
    }
    let mut buf = Vec::new();
    let request = match read_frame::<ControlRequest>(&mut stream, &mut buf) {
        Ok(Some(request)) if same_token(&request.token, token) => request,
        Err(viso_dev_protocol::wire::FrameError::Wire(WireError::Version { peer })) => {
            let reply = ControlReply::refused(
                ENV_NO_DEV_SESSION,
                format!(
                    "`viso run` speaks dev protocol version {DEV_PROTOCOL_VERSION}, this \
                     `viso` {peer}"
                ),
            );
            let _ = write_frame(&mut stream, &reply);
            return;
        }
        _ => return,
    };
    let (reply, answer) = mpsc::channel();
    let ended = match request.ask {
        ControlAsk::Game { .. } => GAME_NOT_RUNNING,
        ControlAsk::Inspect(_) | ControlAsk::Scenario { .. } | ControlAsk::Profile { .. } => {
            ENV_NO_DEV_SESSION
        }
    };
    let call = ControlCall {
        ask: request.ask,
        reply,
    };
    let reply = if deliver(call) {
        answer.recv().ok()
    } else {
        None
    };
    let reply = reply
        .unwrap_or_else(|| ControlReply::refused(ended, "`viso run` ended, or the app left it"));
    let _ = write_frame(&mut stream, &reply);
}

/// Asks the session at `endpoint` (`<address> <token>`, as its lock names
/// it) `ask`, waiting `wait` for the answer, or as long as it takes.
///
/// # Errors
///
/// When the session cannot be reached or does not answer: the message.
pub fn call(
    endpoint: &str,
    ask: ControlAsk,
    wait: Option<Duration>,
) -> Result<ControlReply, String> {
    let (address, token) = endpoint
        .split_once(' ')
        .ok_or_else(|| "the session lock names no control endpoint".to_owned())?;
    let address: SocketAddr = address
        .parse()
        .map_err(|_| format!("the session lock names `{address}`, not an address"))?;
    if !address.ip().is_loopback() {
        return Err(format!("the session lock names `{address}`, not loopback"));
    }
    let unreachable = |error: io::Error| format!("cannot reach `viso run` at {address}: {error}");
    let mut stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).map_err(unreachable)?;
    let request = ControlRequest {
        token: token.to_owned(),
        ask,
    };
    write_frame(&mut stream, &request).map_err(unreachable)?;
    stream.set_read_timeout(wait).map_err(unreachable)?;
    let mut buf = Vec::new();
    match read_frame::<ControlReply>(&mut stream, &mut buf) {
        Ok(Some(reply)) => Ok(reply),
        Ok(None) => Err("`viso run` closed the connection unanswered".to_owned()),
        Err(error) => Err(format!("no answer from `viso run`: {error}")),
    }
}

fn write_string(enc: &mut Encoder, text: &str) {
    enc.write_bytes(text.as_bytes());
}

fn read_string(dec: &mut Decoder<'_>, what: &'static str) -> Result<String, WireError> {
    let text = dec.read_str()?;
    if text.len() > MAX_FIELD {
        return Err(WireError::Bound { what });
    }
    Ok(text.to_owned())
}

fn malformed(dec: &Decoder<'_>) -> WireError {
    WireError::Malformed(viso_ende::DecodeError::Malformed {
        offset: dec.position(),
    })
}

impl Message for ControlRequest {
    fn write(&self, enc: &mut Encoder) {
        enc.write_u16(DEV_PROTOCOL_VERSION);
        write_string(enc, &self.token);
        let (game, ask) = match &self.ask {
            ControlAsk::Inspect(ask) => {
                enc.write_u8(3);
                return ask.encode(enc);
            }
            ControlAsk::Scenario { script } => {
                enc.write_u8(4);
                return write_string(enc, script);
            }
            ControlAsk::Profile { frames, millis } => {
                enc.write_u8(5);
                enc.write_varint(u64::from(*frames));
                return enc.write_varint(*millis);
            }
            ControlAsk::Game { game, ask } => (game, ask),
        };
        match ask {
            GameAsk::Peek { system, state } => {
                enc.write_u8(0);
                write_string(enc, system);
                write_string(enc, state);
            }
            GameAsk::Record {
                span: RecordSpan::Ticks(n),
            } => {
                enc.write_u8(1);
                enc.write_varint(*n);
            }
            GameAsk::Record {
                span: RecordSpan::Millis(n),
            } => {
                enc.write_u8(2);
                enc.write_varint(*n);
            }
        }
        write_string(enc, game);
    }

    fn read(dec: &mut Decoder<'_>) -> Result<Self, WireError> {
        let version = dec.read_u16()?;
        if version != DEV_PROTOCOL_VERSION {
            return Err(WireError::Version { peer: version });
        }
        let token = read_string(dec, "control token")?;
        let ask = match dec.read_u8()? {
            0 => GameAsk::Peek {
                system: read_string(dec, "system name")?,
                state: read_string(dec, "state name")?,
            },
            1 => match dec.read_varint()? {
                n if n <= MAX_RECORD_TICKS => GameAsk::Record {
                    span: RecordSpan::Ticks(n),
                },
                _ => {
                    return Err(WireError::Bound {
                        what: "recorded ticks",
                    });
                }
            },
            2 => GameAsk::Record {
                span: RecordSpan::Millis(dec.read_varint()?),
            },
            3 => {
                return Ok(ControlRequest {
                    token,
                    ask: ControlAsk::Inspect(InspectAsk::decode(dec)?),
                });
            }
            4 => {
                let script = dec.read_str()?;
                if script.len() > MAX_SCENARIO {
                    return Err(WireError::Bound { what: "scenario" });
                }
                return Ok(ControlRequest {
                    token,
                    ask: ControlAsk::Scenario {
                        script: script.to_owned(),
                    },
                });
            }
            5 => {
                let frames = u32::try_from(dec.read_varint()?).map_err(|_| malformed(dec))?;
                return Ok(ControlRequest {
                    token,
                    ask: ControlAsk::Profile {
                        frames,
                        millis: dec.read_varint()?,
                    },
                });
            }
            _ => return Err(malformed(dec)),
        };
        let game = read_string(dec, "game name")?;
        Ok(ControlRequest {
            token,
            ask: ControlAsk::Game { game, ask },
        })
    }
}

impl Message for ControlReply {
    fn write(&self, enc: &mut Encoder) {
        match self {
            ControlReply::State {
                file,
                json,
                tick,
                sessions,
            } => {
                enc.write_u8(0);
                write_string(enc, file);
                write_string(enc, json);
                enc.write_varint(*tick);
                enc.write_varint(u64::from(*sessions));
            }
            ControlReply::Tape { file, tape } => {
                enc.write_u8(1);
                write_string(enc, file);
                enc.write_bytes(tape);
            }
            ControlReply::Inspected {
                answer,
                sources,
                inspector,
            } => {
                enc.write_u8(3);
                enc.write_bytes(answer);
                enc.write_bytes(sources);
                enc.write_varint(u64::from(inspector.bits()));
            }
            ControlReply::Profiled { trace } => {
                enc.write_u8(5);
                enc.write_bytes(trace);
            }
            ControlReply::Played { lines } => {
                enc.write_u8(4);
                enc.write_varint(lines.len() as u64);
                for line in lines {
                    write_string(enc, line);
                }
            }
            ControlReply::Refused {
                code,
                message,
                notes,
            } => {
                enc.write_u8(2);
                write_string(enc, code);
                write_string(enc, message);
                enc.write_varint(notes.len() as u64);
                for note in notes {
                    write_string(enc, note);
                }
            }
        }
    }

    fn read(dec: &mut Decoder<'_>) -> Result<Self, WireError> {
        Ok(match dec.read_u8()? {
            0 => ControlReply::State {
                file: read_string(dec, "file")?,
                json: dec.read_str()?.to_owned(),
                tick: dec.read_varint()?,
                sessions: u32::try_from(dec.read_varint()?).map_err(|_| malformed(dec))?,
            },
            1 => ControlReply::Tape {
                file: read_string(dec, "file")?,
                tape: dec.read_bytes()?.to_vec(),
            },
            2 => {
                let code = read_string(dec, "code")?;
                let message = read_string(dec, "message")?;
                let count = dec.read_varint()?;
                if count > 64 {
                    return Err(WireError::Bound { what: "notes" });
                }
                let notes = (0..count)
                    .map(|_| read_string(dec, "note"))
                    .collect::<Result<_, _>>()?;
                ControlReply::Refused {
                    code,
                    message,
                    notes,
                }
            }
            3 => ControlReply::Inspected {
                answer: dec.read_bytes()?.to_vec(),
                sources: dec.read_bytes()?.to_vec(),
                inspector: InspectorCaps::from_bits(
                    u32::try_from(dec.read_varint()?).map_err(|_| malformed(dec))?,
                ),
            },
            4 => {
                let count = dec.read_varint()?;
                if count > MAX_SCENARIO as u64 {
                    return Err(WireError::Bound {
                        what: "report lines",
                    });
                }
                let lines = (0..count)
                    .map(|_| dec.read_str().map(str::to_owned))
                    .collect::<Result<_, _>>()?;
                ControlReply::Played { lines }
            }
            5 => ControlReply::Profiled {
                trace: dec.read_bytes()?.to_vec(),
            },
            _ => return Err(malformed(dec)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn served(token: &str) -> (Control, mpsc::Receiver<ControlCall>, ControlDeliver) {
        let control = Control::bind(token.to_owned()).unwrap();
        let (tx, rx) = mpsc::channel();
        let deliver: ControlDeliver = Arc::new(move |call| tx.send(call).is_ok());
        (control, rx, deliver)
    }

    #[test]
    fn a_request_reaches_the_session_and_its_answer_the_caller() {
        let (control, calls, deliver) = served("secret");
        let endpoint = control.endpoint().unwrap();
        assert!(endpoint.ends_with(" secret"), "{endpoint}");
        let caller = thread::spawn(move || {
            call(
                &endpoint,
                ControlAsk::Game {
                    game: "movement".into(),
                    ask: GameAsk::Peek {
                        system: "Player".into(),
                        state: "score".into(),
                    },
                },
                Some(Duration::from_secs(10)),
            )
        });
        let mut threads = Vec::new();
        let call = loop {
            control.accept(&deliver, &mut threads);
            if let Ok(call) = calls.recv_timeout(Duration::from_millis(10)) {
                break call;
            }
        };
        assert!(matches!(&call.ask, ControlAsk::Game { game, .. } if game == "movement"));
        let state = ControlReply::State {
            file: "src/movement.vs".into(),
            json: "{\"x\":1}".into(),
            tick: 600,
            sessions: 1,
        };
        call.reply.send(state.clone()).unwrap();
        assert_eq!(caller.join().unwrap(), Ok(state));
        threads.into_iter().for_each(|t| t.join().unwrap());
    }

    #[test]
    fn a_wrong_token_is_dropped_unanswered() {
        let (control, calls, deliver) = served("secret");
        let endpoint = control.endpoint().unwrap().replace("secret", "guess!");
        let ask = GameAsk::Record {
            span: RecordSpan::Ticks(60),
        };
        let ask = ControlAsk::Game {
            game: "movement".into(),
            ask,
        };
        let caller = thread::spawn(move || call(&endpoint, ask, None));
        let mut threads = Vec::new();
        while threads.is_empty() {
            control.accept(&deliver, &mut threads);
        }
        threads.into_iter().for_each(|t| t.join().unwrap());
        assert!(caller.join().unwrap().is_err());
        assert!(calls.try_recv().is_err(), "nothing reached the session");
    }

    #[test]
    fn an_unanswered_call_is_refused_when_the_session_lets_it_go() {
        let (control, calls, deliver) = served("t");
        let endpoint = control.endpoint().unwrap();
        let ask = GameAsk::Record {
            span: RecordSpan::Millis(1000),
        };
        let ask = ControlAsk::Game {
            game: "game".into(),
            ask,
        };
        let caller = thread::spawn(move || call(&endpoint, ask, None));
        let mut threads = Vec::new();
        let call = loop {
            control.accept(&deliver, &mut threads);
            if let Ok(call) = calls.recv_timeout(Duration::from_millis(10)) {
                break call;
            }
        };
        drop(call);
        let Ok(ControlReply::Refused { code, .. }) = caller.join().unwrap() else {
            panic!("not refused");
        };
        assert_eq!(code, GAME_NOT_RUNNING);
    }

    #[test]
    fn mutated_control_frames_never_panic() {
        let bodies = [
            viso_dev_protocol::wire::encode(&ControlRequest {
                token: "t".into(),
                ask: ControlAsk::Game {
                    game: "src/a.vs".into(),
                    ask: GameAsk::Peek {
                        system: "S".into(),
                        state: "s".into(),
                    },
                },
            }),
            viso_dev_protocol::wire::encode(&ControlReply::Inspected {
                answer: vec![1, 2],
                sources: vec![3],
                inspector: InspectorCaps::TREE,
            }),
            viso_dev_protocol::wire::encode(&ControlReply::Refused {
                code: GAME_BUSY.into(),
                message: "busy".into(),
                notes: vec!["n".into()],
            }),
            viso_dev_protocol::wire::encode(&ControlReply::Tape {
                file: "a.vs".into(),
                tape: vec![1, 2, 3],
            }),
        ];
        let mut rng: u64 = 0xBF58_476D_1CE4_E5B9;
        let mut next = |n: usize| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng % n.max(1) as u64) as usize
        };
        for _ in 0..20_000 {
            let mut mutated = bodies[next(bodies.len())].clone();
            for _ in 0..1 + next(3) {
                if mutated.is_empty() {
                    break;
                }
                let at = next(mutated.len());
                match next(4) {
                    0 => mutated[at] ^= 1 << next(8),
                    1 => mutated[at] = [0x00, 0x7F, 0x80, 0xFF][next(4)],
                    2 => mutated.insert(at, next(256) as u8),
                    _ => mutated.truncate(at),
                }
            }
            let _ = ControlRequest::from_frame(&mutated);
            let _ = ControlReply::from_frame(&mutated);
        }
    }

    #[test]
    fn every_message_round_trips_and_bounds_hold() {
        for request in [
            ControlRequest {
                token: "t".into(),
                ask: ControlAsk::Game {
                    game: "src/a.vs".into(),
                    ask: GameAsk::Peek {
                        system: "S".into(),
                        state: "s".into(),
                    },
                },
            },
            ControlRequest {
                token: "t".into(),
                ask: ControlAsk::Game {
                    game: "a".into(),
                    ask: GameAsk::Record {
                        span: RecordSpan::Millis(5),
                    },
                },
            },
            ControlRequest {
                token: "t".into(),
                ask: ControlAsk::Inspect(InspectAsk::Census),
            },
            ControlRequest {
                token: "t".into(),
                ask: ControlAsk::Inspect(InspectAsk::Nodes {
                    window: 0,
                    nodes: vec![(3, 1)],
                }),
            },
            ControlRequest {
                token: "t".into(),
                ask: ControlAsk::Scenario {
                    script: format!("click \"Save\"\n{}", "#".repeat(MAX_FIELD)),
                },
            },
            ControlRequest {
                token: "t".into(),
                ask: ControlAsk::Profile {
                    frames: 600,
                    millis: 0,
                },
            },
        ] {
            let body = viso_dev_protocol::wire::encode(&request);
            assert_eq!(ControlRequest::from_frame(&body), Ok(request));
        }
        for reply in [
            ControlReply::Tape {
                file: "a.vs".into(),
                tape: vec![1, 2, 3],
            },
            ControlReply::Inspected {
                answer: vec![1, 2],
                sources: Vec::new(),
                inspector: InspectorCaps::TREE.with(InspectorCaps::FRAME),
            },
            ControlReply::Played {
                lines: vec![
                    r#"{"line":1,"status":"pass"}"#.into(),
                    r#"{"done":3}"#.into(),
                ],
            },
            ControlReply::Profiled { trace: vec![4, 5] },
            ControlReply::Refused {
                code: GAME_BUSY.into(),
                message: "busy".into(),
                notes: vec!["n".into()],
            },
        ] {
            let body = viso_dev_protocol::wire::encode(&reply);
            assert_eq!(ControlReply::from_frame(&body), Ok(reply));
        }
        let long = ControlRequest {
            token: "t".repeat(MAX_FIELD + 1),
            ask: ControlAsk::Inspect(InspectAsk::Census),
        };
        assert_eq!(
            ControlRequest::from_frame(&viso_dev_protocol::wire::encode(&long)),
            Err(WireError::Bound {
                what: "control token"
            })
        );
    }
}
