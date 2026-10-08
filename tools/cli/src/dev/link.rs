//! The host's end of the dev channel (`Viso_Hot_Reload.md` §34, §46): accept
//! each connection the launched app opens on the loopback listener, check its
//! [`RuntimeHello`] against the session, and once accepted carry what the
//! runtime sends to the session and what the session sends to the runtime.
//!
//! Each connection is read by its own thread. A wrong token is dropped
//! unanswered; another protocol, session, build or compiler schema is refused
//! with a [`Reject`] naming the reason. An accepted runtime gets a writer
//! thread fed by a channel, so the session never blocks on a runtime that
//! reads slowly; the writer ends when the session lets the runtime go or a
//! write fails.

use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use viso_view::dev::wire::{
    BuildId, DEV_PROTOCOL_VERSION, DevSessionId, FrameError, HostExpect, HostHello, HostMessage,
    ProjectFingerprint, Reject, RuntimeHello, RuntimeMessage, SchemaFingerprint, WireError,
    accept_runtime, read_frame, write_frame,
};

/// How long a dev connection has to present the session token.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// What a dev connection must present, and what the host answers it with.
#[derive(Debug, Clone)]
pub struct Expect {
    pub token: String,
    pub dev_session: DevSessionId,
    pub build_id: BuildId,
    pub project: ProjectFingerprint,
    pub schema: SchemaFingerprint,
}

/// An accepted runtime: its hello, and the channel its writer sends from.
#[derive(Debug)]
pub struct Connection {
    pub hello: RuntimeHello,
    pub writer: Sender<HostMessage>,
}

/// What the connection threads hand the session.
#[derive(Debug)]
pub enum LinkEvent {
    /// A runtime the host accepted.
    Connected(Connection),
    /// A message from the accepted runtime.
    Runtime(RuntimeMessage),
    /// The accepted runtime closed its connection.
    Closed,
    /// A connection that was dropped or refused, and why.
    Refused(String),
}

/// Where the connection threads deliver; `false` once nobody listens.
pub type Deliver = Arc<dyn Fn(LinkEvent) -> bool + Send + Sync>;

/// Accepts every connection waiting on `listener`, each read by its own
/// thread.
pub fn accept(
    listener: &TcpListener,
    expect: &Expect,
    deliver: &Deliver,
    threads: &mut Vec<JoinHandle<()>>,
) {
    while let Ok((stream, _)) = listener.accept() {
        let expect = expect.clone();
        let deliver = Arc::clone(deliver);
        threads.push(thread::spawn(move || read_dev(stream, &expect, &*deliver)));
    }
}

/// Reads one dev connection: the runtime's hello, checked against the
/// session — a wrong token is dropped unanswered, another protocol, session,
/// build or schema is refused with the reason — then what the runtime sends
/// until it closes the connection.
fn read_dev(mut stream: TcpStream, expect: &Expect, deliver: &dyn Fn(LinkEvent) -> bool) {
    // A connection that never says hello must not hold the session open
    // after the app exits.
    if stream.set_nonblocking(false).is_err()
        || stream.set_read_timeout(Some(HELLO_TIMEOUT)).is_err()
    {
        return;
    }
    let refuse = |reason: String| {
        deliver(LinkEvent::Refused(reason));
    };
    let no_token = || refuse("a dev connection did not present the session token".into());
    let mut buf = Vec::new();
    let hello = match read_frame::<RuntimeMessage>(&mut stream, &mut buf) {
        Ok(Some(RuntimeMessage::Hello(hello))) => hello,
        Ok(None) => return,
        Err(FrameError::Wire(WireError::Version { peer })) => {
            let reject = Reject::Protocol {
                host: DEV_PROTOCOL_VERSION,
            };
            let _ = write_frame(&mut stream, &HostMessage::Reject(reject));
            return refuse(format!(
                "the app speaks dev protocol version {peer}, `viso run` \
                 {DEV_PROTOCOL_VERSION}; rebuild it"
            ));
        }
        _ => return no_token(),
    };
    let host = HostExpect {
        token: &expect.token,
        dev_session: expect.dev_session,
        build_id: expect.build_id,
        schema: expect.schema,
    };
    match accept_runtime(&hello, &host) {
        Ok(()) => {}
        Err(None) => return no_token(),
        Err(Some(reject)) => {
            let _ = write_frame(&mut stream, &HostMessage::Reject(reject));
            return refuse(reject.to_string());
        }
    }
    let accepted = HostMessage::Hello(HostHello {
        protocol_version: DEV_PROTOCOL_VERSION,
        dev_session: expect.dev_session,
        runtime_session: hello.runtime_session,
        project_fingerprint: expect.project,
        expected_build_id: expect.build_id,
    });
    if write_frame(&mut stream, &accepted).is_err() || stream.set_read_timeout(None).is_err() {
        return;
    }
    let Ok(mut write_half) = stream.try_clone() else {
        return;
    };
    let (writer, to_send) = mpsc::channel::<HostMessage>();
    let spawned = thread::Builder::new()
        .name("viso-dev-write".into())
        .spawn(move || {
            for message in to_send {
                if write_frame(&mut write_half, &message).is_err() {
                    return;
                }
            }
        });
    if spawned.is_err() || !deliver(LinkEvent::Connected(Connection { hello, writer })) {
        return;
    }
    loop {
        match read_frame::<RuntimeMessage>(&mut stream, &mut buf) {
            Ok(Some(RuntimeMessage::Hello(_))) | Err(FrameError::Wire(_)) => {
                refuse("the app sent a malformed dev frame".into());
                break;
            }
            Ok(Some(message)) => {
                if !deliver(LinkEvent::Runtime(message)) {
                    return;
                }
            }
            Ok(None) | Err(FrameError::Io(_)) => break,
        }
    }
    deliver(LinkEvent::Closed);
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::net::Ipv4Addr;
    use std::sync::Mutex;

    use viso_view::dev::wire::{
        self, Domains, LogLevel, Revision, RuntimeSessionId, RuntimeTarget, same_token,
    };

    use super::*;

    fn expect() -> Expect {
        Expect {
            token: "token".into(),
            dev_session: DevSessionId(11),
            build_id: BuildId(22),
            project: ProjectFingerprint(33),
            schema: SchemaFingerprint(55),
        }
    }

    fn hello() -> RuntimeHello {
        RuntimeHello {
            protocol_version: DEV_PROTOCOL_VERSION,
            token: "token".into(),
            dev_session: DevSessionId(11),
            runtime_session: RuntimeSessionId(44),
            build_id: BuildId(22),
            current_revision: Revision::LAUNCH,
            schema_fingerprint: SchemaFingerprint(55),
            capabilities: Domains::NONE,
            target: RuntimeTarget::DesktopHost,
        }
    }

    /// What the host answers a connection that sends `first`, then `rest`
    /// once it is answered, and what the connection delivered.
    fn connect(first: &[u8], rest: &[RuntimeMessage]) -> (Option<HostMessage>, Vec<LinkEvent>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut app = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let got = Arc::new(Mutex::new(Vec::new()));
        let into = Arc::clone(&got);
        let host = thread::spawn(move || {
            read_dev(stream, &expect(), &move |event| {
                into.lock().unwrap().push(event);
                true
            })
        });
        app.write_all(&(first.len() as u32).to_le_bytes()).unwrap();
        app.write_all(first).unwrap();
        let mut buf = Vec::new();
        let answer = read_frame::<HostMessage>(&mut app, &mut buf).ok().flatten();
        for message in rest {
            write_frame(&mut app, message).unwrap();
        }
        drop(app);
        host.join().unwrap();
        let got = std::mem::take(&mut *got.lock().unwrap());
        (answer, got)
    }

    fn hello_frame(hello: RuntimeHello) -> Vec<u8> {
        wire::encode(&RuntimeMessage::Hello(hello))
    }

    #[test]
    fn an_accepted_app_is_answered_and_its_messages_delivered() {
        let messages = [
            RuntimeMessage::Log {
                level: LogLevel::Error,
                line: "boom".into(),
            },
            RuntimeMessage::Dropped { count: 3 },
        ];
        let (answer, got) = connect(&hello_frame(hello()), &messages);
        assert_eq!(
            answer,
            Some(HostMessage::Hello(HostHello {
                protocol_version: DEV_PROTOCOL_VERSION,
                dev_session: DevSessionId(11),
                runtime_session: RuntimeSessionId(44),
                project_fingerprint: ProjectFingerprint(33),
                expected_build_id: BuildId(22),
            }))
        );
        assert!(
            matches!(
                &got[..],
                [
                    LinkEvent::Connected(Connection { hello: h, .. }),
                    LinkEvent::Runtime(RuntimeMessage::Log { line, .. }),
                    LinkEvent::Runtime(RuntimeMessage::Dropped { count: 3 }),
                    LinkEvent::Closed,
                ] if *h == hello() && line == "boom"
            ),
            "{got:?}"
        );
    }

    #[test]
    fn the_writer_carries_what_the_session_sends() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut app = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let (tx, rx) = mpsc::channel();
        let host = thread::spawn(move || {
            read_dev(stream, &expect(), &move |event| tx.send(event).is_ok())
        });
        write_frame(&mut app, &RuntimeMessage::Hello(hello())).unwrap();
        let mut buf = Vec::new();
        assert!(matches!(
            read_frame::<HostMessage>(&mut app, &mut buf),
            Ok(Some(HostMessage::Hello(_)))
        ));
        let Ok(LinkEvent::Connected(connection)) = rx.recv() else {
            panic!("not connected");
        };
        let failure = HostMessage::Failure {
            file: wire::FileId(1),
            lines: vec!["line".into()],
        };
        connection.writer.send(failure.clone()).unwrap();
        assert_eq!(
            read_frame::<HostMessage>(&mut app, &mut buf).unwrap(),
            Some(failure)
        );
        drop(app);
        host.join().unwrap();
        assert!(matches!(rx.recv(), Ok(LinkEvent::Closed)));
    }

    #[test]
    fn a_wrong_token_is_dropped_unanswered() {
        for first in [
            hello_frame(RuntimeHello {
                token: "tokem".into(),
                ..hello()
            }),
            wire::encode(&RuntimeMessage::Dropped { count: 1 }),
            vec![0xFF; 3],
        ] {
            let (answer, got) = connect(&first, &[RuntimeMessage::Dropped { count: 1 }]);
            assert_eq!(answer, None);
            assert!(matches!(&got[..], [LinkEvent::Refused(_)]));
        }
    }

    #[test]
    fn another_protocol_session_build_or_schema_is_refused_with_the_reason() {
        let mut other_version = hello_frame(hello());
        other_version[5..7].copy_from_slice(&(DEV_PROTOCOL_VERSION + 1).to_le_bytes());
        let cases = [
            (
                other_version,
                Reject::Protocol {
                    host: DEV_PROTOCOL_VERSION,
                },
            ),
            (
                hello_frame(RuntimeHello {
                    dev_session: DevSessionId(1),
                    ..hello()
                }),
                Reject::Session,
            ),
            (
                hello_frame(RuntimeHello {
                    build_id: BuildId(1),
                    ..hello()
                }),
                Reject::Build,
            ),
            (
                hello_frame(RuntimeHello {
                    schema_fingerprint: SchemaFingerprint(1),
                    ..hello()
                }),
                Reject::Schema,
            ),
        ];
        for (first, reject) in cases {
            let (answer, got) = connect(&first, &[]);
            assert_eq!(answer, Some(HostMessage::Reject(reject)));
            assert!(
                matches!(&got[..], [LinkEvent::Refused(reason)] if reason.contains("rebuild") || reject == Reject::Session),
                "{got:?}"
            );
        }
    }

    #[test]
    fn a_malformed_frame_ends_the_connection() {
        let (_, got) = connect(
            &hello_frame(hello()),
            &[
                RuntimeMessage::Hello(hello()),
                RuntimeMessage::Dropped { count: 1 },
            ],
        );
        assert!(
            matches!(
                &got[..],
                [
                    LinkEvent::Connected(_),
                    LinkEvent::Refused(_),
                    LinkEvent::Closed
                ]
            ),
            "{got:?}"
        );
        assert!(same_token("ab", "ab") && !same_token("ab", "abc"));
    }
}
