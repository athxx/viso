//! The project watcher: one thread that watches the project tree, and the
//! Rust sources the dev artifact is built from, on the host and hands each
//! settled, content-changed file of the watch scope to the dev session.
//!
//! The thread scans each root once — the project root and every package the
//! app links outside it — watching every directory of the scope and tracking
//! every file of it. It delivers each project file's content as soon as it
//! has settled, so the session knows the tree as it was when the build
//! started; a Rust file found then is only hashed, not delivered, so the
//! build's own read is the baseline and only an edit after it rebuilds. The
//! files Cargo reads beside the sources (the lock file, its configuration,
//! the toolchain pin) are tracked by path whether they exist or not. It then blocks on the platform's file events — kqueue on
//! macOS and FreeBSD, inotify on Linux, `ReadDirectoryChangesW` on Windows —
//! over each watched directory, so an atomic save that renames a new file
//! into place reports as well as an in-place write. An event on a directory
//! rescans it, so a file or directory created later joins the scope. A file
//! is read once its events have stopped for the backend's quiet window (none
//! where an event marks a completed write), so a truncate-then-write burst
//! arrives as one change. A file the events cannot cover (no backend, or a
//! directory that cannot be watched) is polled instead: one `stat` per poll,
//! read once its stamp (length and modification time) has held for a settle
//! window; such a directory is rescanned on a slower poll. A read whose
//! content hash equals the last one delivered (a save that changed nothing, a
//! touch) is dropped on the thread; a file that is gone is not reported.
//!
//! When the Rust scope changes (a `Cargo.toml` edit adds or drops a path
//! dependency) the session hands the new one over: a root it adds is scanned
//! as at the start, and a file that left the scope is no longer reported.

mod scope;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant, SystemTime};

use viso_view::dev::wire::content_hash;

use super::cargo::RustScope;

pub(crate) use scope::{asset_dir, excluded_dir, in_scope};

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
#[path = "kqueue.rs"]
mod events;

#[cfg(any(target_os = "linux", target_os = "android"))]
#[path = "inotify.rs"]
mod events;

#[cfg(windows)]
#[path = "windows.rs"]
mod events;

/// No file events: every file is polled.
#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "linux",
    target_os = "android",
    windows
)))]
mod events {
    use std::path::Path;
    use std::time::Duration;

    pub(super) const QUIET: Duration = Duration::ZERO;

    pub(super) enum Events {}

    pub(super) enum Wake {}

    impl Events {
        pub(super) fn new() -> Option<(Events, Wake)> {
            None
        }

        pub(super) fn add(&mut self, _: &Path) -> Option<u32> {
            match *self {}
        }

        pub(super) fn add_dir(&mut self, _: &Path) -> Option<u32> {
            match *self {}
        }

        pub(super) fn wait(&mut self, _: Option<Duration>, _: &mut Vec<u32>) {
            match *self {}
        }
    }

    impl Wake {
        pub(super) fn wake(&self) {
            match *self {}
        }
    }
}

use events::{Events, QUIET, Wake};

/// How often the thread polls a file the events do not cover.
const POLL: Duration = Duration::from_millis(25);

/// How long a polled file's changed stamp must hold before it is read, and
/// how long a file found by a scan waits before its first read.
const SETTLE: Duration = Duration::from_millis(25);

/// How often a directory the events do not cover is rescanned.
const RESCAN: Duration = Duration::from_millis(250);

/// A settled file whose content differs from the last one delivered.
#[derive(Debug)]
pub struct Change {
    /// The file's canonical path.
    pub path: PathBuf,
    /// Its new content.
    pub bytes: Vec<u8>,
    /// Whether it is of the Rust scope: a change that rebuilds the app.
    pub rust: bool,
}

/// What the watcher watches.
#[derive(Debug, Clone)]
pub(crate) struct WatchScope {
    /// The project root, a canonical path: its sources, catalogs, assets and
    /// manifest.
    pub(crate) project: PathBuf,
    /// The Rust sources the dev artifact is built from.
    pub(crate) rust: RustScope,
}

impl WatchScope {
    /// The directories scanned: the project root and each linked package
    /// outside it, none inside another.
    fn roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = Vec::new();
        let mut candidates: Vec<&PathBuf> = std::iter::once(&self.project)
            .chain(&self.rust.packages)
            .collect();
        candidates.sort_by_key(|dir| dir.components().count());
        for dir in candidates {
            if !roots.iter().any(|root| dir.starts_with(root)) {
                roots.push(dir.clone());
            }
        }
        roots
    }

    /// Whether directory `dir` is left out: what the project scope leaves
    /// out, and the `target/` beside any package's manifest.
    fn excluded(&self, dir: &Path) -> bool {
        if excluded_dir(&self.project, dir) {
            return true;
        }
        dir.file_name().is_some_and(|name| name == "target")
            && dir.parent().is_some_and(|parent| {
                self.rust
                    .packages
                    .iter()
                    .chain(&self.rust.unlinked)
                    .any(|package| package == parent)
            })
    }

    /// Whether file `path` is tracked, and as a Rust file.
    fn tracks(&self, path: &Path) -> Option<bool> {
        if self.rust.contains(path) {
            Some(true)
        } else if path.starts_with(&self.project) && in_scope(&self.project, path) {
            Some(false)
        } else {
            None
        }
    }
}

/// A file the thread watches.
struct Watched {
    path: PathBuf,
    /// The hash of the content last delivered, `None` before the first.
    hash: Option<u64>,
    /// Whether the first read is only hashed: a Rust file as the build
    /// reads it.
    baseline: bool,
    /// The group its events report under, `None` while it is polled.
    group: Option<u32>,
    /// When it is next read, for a file the events cover.
    due: Option<Instant>,
    /// The stamp last seen, `None` while the file cannot be read; polled
    /// files only.
    stamp: Option<(u64, SystemTime)>,
    /// When the stamp last changed, while a read is pending.
    since: Option<Instant>,
}

impl Watched {
    /// Checks the file at `now`, returning its content once a change has
    /// settled and its hash differs from the last delivered.
    fn check(&mut self, now: Instant) -> Option<Vec<u8>> {
        if self.group.is_some() {
            if self.due.is_none_or(|due| due > now) {
                return None;
            }
            self.due = None;
            return self.read();
        }
        let stamp = std::fs::metadata(&self.path)
            .ok()
            .and_then(|meta| Some((meta.len(), meta.modified().ok()?)));
        if stamp != self.stamp {
            self.stamp = stamp;
            self.since = Some(now);
            return None;
        }
        let since = self.since?;
        if stamp.is_none() || now.duration_since(since) < SETTLE {
            return None;
        }
        self.since = None;
        self.read()
    }

    /// The file's content when it differs from the last delivered.
    fn read(&mut self) -> Option<Vec<u8>> {
        let bytes = std::fs::read(&self.path).ok()?;
        let hash = content_hash(&bytes);
        let baseline = std::mem::take(&mut self.baseline);
        if self.hash == Some(hash) {
            return None;
        }
        self.hash = Some(hash);
        (!baseline).then_some(bytes)
    }
}

/// A directory of the scope the thread watches.
struct Dir {
    path: PathBuf,
    /// The group its events report under, `None` while it is rescanned on a
    /// poll.
    group: Option<u32>,
    /// When it is next rescanned, while it is polled.
    rescan: Option<Instant>,
}

/// What interrupts the thread's wait: the events' wake, or an unpark when
/// it only polls.
struct Signal {
    events: Option<Wake>,
    thread: Thread,
}

impl Signal {
    fn ring(&self) {
        if let Some(wake) = &self.events {
            wake.wake();
        }
        self.thread.unpark();
    }
}

/// The handle of the watcher thread. Dropping it stops and joins the thread.
pub(crate) struct Watcher {
    /// The Rust scopes handed over; closed to stop the thread.
    rescope: Option<Sender<RustScope>>,
    signal: Option<Signal>,
    thread: Option<JoinHandle<()>>,
}

impl Watcher {
    /// Starts the thread over `scope`; it hands each change to `deliver`, and
    /// ends when `deliver` returns `false` or the handle is dropped.
    pub(crate) fn spawn(
        scope: WatchScope,
        deliver: impl FnMut(Change) -> bool + Send + 'static,
    ) -> Self {
        let (rescope, rescoped) = mpsc::channel();
        let (events, wake) = Events::new().unzip();
        let thread = thread::Builder::new()
            .name("viso-dev-watch".into())
            .spawn(move || Tree::new(events, scope).run(&rescoped, deliver))
            .ok();
        let signal = thread.as_ref().map(|thread| Signal {
            events: wake,
            thread: thread.thread().clone(),
        });
        Self {
            rescope: Some(rescope),
            signal,
            thread,
        }
    }

    /// Watches the Rust scope `rust` from now on.
    pub(crate) fn rescope(&self, rust: RustScope) {
        if let Some(rescope) = &self.rescope
            && rescope.send(rust).is_ok()
            && let Some(signal) = &self.signal
        {
            signal.ring();
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.rescope = None;
        if let Some(signal) = &self.signal {
            signal.ring();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The watched tree, owned by the thread.
struct Tree {
    events: Option<Events>,
    scope: WatchScope,
    files: Vec<Watched>,
    dirs: Vec<Dir>,
    /// The files Cargo reads whose directory cannot be watched yet, and when
    /// each is tried again.
    pending: Vec<(PathBuf, Instant)>,
    /// Every path tracked or watched, so a rescan adds only what is new.
    known: HashSet<PathBuf>,
    /// Whether a root is being scanned: a Rust file found is then the
    /// build's baseline, not a change.
    baseline: bool,
}

impl Tree {
    fn new(events: Option<Events>, scope: WatchScope) -> Self {
        Tree {
            events,
            scope,
            files: Vec::new(),
            dirs: Vec::new(),
            pending: Vec::new(),
            known: HashSet::new(),
            baseline: false,
        }
    }

    /// The thread's loop.
    fn run(mut self, rescoped: &Receiver<RustScope>, mut deliver: impl FnMut(Change) -> bool) {
        self.cover(Instant::now());
        let mut touched: Vec<u32> = Vec::new();
        loop {
            loop {
                match rescoped.try_recv() {
                    Ok(rust) => {
                        self.scope.rust = rust;
                        self.cover(Instant::now());
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }
            let now = Instant::now();
            for at in 0..self.dirs.len() {
                if self.dirs[at].rescan.is_some_and(|due| due <= now) {
                    self.dirs[at].rescan = Some(now + RESCAN);
                    let path = self.dirs[at].path.clone();
                    self.scan(&path, now);
                }
            }
            self.retry_pending(now);
            for watched in &mut self.files {
                if let Some(bytes) = watched.check(now)
                    && let Some(rust) = self.scope.tracks(&watched.path)
                {
                    let change = Change {
                        path: watched.path.clone(),
                        bytes,
                        rust,
                    };
                    if !deliver(change) {
                        return;
                    }
                }
            }
            let mut timeout = self
                .files
                .iter()
                .filter_map(|watched| watched.due)
                .chain(self.dirs.iter().filter_map(|dir| dir.rescan))
                .chain(self.pending.iter().map(|(_, retry)| *retry))
                .min()
                .map(|due| due.saturating_duration_since(Instant::now()));
            if self.files.iter().any(|watched| watched.group.is_none()) {
                timeout = Some(timeout.map_or(POLL, |timeout| timeout.min(POLL)));
            }
            match &mut self.events {
                Some(events) => {
                    touched.clear();
                    events.wait(timeout, &mut touched);
                    let now = Instant::now();
                    let due = now + QUIET;
                    for watched in &mut self.files {
                        if watched.group.is_some_and(|group| touched.contains(&group)) {
                            watched.due = Some(due);
                        }
                    }
                    for at in 0..self.dirs.len() {
                        if self.dirs[at]
                            .group
                            .is_some_and(|group| touched.contains(&group))
                        {
                            let path = self.dirs[at].path.clone();
                            self.scan(&path, now);
                        }
                    }
                }
                None => match timeout {
                    Some(timeout) => thread::park_timeout(timeout),
                    None => thread::park(),
                },
            }
        }
    }

    /// Scans each root of the scope not yet watched and tracks each file
    /// Cargo reads, every Rust file found the build's baseline.
    fn cover(&mut self, now: Instant) {
        self.baseline = true;
        for root in self.scope.roots() {
            self.add_dir(&root, now);
        }
        for path in self.scope.rust.files.clone() {
            if self.known.contains(&path) {
                continue;
            }
            match &mut self.events {
                Some(events) => match events.add(&path) {
                    Some(group) => self.track(path, Some(group), now),
                    None => {
                        self.known.insert(path.clone());
                        self.pending.push((path, now + RESCAN));
                    }
                },
                None => self.track(path, None, now),
            }
        }
        self.baseline = false;
    }

    /// Tracks each pending file whose directory can now be watched; one that
    /// appears with it is a change.
    fn retry_pending(&mut self, now: Instant) {
        let mut at = 0;
        while at < self.pending.len() {
            if self.pending[at].1 > now {
                at += 1;
                continue;
            }
            let Some(events) = &mut self.events else {
                return;
            };
            match events.add(&self.pending[at].0) {
                Some(group) => {
                    let (path, _) = self.pending.swap_remove(at);
                    self.known.remove(&path);
                    self.track(path, Some(group), now);
                }
                None => {
                    self.pending[at].1 = now + RESCAN;
                    at += 1;
                }
            }
        }
    }

    /// Tracks file `path`, its events under `group` or polled; a Rust file
    /// found while a root is scanned, or a file Cargo reads that is there
    /// then, is the baseline.
    fn track(&mut self, path: PathBuf, group: Option<u32>, now: Instant) {
        if !self.known.insert(path.clone()) {
            return;
        }
        let baseline = self.baseline && self.scope.tracks(&path) == Some(true) && path.is_file();
        self.files.push(Watched {
            path,
            hash: None,
            baseline,
            group,
            // The first read waits out a settle window like any other: a save
            // in flight would otherwise be read half written. Its events push
            // the read back until it is quiet.
            due: group.map(|_| now + SETTLE),
            stamp: None,
            since: None,
        });
    }

    /// Watches directory `path` of the scope and scans it.
    fn add_dir(&mut self, path: &Path, now: Instant) {
        if !self.known.insert(path.to_owned()) {
            return;
        }
        let group = self.events.as_mut().and_then(|events| events.add_dir(path));
        self.dirs.push(Dir {
            path: path.to_owned(),
            group,
            rescan: group.is_none().then_some(now + RESCAN),
        });
        self.scan(path, now);
    }

    /// Adds what directory `dir` holds of the scope and is not yet known:
    /// each directory is watched and scanned in turn, each file tracked and
    /// read once it has settled.
    fn scan(&mut self, dir: &Path, now: Instant) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if !self.scope.excluded(&path) {
                    self.add_dir(&path, now);
                }
            } else if kind.is_file()
                && self.scope.tracks(&path).is_some()
                && !self.known.contains(&path)
            {
                let group = self.events.as_mut().and_then(|events| events.add(&path));
                self.track(path, group, now);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// A temporary project tree.
    fn project(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("viso-watch-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("Viso.toml"), "[package]\nname = \"app\"\n").unwrap();
        std::fs::write(dir.join("src/view.vs"), "a").unwrap();
        dir.canonicalize().unwrap()
    }

    /// A watcher over the project at `root` and what it delivered.
    fn watching(root: &Path) -> (Watcher, Arc<Mutex<Vec<Change>>>) {
        watching_rust(root, RustScope::default())
    }

    /// A watcher over the project at `root` and the Rust scope `rust`, and
    /// what it delivered.
    fn watching_rust(root: &Path, rust: RustScope) -> (Watcher, Arc<Mutex<Vec<Change>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let into = Arc::clone(&seen);
        let scope = WatchScope {
            project: root.to_owned(),
            rust,
        };
        let watcher = Watcher::spawn(scope, move |change| {
            into.lock().unwrap().push(change);
            true
        });
        (watcher, seen)
    }

    /// A Rust package `name` beside the project trees, with a source.
    fn package(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("viso-watch-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"dep\"\n").unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
        dir.canonicalize().unwrap()
    }

    /// Whether anything was delivered for `path` by now.
    fn delivered(seen: &Mutex<Vec<Change>>, path: &Path) -> bool {
        seen.lock().unwrap().iter().any(|c| c.path == path)
    }

    /// The content delivered for `path` within a second after `taken`
    /// deliveries, if any.
    fn next(seen: &Mutex<Vec<Change>>, path: &Path, taken: &mut usize) -> Option<Vec<u8>> {
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            {
                let seen = seen.lock().unwrap();
                if let Some(at) = seen[*taken..].iter().position(|c| c.path == path) {
                    *taken += at + 1;
                    return Some(seen[*taken - 1].bytes.clone());
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
        None
    }

    #[test]
    fn the_scope_is_delivered_at_start_then_each_settled_edit_once() {
        let root = project("edit");
        let view = root.join("src/view.vs");
        let (_watcher, seen) = watching(&root);
        let mut taken = 0;
        assert_eq!(next(&seen, &view, &mut taken).as_deref(), Some(&b"a"[..]));
        let manifest = seen
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.path == root.join("Viso.toml"));
        assert!(manifest, "the manifest is in scope");
        thread::sleep(POLL * 3);
        std::fs::write(&view, "ab").unwrap();
        assert_eq!(next(&seen, &view, &mut taken).as_deref(), Some(&b"ab"[..]));
        assert_eq!(
            next(&seen, &view, &mut taken),
            None,
            "one edit is one change"
        );
        std::fs::write(&view, "ab").unwrap();
        assert_eq!(
            next(&seen, &view, &mut taken),
            None,
            "same content is dropped"
        );
    }

    #[test]
    fn rust_files_are_their_builds_baseline_and_arrive_once_edited() {
        let root = project("rust");
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"app\"\n").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::create_dir_all(root.join("target/debug/build")).unwrap();
        std::fs::write(root.join("target/debug/build/out.rs"), "").unwrap();
        let dep = package("rust-dep");
        std::fs::write(dep.join("src/view.vs"), "a").unwrap();
        let rust = RustScope {
            packages: vec![root.clone(), dep.clone()],
            unlinked: Vec::new(),
            files: vec![root.join("Cargo.lock"), root.join(".cargo/config.toml")],
        };
        let (_watcher, seen) = watching_rust(&root, rust);
        let mut taken = 0;
        next(&seen, &root.join("src/view.vs"), &mut taken);
        thread::sleep(SETTLE * 4);
        for path in [
            root.join("src/main.rs"),
            root.join("Cargo.toml"),
            dep.join("src/lib.rs"),
        ] {
            assert!(
                !delivered(&seen, &path),
                "{} is the baseline",
                path.display()
            );
        }
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        thread::sleep(SETTLE * 4);
        assert!(
            !delivered(&seen, &root.join("src/main.rs")),
            "a save that changed nothing"
        );
        for (path, text) in [
            (root.join("src/main.rs"), "fn main() { f() }\n"),
            (dep.join("src/lib.rs"), "pub fn f() { g() }\n"),
            (root.join("src/later.rs"), "mod x;\n"),
            (root.join("Cargo.lock"), "version = 4\n"),
        ] {
            std::fs::write(&path, text).unwrap();
            assert!(
                next(&seen, &path, &mut taken).is_some(),
                "{}",
                path.display()
            );
            assert!(
                seen.lock().unwrap()[taken - 1].rust,
                "{} is Rust",
                path.display()
            );
        }
        std::fs::create_dir_all(root.join(".cargo")).unwrap();
        let config = root.join(".cargo/config.toml");
        std::fs::write(&config, "[build]\n").unwrap();
        let deadline = Instant::now() + RESCAN * 8;
        while !delivered(&seen, &config) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(delivered(&seen, &config), "the configuration appears");
        std::fs::write(dep.join("src/view.vs"), "b").unwrap();
        std::fs::write(root.join("target/debug/build/out.rs"), "x").unwrap();
        thread::sleep(SETTLE * 4);
        assert!(
            !delivered(&seen, &dep.join("src/view.vs")),
            "a package outside the project brings only its Rust"
        );
        assert!(!delivered(&seen, &root.join("target/debug/build/out.rs")));
    }

    #[test]
    fn a_new_rust_scope_scans_the_packages_it_adds_and_drops_the_rest() {
        let root = project("rescope");
        let old = package("rescope-old");
        let new = package("rescope-new");
        let rust = |dep: &Path| RustScope {
            packages: vec![root.clone(), dep.to_owned()],
            unlinked: Vec::new(),
            files: Vec::new(),
        };
        let (watcher, seen) = watching_rust(&root, rust(&old));
        let mut taken = 0;
        next(&seen, &root.join("src/view.vs"), &mut taken);
        watcher.rescope(rust(&new));
        thread::sleep(SETTLE * 4);
        assert!(!delivered(&seen, &new.join("src/lib.rs")), "the baseline");
        std::fs::write(old.join("src/lib.rs"), "pub fn old() {}\n").unwrap();
        std::fs::write(new.join("src/lib.rs"), "pub fn new() {}\n").unwrap();
        assert!(next(&seen, &new.join("src/lib.rs"), &mut taken).is_some());
        thread::sleep(SETTLE * 4);
        assert!(!delivered(&seen, &old.join("src/lib.rs")), "out of scope");
    }

    #[test]
    fn an_atomic_save_that_renames_a_new_file_into_place_arrives() {
        let root = project("atomic");
        let view = root.join("src/view.vs");
        let (_watcher, seen) = watching(&root);
        let mut taken = 0;
        next(&seen, &view, &mut taken);
        let staged = view.with_extension("vs.tmp");
        std::fs::write(&staged, "renamed").unwrap();
        std::fs::rename(&staged, &view).unwrap();
        assert_eq!(
            next(&seen, &view, &mut taken).as_deref(),
            Some(&b"renamed"[..])
        );
        std::fs::write(&view, "again").unwrap();
        assert_eq!(
            next(&seen, &view, &mut taken).as_deref(),
            Some(&b"again"[..]),
            "the replaced file still reports"
        );
    }

    #[test]
    fn a_file_or_directory_created_later_joins_the_scope() {
        let root = project("created");
        let (_watcher, seen) = watching(&root);
        let mut taken = 0;
        next(&seen, &root.join("src/view.vs"), &mut taken);
        let later = root.join("src/later.vs");
        std::fs::write(&later, "new").unwrap();
        assert_eq!(
            next(&seen, &later, &mut taken).as_deref(),
            Some(&b"new"[..])
        );
        let nested = root.join("src/feature/deep");
        std::fs::create_dir_all(&nested).unwrap();
        let deep = nested.join("page.vs");
        std::fs::write(&deep, "deep").unwrap();
        assert_eq!(
            next(&seen, &deep, &mut taken).as_deref(),
            Some(&b"deep"[..])
        );
        std::fs::create_dir_all(root.join("i18n")).unwrap();
        let catalog = root.join("i18n/en.toml");
        std::fs::write(&catalog, "title = \"T\"\n").unwrap();
        assert!(next(&seen, &catalog, &mut taken).is_some());
        std::fs::create_dir_all(root.join("assets/icons")).unwrap();
        let icon = root.join("assets/icons/star.png");
        std::fs::write(&icon, b"\x89PNG\r\n\x1a\n\xff").unwrap();
        assert_eq!(
            next(&seen, &icon, &mut taken).as_deref(),
            Some(&b"\x89PNG\r\n\x1a\n\xff"[..]),
            "an asset arrives as its bytes"
        );
    }

    #[test]
    fn excluded_trees_and_editor_files_are_not_watched() {
        let root = project("excluded");
        for dir in ["target/debug", ".git", "dist", "src/.cache", "gen"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(
            root.join("gen/CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55",
        )
        .unwrap();
        let ignored = [
            "target/debug/out.vs",
            ".git/x.vs",
            "dist/x.vs",
            "src/.cache/x.vs",
            "gen/x.vs",
            "src/.#view.vs",
            "src/view.vs~",
            "src/notes.txt",
        ];
        for file in ignored {
            std::fs::write(root.join(file), "x").unwrap();
        }
        let (_watcher, seen) = watching(&root);
        let mut taken = 0;
        next(&seen, &root.join("src/view.vs"), &mut taken);
        thread::sleep(SETTLE * 4);
        let seen = seen.lock().unwrap();
        for file in ignored {
            assert!(!seen.iter().any(|c| c.path == root.join(file)), "{file}");
        }
    }

    /// The coalescing workload (`Viso_Hot_Reload.md` §7.1): a tool writing
    /// `n` related files back to back, as an AI agent applies a multi-file
    /// edit, and the spread of their arrivals, first to last, which the
    /// session's coalescing window must cover. A release measurement:
    /// `cargo test --release -p viso-cli --bin viso -- --ignored
    /// multi_file_save_spread --nocapture`.
    #[test]
    #[ignore = "a release measurement"]
    fn multi_file_save_spread() {
        const ROUNDS: usize = 40;
        for n in [1usize, 2, 4, 8, 16] {
            let root = project(&format!("spread-{n}"));
            let paths: Vec<PathBuf> = (0..n).map(|i| root.join(format!("src/f{i}.vs"))).collect();
            for path in &paths {
                std::fs::write(path, "0").unwrap();
            }
            let arrivals = Arc::new(Mutex::new(Vec::new()));
            let into = Arc::clone(&arrivals);
            let scope = WatchScope {
                project: root.clone(),
                rust: RustScope::default(),
            };
            let _watcher = Watcher::spawn(scope, move |change| {
                into.lock().unwrap().push((change.path, Instant::now()));
                true
            });
            thread::sleep(Duration::from_millis(200));
            let mut spreads = Vec::new();
            let mut latencies = Vec::new();
            for round in 0..ROUNDS {
                arrivals.lock().unwrap().clear();
                thread::sleep(Duration::from_millis(5 + (round as u64 * 7) % 25));
                let started = Instant::now();
                for path in &paths {
                    std::fs::write(path, format!("r{round}")).unwrap();
                }
                let deadline = started + Duration::from_secs(2);
                while arrivals.lock().unwrap().len() < n && Instant::now() < deadline {
                    thread::sleep(Duration::from_micros(200));
                }
                let seen = arrivals.lock().unwrap();
                assert_eq!(seen.len(), n, "every file arrives");
                let first = seen.iter().map(|(_, at)| *at).min().unwrap();
                let last = seen.iter().map(|(_, at)| *at).max().unwrap();
                spreads.push(last - first);
                latencies.push(first - started);
            }
            spreads.sort();
            latencies.sort();
            let ms = |d: Duration| d.as_secs_f64() * 1e3;
            println!(
                "{n} file(s): spread median {:.3} ms, p95 {:.3} ms, max {:.3} ms; \
                 first arrival median {:.3} ms",
                ms(spreads[ROUNDS / 2]),
                ms(spreads[ROUNDS * 95 / 100]),
                ms(spreads[ROUNDS - 1]),
                ms(latencies[ROUNDS / 2]),
            );
        }
    }

    #[test]
    fn a_polled_file_is_read_once_its_stamp_settles() {
        let root = project("polled");
        let mut watched = Watched {
            path: root.join("src/view.vs"),
            hash: Some(content_hash(b"old")),
            baseline: false,
            group: None,
            due: None,
            stamp: None,
            since: None,
        };
        let start = Instant::now();
        assert_eq!(watched.check(start), None, "a new stamp starts the window");
        assert_eq!(watched.check(start + SETTLE / 2), None);
        assert_eq!(watched.check(start + SETTLE).as_deref(), Some(&b"a"[..]));
        assert_eq!(watched.check(start + SETTLE * 3), None, "read once");
    }
}
