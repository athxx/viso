//! The project watcher (`Viso_Hot_Reload.md` §6, §7): one thread that watches
//! the project tree on the host and hands each settled, content-changed file
//! of the watch scope to the dev session.
//!
//! The thread scans the project root once, watching every directory of the
//! scope and tracking every file of it, and delivers each file's content as
//! soon as it has settled, so the session knows the tree as it was when the
//! build started. It then blocks on the platform's file events — kqueue on
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

mod scope;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant, SystemTime};

use viso_view::dev::wire::source_hash;

pub(crate) use scope::{excluded_dir, in_scope};

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
    pub text: String,
}

/// A file the thread watches.
struct Watched {
    path: PathBuf,
    /// The hash of the content last delivered, `None` before the first.
    hash: Option<u64>,
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
    fn check(&mut self, now: Instant) -> Option<String> {
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
    fn read(&mut self) -> Option<String> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        let hash = source_hash(&text);
        if self.hash == Some(hash) {
            return None;
        }
        self.hash = Some(hash);
        Some(text)
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
    stop: Option<Sender<()>>,
    signal: Option<Signal>,
    thread: Option<JoinHandle<()>>,
}

impl Watcher {
    /// Starts the thread over the project rooted at `root`, a canonical path;
    /// it hands each change to `deliver`, and ends when `deliver` returns
    /// `false` or the handle is dropped.
    pub(crate) fn spawn(
        root: PathBuf,
        deliver: impl FnMut(Change) -> bool + Send + 'static,
    ) -> Self {
        let (stop, stopped) = mpsc::channel();
        let (events, wake) = Events::new().unzip();
        let thread = thread::Builder::new()
            .name("viso-dev-watch".into())
            .spawn(move || Tree::new(events).run(&root, &stopped, deliver))
            .ok();
        let signal = thread.as_ref().map(|thread| Signal {
            events: wake,
            thread: thread.thread().clone(),
        });
        Self {
            stop: Some(stop),
            signal,
            thread,
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop = None;
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
    files: Vec<Watched>,
    dirs: Vec<Dir>,
    /// Every path tracked or watched, so a rescan adds only what is new.
    known: HashSet<PathBuf>,
}

impl Tree {
    fn new(events: Option<Events>) -> Self {
        Tree {
            events,
            files: Vec::new(),
            dirs: Vec::new(),
            known: HashSet::new(),
        }
    }

    /// The thread's loop.
    fn run(mut self, root: &Path, stopped: &Receiver<()>, mut deliver: impl FnMut(Change) -> bool) {
        self.add_dir(root, root, Instant::now());
        let mut touched: Vec<u32> = Vec::new();
        loop {
            if !matches!(stopped.try_recv(), Err(TryRecvError::Empty)) {
                return;
            }
            let now = Instant::now();
            for at in 0..self.dirs.len() {
                if self.dirs[at].rescan.is_some_and(|due| due <= now) {
                    self.dirs[at].rescan = Some(now + RESCAN);
                    let path = self.dirs[at].path.clone();
                    self.scan(root, &path, now);
                }
            }
            for watched in &mut self.files {
                if let Some(text) = watched.check(now) {
                    let change = Change {
                        path: watched.path.clone(),
                        text,
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
                            self.scan(root, &path, now);
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

    /// Watches directory `path` of the scope rooted at `root` and scans it.
    fn add_dir(&mut self, root: &Path, path: &Path, now: Instant) {
        if !self.known.insert(path.to_owned()) {
            return;
        }
        let group = self.events.as_mut().and_then(|events| events.add_dir(path));
        self.dirs.push(Dir {
            path: path.to_owned(),
            group,
            rescan: group.is_none().then_some(now + RESCAN),
        });
        self.scan(root, path, now);
    }

    /// Adds what directory `dir` holds of the scope and is not yet known:
    /// each directory is watched and scanned in turn, each file tracked and
    /// read once it has settled.
    fn scan(&mut self, root: &Path, dir: &Path, now: Instant) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if !excluded_dir(root, &path) {
                    self.add_dir(root, &path, now);
                }
            } else if kind.is_file() && in_scope(root, &path) && self.known.insert(path.clone()) {
                let group = self.events.as_mut().and_then(|events| events.add(&path));
                self.files.push(Watched {
                    path,
                    hash: None,
                    group,
                    // The first read waits out a settle window like any
                    // other: a save in flight would otherwise be read half
                    // written. Its events push the read back until it is
                    // quiet.
                    due: group.map(|_| now + SETTLE),
                    stamp: None,
                    since: None,
                });
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

    /// A watcher over `root` and what it delivered.
    fn watching(root: &Path) -> (Watcher, Arc<Mutex<Vec<Change>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let into = Arc::clone(&seen);
        let watcher = Watcher::spawn(root.to_owned(), move |change| {
            into.lock().unwrap().push(change);
            true
        });
        (watcher, seen)
    }

    /// The content delivered for `path` within a second after `taken`
    /// deliveries, if any.
    fn next(seen: &Mutex<Vec<Change>>, path: &Path, taken: &mut usize) -> Option<String> {
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            {
                let seen = seen.lock().unwrap();
                if let Some(at) = seen[*taken..].iter().position(|c| c.path == path) {
                    *taken += at + 1;
                    return Some(seen[*taken - 1].text.clone());
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
        assert_eq!(next(&seen, &view, &mut taken).as_deref(), Some("a"));
        let manifest = seen
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.path == root.join("Viso.toml"));
        assert!(manifest, "the manifest is in scope");
        thread::sleep(POLL * 3);
        std::fs::write(&view, "ab").unwrap();
        assert_eq!(next(&seen, &view, &mut taken).as_deref(), Some("ab"));
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
    fn an_atomic_save_that_renames_a_new_file_into_place_arrives() {
        let root = project("atomic");
        let view = root.join("src/view.vs");
        let (_watcher, seen) = watching(&root);
        let mut taken = 0;
        next(&seen, &view, &mut taken);
        let staged = view.with_extension("vs.tmp");
        std::fs::write(&staged, "renamed").unwrap();
        std::fs::rename(&staged, &view).unwrap();
        assert_eq!(next(&seen, &view, &mut taken).as_deref(), Some("renamed"));
        std::fs::write(&view, "again").unwrap();
        assert_eq!(
            next(&seen, &view, &mut taken).as_deref(),
            Some("again"),
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
        assert_eq!(next(&seen, &later, &mut taken).as_deref(), Some("new"));
        let nested = root.join("src/feature/deep");
        std::fs::create_dir_all(&nested).unwrap();
        let deep = nested.join("page.vs");
        std::fs::write(&deep, "deep").unwrap();
        assert_eq!(next(&seen, &deep, &mut taken).as_deref(), Some("deep"));
        std::fs::create_dir_all(root.join("i18n")).unwrap();
        let catalog = root.join("i18n/en.toml");
        std::fs::write(&catalog, "title = \"T\"\n").unwrap();
        assert!(next(&seen, &catalog, &mut taken).is_some());
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
            let _watcher = Watcher::spawn(root.clone(), move |change| {
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
            hash: Some(source_hash("old")),
            group: None,
            due: None,
            stamp: None,
            since: None,
        };
        let start = Instant::now();
        assert_eq!(watched.check(start), None, "a new stamp starts the window");
        assert_eq!(watched.check(start + SETTLE / 2), None);
        assert_eq!(watched.check(start + SETTLE).as_deref(), Some("a"));
        assert_eq!(watched.check(start + SETTLE * 3), None, "read once");
    }
}
