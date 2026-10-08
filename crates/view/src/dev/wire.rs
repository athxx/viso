//! The dev channel between `viso run` (the host) and a running dev artifact
//! (the runtime): `viso-ende` binary frames, one message family per direction
//! (`Viso_Hot_Reload.md` §33–§37, §46).
//!
//! A frame is a four-byte little-endian length and one message, at most
//! [`MAX_FRAME`] bytes. Each message starts with a stable one-byte tag. The
//! runtime connects and speaks first: its [`RuntimeHello`] leads with the
//! stream's [`ProtocolTag`] and the [`DEV_PROTOCOL_VERSION`], then proves the
//! connection is the app this session launched with the session token. The
//! host answers [`HostHello`] or [`Reject`]; both keep their version-first
//! layout in every protocol version, so a peer of another version is refused
//! with a reason instead of being decoded by guesswork. After the handshake the
//! host sends [`PatchBundle`]s and the runtime answers each with a
//! [`PatchAck`] or a [`PatchNack`].
//!
//! Decoding is bounded: every string and count has a cap checked before it is
//! read, and no message nests, so the depth is fixed. Malformed input is a
//! typed [`WireError`], never a panic.

use std::fmt;
use std::io::{self, Read, Write};

use viso_ende::{Decode, DecodeError, Decoder, Encode, Encoder, ProtocolTag};

/// The dev channel protocol version. Version 2 is the runtime-first handshake
/// with session, build and revision binding.
pub const DEV_PROTOCOL_VERSION: u16 = 2;

/// The environment variable `viso run` passes the dev channel's loopback
/// address in.
pub const DEV_RUNTIME_ENV: &str = "VISO_DEV_RUNTIME";

/// The environment variable `viso run` passes the session token in.
pub const DEV_TOKEN_ENV: &str = "VISO_DEV_TOKEN";

/// The environment variable `viso run` passes the dev session id in, as 32
/// hex digits.
pub const DEV_SESSION_ENV: &str = "VISO_DEV_SESSION";

/// The environment variable `viso run` passes the build id of the artifact it
/// launched in, as 32 hex digits.
pub const DEV_BUILD_ENV: &str = "VISO_DEV_BUILD";

/// The largest frame either side writes or reads.
pub const MAX_FRAME: usize = 4 << 20;

/// The longest session token a hello may carry.
pub const MAX_TOKEN: usize = 64;

/// The most diagnostic codes a NACK carries, and the longest code.
pub const MAX_CODES: usize = 64;
pub const MAX_CODE: usize = 32;

/// The longest log line a runtime sends.
pub const MAX_LOG: usize = 16 << 10;

/// The most sections a patch carries.
pub const MAX_SECTIONS: usize = 4096;

/// A 128-bit identity, written as its two halves.
macro_rules! id128 {
    ($($(#[$doc:meta])* $name:ident;)+) => {$(
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
        pub struct $name(pub u128);

        impl $name {
            /// Parses the 32-hex-digit spelling.
            pub fn from_hex(hex: &str) -> Option<Self> {
                (hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
                    .then(|| u128::from_str_radix(hex, 16).ok().map(Self))
                    .flatten()
            }

            /// The 32-hex-digit spelling.
            pub fn to_hex(self) -> String {
                format!("{:032x}", self.0)
            }
        }

        impl Encode for $name {
            fn encode(&self, enc: &mut Encoder) {
                enc.write_u64((self.0 >> 64) as u64);
                enc.write_u64(self.0 as u64);
            }
        }

        impl Decode for $name {
            fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
                let hi = dec.read_u64()?;
                let lo = dec.read_u64()?;
                Ok(Self((u128::from(hi) << 64) | u128::from(lo)))
            }
        }
    )+};
}

id128! {
    /// One `viso run` session.
    DevSessionId;
    /// One launch of a running app within a session.
    RuntimeSessionId;
    /// The build the artifact was launched as (`Viso_CLI.md` §46).
    BuildId;
    /// The project's source graph (`Viso_CLI.md` §46).
    ProjectFingerprint;
    /// The app schema patches are compiled against.
    SchemaFingerprint;
}

/// A runtime revision: what the running app matches. A launch starts at
/// [`Revision::LAUNCH`], each applied patch moves it to the patch's
/// `next_revision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Revision(pub u64);

impl Revision {
    /// The revision of the artifact as built.
    pub const LAUNCH: Revision = Revision(1);
}

/// A patch domain, with its stable wire tag (§39).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Domain {
    Ui = 0,
    Module = 1,
    State = 2,
    System = 3,
    Shader = 4,
    Resource = 5,
}

impl Domain {
    pub const ALL: [Domain; 6] = [
        Domain::Ui,
        Domain::Module,
        Domain::State,
        Domain::System,
        Domain::Shader,
        Domain::Resource,
    ];

    fn from_tag(tag: u8) -> Option<Domain> {
        Domain::ALL.get(usize::from(tag)).copied()
    }

    /// The `dev` event spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Domain::Ui => "ui",
            Domain::Module => "module",
            Domain::State => "state",
            Domain::System => "system",
            Domain::Shader => "shader",
            Domain::Resource => "resource",
        }
    }
}

/// A set of domains: what a runtime can apply, or what a patch applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Domains(u32);

impl Domains {
    pub const NONE: Domains = Domains(0);

    pub fn with(self, domain: Domain) -> Self {
        Domains(self.0 | 1 << domain as u8)
    }

    pub fn contains(self, domain: Domain) -> bool {
        self.0 & 1 << domain as u8 != 0
    }

    /// The domains of the set, in tag order.
    pub fn iter(self) -> impl Iterator<Item = Domain> {
        Domain::ALL.into_iter().filter(move |d| self.contains(*d))
    }
}

/// Where the runtime runs (§4.1, §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeTarget {
    DesktopHost,
    AndroidEmulator,
    IosSimulator,
    Web,
}

impl RuntimeTarget {
    /// The `dev` event spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            RuntimeTarget::DesktopHost => "desktop",
            RuntimeTarget::AndroidEmulator => "android-emulator",
            RuntimeTarget::IosSimulator => "ios-simulator",
            RuntimeTarget::Web => "web",
        }
    }
}

/// A §51 stage, where a patch failed or what it reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Stage {
    Watch = 0,
    Parse = 1,
    Resolve = 2,
    Typecheck = 3,
    Capability = 4,
    PatchPlan = 5,
    StateCompat = 6,
    ShaderCompile = 7,
    GpuValidate = 8,
    ResourceBuild = 9,
    Transport = 10,
    RuntimeStage = 11,
    RuntimeCommit = 12,
    WarmRestart = 13,
    SnapshotRestore = 14,
}

impl Stage {
    const ALL: [Stage; 15] = [
        Stage::Watch,
        Stage::Parse,
        Stage::Resolve,
        Stage::Typecheck,
        Stage::Capability,
        Stage::PatchPlan,
        Stage::StateCompat,
        Stage::ShaderCompile,
        Stage::GpuValidate,
        Stage::ResourceBuild,
        Stage::Transport,
        Stage::RuntimeStage,
        Stage::RuntimeCommit,
        Stage::WarmRestart,
        Stage::SnapshotRestore,
    ];

    /// The §51 spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Watch => "watch",
            Stage::Parse => "parse",
            Stage::Resolve => "resolve",
            Stage::Typecheck => "typecheck",
            Stage::Capability => "capability",
            Stage::PatchPlan => "patch-plan",
            Stage::StateCompat => "state-compat",
            Stage::ShaderCompile => "shader-compile",
            Stage::GpuValidate => "gpu-validate",
            Stage::ResourceBuild => "resource-build",
            Stage::Transport => "transport",
            Stage::RuntimeStage => "runtime-stage",
            Stage::RuntimeCommit => "runtime-commit",
            Stage::WarmRestart => "warm-restart",
            Stage::SnapshotRestore => "snapshot-restore",
        }
    }
}

/// The runtime's first frame: who it is and what it can apply (§34, §4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeHello {
    pub protocol_version: u16,
    /// The session token the app was launched with.
    pub token: String,
    /// The session the app was launched in.
    pub dev_session: DevSessionId,
    /// This launch, chosen by the runtime.
    pub runtime_session: RuntimeSessionId,
    /// The build the app was launched as.
    pub build_id: BuildId,
    pub current_revision: Revision,
    pub schema_fingerprint: SchemaFingerprint,
    /// The domains the runtime applies; the host sends no other.
    pub capabilities: Domains,
    pub target: RuntimeTarget,
}

/// The host's acceptance of a runtime (§34).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostHello {
    pub protocol_version: u16,
    pub dev_session: DevSessionId,
    /// The runtime accepted, echoed.
    pub runtime_session: RuntimeSessionId,
    pub project_fingerprint: ProjectFingerprint,
    /// The build the host compiles patches for.
    pub expected_build_id: BuildId,
}

/// Why the host refused a runtime. The runtime cannot recover in place: a
/// mismatch asks for a rebuild or a relaunch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    /// The runtime speaks another protocol version than `host`.
    Protocol { host: u16 },
    /// The runtime is not the build the host compiles for.
    Build,
    /// The runtime names another session.
    Session,
}

impl fmt::Display for Reject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reject::Protocol { host } => write!(
                f,
                "the app speaks another dev protocol version than `viso run` ({host}); rebuild it"
            ),
            Reject::Build => {
                f.write_str("the app is not the build `viso run` compiles for; rebuild it")
            }
            Reject::Session => f.write_str("the app belongs to another `viso run` session"),
        }
    }
}

/// One patch: a revision step of the running app (§35). Its sections, at most
/// one per domain and in tag order, commit together or not at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchBundle {
    pub dev_session: DevSessionId,
    pub target_runtime: RuntimeSessionId,
    pub base_revision: Revision,
    pub next_revision: Revision,
    pub build_id: BuildId,
    pub sections: Vec<PatchSection>,
}

/// A domain's part of a patch. Each domain's payload arrives with the phase
/// that applies it; until then its tag is reserved and decodes as
/// [`WireError::UnsupportedDomain`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchSection {}

impl PatchSection {
    pub fn domain(&self) -> Domain {
        match *self {}
    }
}

/// How long the runtime's part of a patch took.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PatchTimings {
    /// Decoding the frame and checking the patch.
    pub decode_us: u64,
    /// Linking and staging the candidate.
    pub stage_us: u64,
    /// The commit at the frame boundary.
    pub commit_us: u64,
}

/// A patch applied (§37).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchAck {
    /// The revision the runtime now matches.
    pub revision: Revision,
    pub applied_domains: Domains,
    /// States reset to their initializer, focus or scroll lost.
    pub scoped_resets: u32,
    pub timings: PatchTimings,
}

/// A patch refused; the runtime keeps `last_good_revision` (§37).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchNack {
    pub base_revision: Revision,
    pub candidate_revision: Revision,
    pub stage: Stage,
    /// The refusal's codes: a `NACK_*` code of this module or a diagnostic
    /// code, each at most [`MAX_CODE`] bytes, at most [`MAX_CODES`].
    pub diagnostic_codes: Vec<String>,
    pub last_good_revision: Revision,
}

/// The patch is for another session or launch.
pub const NACK_UNKNOWN_SESSION: &str = "NACK_UNKNOWN_SESSION";
/// The patch was compiled for another build.
pub const NACK_BUILD_MISMATCH: &str = "NACK_BUILD_MISMATCH";
/// The patch's base is not the runtime's revision (§36).
pub const NACK_REVISION_MISMATCH: &str = "NACK_REVISION_MISMATCH";
/// The patch does not move the revision forward.
pub const NACK_REVISION_ORDER: &str = "NACK_REVISION_ORDER";
/// The patch carries a domain the runtime did not advertise.
pub const NACK_UNSUPPORTED_DOMAIN: &str = "NACK_UNSUPPORTED_DOMAIN";
/// The patch frame did not decode.
pub const NACK_MALFORMED: &str = "NACK_MALFORMED";

/// A log line's level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

/// What the host sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostMessage {
    Hello(HostHello),
    Reject(Reject),
    Patch(Box<PatchBundle>),
}

/// What the runtime sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeMessage {
    Hello(RuntimeHello),
    Ack(PatchAck),
    Nack(PatchNack),
    Log {
        level: LogLevel,
        line: String,
    },
    /// Messages the runtime dropped because the host read too slowly.
    Dropped {
        count: u32,
    },
    /// A reload the app compiled itself, as `viso_dsl`'s report codec writes
    /// it. Removed with the in-app compiler.
    InAppReload(Vec<u8>),
}

/// Why a frame or message was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    /// The stream is not a dev channel stream of this wire format.
    NotDevStream,
    /// A hello of another protocol version.
    Version { peer: u16 },
    /// A frame over [`MAX_FRAME`].
    FrameTooLarge { len: usize },
    /// A string or count over its cap.
    Bound { what: &'static str },
    /// A section of a reserved domain this build cannot decode.
    UnsupportedDomain(Domain),
    /// The bytes do not spell a message.
    Malformed(DecodeError),
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::NotDevStream => f.write_str("not a dev channel stream"),
            WireError::Version { peer } => write!(f, "dev protocol version {peer}"),
            WireError::FrameTooLarge { len } => write!(f, "a {len}-byte dev frame"),
            WireError::Bound { what } => write!(f, "{what} over its bound"),
            WireError::UnsupportedDomain(domain) => {
                write!(f, "a `{}` section this build cannot apply", domain.as_str())
            }
            WireError::Malformed(error) => write!(f, "malformed dev frame: {error}"),
        }
    }
}

impl From<DecodeError> for WireError {
    fn from(error: DecodeError) -> Self {
        WireError::Malformed(error)
    }
}

/// A dev channel message.
pub trait Message: Sized {
    fn write(&self, enc: &mut Encoder);
    fn read(dec: &mut Decoder<'_>) -> Result<Self, WireError>;

    /// Decodes one message from a whole frame body.
    fn from_frame(body: &[u8]) -> Result<Self, WireError> {
        let mut dec = Decoder::new(body);
        let message = Self::read(&mut dec)?;
        dec.finish()?;
        Ok(message)
    }
}

/// The frame body of `message`.
pub fn encode<M: Message>(message: &M) -> Vec<u8> {
    let mut enc = Encoder::new();
    message.write(&mut enc);
    enc.into_bytes()
}

/// Writes `message` to `out` as one frame.
pub fn write_frame<M: Message>(out: &mut impl Write, message: &M) -> io::Result<()> {
    let mut enc = Encoder::new();
    enc.write_raw(&[0; 4]);
    message.write(&mut enc);
    let mut frame = enc.into_bytes();
    let len = frame.len() - 4;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            WireError::FrameTooLarge { len }.to_string(),
        ));
    }
    frame[..4].copy_from_slice(&(len as u32).to_le_bytes());
    out.write_all(&frame)
}

/// Why reading a frame failed.
#[derive(Debug)]
pub enum FrameError {
    Io(io::Error),
    Wire(WireError),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::Io(error) => error.fmt(f),
            FrameError::Wire(error) => error.fmt(f),
        }
    }
}

/// Reads the next frame's body from `input` into `buf`; `false` at the end
/// of the stream between frames. A frame over [`MAX_FRAME`] is refused before
/// its body is read.
pub fn read_body(input: &mut impl Read, buf: &mut Vec<u8>) -> Result<bool, FrameError> {
    let mut len = [0; 4];
    match input.read_exact(&mut len) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(false),
        Err(error) => return Err(FrameError::Io(error)),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(FrameError::Wire(WireError::FrameTooLarge { len }));
    }
    buf.clear();
    buf.resize(len, 0);
    input.read_exact(buf).map_err(FrameError::Io)?;
    Ok(true)
}

/// Reads the next frame from `input` into `buf` and decodes it; `None` at the
/// end of the stream between frames.
pub fn read_frame<M: Message>(
    input: &mut impl Read,
    buf: &mut Vec<u8>,
) -> Result<Option<M>, FrameError> {
    if !read_body(input, buf)? {
        return Ok(None);
    }
    M::from_frame(buf).map(Some).map_err(FrameError::Wire)
}

/// Whether `offered` is `token`, comparing every byte whatever the first
/// mismatch.
pub fn same_token(offered: &str, token: &str) -> bool {
    offered.len() == token.len()
        && offered
            .bytes()
            .zip(token.bytes())
            .fold(0, |diff, (a, b)| diff | (a ^ b))
            == 0
}

/// What the host expects of a runtime.
#[derive(Debug, Clone)]
pub struct HostExpect<'a> {
    pub token: &'a str,
    pub dev_session: DevSessionId,
    pub build_id: BuildId,
}

/// The host's verdict on a runtime's hello: `Ok` to accept, `Err(Some)` to
/// refuse with a reason, `Err(None)` to drop the connection without one (a
/// wrong token learns nothing).
pub fn accept_runtime(hello: &RuntimeHello, expect: &HostExpect<'_>) -> Result<(), Option<Reject>> {
    if !same_token(&hello.token, expect.token) {
        return Err(None);
    }
    if hello.dev_session != expect.dev_session {
        return Err(Some(Reject::Session));
    }
    if hello.build_id != expect.build_id {
        return Err(Some(Reject::Build));
    }
    Ok(())
}

/// The runtime's verdict on the host's answer to `mine`.
pub fn accept_host(hello: &HostHello, mine: &RuntimeHello) -> Result<(), Reject> {
    if hello.protocol_version != DEV_PROTOCOL_VERSION {
        return Err(Reject::Protocol {
            host: hello.protocol_version,
        });
    }
    if hello.dev_session != mine.dev_session || hello.runtime_session != mine.runtime_session {
        return Err(Reject::Session);
    }
    if hello.expected_build_id != mine.build_id {
        return Err(Reject::Build);
    }
    Ok(())
}

/// Who a runtime is once the host accepted it, and the revision it matches.
#[derive(Debug, Clone)]
pub struct RuntimeIdentity {
    pub dev_session: DevSessionId,
    pub runtime_session: RuntimeSessionId,
    pub build_id: BuildId,
    pub capabilities: Domains,
    pub current_revision: Revision,
}

impl RuntimeIdentity {
    /// Checks `patch` against the runtime before anything is staged (§36): it
    /// must name this session and launch, this build, start at the current
    /// revision, move forward, and carry only advertised domains. A refusal
    /// keeps the current revision.
    pub fn check(&self, patch: &PatchBundle) -> Result<(), PatchNack> {
        let refuse = |stage, code: &str| PatchNack {
            base_revision: patch.base_revision,
            candidate_revision: patch.next_revision,
            stage,
            diagnostic_codes: vec![code.to_owned()],
            last_good_revision: self.current_revision,
        };
        if patch.dev_session != self.dev_session || patch.target_runtime != self.runtime_session {
            return Err(refuse(Stage::Transport, NACK_UNKNOWN_SESSION));
        }
        if patch.build_id != self.build_id {
            return Err(refuse(Stage::Transport, NACK_BUILD_MISMATCH));
        }
        if patch.base_revision != self.current_revision {
            return Err(refuse(Stage::RuntimeStage, NACK_REVISION_MISMATCH));
        }
        if patch.next_revision <= patch.base_revision {
            return Err(refuse(Stage::RuntimeStage, NACK_REVISION_ORDER));
        }
        if patch
            .sections
            .iter()
            .any(|s| !self.capabilities.contains(s.domain()))
        {
            return Err(refuse(Stage::RuntimeStage, NACK_UNSUPPORTED_DOMAIN));
        }
        Ok(())
    }

    /// The NACK for a host frame that did not decode: a section of a domain
    /// this build cannot apply, or malformed bytes. The frame boundary held, so
    /// the channel stays open.
    pub fn undecodable(&self, error: WireError) -> PatchNack {
        let code = match error {
            WireError::UnsupportedDomain(_) => NACK_UNSUPPORTED_DOMAIN,
            _ => NACK_MALFORMED,
        };
        PatchNack {
            base_revision: self.current_revision,
            candidate_revision: self.current_revision,
            stage: Stage::Transport,
            diagnostic_codes: vec![code.to_owned()],
            last_good_revision: self.current_revision,
        }
    }
}

// Message tags. Stable: a tag is never reused for another message.
const HOST_HELLO: u8 = 0;
const HOST_REJECT: u8 = 1;
const HOST_PATCH: u8 = 2;

const RUNTIME_HELLO: u8 = 0;
const RUNTIME_ACK: u8 = 1;
const RUNTIME_NACK: u8 = 2;
const RUNTIME_LOG: u8 = 3;
const RUNTIME_DROPPED: u8 = 4;
const RUNTIME_IN_APP_RELOAD: u8 = 5;

const REJECT_PROTOCOL: u8 = 0;
const REJECT_BUILD: u8 = 1;
const REJECT_SESSION: u8 = 2;

/// The version preamble of a hello and a reject: the stream tag, then the dev
/// protocol version, a layout every version keeps.
fn write_version(enc: &mut Encoder) {
    ProtocolTag::current().encode(enc);
    enc.write_u16(DEV_PROTOCOL_VERSION);
}

/// Reads a hello's version preamble; another version stops the decode with
/// its number, before any field whose layout may differ.
fn read_version(dec: &mut Decoder<'_>) -> Result<u16, WireError> {
    let tag = ProtocolTag::decode(dec).map_err(|_| WireError::NotDevStream)?;
    if !tag.is_compatible() {
        return Err(WireError::NotDevStream);
    }
    let version = dec.read_u16()?;
    if version != DEV_PROTOCOL_VERSION {
        return Err(WireError::Version { peer: version });
    }
    Ok(version)
}

fn malformed(dec: &Decoder<'_>) -> WireError {
    WireError::Malformed(DecodeError::Malformed {
        offset: dec.position(),
    })
}

/// A string of at most `max` bytes, its length checked before it is read.
fn read_string(dec: &mut Decoder<'_>, max: usize, what: &'static str) -> Result<String, WireError> {
    let at = dec.position();
    let len = dec.read_varint()?;
    if len > max as u64 {
        return Err(WireError::Bound { what });
    }
    let bytes = dec.read_raw(len as usize)?;
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| WireError::Malformed(DecodeError::InvalidUtf8 { offset: at }))
}

fn write_string(enc: &mut Encoder, text: &str) {
    enc.write_varint(text.len() as u64);
    enc.write_raw(text.as_bytes());
}

/// A count of at most `max`, and never more than the bytes left.
fn read_count(dec: &mut Decoder<'_>, max: usize, what: &'static str) -> Result<usize, WireError> {
    let count = dec.read_varint()?;
    if count > max as u64 {
        return Err(WireError::Bound { what });
    }
    if count > dec.remaining() as u64 {
        return Err(malformed(dec));
    }
    Ok(count as usize)
}

fn write_revision(enc: &mut Encoder, revision: Revision) {
    enc.write_varint(revision.0);
}

fn read_revision(dec: &mut Decoder<'_>) -> Result<Revision, WireError> {
    Ok(Revision(dec.read_varint()?))
}

fn read_domains(dec: &mut Decoder<'_>) -> Result<Domains, WireError> {
    let bits = dec.read_varint()?;
    if bits >> Domain::ALL.len() != 0 {
        return Err(malformed(dec));
    }
    Ok(Domains(bits as u32))
}

fn read_u32(dec: &mut Decoder<'_>) -> Result<u32, WireError> {
    let value = dec.read_varint()?;
    u32::try_from(value).map_err(|_| malformed(dec))
}

impl Message for HostMessage {
    fn write(&self, enc: &mut Encoder) {
        match self {
            HostMessage::Hello(hello) => {
                enc.write_u8(HOST_HELLO);
                ProtocolTag::current().encode(enc);
                enc.write_u16(hello.protocol_version);
                hello.dev_session.encode(enc);
                hello.runtime_session.encode(enc);
                hello.project_fingerprint.encode(enc);
                hello.expected_build_id.encode(enc);
            }
            HostMessage::Reject(reject) => {
                enc.write_u8(HOST_REJECT);
                write_version(enc);
                match reject {
                    Reject::Protocol { host } => {
                        enc.write_u8(REJECT_PROTOCOL);
                        enc.write_u16(*host);
                    }
                    Reject::Build => enc.write_u8(REJECT_BUILD),
                    Reject::Session => enc.write_u8(REJECT_SESSION),
                }
            }
            HostMessage::Patch(patch) => {
                enc.write_u8(HOST_PATCH);
                patch.dev_session.encode(enc);
                patch.target_runtime.encode(enc);
                write_revision(enc, patch.base_revision);
                write_revision(enc, patch.next_revision);
                patch.build_id.encode(enc);
                enc.write_varint(patch.sections.len() as u64);
                // No domain's payload is defined yet, so there is none to write.
                if let Some(section) = patch.sections.first() {
                    match *section {}
                }
            }
        }
    }

    fn read(dec: &mut Decoder<'_>) -> Result<Self, WireError> {
        match dec.read_u8()? {
            HOST_HELLO => Ok(HostMessage::Hello(HostHello {
                protocol_version: read_version(dec)?,
                dev_session: DevSessionId::decode(dec)?,
                runtime_session: RuntimeSessionId::decode(dec)?,
                project_fingerprint: ProjectFingerprint::decode(dec)?,
                expected_build_id: BuildId::decode(dec)?,
            })),
            HOST_REJECT => {
                // A reject keeps its layout across versions: whatever the
                // host speaks, the reason reads.
                let tag = ProtocolTag::decode(dec).map_err(|_| WireError::NotDevStream)?;
                if !tag.is_compatible() {
                    return Err(WireError::NotDevStream);
                }
                let _host_version = dec.read_u16()?;
                let reject = match dec.read_u8()? {
                    REJECT_PROTOCOL => Reject::Protocol {
                        host: dec.read_u16()?,
                    },
                    REJECT_BUILD => Reject::Build,
                    REJECT_SESSION => Reject::Session,
                    _ => return Err(malformed(dec)),
                };
                Ok(HostMessage::Reject(reject))
            }
            HOST_PATCH => {
                let dev_session = DevSessionId::decode(dec)?;
                let target_runtime = RuntimeSessionId::decode(dec)?;
                let base_revision = read_revision(dec)?;
                let next_revision = read_revision(dec)?;
                let build_id = BuildId::decode(dec)?;
                let count = read_count(dec, MAX_SECTIONS, "patch sections")?;
                if count > 0 {
                    let tag = dec.read_u8()?;
                    return Err(match Domain::from_tag(tag) {
                        Some(domain) => WireError::UnsupportedDomain(domain),
                        None => malformed(dec),
                    });
                }
                Ok(HostMessage::Patch(Box::new(PatchBundle {
                    dev_session,
                    target_runtime,
                    base_revision,
                    next_revision,
                    build_id,
                    sections: Vec::new(),
                })))
            }
            _ => Err(malformed(dec)),
        }
    }
}

impl Message for RuntimeMessage {
    fn write(&self, enc: &mut Encoder) {
        match self {
            RuntimeMessage::Hello(hello) => {
                enc.write_u8(RUNTIME_HELLO);
                ProtocolTag::current().encode(enc);
                enc.write_u16(hello.protocol_version);
                write_string(enc, &hello.token);
                hello.dev_session.encode(enc);
                hello.runtime_session.encode(enc);
                hello.build_id.encode(enc);
                write_revision(enc, hello.current_revision);
                hello.schema_fingerprint.encode(enc);
                enc.write_varint(u64::from(hello.capabilities.0));
                enc.write_u8(hello.target as u8);
            }
            RuntimeMessage::Ack(ack) => {
                enc.write_u8(RUNTIME_ACK);
                write_revision(enc, ack.revision);
                enc.write_varint(u64::from(ack.applied_domains.0));
                enc.write_varint(u64::from(ack.scoped_resets));
                enc.write_varint(ack.timings.decode_us);
                enc.write_varint(ack.timings.stage_us);
                enc.write_varint(ack.timings.commit_us);
            }
            RuntimeMessage::Nack(nack) => {
                enc.write_u8(RUNTIME_NACK);
                write_revision(enc, nack.base_revision);
                write_revision(enc, nack.candidate_revision);
                enc.write_u8(nack.stage as u8);
                enc.write_varint(nack.diagnostic_codes.len() as u64);
                for code in &nack.diagnostic_codes {
                    write_string(enc, code);
                }
                write_revision(enc, nack.last_good_revision);
            }
            RuntimeMessage::Log { level, line } => {
                enc.write_u8(RUNTIME_LOG);
                enc.write_u8(*level as u8);
                write_string(enc, line);
            }
            RuntimeMessage::Dropped { count } => {
                enc.write_u8(RUNTIME_DROPPED);
                enc.write_varint(u64::from(*count));
            }
            RuntimeMessage::InAppReload(report) => {
                enc.write_u8(RUNTIME_IN_APP_RELOAD);
                enc.write_bytes(report);
            }
        }
    }

    fn read(dec: &mut Decoder<'_>) -> Result<Self, WireError> {
        match dec.read_u8()? {
            RUNTIME_HELLO => {
                let protocol_version = read_version(dec)?;
                let token = read_string(dec, MAX_TOKEN, "session token")?;
                let dev_session = DevSessionId::decode(dec)?;
                let runtime_session = RuntimeSessionId::decode(dec)?;
                let build_id = BuildId::decode(dec)?;
                let current_revision = read_revision(dec)?;
                let schema_fingerprint = SchemaFingerprint::decode(dec)?;
                let capabilities = read_domains(dec)?;
                let target = match dec.read_u8()? {
                    0 => RuntimeTarget::DesktopHost,
                    1 => RuntimeTarget::AndroidEmulator,
                    2 => RuntimeTarget::IosSimulator,
                    3 => RuntimeTarget::Web,
                    _ => return Err(malformed(dec)),
                };
                Ok(RuntimeMessage::Hello(RuntimeHello {
                    protocol_version,
                    token,
                    dev_session,
                    runtime_session,
                    build_id,
                    current_revision,
                    schema_fingerprint,
                    capabilities,
                    target,
                }))
            }
            RUNTIME_ACK => Ok(RuntimeMessage::Ack(PatchAck {
                revision: read_revision(dec)?,
                applied_domains: read_domains(dec)?,
                scoped_resets: read_u32(dec)?,
                timings: PatchTimings {
                    decode_us: dec.read_varint()?,
                    stage_us: dec.read_varint()?,
                    commit_us: dec.read_varint()?,
                },
            })),
            RUNTIME_NACK => {
                let base_revision = read_revision(dec)?;
                let candidate_revision = read_revision(dec)?;
                let stage = *Stage::ALL
                    .get(usize::from(dec.read_u8()?))
                    .ok_or_else(|| malformed(dec))?;
                let count = read_count(dec, MAX_CODES, "diagnostic codes")?;
                let mut diagnostic_codes = Vec::with_capacity(count);
                for _ in 0..count {
                    diagnostic_codes.push(read_string(dec, MAX_CODE, "diagnostic code")?);
                }
                Ok(RuntimeMessage::Nack(PatchNack {
                    base_revision,
                    candidate_revision,
                    stage,
                    diagnostic_codes,
                    last_good_revision: read_revision(dec)?,
                }))
            }
            RUNTIME_LOG => {
                let level = match dec.read_u8()? {
                    0 => LogLevel::Info,
                    1 => LogLevel::Warn,
                    2 => LogLevel::Error,
                    _ => return Err(malformed(dec)),
                };
                Ok(RuntimeMessage::Log {
                    level,
                    line: read_string(dec, MAX_LOG, "log line")?,
                })
            }
            RUNTIME_DROPPED => Ok(RuntimeMessage::Dropped {
                count: read_u32(dec)?,
            }),
            RUNTIME_IN_APP_RELOAD => Ok(RuntimeMessage::InAppReload(dec.read_bytes()?.to_vec())),
            _ => Err(malformed(dec)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime_hello() -> RuntimeHello {
        RuntimeHello {
            protocol_version: DEV_PROTOCOL_VERSION,
            token: "0123456789abcdef0123456789abcdef".into(),
            dev_session: DevSessionId(7),
            runtime_session: RuntimeSessionId(u128::MAX - 3),
            build_id: BuildId(0xB01D << 64 | 1),
            current_revision: Revision::LAUNCH,
            schema_fingerprint: SchemaFingerprint(42),
            capabilities: Domains::NONE.with(Domain::Ui).with(Domain::State),
            target: RuntimeTarget::DesktopHost,
        }
    }

    fn identity() -> RuntimeIdentity {
        let hello = runtime_hello();
        RuntimeIdentity {
            dev_session: hello.dev_session,
            runtime_session: hello.runtime_session,
            build_id: hello.build_id,
            capabilities: hello.capabilities,
            current_revision: Revision(3),
        }
    }

    fn patch(base: u64, next: u64) -> PatchBundle {
        let id = identity();
        PatchBundle {
            dev_session: id.dev_session,
            target_runtime: id.runtime_session,
            base_revision: Revision(base),
            next_revision: Revision(next),
            build_id: id.build_id,
            sections: Vec::new(),
        }
    }

    fn host_messages() -> Vec<HostMessage> {
        let hello = runtime_hello();
        vec![
            HostMessage::Hello(HostHello {
                protocol_version: DEV_PROTOCOL_VERSION,
                dev_session: hello.dev_session,
                runtime_session: hello.runtime_session,
                project_fingerprint: ProjectFingerprint(9),
                expected_build_id: hello.build_id,
            }),
            HostMessage::Reject(Reject::Protocol { host: 2 }),
            HostMessage::Reject(Reject::Build),
            HostMessage::Reject(Reject::Session),
            HostMessage::Patch(Box::new(patch(3, 4))),
        ]
    }

    fn runtime_messages() -> Vec<RuntimeMessage> {
        vec![
            RuntimeMessage::Hello(runtime_hello()),
            RuntimeMessage::Ack(PatchAck {
                revision: Revision(4),
                applied_domains: Domains::NONE.with(Domain::Ui),
                scoped_resets: 2,
                timings: PatchTimings {
                    decode_us: 1,
                    stage_us: 20,
                    commit_us: 300,
                },
            }),
            RuntimeMessage::Nack(PatchNack {
                base_revision: Revision(2),
                candidate_revision: Revision(4),
                stage: Stage::SnapshotRestore,
                diagnostic_codes: vec![NACK_REVISION_MISMATCH.into(), "E2103".into()],
                last_good_revision: Revision(3),
            }),
            RuntimeMessage::Log {
                level: LogLevel::Warn,
                line: "ü".repeat(10),
            },
            RuntimeMessage::Dropped { count: u32::MAX },
            RuntimeMessage::InAppReload(vec![1, 2, 3]),
        ]
    }

    fn round_trip<M: Message + PartialEq + fmt::Debug>(message: &M) {
        let mut stream = Vec::new();
        write_frame(&mut stream, message).unwrap();
        let mut buf = Vec::new();
        let read: Option<M> = read_frame(&mut stream.as_slice(), &mut buf).unwrap();
        assert_eq!(read.as_ref(), Some(message));
    }

    #[test]
    fn every_message_round_trips() {
        host_messages().iter().for_each(round_trip);
        runtime_messages().iter().for_each(round_trip);
        for stage in Stage::ALL {
            round_trip(&RuntimeMessage::Nack(PatchNack {
                stage,
                ..match &runtime_messages()[2] {
                    RuntimeMessage::Nack(nack) => nack.clone(),
                    _ => unreachable!(),
                }
            }));
        }
        for target in [
            RuntimeTarget::DesktopHost,
            RuntimeTarget::AndroidEmulator,
            RuntimeTarget::IosSimulator,
            RuntimeTarget::Web,
        ] {
            round_trip(&RuntimeMessage::Hello(RuntimeHello {
                target,
                ..runtime_hello()
            }));
        }
    }

    #[test]
    fn ids_spell_as_32_hex_digits() {
        let id = BuildId(0x0123_4567_89ab_cdef_0011_2233_4455_6677);
        assert_eq!(id.to_hex(), "0123456789abcdef0011223344556677");
        assert_eq!(BuildId::from_hex(&id.to_hex()), Some(id));
        assert_eq!(BuildId::from_hex("0123"), None);
        assert_eq!(BuildId::from_hex(&"g".repeat(32)), None);
        assert_eq!(BuildId::from_hex(&"+1".repeat(16)), None);
    }

    /// The frame body of `message`, edited.
    fn body_with(message: &impl Message, edit: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
        let mut body = encode(message);
        edit(&mut body);
        body
    }

    #[test]
    fn every_bound_is_checked_before_the_read() {
        let long_token = RuntimeMessage::Hello(RuntimeHello {
            token: "t".repeat(MAX_TOKEN + 1),
            ..runtime_hello()
        });
        assert_eq!(
            RuntimeMessage::from_frame(&encode(&long_token)),
            Err(WireError::Bound {
                what: "session token"
            })
        );
        let nack = |codes: Vec<String>| {
            RuntimeMessage::Nack(PatchNack {
                base_revision: Revision(1),
                candidate_revision: Revision(2),
                stage: Stage::Parse,
                diagnostic_codes: codes,
                last_good_revision: Revision(1),
            })
        };
        assert_eq!(
            RuntimeMessage::from_frame(&encode(&nack(vec!["E".into(); MAX_CODES + 1]))),
            Err(WireError::Bound {
                what: "diagnostic codes"
            })
        );
        assert_eq!(
            RuntimeMessage::from_frame(&encode(&nack(vec!["E".repeat(MAX_CODE + 1)]))),
            Err(WireError::Bound {
                what: "diagnostic code"
            })
        );
        let log = RuntimeMessage::Log {
            level: LogLevel::Info,
            line: "x".repeat(MAX_LOG + 1),
        };
        assert_eq!(
            RuntimeMessage::from_frame(&encode(&log)),
            Err(WireError::Bound { what: "log line" })
        );
        // A section count over the cap, then one within the cap but over the
        // bytes left.
        let sections = |count: u64| {
            body_with(&HostMessage::Patch(Box::new(patch(3, 4))), |body| {
                body.pop();
                let mut enc = Encoder::new();
                enc.write_varint(count);
                body.extend_from_slice(enc.as_bytes());
            })
        };
        assert_eq!(
            HostMessage::from_frame(&sections(MAX_SECTIONS as u64 + 1)),
            Err(WireError::Bound {
                what: "patch sections"
            })
        );
        assert!(matches!(
            HostMessage::from_frame(&sections(2)),
            Err(WireError::Malformed(_))
        ));
    }

    #[test]
    fn a_frame_over_the_cap_is_refused_unread() {
        let mut stream = ((MAX_FRAME + 1) as u32).to_le_bytes().to_vec();
        let mut buf = Vec::new();
        let read = read_frame::<HostMessage>(&mut stream.as_slice(), &mut buf);
        assert!(matches!(
            read,
            Err(FrameError::Wire(WireError::FrameTooLarge { .. }))
        ));
        assert!(buf.capacity() < MAX_FRAME, "the body was not allocated");
        let huge = RuntimeMessage::InAppReload(vec![0; MAX_FRAME]);
        stream.clear();
        assert!(write_frame(&mut stream, &huge).is_err());
        assert!(stream.is_empty(), "nothing was written");
    }

    #[test]
    fn a_truncated_frame_is_an_error_at_every_cut() {
        for body in host_messages().iter().map(encode) {
            for cut in 0..body.len() {
                assert!(
                    HostMessage::from_frame(&body[..cut]).is_err(),
                    "{body:?} at {cut}"
                );
            }
        }
        for body in runtime_messages().iter().map(encode) {
            for cut in 0..body.len() {
                assert!(
                    RuntimeMessage::from_frame(&body[..cut]).is_err(),
                    "{body:?} at {cut}"
                );
            }
        }
        // A stream cut inside a frame is an I/O error, between frames the end.
        let mut stream = Vec::new();
        write_frame(&mut stream, &RuntimeMessage::Dropped { count: 1 }).unwrap();
        let mut buf = Vec::new();
        let short = &stream[..stream.len() - 1];
        assert!(matches!(
            read_frame::<RuntimeMessage>(&mut &short[..], &mut buf),
            Err(FrameError::Io(_))
        ));
        assert!(matches!(
            read_frame::<RuntimeMessage>(&mut &[][..], &mut buf),
            Ok(None)
        ));
    }

    #[test]
    fn corrupted_bytes_never_panic() {
        let bodies: Vec<Vec<u8>> = host_messages()
            .iter()
            .map(encode)
            .chain(runtime_messages().iter().map(encode))
            .collect();
        for body in &bodies {
            for at in 0..body.len() {
                for byte in [0x00, 0x7F, 0x80, 0xFF] {
                    let mut corrupt = body.clone();
                    corrupt[at] = byte;
                    let _ = HostMessage::from_frame(&corrupt);
                    let _ = RuntimeMessage::from_frame(&corrupt);
                }
            }
        }
        assert!(matches!(
            HostMessage::from_frame(&[9]),
            Err(WireError::Malformed(_))
        ));
        assert!(matches!(
            RuntimeMessage::from_frame(&[9]),
            Err(WireError::Malformed(_))
        ));
        // A known domain bit past the table is not a domain.
        let ack = body_with(&runtime_messages()[1], |body| body[2] = 0x40);
        assert!(RuntimeMessage::from_frame(&ack).is_err());
    }

    #[test]
    fn another_protocol_version_is_named_not_guessed() {
        // A runtime of version 3 with a layout this build does not know.
        let mut body = vec![RUNTIME_HELLO];
        let mut enc = Encoder::new();
        ProtocolTag::current().encode(&mut enc);
        enc.write_u16(3);
        enc.write_raw(&[0xEE; 7]);
        body.extend_from_slice(enc.as_bytes());
        assert_eq!(
            RuntimeMessage::from_frame(&body),
            Err(WireError::Version { peer: 3 })
        );
        // A host of another version is named the same way.
        body[0] = HOST_HELLO;
        assert_eq!(
            HostMessage::from_frame(&body),
            Err(WireError::Version { peer: 3 })
        );
        // A reject reads whatever version the host speaks.
        let reject = body_with(&HostMessage::Reject(Reject::Build), |body| {
            body[5] = 9;
        });
        assert_eq!(
            HostMessage::from_frame(&reject),
            Ok(HostMessage::Reject(Reject::Build))
        );
        // Not a dev stream at all.
        let foreign = body_with(&runtime_messages()[0], |body| body[1] = b'X');
        assert_eq!(
            RuntimeMessage::from_frame(&foreign),
            Err(WireError::NotDevStream)
        );
    }

    #[test]
    fn a_reserved_domain_is_named() {
        let body = body_with(&HostMessage::Patch(Box::new(patch(3, 4))), |body| {
            *body.last_mut().unwrap() = 1;
            body.push(Domain::Shader as u8);
        });
        let error = HostMessage::from_frame(&body).unwrap_err();
        assert_eq!(error, WireError::UnsupportedDomain(Domain::Shader));
        let nack = identity().undecodable(error);
        assert_eq!(nack.diagnostic_codes, [NACK_UNSUPPORTED_DOMAIN]);
        assert_eq!(nack.last_good_revision, Revision(3));
        let nack = identity().undecodable(WireError::NotDevStream);
        assert_eq!(nack.diagnostic_codes, [NACK_MALFORMED]);
    }

    #[test]
    fn a_patch_applies_only_from_the_current_revision_forward() {
        let id = identity();
        assert_eq!(id.check(&patch(3, 4)), Ok(()));
        assert_eq!(
            id.check(&patch(3, 9)),
            Ok(()),
            "the host may skip revisions"
        );
        let code = |p: PatchBundle| {
            let nack = id.check(&p).unwrap_err();
            assert_eq!(nack.last_good_revision, Revision(3), "last-good kept");
            assert_eq!(
                (nack.base_revision, nack.candidate_revision),
                (p.base_revision, p.next_revision)
            );
            (nack.stage, nack.diagnostic_codes[0].clone())
        };
        for (base, next) in [(2, 4), (4, 5), (0, 1)] {
            assert_eq!(
                code(patch(base, next)),
                (Stage::RuntimeStage, NACK_REVISION_MISMATCH.into())
            );
        }
        for next in [3, 2] {
            assert_eq!(
                code(patch(3, next)),
                (Stage::RuntimeStage, NACK_REVISION_ORDER.into())
            );
        }
        let other = |edit: fn(&mut PatchBundle)| {
            let mut p = patch(3, 4);
            edit(&mut p);
            code(p)
        };
        assert_eq!(
            other(|p| p.dev_session = DevSessionId(8)),
            (Stage::Transport, NACK_UNKNOWN_SESSION.into())
        );
        assert_eq!(
            other(|p| p.target_runtime = RuntimeSessionId(8)),
            (Stage::Transport, NACK_UNKNOWN_SESSION.into())
        );
        assert_eq!(
            other(|p| p.build_id = BuildId(8)),
            (Stage::Transport, NACK_BUILD_MISMATCH.into())
        );
    }

    #[test]
    fn the_handshake_binds_token_session_and_build() {
        let hello = runtime_hello();
        let expect = HostExpect {
            token: &hello.token,
            dev_session: hello.dev_session,
            build_id: hello.build_id,
        };
        assert_eq!(accept_runtime(&hello, &expect), Ok(()));
        let wrong_token = RuntimeHello {
            token: "0123456789abcdef0123456789abcdee".into(),
            ..hello.clone()
        };
        assert_eq!(accept_runtime(&wrong_token, &expect), Err(None));
        let short_token = RuntimeHello {
            token: "0".into(),
            ..hello.clone()
        };
        assert_eq!(accept_runtime(&short_token, &expect), Err(None));
        let other_session = RuntimeHello {
            dev_session: DevSessionId(1),
            ..hello.clone()
        };
        assert_eq!(
            accept_runtime(&other_session, &expect),
            Err(Some(Reject::Session))
        );
        let other_build = RuntimeHello {
            build_id: BuildId(1),
            ..hello.clone()
        };
        assert_eq!(
            accept_runtime(&other_build, &expect),
            Err(Some(Reject::Build))
        );

        let host = HostHello {
            protocol_version: DEV_PROTOCOL_VERSION,
            dev_session: hello.dev_session,
            runtime_session: hello.runtime_session,
            project_fingerprint: ProjectFingerprint(1),
            expected_build_id: hello.build_id,
        };
        assert_eq!(accept_host(&host, &hello), Ok(()));
        let cases = [
            (
                HostHello {
                    protocol_version: 1,
                    ..host.clone()
                },
                Reject::Protocol { host: 1 },
            ),
            (
                HostHello {
                    runtime_session: RuntimeSessionId(5),
                    ..host.clone()
                },
                Reject::Session,
            ),
            (
                HostHello {
                    expected_build_id: BuildId(5),
                    ..host.clone()
                },
                Reject::Build,
            ),
        ];
        for (answer, reject) in cases {
            assert_eq!(accept_host(&answer, &hello), Err(reject));
        }
    }
}
