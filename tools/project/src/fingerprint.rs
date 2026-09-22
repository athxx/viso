//! Content-addressed identity: [`ProjectFingerprint`] and [`BuildId`]
//! (`Viso_CLI.md` section 46).
//!
//! A [`BuildId`] answers "is this artifact the one I think it is". The T5 handshake
//! compares it across a process boundary before accepting a patch, the cache uses
//! it to decide reuse, and traces are filed under it — so it must be derived from
//! content, never from a clock. A timestamp would make two identical builds
//! different and two different builds indistinguishable after a `touch`, which is
//! exactly backwards.
//!
//! The hash is FNV-1a over 128 bits, the same choice and the same constants as
//! `viso_dsl`'s symbol identity. It is not `DefaultHasher`: that algorithm is
//! explicitly unspecified across Rust versions, and an id that changes when the
//! compiler is upgraded would invalidate every cache entry and reject every live
//! reconnect for no reason. FNV is also not a cryptographic hash and is not used as
//! one — nothing here defends against an adversary choosing inputs.
//!
//! Every fingerprint is prefixed by [`FINGERPRINT_VERSION`]. When the *derivation*
//! changes, that constant changes, and every id changes with it — which is what
//! stops a stale cache entry from being mistaken for a fresh one.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::diag::{ConfigCode, ConfigDiagnostic};
use crate::target::{HostOs, Profile, Target};

/// The version of the fingerprint derivation itself. Bump on any change to what is
/// hashed or in what order.
pub const FINGERPRINT_VERSION: u32 = 1;

/// FNV-1a 128-bit offset basis, high half.
const OFFSET_BASIS_HI: u64 = 0x6c62_272e_07bb_0142;
/// FNV-1a 128-bit offset basis, low half.
const OFFSET_BASIS_LO: u64 = 0x62b8_2175_6295_c58d;
/// FNV-1a 128-bit prime, high half.
const PRIME_HI: u64 = 0x0000_0000_0100_0000;
/// FNV-1a 128-bit prime, low half.
const PRIME_LO: u64 = 0x0000_0000_0000_013b;

/// Bytes read per `read` call while hashing a file.
///
/// Fingerprinting must not scale its peak memory with the largest file in the
/// project: a 200 MB asset is hashed through a 64 KiB window like everything else.
const CHUNK: usize = 64 * 1024;

/// A 128-bit content hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(C)]
pub struct Hash128 {
    /// High 64 bits.
    pub hi: u64,
    /// Low 64 bits.
    pub lo: u64,
}

impl Hash128 {
    /// Builds a hash from its halves.
    pub const fn from_parts(hi: u64, lo: u64) -> Self {
        Self { hi, lo }
    }

    /// The 32-character lowercase hex form — the spelling used in paths, on the
    /// wire, and in every message.
    pub fn to_hex(self) -> String {
        format!("{:016x}{:016x}", self.hi, self.lo)
    }
}

impl fmt::Display for Hash128 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}{:016x}", self.hi, self.lo)
    }
}

/// Incremental FNV-1a-128.
///
/// Every multi-byte input goes through [`Hasher::chunk`], which length-prefixes.
/// Without that, `["ab", "c"]` and `["a", "bc"]` would hash identically, and a
/// project with files `ab`/`c` would be indistinguishable from one with `a`/`bc`.
#[derive(Debug, Clone)]
pub struct Hasher {
    hi: u64,
    lo: u64,
}

impl Default for Hasher {
    fn default() -> Self {
        Self::new()
    }
}

impl Hasher {
    /// A hasher at the offset basis.
    pub const fn new() -> Self {
        Self {
            hi: OFFSET_BASIS_HI,
            lo: OFFSET_BASIS_LO,
        }
    }

    /// FNV-1a step: `hash = (hash XOR byte) * prime`, modulo 2^128.
    #[inline]
    pub fn byte(&mut self, byte: u8) {
        let mut hash = ((self.hi as u128) << 64) | (self.lo as u128);
        hash ^= byte as u128;
        let prime = ((PRIME_HI as u128) << 64) | (PRIME_LO as u128);
        hash = hash.wrapping_mul(prime);
        self.hi = (hash >> 64) as u64;
        self.lo = hash as u64;
    }

    /// Absorbs bytes without a length prefix. Use [`Hasher::chunk`] for anything
    /// that sits next to another field.
    #[inline]
    pub fn bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.byte(b);
        }
    }

    /// Absorbs a length-prefixed field, so adjacent fields cannot be confused by
    /// concatenation.
    #[inline]
    pub fn chunk(&mut self, bytes: &[u8]) {
        self.bytes(&(bytes.len() as u64).to_le_bytes());
        self.bytes(bytes);
    }

    /// Absorbs a length-prefixed string.
    #[inline]
    pub fn str(&mut self, text: &str) {
        self.chunk(text.as_bytes());
    }

    /// Absorbs a fixed-width integer (no length prefix needed).
    #[inline]
    pub fn u64(&mut self, value: u64) {
        self.bytes(&value.to_le_bytes());
    }

    /// Absorbs another hash.
    #[inline]
    pub fn hash(&mut self, hash: Hash128) {
        self.u64(hash.hi);
        self.u64(hash.lo);
    }

    /// The hash of everything absorbed so far.
    pub const fn finish(&self) -> Hash128 {
        Hash128 {
            hi: self.hi,
            lo: self.lo,
        }
    }
}

/// The identity of a project's source graph.
///
/// Covers the manifest plus every `.rs` and `.vs` file under the root, by relative
/// path and content. Two checkouts of the same commit produce the same
/// fingerprint on any machine; a one-byte edit anywhere produces a different one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectFingerprint(Hash128);

impl ProjectFingerprint {
    /// Walks `root` and fingerprints its source graph.
    pub fn compute(root: impl AsRef<Path>) -> Result<Self, ConfigDiagnostic> {
        let root = root.as_ref();
        let sources = source_graph(root)?;
        let mut hasher = Hasher::new();
        hasher.u64(FINGERPRINT_VERSION as u64);
        hasher.u64(sources.len() as u64);
        for path in &sources {
            // The relative path with `/` separators, so the same tree fingerprints
            // identically on Windows and Unix.
            hasher.str(&relative_slash(root, path));
            hasher.hash(hash_file(path)?);
        }
        Ok(Self(hasher.finish()))
    }

    /// Fingerprints an explicit `(relative path, content)` list.
    ///
    /// The pure form of [`ProjectFingerprint::compute`]: the same derivation with no
    /// filesystem, which is how the derivation's properties are tested without
    /// building a directory tree for each one. The caller supplies the order; use
    /// sorted paths to match the walk.
    pub fn from_sources<'p, 'c>(sources: impl IntoIterator<Item = (&'p str, &'c [u8])>) -> Self {
        let entries: Vec<(&str, &[u8])> = sources.into_iter().collect();
        let mut hasher = Hasher::new();
        hasher.u64(FINGERPRINT_VERSION as u64);
        hasher.u64(entries.len() as u64);
        for (path, content) in entries {
            hasher.str(path);
            hasher.hash(content_hash(content));
        }
        Self(hasher.finish())
    }

    /// The underlying hash.
    pub const fn hash(self) -> Hash128 {
        self.0
    }
}

impl fmt::Display for ProjectFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Compiler and host identity, one of the [`BuildId`] inputs.
///
/// Plain data supplied by the caller rather than something this crate detects: a
/// resolution library that shells out to `rustc` would make `viso config show`
/// depend on a subprocess, and the CLI already knows its own toolchain
/// (`Viso_CLI.md` section 64).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Toolchain {
    /// The compiler version string, e.g. `rustc 1.98.1 (abcdef 2026-01-01)`.
    pub rustc: String,
    /// The host triple or equivalent, e.g. `aarch64-macos`.
    pub host: String,
}

impl Toolchain {
    /// Builds a toolchain identity.
    pub fn new(rustc: impl Into<String>, host: impl Into<String>) -> Self {
        Self {
            rustc: rustc.into(),
            host: host.into(),
        }
    }

    /// `<arch>-<os>` for the machine this binary was compiled for, or
    /// `<arch>-unknown` on an OS with no Tier-1 path.
    pub fn current_host() -> String {
        let os = HostOs::current().map_or("unknown", |os| os.as_str());
        format!("{}-{os}", std::env::consts::ARCH)
    }

    fn absorb(&self, hasher: &mut Hasher) {
        hasher.str(&self.rustc);
        hasher.str(&self.host);
    }
}

impl fmt::Display for Toolchain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.rustc, self.host)
    }
}

/// The identity of one build or dev session (`Viso_CLI.md` section 46).
///
/// Binds the five inputs that can make two artifacts behave differently: the
/// source graph, the target, the profile, the toolchain, and the resolved
/// configuration. Nothing else belongs here — adding an input that does not change
/// behavior would invalidate caches for free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BuildId(Hash128);

impl BuildId {
    /// Derives the id from its five inputs.
    pub fn compute(
        fingerprint: ProjectFingerprint,
        target: Target,
        profile: Profile,
        toolchain: &Toolchain,
        config: Hash128,
    ) -> Self {
        let mut hasher = Hasher::new();
        hasher.u64(FINGERPRINT_VERSION as u64);
        hasher.hash(fingerprint.hash());
        hasher.str(target.as_str());
        hasher.str(profile.as_str());
        toolchain.absorb(&mut hasher);
        hasher.hash(config);
        Self(hasher.finish())
    }

    /// The underlying hash.
    pub const fn hash(self) -> Hash128 {
        self.0
    }

    /// The 32-character hex form used in paths and on the wire.
    pub fn to_hex(self) -> String {
        self.0.to_hex()
    }
}

impl fmt::Display for BuildId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Source-file extensions that participate in the fingerprint.
///
/// The two first-class source formats (AGENTS 32). Assets are deliberately out:
/// they are hot-reloadable independently (T7) and a large binary asset should not
/// invalidate the code identity that the handshake compares.
const SOURCE_EXTENSIONS: &[&str] = &["rs", "vs"];

/// Directory names never descended into.
///
/// `target` holds the outputs whose identity we are computing — including it would
/// make the fingerprint depend on itself.
const SKIP_DIRS: &[&str] = &["target", "node_modules", "dist"];

/// Every fingerprinted file under `root`, sorted by relative path.
///
/// Sorted, not walk-ordered: `read_dir` order is filesystem-defined, so an
/// unsorted list would make the same tree fingerprint differently on two machines.
fn source_graph(root: &Path) -> Result<Vec<PathBuf>, ConfigDiagnostic> {
    let mut out = Vec::new();
    let manifest = root.join(crate::discovery::MANIFEST_NAME);
    if manifest.is_file() {
        out.push(manifest);
    }
    // An explicit stack rather than recursion: a deep tree must not risk the
    // process stack, and the traversal order does not matter because the result is
    // sorted.
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|err| {
            ConfigDiagnostic::error(
                ConfigCode::ManifestUnreadable,
                format!("could not read `{}`: {err}", dir.display()),
            )
            .at(&dir)
        })?;
        for entry in entries {
            let entry = entry.map_err(|err| {
                ConfigDiagnostic::error(
                    ConfigCode::ManifestUnreadable,
                    format!("could not read an entry in `{}`: {err}", dir.display()),
                )
                .at(&dir)
            })?;
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // `file_type` does not follow symlinks, so a link pointing back up the
            // tree is skipped rather than walked into forever.
            let file_type = entry.file_type().map_err(|err| {
                ConfigDiagnostic::error(
                    ConfigCode::ManifestUnreadable,
                    format!("could not inspect `{}`: {err}", path.display()),
                )
                .at(&path)
            })?;
            if file_type.is_dir() {
                if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
                    continue;
                }
                stack.push(path);
            } else if file_type.is_file() {
                let is_source = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| SOURCE_EXTENSIONS.contains(&e));
                if is_source {
                    out.push(path);
                }
            }
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// The content hash of a byte slice: the bytes, then their length.
///
/// The length is a *suffix* rather than a prefix — unusual, and deliberate. It makes
/// the framing unambiguous exactly as a prefix would, while staying computable in
/// one forward pass over a stream whose size is not known until the end. That is
/// what lets [`hash_file`] hash a file it never fully holds in memory and still
/// produce the same value as this function.
fn content_hash(bytes: &[u8]) -> Hash128 {
    let mut hasher = Hasher::new();
    hasher.bytes(bytes);
    hasher.u64(bytes.len() as u64);
    hasher.finish()
}

/// The content hash of one file, read through a bounded window.
fn hash_file(path: &Path) -> Result<Hash128, ConfigDiagnostic> {
    let mut file = std::fs::File::open(path).map_err(|err| {
        ConfigDiagnostic::error(
            ConfigCode::ManifestUnreadable,
            format!("could not open `{}`: {err}", path.display()),
        )
        .at(path)
    })?;
    let mut hasher = Hasher::new();
    let mut buffer = vec![0u8; CHUNK];
    let mut total = 0u64;
    loop {
        let read = file.read(&mut buffer).map_err(|err| {
            ConfigDiagnostic::error(
                ConfigCode::ManifestUnreadable,
                format!("could not read `{}`: {err}", path.display()),
            )
            .at(path)
        })?;
        if read == 0 {
            break;
        }
        hasher.bytes(&buffer[..read]);
        total += read as u64;
    }
    hasher.u64(total);
    Ok(hasher.finish())
}

/// `path` relative to `root`, with `/` separators.
fn relative_slash(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;

    fn toolchain() -> Toolchain {
        Toolchain::new("rustc 1.98.1", "aarch64-macos")
    }

    /// FNV-1a-128 against the reference vectors. The algorithm is pinned by test
    /// because these ids are written into paths and compared across a process
    /// boundary: a silent change to the arithmetic would break every cache entry
    /// and every reconnect at once.
    #[test]
    fn the_hash_matches_the_fnv_reference_vectors() {
        let empty = Hasher::new().finish();
        assert_eq!(
            empty,
            Hash128::from_parts(OFFSET_BASIS_HI, OFFSET_BASIS_LO),
            "no input is the offset basis"
        );

        let mut h = Hasher::new();
        h.bytes(b"a");
        assert_eq!(h.finish().to_hex(), "d228cb696f1a8caf78912b704e4a8964");

        let mut h = Hasher::new();
        h.bytes(b"foobar");
        assert_eq!(h.finish().to_hex(), "343e1662793c64bf6f0d3597ba446f18");
    }

    /// Length prefixing is what makes a tuple of fields unambiguous. Without it a
    /// project with files `ab`, `c` and one with `a`, `bc` would share an id.
    #[test]
    fn adjacent_fields_cannot_collide_by_concatenation() {
        let mut a = Hasher::new();
        a.str("ab");
        a.str("c");

        let mut b = Hasher::new();
        b.str("a");
        b.str("bc");

        assert_ne!(a.finish(), b.finish());

        let split = ProjectFingerprint::from_sources([("ab", b"x".as_slice()), ("c", b"y")]);
        let other = ProjectFingerprint::from_sources([("a", b"x".as_slice()), ("bc", b"y")]);
        assert_ne!(split, other);
    }

    /// The property the fingerprint exists for: identical content, identical id;
    /// any change anywhere, different id.
    #[test]
    fn the_fingerprint_is_content_addressed() {
        let base = || {
            ProjectFingerprint::from_sources([
                ("Viso.toml", b"[package]\nname = \"a\"\n".as_slice()),
                ("src/main.rs", b"fn main() {}".as_slice()),
            ])
        };
        assert_eq!(base(), base(), "the same input twice is the same id");

        // Content change.
        assert_ne!(
            base(),
            ProjectFingerprint::from_sources([
                ("Viso.toml", b"[package]\nname = \"a\"\n".as_slice()),
                ("src/main.rs", b"fn main() { }".as_slice()),
            ])
        );
        // Path change with identical content.
        assert_ne!(
            base(),
            ProjectFingerprint::from_sources([
                ("Viso.toml", b"[package]\nname = \"a\"\n".as_slice()),
                ("src/lib.rs", b"fn main() {}".as_slice()),
            ])
        );
        // A file added.
        assert_ne!(
            base(),
            ProjectFingerprint::from_sources([
                ("Viso.toml", b"[package]\nname = \"a\"\n".as_slice()),
                ("src/main.rs", b"fn main() {}".as_slice()),
                ("src/view.vs", b"Text {}".as_slice()),
            ])
        );
        // An empty file is not the same as no file.
        assert_ne!(
            ProjectFingerprint::from_sources([("a.rs", b"".as_slice())]),
            ProjectFingerprint::from_sources(std::iter::empty::<(&str, &[u8])>())
        );
    }

    /// Every one of the five [`BuildId`] inputs must move the id — an input that did
    /// not would let two artifacts that behave differently share an identity, which
    /// is precisely the mismatch the T5 handshake exists to catch.
    #[test]
    fn every_build_id_input_changes_the_id() {
        let fp = ProjectFingerprint::from_sources([("a.rs", b"one".as_slice())]);
        let other_fp = ProjectFingerprint::from_sources([("a.rs", b"two".as_slice())]);
        let cfg = Hash128::from_parts(1, 2);
        let base = BuildId::compute(fp, Target::Host, Profile::Dev, &toolchain(), cfg);

        assert_eq!(
            base,
            BuildId::compute(fp, Target::Host, Profile::Dev, &toolchain(), cfg),
            "the derivation is deterministic"
        );

        assert_ne!(
            base,
            BuildId::compute(other_fp, Target::Host, Profile::Dev, &toolchain(), cfg),
            "source graph"
        );
        assert_ne!(
            base,
            BuildId::compute(fp, Target::Headless, Profile::Dev, &toolchain(), cfg),
            "target"
        );
        assert_ne!(
            base,
            BuildId::compute(fp, Target::Host, Profile::Release, &toolchain(), cfg),
            "profile"
        );
        assert_ne!(
            base,
            BuildId::compute(
                fp,
                Target::Host,
                Profile::Dev,
                &Toolchain::new("rustc 1.99.0", "aarch64-macos"),
                cfg
            ),
            "compiler version"
        );
        assert_ne!(
            base,
            BuildId::compute(
                fp,
                Target::Host,
                Profile::Dev,
                &Toolchain::new("rustc 1.98.1", "x86_64-linux"),
                cfg
            ),
            "host"
        );
        assert_ne!(
            base,
            BuildId::compute(
                fp,
                Target::Host,
                Profile::Dev,
                &toolchain(),
                Hash128::from_parts(1, 3)
            ),
            "resolved configuration"
        );
    }

    #[test]
    fn a_build_id_prints_as_thirty_two_hex_characters() {
        let id = BuildId::compute(
            ProjectFingerprint::from_sources([("a.rs", b"x".as_slice())]),
            Target::Host,
            Profile::Dev,
            &toolchain(),
            Hash128::from_parts(0, 0),
        );
        let hex = id.to_hex();
        assert_eq!(hex.len(), 32);
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
        );
        assert_eq!(hex, id.to_string());
    }

    /// The walk and the pure form must agree, otherwise the property tests above
    /// describe a derivation the real one does not use.
    #[test]
    fn the_walk_agrees_with_the_pure_derivation() {
        let s = Scratch::new("fp-walk");
        let manifest = "[package]\nname = \"a\"\n";
        s.write("Viso.toml", manifest);
        s.write("src/main.rs", "fn main() {}");
        s.write("src/ui/view.vs", "Text {}");

        let walked = ProjectFingerprint::compute(s.path()).unwrap();
        let expected = ProjectFingerprint::from_sources([
            ("Viso.toml", manifest.as_bytes()),
            ("src/main.rs", b"fn main() {}".as_slice()),
            ("src/ui/view.vs", b"Text {}".as_slice()),
        ]);
        assert_eq!(walked, expected);
    }

    /// The exclusions. `target/` above all: it holds the artifacts whose identity is
    /// being computed, so including it would make the fingerprint depend on its own
    /// output and never stabilize.
    #[test]
    fn the_walk_skips_outputs_hidden_dirs_and_non_source_files() {
        let s = Scratch::new("fp-skip");
        s.write("Viso.toml", "[package]\nname = \"a\"\n");
        s.write("src/main.rs", "fn main() {}");
        let before = ProjectFingerprint::compute(s.path()).unwrap();

        s.write("target/debug/thing.rs", "generated");
        s.write("target/viso/build/x/app.rs", "generated");
        s.write(".git/hooks/pre-commit.rs", "hook");
        s.write("node_modules/pkg/index.rs", "vendored");
        s.write("dist/out.rs", "built");
        s.write("README.md", "docs");
        s.write("assets/icon.png", "not source");
        assert_eq!(
            ProjectFingerprint::compute(s.path()).unwrap(),
            before,
            "excluded paths must not move the fingerprint"
        );

        // A real source file still does.
        s.write("src/lib.rs", "pub fn f() {}");
        assert_ne!(ProjectFingerprint::compute(s.path()).unwrap(), before);
    }

    /// Directory order from `read_dir` is filesystem-defined, so the walk sorts.
    /// Two trees with the same files must agree regardless of creation order.
    #[test]
    fn the_walk_is_order_independent() {
        let a = Scratch::new("fp-order-a");
        a.write("Viso.toml", "[package]\nname = \"a\"\n");
        a.write("src/a.rs", "1");
        a.write("src/b.rs", "2");
        a.write("src/c.rs", "3");

        let b = Scratch::new("fp-order-b");
        b.write("src/c.rs", "3");
        b.write("src/b.rs", "2");
        b.write("src/a.rs", "1");
        b.write("Viso.toml", "[package]\nname = \"a\"\n");

        assert_eq!(
            ProjectFingerprint::compute(a.path()).unwrap(),
            ProjectFingerprint::compute(b.path()).unwrap()
        );
    }

    /// Files larger than one read window must hash the same as their content — the
    /// chunking is an implementation detail and must not be visible in the result.
    #[test]
    fn chunked_reads_hash_the_same_as_whole_content() {
        let s = Scratch::new("fp-large");
        let big: String = std::iter::repeat_n('x', CHUNK * 2 + 17).collect();
        s.write("Viso.toml", "[package]\nname = \"a\"\n");
        s.write("src/big.rs", &big);

        let walked = ProjectFingerprint::compute(s.path()).unwrap();
        let expected = ProjectFingerprint::from_sources([
            ("Viso.toml", "[package]\nname = \"a\"\n".as_bytes()),
            ("src/big.rs", big.as_bytes()),
        ]);
        assert_eq!(walked, expected);
    }

    #[test]
    fn the_current_host_string_is_populated() {
        let host = Toolchain::current_host();
        assert!(host.contains('-'), "{host}");
        assert!(host.starts_with(std::env::consts::ARCH));
    }
}
