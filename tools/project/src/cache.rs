//! Cache layout (`Viso_CLI.md` section 45) and advisory locks (section 44).
//!
//! Everything Viso writes lives under two directories of the project root:
//!
//! ```text
//! target/viso/
//!     build/<build-id>/        one directory per resolved build identity
//!     cache/{dsl,shader,schema,web}/
//!     dev/                     dev session state
//!     generated/               generated sources
//!     traces/                  captured traces
//!     locks/                   the lock files below
//! dist/{macos,windows,linux,headless,ios,android,web}/
//! ```
//!
//! Two directories rather than one because they have different lifetimes: `target/`
//! is disposable build state that `viso clean` may remove wholesale, and `dist/` is
//! the output a user ships and may have already copied a path from. Mixing them
//! would make `clean` either useless or dangerous.
//!
//! The locks are advisory files, not `flock`. A lock file that names its holder can
//! be *reported* — "held by pid 4123, 40 seconds old" — where a kernel lock can only
//! block or fail. For a developer tool where the common failure is a second terminal
//! or an abandoned dev server, the diagnosis is the feature.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::diag::{ConfigCode, ConfigDiagnostic};
use crate::fingerprint::BuildId;
use crate::target::{HostOs, Target};

/// The per-language caches of section 45.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CacheKind {
    /// Resolved DSL output.
    Dsl,
    /// Compiled shaders.
    Shader,
    /// Generated schemas.
    Schema,
    /// Web bundling intermediates.
    Web,
}

impl CacheKind {
    /// The directory name.
    pub const fn as_str(self) -> &'static str {
        match self {
            CacheKind::Dsl => "dsl",
            CacheKind::Shader => "shader",
            CacheKind::Schema => "schema",
            CacheKind::Web => "web",
        }
    }

    /// Every cache, for `ensure` and `clean`.
    pub const ALL: &'static [CacheKind] = &[
        CacheKind::Dsl,
        CacheKind::Shader,
        CacheKind::Schema,
        CacheKind::Web,
    ];
}

impl fmt::Display for CacheKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where everything goes, for one project.
///
/// Pure path arithmetic: constructing a `Layout` touches no disk, so a command can
/// print a path (`viso config path`) without creating anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    root: PathBuf,
    /// `None` on a host Viso does not name, which only affects the `dist/host`
    /// directory name. An unnamed host is not a reason to refuse to compute paths.
    host: Option<HostOs>,
}

impl Layout {
    /// The layout for a project root, on this host.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            host: HostOs::current(),
        }
    }

    /// The layout as it would be on another host — how the cross-platform `dist/`
    /// naming is tested without a second machine.
    pub fn with_host(root: impl Into<PathBuf>, host: HostOs) -> Self {
        Self {
            root: root.into(),
            host: Some(host),
        }
    }

    /// The project root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `target/`.
    pub fn target_dir(&self) -> PathBuf {
        self.root.join("target")
    }

    /// `target/viso/`.
    pub fn viso_dir(&self) -> PathBuf {
        self.target_dir().join("viso")
    }

    /// `target/viso/build/<build-id>/` — one directory per build identity.
    ///
    /// Keyed by the whole [`BuildId`], so two configurations never overwrite each
    /// other's output and switching back to a previous one finds it intact.
    pub fn build(&self, id: BuildId) -> PathBuf {
        self.viso_dir().join("build").join(id.to_hex())
    }

    /// `target/viso/build/`.
    pub fn builds(&self) -> PathBuf {
        self.viso_dir().join("build")
    }

    /// `target/viso/cache/<kind>/`.
    pub fn cache(&self, kind: CacheKind) -> PathBuf {
        self.viso_dir().join("cache").join(kind.as_str())
    }

    /// `target/viso/dev/`.
    pub fn dev(&self) -> PathBuf {
        self.viso_dir().join("dev")
    }

    /// `target/viso/generated/`.
    pub fn generated(&self) -> PathBuf {
        self.viso_dir().join("generated")
    }

    /// `target/viso/traces/`.
    pub fn traces(&self) -> PathBuf {
        self.viso_dir().join("traces")
    }

    /// `target/viso/locks/`.
    pub fn locks(&self) -> PathBuf {
        self.viso_dir().join("locks")
    }

    /// `dist/<platform>/` for a target.
    pub fn dist(&self, target: Target) -> PathBuf {
        self.root.join("dist").join(self.platform_dir(target))
    }

    /// `dist/`.
    pub fn dist_root(&self) -> PathBuf {
        self.root.join("dist")
    }

    /// The `dist/` subdirectory name.
    ///
    /// Named by *platform*, not by target: `host` and `headless` both produce a macOS
    /// binary on a Mac, and a user looking for their app should find one directory per
    /// thing they can ship rather than one per way of asking for it. Headless keeps its
    /// own directory because it is a distinct artifact, not a distinct platform.
    fn platform_dir(&self, target: Target) -> &'static str {
        match target {
            Target::Host => self.host.map_or("host", HostOs::as_str),
            Target::Headless => "headless",
            Target::Ios => "ios",
            Target::Android => "android",
            Target::WebGpu | Target::WebDom | Target::WebHybrid => "web",
        }
    }

    /// Creates every directory that exists independently of a build.
    ///
    /// Idempotent, and deliberately does not create `build/<id>/`: that one belongs to
    /// a build that has actually started, and pre-creating it would leave empty
    /// directories that look like completed builds.
    pub fn ensure(&self) -> Result<(), ConfigDiagnostic> {
        let mut dirs = vec![
            self.builds(),
            self.dev(),
            self.generated(),
            self.traces(),
            self.locks(),
        ];
        dirs.extend(CacheKind::ALL.iter().map(|k| self.cache(*k)));
        for dir in dirs {
            create_dir_all(&dir)?;
        }
        Ok(())
    }

    /// What `viso clean` removes, in the order it should remove it.
    ///
    /// `dist/` is not in the list. Removing a user's shipped output because they asked
    /// to clean the build cache is the kind of surprise that is only discovered after
    /// it has cost someone something; `viso clean --dist` will ask for it by name.
    pub fn cleanable(&self) -> Vec<PathBuf> {
        let mut dirs = vec![self.builds()];
        dirs.extend(CacheKind::ALL.iter().map(|k| self.cache(*k)));
        dirs.push(self.generated());
        dirs.push(self.traces());
        // `dev/` and `locks/` are live session state, not cache: removing them under a
        // running dev server would strand it.
        dirs
    }
}

/// What a lock protects (`Viso_CLI.md` section 44).
///
/// Three granularities, each keyed by the thing that would actually collide. The
/// keying matters more than the count: a single project-wide lock would serialize
/// `viso build --target host` against `viso build --target headless`, which are
/// independent builds a developer runs side by side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockKind {
    /// Writes to the build cache for one target.
    BuildCache {
        /// The target being built.
        target: Target,
    },
    /// Writes to the packaged output for one target.
    PackageOutput {
        /// The target being packaged.
        target: Target,
    },
    /// A dev session for one target and device.
    DevSession {
        /// The target being run.
        target: Target,
        /// The device, for targets that have several. `None` is the default device.
        device: Option<&'static str>,
    },
}

impl LockKind {
    /// The lock file name.
    fn file_name(self) -> String {
        match self {
            LockKind::BuildCache { target } => format!("build-{}.lock", target.as_str()),
            LockKind::PackageOutput { target } => format!("package-{}.lock", target.as_str()),
            LockKind::DevSession { target, device } => match device {
                Some(device) => format!("dev-{}-{device}.lock", target.as_str()),
                None => format!("dev-{}.lock", target.as_str()),
            },
        }
    }

    /// The human name used in diagnostics.
    fn description(self) -> String {
        match self {
            LockKind::BuildCache { target } => format!("build cache for `{}`", target.as_str()),
            LockKind::PackageOutput { target } => {
                format!("package output for `{}`", target.as_str())
            }
            LockKind::DevSession { target, device } => match device {
                Some(device) => format!("dev session for `{}` on `{device}`", target.as_str()),
                None => format!("dev session for `{}`", target.as_str()),
            },
        }
    }
}

/// Who holds a lock, as recorded in the lock file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockHolder {
    /// The process that took it.
    pub pid: u32,
    /// When, in seconds since the Unix epoch.
    pub acquired_unix: u64,
    /// What it is protecting.
    pub description: String,
}

impl LockHolder {
    /// How long ago it was taken, or `None` if the clock has moved backwards.
    pub fn age(&self) -> Option<Duration> {
        let now = unix_seconds();
        now.checked_sub(self.acquired_unix).map(Duration::from_secs)
    }

    fn encode(&self) -> String {
        format!(
            "{}\n{}\n{}\n",
            self.pid, self.acquired_unix, self.description
        )
    }

    /// Parses a lock file. A malformed file still yields a holder, because the
    /// important part of the answer is "something holds this" — refusing to parse
    /// would turn a stale lock into an unexplained failure.
    fn decode(text: &str) -> Self {
        let mut lines = text.lines();
        Self {
            pid: lines
                .next()
                .and_then(|l| l.trim().parse().ok())
                .unwrap_or(0),
            acquired_unix: lines
                .next()
                .and_then(|l| l.trim().parse().ok())
                .unwrap_or(0),
            description: lines.next().unwrap_or("unknown").trim().to_string(),
        }
    }
}

impl fmt::Display for LockHolder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "pid {}", self.pid)?;
        if let Some(age) = self.age() {
            write!(f, ", {}s ago", age.as_secs())?;
        }
        Ok(())
    }
}

/// Why a lock could not be taken.
#[derive(Debug)]
pub enum LockError {
    /// Someone else has it.
    Held {
        /// The lock file.
        path: PathBuf,
        /// Who, as far as the file says.
        holder: LockHolder,
    },
    /// The filesystem refused.
    Io(ConfigDiagnostic),
}

impl LockError {
    /// The diagnostic to report.
    ///
    /// Contention is an environment failure (exit 3), not a source error: nothing in
    /// the project is wrong, and a build script that retries should be able to tell
    /// the two apart by exit code alone.
    pub fn diagnostic(&self) -> ConfigDiagnostic {
        match self {
            LockError::Held { path, holder } => ConfigDiagnostic::error(
                ConfigCode::TargetUnavailable,
                format!("{} is already in use ({holder})", holder.description),
            )
            .at(path)
            .note("another viso process is probably running")
            .note(format!(
                "if it is not, remove `{}` and try again",
                path.display()
            )),
            LockError::Io(diag) => diag.clone(),
        }
    }
}

impl fmt::Display for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.diagnostic())
    }
}

/// An acquired lock, released when dropped.
///
/// Acquisition is `create_new`: one atomic filesystem operation that both tests and
/// takes the lock, so two processes racing cannot both win. There is no blocking
/// variant — a developer tool that hangs with no output is worse than one that says
/// who holds the lock and exits.
#[derive(Debug)]
pub struct LockGuard {
    path: PathBuf,
    kind: LockKind,
}

impl LockGuard {
    /// Takes the lock, or reports who has it.
    pub fn acquire(layout: &Layout, kind: LockKind) -> Result<Self, LockError> {
        let dir = layout.locks();
        create_dir_all(&dir).map_err(LockError::Io)?;
        let path = dir.join(kind.file_name());
        let holder = LockHolder {
            pid: std::process::id(),
            acquired_unix: unix_seconds(),
            description: kind.description(),
        };
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                use io::Write;
                // A failed write leaves a lock file that names nobody, so remove it and
                // report rather than holding a lock we cannot explain.
                if let Err(err) = file.write_all(holder.encode().as_bytes()) {
                    let _ = fs::remove_file(&path);
                    return Err(LockError::Io(io_diagnostic(&path, "write lock file", &err)));
                }
                Ok(Self { path, kind })
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                let holder = fs::read_to_string(&path)
                    .map(|text| LockHolder::decode(&text))
                    .unwrap_or(LockHolder {
                        pid: 0,
                        acquired_unix: 0,
                        description: kind.description(),
                    });
                Err(LockError::Held { path, holder })
            }
            Err(err) => Err(LockError::Io(io_diagnostic(
                &path,
                "create lock file",
                &err,
            ))),
        }
    }

    /// Who holds this lock, without trying to take it.
    pub fn holder(layout: &Layout, kind: LockKind) -> Option<LockHolder> {
        fs::read_to_string(layout.locks().join(kind.file_name()))
            .ok()
            .map(|text| LockHolder::decode(&text))
    }

    /// Removes a lock older than `older_than`, reporting whether it did.
    ///
    /// Age, not liveness: asking the OS whether a pid is alive needs a platform call,
    /// and a recycled pid answers "yes" for the wrong process. An explicit age
    /// threshold chosen by the caller is both portable and honest about what it knows.
    pub fn remove_stale(
        layout: &Layout,
        kind: LockKind,
        older_than: Duration,
    ) -> Result<bool, ConfigDiagnostic> {
        let path = layout.locks().join(kind.file_name());
        let Some(holder) = Self::holder(layout, kind) else {
            return Ok(false);
        };
        match holder.age() {
            Some(age) if age >= older_than => match fs::remove_file(&path) {
                Ok(()) => Ok(true),
                Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(err) => Err(io_diagnostic(&path, "remove stale lock", &err)),
            },
            _ => Ok(false),
        }
    }

    /// The lock file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What this lock protects.
    pub const fn kind(&self) -> LockKind {
        self.kind
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // Nothing useful can be done about a failed release, and panicking in a drop
        // during unwinding aborts the process. A lock file left behind is recoverable;
        // an abort is not.
        let _ = fs::remove_file(&self.path);
    }
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn create_dir_all(path: &Path) -> Result<(), ConfigDiagnostic> {
    fs::create_dir_all(path).map_err(|err| io_diagnostic(path, "create directory", &err))
}

fn io_diagnostic(path: &Path, action: &str, err: &io::Error) -> ConfigDiagnostic {
    ConfigDiagnostic::error(
        ConfigCode::ManifestUnreadable,
        format!("could not {action}: {err}"),
    )
    .at(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::{Hash128, ProjectFingerprint, Toolchain};
    use crate::scratch::Scratch;
    use crate::target::Profile;

    fn build_id() -> BuildId {
        BuildId::compute(
            ProjectFingerprint::from_sources([("a.rs", b"x".as_slice())]),
            Target::Host,
            Profile::Dev,
            &Toolchain::new("rustc 1.98.1", "aarch64-macos"),
            Hash128::from_parts(1, 2),
        )
    }

    /// The layout is pinned relative to the project root, because these paths appear
    /// in scripts, `.gitignore` files and CI caches: moving one later is a breaking
    /// change for users who never called an API.
    #[test]
    fn every_documented_directory_sits_where_the_specification_says() {
        let layout = Layout::new("/p");
        assert_eq!(layout.viso_dir(), Path::new("/p/target/viso"));
        assert_eq!(layout.builds(), Path::new("/p/target/viso/build"));
        assert_eq!(layout.dev(), Path::new("/p/target/viso/dev"));
        assert_eq!(layout.generated(), Path::new("/p/target/viso/generated"));
        assert_eq!(layout.traces(), Path::new("/p/target/viso/traces"));
        assert_eq!(layout.locks(), Path::new("/p/target/viso/locks"));
        for (kind, name) in [
            (CacheKind::Dsl, "dsl"),
            (CacheKind::Shader, "shader"),
            (CacheKind::Schema, "schema"),
            (CacheKind::Web, "web"),
        ] {
            assert_eq!(
                layout.cache(kind),
                Path::new("/p/target/viso/cache").join(name)
            );
        }
    }

    #[test]
    fn a_build_directory_is_named_by_the_whole_build_id() {
        let layout = Layout::new("/p");
        let id = build_id();
        assert_eq!(layout.build(id), layout.builds().join(id.to_hex()));
        assert_eq!(id.to_hex().len(), 32);
    }

    /// Two configurations must not share a build directory, or a `--profile release`
    /// build would overwrite the dev one and the next dev run would silently use it.
    #[test]
    fn different_build_ids_get_different_directories() {
        let layout = Layout::new("/p");
        let fingerprint = ProjectFingerprint::from_sources([("a.rs", b"x".as_slice())]);
        let toolchain = Toolchain::new("rustc 1.98.1", "aarch64-macos");
        let dev = BuildId::compute(
            fingerprint,
            Target::Host,
            Profile::Dev,
            &toolchain,
            Hash128::from_parts(1, 2),
        );
        let release = BuildId::compute(
            fingerprint,
            Target::Host,
            Profile::Release,
            &toolchain,
            Hash128::from_parts(1, 2),
        );
        assert_ne!(layout.build(dev), layout.build(release));
    }

    /// `dist/` is named by platform, and the name does not depend on which machine
    /// asked — otherwise a checked-in path would break for half a team.
    #[test]
    fn dist_directories_are_named_by_platform() {
        let mac = Layout::with_host("/p", HostOs::MacOs);
        assert_eq!(mac.dist(Target::Host), Path::new("/p/dist/macos"));
        assert_eq!(mac.dist(Target::Headless), Path::new("/p/dist/headless"));
        assert_eq!(mac.dist(Target::Ios), Path::new("/p/dist/ios"));
        assert_eq!(mac.dist(Target::Android), Path::new("/p/dist/android"));
        for web in [Target::WebGpu, Target::WebDom, Target::WebHybrid] {
            assert_eq!(mac.dist(web), Path::new("/p/dist/web"));
        }

        // Only the host target follows the host.
        let windows = Layout::with_host("/p", HostOs::Windows);
        assert_eq!(windows.dist(Target::Host), Path::new("/p/dist/windows"));
        assert_eq!(windows.dist(Target::Ios), mac.dist(Target::Ios));
    }

    /// Constructing a layout and asking for paths must not create anything: `viso
    /// config path` prints paths in a directory the user may not want written to.
    #[test]
    fn asking_for_a_path_does_not_create_it() {
        let s = Scratch::new("paths");
        let layout = Layout::new(s.path());
        let _ = layout.viso_dir();
        let _ = layout.build(build_id());
        let _ = layout.dist(Target::Host);
        assert!(!layout.target_dir().exists());
    }

    #[test]
    fn ensure_creates_the_session_directories_and_is_idempotent() {
        let s = Scratch::new("ensure");
        let layout = Layout::new(s.path());
        layout.ensure().unwrap();
        layout.ensure().unwrap();

        for dir in [
            layout.builds(),
            layout.dev(),
            layout.generated(),
            layout.traces(),
            layout.locks(),
        ] {
            assert!(dir.is_dir(), "{}", dir.display());
        }
        for kind in CacheKind::ALL {
            assert!(layout.cache(*kind).is_dir(), "{kind}");
        }
        // Not a build directory: an empty one would look like a finished build.
        assert!(!layout.build(build_id()).exists());
        // Not dist: nothing has been produced yet.
        assert!(!layout.dist_root().exists());
    }

    /// `clean` must not remove shipped output or live session state. Both mistakes
    /// are only noticed after they have cost someone something.
    #[test]
    fn clean_covers_the_caches_and_spares_output_and_sessions() {
        let layout = Layout::new("/p");
        let cleanable = layout.cleanable();
        assert!(cleanable.contains(&layout.builds()));
        assert!(cleanable.contains(&layout.generated()));
        assert!(cleanable.contains(&layout.traces()));
        for kind in CacheKind::ALL {
            assert!(cleanable.contains(&layout.cache(*kind)), "{kind}");
        }
        assert!(!cleanable.contains(&layout.dist_root()));
        assert!(!cleanable.contains(&layout.dev()));
        assert!(!cleanable.contains(&layout.locks()));
    }

    #[test]
    fn a_lock_is_taken_once_and_released_on_drop() {
        let s = Scratch::new("lock");
        let layout = Layout::new(s.path());
        let kind = LockKind::BuildCache {
            target: Target::Host,
        };

        let guard = LockGuard::acquire(&layout, kind).unwrap();
        assert!(guard.path().is_file());
        let path = guard.path().to_path_buf();

        // A second acquisition fails rather than blocking, and says who holds it.
        match LockGuard::acquire(&layout, kind) {
            Err(LockError::Held { holder, .. }) => {
                assert_eq!(holder.pid, std::process::id());
                assert!(holder.description.contains("build cache"));
                let diag = LockError::Held {
                    path: path.clone(),
                    holder,
                }
                .diagnostic();
                assert_eq!(
                    diag.code.exit_code(),
                    3,
                    "contention is an environment error"
                );
                assert!(diag.message.contains("already in use"));
            }
            other => panic!("expected contention, got {other:?}"),
        }

        drop(guard);
        assert!(!path.exists(), "the lock is released");
        // And can be taken again.
        let _again = LockGuard::acquire(&layout, kind).unwrap();
    }

    /// The point of the three granularities: independent work stays concurrent. A
    /// single project-wide lock would make `viso build --target host` wait for
    /// `--target headless`, which is the exact pair a developer runs side by side.
    #[test]
    fn independent_locks_do_not_contend() {
        let s = Scratch::new("lock-granularity");
        let layout = Layout::new(s.path());

        let host_build = LockGuard::acquire(
            &layout,
            LockKind::BuildCache {
                target: Target::Host,
            },
        )
        .unwrap();
        let headless_build = LockGuard::acquire(
            &layout,
            LockKind::BuildCache {
                target: Target::Headless,
            },
        )
        .unwrap();
        let host_package = LockGuard::acquire(
            &layout,
            LockKind::PackageOutput {
                target: Target::Host,
            },
        )
        .unwrap();
        let session = LockGuard::acquire(
            &layout,
            LockKind::DevSession {
                target: Target::Host,
                device: None,
            },
        )
        .unwrap();
        let other_device = LockGuard::acquire(
            &layout,
            LockKind::DevSession {
                target: Target::Host,
                device: Some("iphone-15"),
            },
        )
        .unwrap();

        let paths = [
            host_build.path(),
            headless_build.path(),
            host_package.path(),
            session.path(),
            other_device.path(),
        ];
        for (i, a) in paths.iter().enumerate() {
            for b in &paths[i + 1..] {
                assert_ne!(a, b, "two granularities share a file");
            }
            assert!(a.is_file());
        }

        // But the same session does contend.
        assert!(matches!(
            LockGuard::acquire(
                &layout,
                LockKind::DevSession {
                    target: Target::Host,
                    device: Some("iphone-15"),
                },
            ),
            Err(LockError::Held { .. })
        ));
    }

    #[test]
    fn a_lock_records_who_holds_it() {
        let s = Scratch::new("lock-holder");
        let layout = Layout::new(s.path());
        let kind = LockKind::PackageOutput {
            target: Target::Headless,
        };
        assert!(LockGuard::holder(&layout, kind).is_none());

        let _guard = LockGuard::acquire(&layout, kind).unwrap();
        let holder = LockGuard::holder(&layout, kind).unwrap();
        assert_eq!(holder.pid, std::process::id());
        assert!(holder.age().unwrap() < Duration::from_secs(60));
        assert!(holder.to_string().starts_with("pid "));
    }

    /// A lock file is never stolen implicitly: removal is a separate, age-gated call,
    /// so a slow build is never sabotaged by an impatient second process.
    #[test]
    fn a_stale_lock_is_removed_only_when_asked_and_only_when_old() {
        let s = Scratch::new("lock-stale");
        let layout = Layout::new(s.path());
        let kind = LockKind::DevSession {
            target: Target::Host,
            device: None,
        };
        let guard = LockGuard::acquire(&layout, kind).unwrap();
        let path = guard.path().to_path_buf();
        std::mem::forget(guard); // simulate a process that died without releasing

        assert!(!LockGuard::remove_stale(&layout, kind, Duration::from_secs(3600)).unwrap());
        assert!(path.is_file(), "a fresh lock is left alone");

        assert!(LockGuard::remove_stale(&layout, kind, Duration::from_secs(0)).unwrap());
        assert!(!path.exists());
        // Removing what is not there is not an error.
        assert!(!LockGuard::remove_stale(&layout, kind, Duration::from_secs(0)).unwrap());

        let _retaken = LockGuard::acquire(&layout, kind).unwrap();
    }

    /// A truncated or hand-edited lock file must still produce a report. Failing to
    /// parse would turn a recoverable stale lock into an unexplained error.
    #[test]
    fn a_malformed_lock_file_still_names_something() {
        let holder = LockHolder::decode("");
        assert_eq!(holder.pid, 0);
        assert_eq!(holder.description, "unknown");
        assert_eq!(LockHolder::decode("nonsense\n").pid, 0);

        let round_trip = LockHolder {
            pid: 4123,
            acquired_unix: 1_700_000_000,
            description: "build cache for `host`".to_string(),
        };
        assert_eq!(LockHolder::decode(&round_trip.encode()), round_trip);
    }
}
