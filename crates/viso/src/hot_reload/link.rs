//! The dev channel to `viso run`: one thread that connects to the loopback
//! address the CLI passed, says hello with the session token, and writes each
//! reload event the loop hands it.
//!
//! The loop only encodes and enqueues, so a slow or gone CLI never stalls a
//! frame; a failed connect or write ends the thread, and later events are
//! dropped. An address that is not loopback is refused.

use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use viso_dsl::hotreload::event::{
    DEV_PROTOCOL_VERSION, DEV_RUNTIME_ENV, DEV_TOKEN_ENV, DevMessage, ReloadEvent, write_frame,
};

/// How long the thread waits for the CLI to accept.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// The app's end of the dev channel.
pub(crate) struct DevLink {
    events: Option<Sender<ReloadEvent>>,
    thread: Option<JoinHandle<()>>,
}

impl DevLink {
    /// The link `viso run` asked for through the environment, if any.
    pub(crate) fn from_env() -> Option<Self> {
        let addr: SocketAddr = std::env::var(DEV_RUNTIME_ENV).ok()?.parse().ok()?;
        if !addr.ip().is_loopback() {
            return None;
        }
        let token = std::env::var(DEV_TOKEN_ENV).ok()?;
        Some(Self::connect(addr, token))
    }

    /// Starts the thread that connects to `addr` and introduces itself with
    /// `token`.
    fn connect(addr: SocketAddr, token: String) -> Self {
        let (events, queue) = mpsc::channel::<ReloadEvent>();
        let thread = thread::Builder::new()
            .name("viso-dev-link".into())
            .spawn(move || {
                let Ok(mut stream) = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) else {
                    return;
                };
                let _ = stream.set_nodelay(true);
                let hello = DevMessage::Hello {
                    protocol_version: DEV_PROTOCOL_VERSION,
                    token,
                };
                if write_frame(&mut stream, &hello).is_err() {
                    return;
                }
                for event in queue {
                    if write_frame(&mut stream, &DevMessage::Reload(Box::new(event))).is_err() {
                        return;
                    }
                }
            })
            .ok();
        Self {
            events: Some(events),
            thread,
        }
    }

    /// Hands `event` to the thread.
    pub(crate) fn send(&self, event: ReloadEvent) {
        if let Some(events) = &self.events {
            let _ = events.send(event);
        }
    }
}

impl Drop for DevLink {
    /// Closes the queue, letting the thread write what is left and exit.
    fn drop(&mut self) {
        self.events = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use viso_dsl::hotreload::event::{ReloadOutcome, ReloadStage, read_frame};

    use super::*;

    fn event(revision: u32) -> ReloadEvent {
        ReloadEvent {
            file: "view.vs".into(),
            source: String::new(),
            base_revision: revision - 1,
            candidate_revision: revision,
            last_good_revision: revision,
            outcome: ReloadOutcome::Applied,
            stage: ReloadStage::RuntimeCommit,
            elapsed_us: 10,
            mounts: 1,
            migrated: 1,
            reset: 0,
            focus_lost: 0,
            scroll_lost: 0,
            handlers_lost: 0,
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn the_link_says_hello_then_sends_each_event_in_order() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let link = DevLink::connect(listener.local_addr().unwrap(), "token".into());
        link.send(event(1));
        link.send(event(2));
        drop(link);

        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = Vec::new();
        let mut next = || read_frame(&mut stream, &mut buf).unwrap();
        assert_eq!(
            next(),
            Some(DevMessage::Hello {
                protocol_version: DEV_PROTOCOL_VERSION,
                token: "token".into(),
            })
        );
        assert_eq!(next(), Some(DevMessage::Reload(Box::new(event(1)))));
        assert_eq!(next(), Some(DevMessage::Reload(Box::new(event(2)))));
        assert_eq!(next(), None);
    }

    #[test]
    fn a_link_with_no_listener_drops_its_events() {
        // A bound then closed port refuses the connect.
        let addr = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let link = DevLink::connect(addr, "token".into());
        link.send(event(1));
        drop(link);
    }
}
