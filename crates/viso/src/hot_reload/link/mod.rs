//! The app's end of the dev channel to `viso run` (`Viso_Hot_Reload.md` §34,
//! §46): connect to the loopback address the CLI passed, introduce the launch
//! with a [`RuntimeHello`], and once the host accepts it carry patches in and
//! ACKs, NACKs and reports out.
//!
//! Two threads own the socket. The link thread connects, shakes hands, starts
//! the reader and then writes: it drains a bounded outgoing queue the loop
//! only `try_send`s into, so when the queue is full a message is dropped and
//! counted, and the count goes out as [`RuntimeMessage::Dropped`] once there
//! is room. The reader decodes each host frame and hands it to the loop
//! through a bounded queue, waking it; a full queue stalls the reader, never
//! the loop. The reader also assembles the blobs the host sends ahead of a
//! patch and decodes the assets the patch names, so no asset decodes on the
//! loop. A snapshot the app captured goes out whole: the writer cuts it into
//! blob chunks, so it never fills the queue; and the reader assembles the
//! snapshot a restore carries before the loop sees the restore. A failed
//! connect, handshake or write ends the threads, and later messages are
//! dropped. An address that is not loopback is refused.

#[cfg(not(target_arch = "wasm32"))]
use std::collections::hash_map::RandomState;
#[cfg(not(target_arch = "wasm32"))]
use std::hash::{BuildHasher, Hasher};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use std::{
    io::Write,
    net::TcpStream,
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
    thread::{self, JoinHandle},
};

#[cfg(not(target_arch = "wasm32"))]
use viso_platform::LoopWaker;
use viso_platform::{Instant, launch_var};
use viso_render::asset::ResourceId;
use viso_text::font_format;
use viso_ui::{AssetBytes, AssetPayload};
use viso_view::dev::wire::{
    BlobRef, Blobs, BuildId, DEV_BUILD_ENV, DEV_PROTOCOL_VERSION, DEV_RUNTIME_ENV, DEV_SESSION_ENV,
    DEV_TOKEN_ENV, DevSessionId, Domains, FileId, GameQuery, HostMessage, Message, PatchBundle,
    PatchSection, RestoreBundle, Revision, RuntimeHello, RuntimeIdentity, RuntimeMessage,
    RuntimeSessionId, RuntimeTarget, SchemaFingerprint, WindowGeometry, WireError, blob_chunks,
};
#[cfg(not(target_arch = "wasm32"))]
use viso_view::dev::wire::{FrameError, Reject, accept_host, read_body, read_frame, write_frame};

/// What this runtime runs on, for the host's per-target work.
const RUNTIME_TARGET: RuntimeTarget = if cfg!(target_os = "ios") {
    RuntimeTarget::IosSimulator
} else if cfg!(target_os = "android") {
    RuntimeTarget::AndroidEmulator
} else if cfg!(target_arch = "wasm32") {
    RuntimeTarget::Web
} else {
    RuntimeTarget::DesktopHost
};

/// How long the thread waits for the CLI to accept, and then to answer.
#[cfg(not(target_arch = "wasm32"))]
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(not(target_arch = "wasm32"))]
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// How many host frames wait for the loop, and how many messages wait for the
/// writer.
#[cfg(not(target_arch = "wasm32"))]
const INCOMING: usize = 8;
#[cfg(not(target_arch = "wasm32"))]
const OUTGOING: usize = 64;

/// What the reader hands the loop.
pub(crate) enum Incoming {
    /// The host accepted the launch.
    Connected(RuntimeIdentity),
    /// A patch, the assets its `resource` section names, in its order, and
    /// how long its frame and assets took to decode.
    Patch(Box<PatchBundle>, Vec<LoadedAsset>, Duration),
    /// The host's verdict on a rejected edit, or its clearing.
    Failure { file: FileId, lines: Vec<String> },
    /// A question about a running game.
    Game(GameQuery),
    /// The host asks for the app's snapshot.
    Snapshot,
    /// The state to restore, and the bytes of its snapshot, `None` when they
    /// did not arrive whole.
    Restore(Box<RestoreBundle>, Option<Arc<[u8]>>),
    /// A host frame that did not decode.
    Undecodable(WireError),
}

/// What the loop hands the writer.
enum Outgoing {
    Message(RuntimeMessage),
    /// A snapshot's bytes, its windows and how long capturing them took,
    /// written as blob chunks and the message naming them.
    Snapshot(Vec<u8>, Vec<WindowGeometry>, u64),
}

/// An asset a patch names, as its blob decoded.
pub(crate) struct LoadedAsset {
    pub(crate) id: ResourceId,
    /// Its path under `assets/`.
    pub(crate) path: String,
    /// What it decoded to, or why it did not load.
    pub(crate) loaded: Result<Loaded, String>,
}

/// What an asset's bytes decoded to.
pub(crate) enum Loaded {
    /// An image: its bytes and payload.
    Image(AssetBytes, AssetPayload),
    /// A font whose every face parses: its sfnt bytes.
    Font(Arc<[u8]>),
}

#[cfg(target_arch = "wasm32")]
mod web;
#[cfg(target_arch = "wasm32")]
pub(crate) use web::DevLink;

/// The address and hello of the link `viso run` asked for through the launch
/// variables, if any, for a runtime compiled against `schema` that applies
/// `capabilities`. An address that is not loopback is refused.
fn launch_hello(
    schema: SchemaFingerprint,
    capabilities: Domains,
) -> Option<(SocketAddr, RuntimeHello)> {
    let addr: SocketAddr = launch_var(DEV_RUNTIME_ENV)?.parse().ok()?;
    if !addr.ip().is_loopback() {
        return None;
    }
    let hello = RuntimeHello {
        protocol_version: DEV_PROTOCOL_VERSION,
        token: launch_var(DEV_TOKEN_ENV)?,
        dev_session: DevSessionId::from_hex(&launch_var(DEV_SESSION_ENV)?)?,
        runtime_session: RuntimeSessionId(random_u128()),
        build_id: BuildId::from_hex(&launch_var(DEV_BUILD_ENV)?)?,
        current_revision: Revision::LAUNCH,
        schema_fingerprint: schema,
        capabilities,
        target: RUNTIME_TARGET,
        shader_lang: <viso_gpu::Backend as viso_gpu::GpuBackend>::SHADER_LANG,
    };
    Some((addr, hello))
}

/// The identity the host accepted `hello` under.
fn identity(hello: &RuntimeHello) -> RuntimeIdentity {
    RuntimeIdentity {
        dev_session: hello.dev_session,
        runtime_session: hello.runtime_session,
        build_id: hello.build_id,
        capabilities: hello.capabilities,
        current_revision: hello.current_revision,
    }
}

/// What one host frame after the handshake comes to.
enum Frame {
    /// A message for the loop.
    Deliver(Incoming),
    /// A blob kept for the patch or restore it comes ahead of.
    Kept,
    /// A second hello or a reject: the link ends.
    End,
}

/// What the host frame `body` comes to, the blobs ahead of a patch or a
/// restore kept in `blobs` until it arrives.
fn host_frame(body: &[u8], blobs: &mut Blobs) -> Frame {
    let started = Instant::now();
    Frame::Deliver(match HostMessage::from_frame(body) {
        Ok(HostMessage::Blob(chunk)) => {
            blobs.add(chunk);
            return Frame::Kept;
        }
        Ok(HostMessage::Patch(patch)) => {
            let assets = load_assets(&patch, blobs);
            Incoming::Patch(patch, assets, started.elapsed())
        }
        Ok(HostMessage::Failure { file, lines }) => Incoming::Failure { file, lines },
        Ok(HostMessage::Game(query)) => Incoming::Game(query),
        Ok(HostMessage::Snapshot) => Incoming::Snapshot,
        Ok(HostMessage::Restore(restore)) => {
            let snapshot = blobs.take(restore.snapshot);
            blobs.clear();
            Incoming::Restore(restore, snapshot)
        }
        Ok(HostMessage::Hello(_) | HostMessage::Reject(_)) => return Frame::End,
        Err(error) => Incoming::Undecodable(error),
    })
}

/// Writes `outgoing` with `write`, one message at a time: a snapshot as
/// its blob chunks, then the message naming them.
fn write_outgoing<E>(
    outgoing: Outgoing,
    mut write: impl FnMut(&RuntimeMessage) -> Result<(), E>,
) -> Result<(), E> {
    match outgoing {
        Outgoing::Message(message) => write(&message),
        Outgoing::Snapshot(bytes, windows, capture_us) => {
            for chunk in blob_chunks(&bytes) {
                write(&RuntimeMessage::Blob(chunk))?;
            }
            write(&RuntimeMessage::Snapshot {
                blob: BlobRef::of(&bytes),
                windows,
                capture_us,
            })
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// The app's end of the dev channel.
pub(crate) struct DevLink {
    outgoing: Option<SyncSender<Outgoing>>,
    incoming: Receiver<Incoming>,
    /// Messages dropped since the last that went out.
    dropped: u32,
    /// The link thread, the writer once connected.
    link: Option<JoinHandle<()>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl DevLink {
    /// The link `viso run` asked for through the launch variables, if any,
    /// for a runtime compiled against `schema` that applies `capabilities`;
    /// `wake` kicks the loop when a host frame arrives.
    pub(crate) fn from_launch(
        wake: LoopWaker,
        schema: SchemaFingerprint,
        capabilities: Domains,
    ) -> Option<Self> {
        let (addr, hello) = launch_hello(schema, capabilities)?;
        Some(Self::connect(addr, hello, wake))
    }

    /// Starts the reader, which connects to `addr` and introduces the launch
    /// with `hello`.
    pub(super) fn connect(addr: SocketAddr, hello: RuntimeHello, wake: LoopWaker) -> Self {
        let (outgoing, to_send) = mpsc::sync_channel::<Outgoing>(OUTGOING);
        let (deliver, incoming) = mpsc::sync_channel::<Incoming>(INCOMING);
        let link = thread::Builder::new()
            .name("viso-dev-link".into())
            .spawn(move || {
                let Some((stream, identity)) = handshake(addr, &hello) else {
                    return;
                };
                let Ok(read_half) = stream.try_clone() else {
                    return;
                };
                if deliver.send(Incoming::Connected(identity)).is_err() {
                    return;
                }
                wake.wake();
                let reader = thread::Builder::new()
                    .name("viso-dev-read".into())
                    .spawn(move || read_all(read_half, &deliver, &wake));
                if reader.is_ok() {
                    write_all(stream, &to_send);
                }
            })
            .ok();
        Self {
            outgoing: Some(outgoing),
            incoming,
            dropped: 0,
            link,
        }
    }

    /// Queues `message` for the host, or counts it dropped when the queue is
    /// full.
    pub(crate) fn send(&mut self, message: RuntimeMessage) {
        let Some(outgoing) = &self.outgoing else {
            return;
        };
        if self.dropped > 0 {
            match outgoing.try_send(Outgoing::Message(RuntimeMessage::Dropped {
                count: self.dropped,
            })) {
                Ok(()) => self.dropped = 0,
                Err(TrySendError::Full(_)) => {
                    self.dropped = self.dropped.saturating_add(1);
                    return;
                }
                Err(TrySendError::Disconnected(_)) => return,
            }
        }
        if let Err(TrySendError::Full(_)) = outgoing.try_send(Outgoing::Message(message)) {
            self.dropped = self.dropped.saturating_add(1);
        }
    }

    /// Hands the writer the snapshot `bytes` and the app's `windows`,
    /// captured in `capture_us`: it is never dropped, so this waits while the
    /// queue is full.
    pub(crate) fn send_snapshot(
        &mut self,
        bytes: Vec<u8>,
        windows: Vec<WindowGeometry>,
        capture_us: u64,
    ) {
        if let Some(outgoing) = &self.outgoing {
            let _ = outgoing.send(Outgoing::Snapshot(bytes, windows, capture_us));
        }
    }

    /// The next host frame the reader delivered.
    pub(crate) fn poll(&self) -> Option<Incoming> {
        self.incoming.try_recv().ok()
    }

    /// The next host frame the reader delivers before `deadline`; `None` at
    /// the deadline or once the link ended.
    pub(crate) fn wait(&self, deadline: Instant) -> Option<Incoming> {
        let left = deadline.checked_duration_since(Instant::now())?;
        self.incoming.recv_timeout(left).ok()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for DevLink {
    /// Closes the outgoing queue and waits for the writer to send what is
    /// left; the reader ends when the host closes the connection.
    fn drop(&mut self) {
        self.outgoing = None;
        if let Some(link) = self.link.take() {
            let _ = link.join();
        }
    }
}

/// Connects and shakes hands; the stream and the accepted identity, or `None`
/// (a refusal is reported on stderr).
#[cfg(not(target_arch = "wasm32"))]
fn handshake(addr: SocketAddr, hello: &RuntimeHello) -> Option<(TcpStream, RuntimeIdentity)> {
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).ok()?;
    let _ = stream.set_nodelay(true);
    write_frame(&mut stream, &RuntimeMessage::Hello(hello.clone())).ok()?;
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT)).ok()?;
    let mut buf = Vec::new();
    let answer = read_frame::<HostMessage>(&mut stream, &mut buf);
    let refused = |reject: Reject| {
        eprintln!("[viso] dev channel refused: {reject}");
        None
    };
    match answer {
        Ok(Some(HostMessage::Hello(host))) => {
            if let Err(reject) = accept_host(&host, hello) {
                return refused(reject);
            }
        }
        Ok(Some(HostMessage::Reject(reject))) => return refused(reject),
        Err(FrameError::Wire(WireError::Version { peer })) => {
            return refused(Reject::Protocol { host: peer });
        }
        _ => return None,
    }
    stream.set_read_timeout(None).ok()?;
    Some((stream, identity(hello)))
}

/// Delivers each host frame to the loop until the host closes the connection
/// or the loop drops the link. A frame that does not decode is delivered as
/// such — its length kept the stream in step — but a second hello is a
/// protocol violation that ends the link.
#[cfg(not(target_arch = "wasm32"))]
fn read_all(mut stream: TcpStream, deliver: &SyncSender<Incoming>, wake: &LoopWaker) {
    let mut buf = Vec::new();
    let mut blobs = Blobs::default();
    loop {
        match read_body(&mut stream, &mut buf) {
            Ok(true) => {}
            Ok(false) | Err(FrameError::Io(_)) => return,
            Err(FrameError::Wire(error)) => {
                // An oversized frame: its body was not read, so the stream is
                // out of step.
                let _ = deliver.send(Incoming::Undecodable(error));
                wake.wake();
                return;
            }
        }
        let incoming = match host_frame(&buf, &mut blobs) {
            Frame::Deliver(incoming) => incoming,
            Frame::Kept => continue,
            Frame::End => return,
        };
        if deliver.send(incoming).is_err() {
            return;
        }
        wake.wake();
    }
}

/// Each asset `patch` names, claimed from `blobs` and decoded; the blobs are
/// dropped after, claimed or not.
fn load_assets(patch: &PatchBundle, blobs: &mut Blobs) -> Vec<LoadedAsset> {
    let mut assets = Vec::new();
    for section in &patch.sections {
        let PatchSection::Resource(resources) = section else {
            continue;
        };
        for asset in &resources.assets {
            let loaded = match blobs.claim(asset) {
                // A font is told apart by its sfnt header, as the host told it.
                Some(bytes) if font_format::detect(&bytes).is_some() => {
                    if font_format::parses(&bytes) {
                        Ok(Loaded::Font(bytes))
                    } else {
                        Err("does not decode: a face of it does not parse".to_owned())
                    }
                }
                Some(bytes) => crate::assets::decode(&bytes)
                    .map(|payload| Loaded::Image(AssetBytes::Shared(bytes), payload))
                    .map_err(|error| format!("does not decode: {error}")),
                None => Err("its bytes did not arrive whole".to_owned()),
            };
            assets.push(LoadedAsset {
                id: asset.id(),
                path: asset.path.clone(),
                loaded,
            });
        }
    }
    blobs.clear();
    assets
}

/// Writes every queued message until the loop closes the queue or a write
/// fails.
#[cfg(not(target_arch = "wasm32"))]
fn write_all(mut stream: TcpStream, to_send: &Receiver<Outgoing>) {
    for outgoing in to_send {
        if write_outgoing(outgoing, |message| write_frame(&mut stream, message)).is_err() {
            return;
        }
    }
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Write);
}

/// A random 128-bit launch id.
#[cfg(not(target_arch = "wasm32"))]
fn random_u128() -> u128 {
    let state = RandomState::new();
    let word = |salt: u8| {
        let mut hasher = state.build_hasher();
        hasher.write_u8(salt);
        hasher.finish()
    };
    (u128::from(word(0)) << 64) | u128::from(word(1))
}

/// A page's: its hasher keys are fixed, so `Math.random`, which tells
/// launches apart (the session's secret is the CLI's token).
#[cfg(target_arch = "wasm32")]
fn random_u128() -> u128 {
    (0..4).fold(0, |id, _| {
        (id << 32) | (js_sys::Math::random() * f64::from(u32::MAX)) as u128
    })
}

/// A host end of the dev channel for tests: a loopback listener that accepts
/// one app and answers its hello.
#[cfg(test)]
pub(crate) mod fake {
    use std::net::TcpListener;

    use viso_view::dev::wire::{HostHello, ProjectFingerprint};

    use super::*;

    pub(crate) const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    pub(crate) const SESSION: DevSessionId = DevSessionId(0x5E55);
    pub(crate) const BUILD: BuildId = BuildId(0xB1D);

    /// The hello `viso run`'s environment makes an app send.
    pub(crate) fn hello() -> RuntimeHello {
        RuntimeHello {
            protocol_version: DEV_PROTOCOL_VERSION,
            token: TOKEN.into(),
            dev_session: SESSION,
            runtime_session: RuntimeSessionId(random_u128()),
            build_id: BUILD,
            current_revision: Revision::LAUNCH,
            schema_fingerprint: SchemaFingerprint(0),
            capabilities: super::super::APPLIES,
            target: RUNTIME_TARGET,
            shader_lang: <viso_gpu::Backend as viso_gpu::GpuBackend>::SHADER_LANG,
        }
    }

    pub(crate) struct Host {
        pub(crate) listener: TcpListener,
        pub(crate) stream: Option<TcpStream>,
        pub(crate) hello: Option<RuntimeHello>,
        buf: Vec<u8>,
    }

    impl Host {
        pub(crate) fn bind() -> Self {
            Host {
                listener: TcpListener::bind("127.0.0.1:0").unwrap(),
                stream: None,
                hello: None,
                buf: Vec::new(),
            }
        }

        pub(crate) fn addr(&self) -> SocketAddr {
            self.listener.local_addr().unwrap()
        }

        /// Accepts the app and reads its hello, without answering.
        pub(crate) fn accept(&mut self) -> &RuntimeHello {
            let (stream, _) = self.listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            self.stream = Some(stream);
            match self.read() {
                Some(RuntimeMessage::Hello(hello)) => self.hello.insert(hello),
                other => panic!("not a hello: {other:?}"),
            }
        }

        /// Answers the hello as `viso run` does.
        pub(crate) fn welcome(&mut self) {
            let hello = self.hello.clone().expect("accepted");
            self.send(&HostMessage::Hello(HostHello {
                protocol_version: DEV_PROTOCOL_VERSION,
                dev_session: hello.dev_session,
                runtime_session: hello.runtime_session,
                project_fingerprint: ProjectFingerprint(1),
                expected_build_id: hello.build_id,
            }));
        }

        pub(crate) fn send(&mut self, message: &HostMessage) {
            write_frame(self.stream.as_mut().unwrap(), message).unwrap();
        }

        pub(crate) fn send_raw(&mut self, body: &[u8]) {
            let stream = self.stream.as_mut().unwrap();
            stream
                .write_all(&(body.len() as u32).to_le_bytes())
                .unwrap();
            stream.write_all(body).unwrap();
        }

        pub(crate) fn read(&mut self) -> Option<RuntimeMessage> {
            read_frame(self.stream.as_mut().unwrap(), &mut self.buf).unwrap()
        }

        /// A patch for the accepted app from `base` to `next`.
        pub(crate) fn patch(&self, base: u64, next: u64) -> HostMessage {
            let hello = self.hello.as_ref().expect("accepted");
            HostMessage::Patch(Box::new(PatchBundle {
                dev_session: hello.dev_session,
                target_runtime: hello.runtime_session,
                base_revision: Revision(base),
                next_revision: Revision(next),
                build_id: hello.build_id,
                sections: Vec::new(),
            }))
        }
    }

    /// Polls `link` until it delivers, for at most ten seconds.
    pub(crate) fn next(link: &DevLink) -> Incoming {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(incoming) = link.poll() {
                return incoming;
            }
            assert!(Instant::now() < deadline, "the link delivered nothing");
            thread::sleep(Duration::from_millis(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use viso_view::dev::wire::{LogLevel, NACK_MALFORMED, ProjectFingerprint};

    use super::fake::{self, Host};
    use super::*;

    fn waker() -> (LoopWaker, Arc<AtomicUsize>) {
        let kicks = Arc::new(AtomicUsize::new(0));
        let counted = kicks.clone();
        let waker = LoopWaker::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        (waker, kicks)
    }

    fn log(line: &str) -> RuntimeMessage {
        RuntimeMessage::Log {
            level: LogLevel::Info,
            line: line.into(),
        }
    }

    #[test]
    fn the_link_introduces_itself_then_carries_patches_in_and_messages_out() {
        let mut host = Host::bind();
        let (wake, kicks) = waker();
        let hello = fake::hello();
        let mut link = DevLink::connect(host.addr(), hello.clone(), wake);
        assert_eq!(host.accept(), &hello);
        host.welcome();
        let Incoming::Connected(identity) = fake::next(&link) else {
            panic!("not connected");
        };
        assert_eq!(
            (identity.runtime_session, identity.current_revision),
            (hello.runtime_session, Revision::LAUNCH)
        );
        host.send(&host.patch(1, 2));
        assert!(
            matches!(fake::next(&link), Incoming::Patch(p, ..) if p.next_revision == Revision(2))
        );
        assert!(
            kicks.load(Ordering::SeqCst) >= 2,
            "each delivery wakes the loop"
        );

        // A frame that does not decode is delivered as such, and the
        // channel stays in step.
        host.send_raw(&[0xEE, 1, 2]);
        assert!(matches!(fake::next(&link), Incoming::Undecodable(_)));
        host.send(&host.patch(2, 3));
        assert!(matches!(fake::next(&link), Incoming::Patch(..)));

        link.send(log("one"));
        link.send(RuntimeMessage::Nack(
            identity.undecodable(WireError::NotDevStream),
        ));
        drop(link);
        assert_eq!(host.read(), Some(log("one")));
        assert!(
            matches!(host.read(), Some(RuntimeMessage::Nack(n)) if n.diagnostic_codes == [NACK_MALFORMED])
        );
        assert_eq!(host.read(), None, "dropping the link closes it");
    }

    #[test]
    fn a_full_queue_drops_and_reports_the_count() {
        let mut host = Host::bind();
        let (wake, _) = waker();
        let mut link = DevLink::connect(host.addr(), fake::hello(), wake);
        host.accept();
        // Unanswered, the link does not write yet: the queue fills.
        for n in 0..OUTGOING + 5 {
            link.send(log(&n.to_string()));
        }
        assert_eq!(link.dropped, 5);
        host.welcome();
        assert!(matches!(fake::next(&link), Incoming::Connected(_)));
        for n in 0..OUTGOING {
            assert_eq!(host.read(), Some(log(&n.to_string())));
        }
        link.send(log("after"));
        assert_eq!(host.read(), Some(RuntimeMessage::Dropped { count: 5 }));
        assert_eq!(host.read(), Some(log("after")));
        assert_eq!(link.dropped, 0);
    }

    #[test]
    fn a_refused_or_foreign_answer_ends_the_link() {
        let answers = [
            HostMessage::Reject(Reject::Build),
            HostMessage::Hello(viso_view::dev::wire::HostHello {
                protocol_version: DEV_PROTOCOL_VERSION,
                dev_session: fake::SESSION,
                runtime_session: RuntimeSessionId(1),
                project_fingerprint: ProjectFingerprint(1),
                expected_build_id: fake::BUILD,
            }),
            HostMessage::Hello(viso_view::dev::wire::HostHello {
                protocol_version: DEV_PROTOCOL_VERSION,
                dev_session: fake::SESSION,
                runtime_session: RuntimeSessionId(7),
                project_fingerprint: ProjectFingerprint(1),
                expected_build_id: BuildId(9),
            }),
        ];
        for answer in answers {
            let mut host = Host::bind();
            let (wake, _) = waker();
            let mut hello = fake::hello();
            hello.runtime_session = RuntimeSessionId(7);
            let mut link = DevLink::connect(host.addr(), hello, wake);
            host.accept();
            host.send(&answer);
            link.send(log("lost"));
            drop(link);
            assert_eq!(host.read(), None, "{answer:?}");
        }
    }

    #[test]
    fn a_link_with_no_listener_drops_its_messages() {
        // A bound then closed port refuses the connect.
        let addr = Host::bind().addr();
        let (wake, _) = waker();
        let mut link = DevLink::connect(addr, fake::hello(), wake);
        link.send(log("lost"));
        drop(link);
    }
}
