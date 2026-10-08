//! The development watcher: one thread that waits on the mounted `.vs` files
//! and hands each content-changed save to the loop.
//!
//! The thread blocks on the platform's file events — kqueue on macOS, iOS
//! and FreeBSD, inotify on Linux and Android, `ReadDirectoryChangesW` on
//! Windows — over each file's directory, so an atomic save that renames a new
//! file into place reports as well as an in-place write. A file is read once
//! its events have stopped for the backend's quiet window (none where an
//! event marks a completed write), so a truncate-then-write burst arrives as
//! one change. A file the events cannot cover (no backend, or its directory
//! cannot be watched) is polled instead: one `stat` per poll, read once its
//! stamp (length and modification time) has held for a settle window. A
//! read whose content hash equals the last one delivered (a save that
//! changed nothing, a touch) is dropped on the thread.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant, SystemTime};

use viso_platform::LoopWaker;

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

/// How long a polled file's changed stamp must hold before it is read.
const SETTLE: Duration = Duration::from_millis(25);

/// The hash a watched file's content is compared by.
pub(crate) fn content_hash(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// A settled save whose content differs from the last one delivered.
#[derive(Debug)]
pub(crate) struct Change {
    /// The file's index, as it was [watched](Watcher::watch).
    pub file: usize,
    /// The file's new content.
    pub source: String,
}

/// A file the thread watches.
struct Watched {
    file: usize,
    path: PathBuf,
    /// The hash of the content last delivered (or embedded at build).
    hash: u64,
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
        let source = std::fs::read_to_string(&self.path).ok()?;
        let hash = content_hash(&source);
        if hash == self.hash {
            return None;
        }
        self.hash = hash;
        Some(source)
    }
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
    files: Option<Sender<Watched>>,
    changes: Receiver<Change>,
    signal: Option<Signal>,
    thread: Option<JoinHandle<()>>,
}

impl Watcher {
    /// Starts the thread; each change it delivers wakes the loop through
    /// `waker`.
    pub(crate) fn spawn(waker: LoopWaker) -> Self {
        let (files, watch) = mpsc::channel();
        let (deliver, changes) = mpsc::channel();
        let (events, wake) = Events::new().unzip();
        let thread = thread::Builder::new()
            .name("viso-hot-reload".into())
            .spawn(move || run(&watch, &deliver, &waker, events))
            .ok();
        let signal = thread.as_ref().map(|thread| Signal {
            events: wake,
            thread: thread.thread().clone(),
        });
        Self {
            files: Some(files),
            changes,
            signal,
            thread,
        }
    }

    /// Starts watching `path` as file `file`, whose content the running build
    /// holds hashes to `hash`. It is read once it has settled, so an edit made
    /// between the build and the launch still arrives, and one in flight
    /// arrives whole.
    pub(crate) fn watch(&self, file: usize, path: PathBuf, hash: u64) {
        if let Some(files) = &self.files {
            let _ = files.send(Watched {
                file,
                path,
                hash,
                group: None,
                due: None,
                stamp: None,
                since: None,
            });
        }
        if let Some(signal) = &self.signal {
            signal.ring();
        }
    }

    /// The next delivered change, if any.
    pub(crate) fn try_recv(&self) -> Option<Change> {
        self.changes.try_recv().ok()
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.files = None;
        if let Some(signal) = &self.signal {
            signal.ring();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The thread's loop: it ends when the handle is dropped.
fn run(
    watch: &Receiver<Watched>,
    deliver: &Sender<Change>,
    waker: &LoopWaker,
    mut events: Option<Events>,
) {
    let mut files: Vec<Watched> = Vec::new();
    let mut touched: Vec<u32> = Vec::new();
    loop {
        loop {
            match watch.try_recv() {
                Ok(mut file) => {
                    file.group = events.as_mut().and_then(|events| events.add(&file.path));
                    // The first read waits out a settle window like any
                    // other: a save in flight at tracking start would
                    // otherwise be read half written. Its events push the
                    // read back until it is quiet.
                    file.due = file.group.map(|_| Instant::now() + SETTLE);
                    files.push(file);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        let now = Instant::now();
        let mut woke = false;
        for watched in &mut files {
            if let Some(source) = watched.check(now) {
                let change = Change {
                    file: watched.file,
                    source,
                };
                if deliver.send(change).is_err() {
                    return;
                }
                woke = true;
            }
        }
        if woke {
            waker.wake();
        }
        let mut timeout = files
            .iter()
            .filter_map(|watched| watched.due)
            .min()
            .map(|due| due.saturating_duration_since(Instant::now()));
        if files.iter().any(|watched| watched.group.is_none()) {
            timeout = Some(timeout.map_or(POLL, |timeout| timeout.min(POLL)));
        }
        match &mut events {
            Some(events) => {
                touched.clear();
                events.wait(timeout, &mut touched);
                let due = Instant::now() + QUIET;
                for watched in &mut files {
                    if watched.group.is_some_and(|group| touched.contains(&group)) {
                        watched.due = Some(due);
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// A watcher over one temporary file holding `initial`, and the count of
    /// loop wakes it made.
    fn watching(name: &str, initial: &str) -> (Watcher, PathBuf, Arc<AtomicU32>) {
        let dir = std::env::temp_dir().join(format!("viso-watch-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("view.vs");
        std::fs::write(&path, initial).unwrap();
        let wakes = Arc::new(AtomicU32::new(0));
        let counted = Arc::clone(&wakes);
        let watcher = Watcher::spawn(LoopWaker::new(move || {
            counted.fetch_add(1, Ordering::Relaxed);
        }));
        watcher.watch(7, path.clone(), content_hash(initial));
        (watcher, path, wakes)
    }

    /// The change the watcher delivers within a second, if any.
    fn next(watcher: &Watcher) -> Option<Change> {
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if let Some(change) = watcher.try_recv() {
                return Some(change);
            }
            thread::sleep(Duration::from_millis(5));
        }
        None
    }

    #[test]
    fn a_settled_edit_arrives_once_and_wakes_the_loop() {
        let (watcher, path, wakes) = watching("edit", "a");
        thread::sleep(POLL * 3);
        std::fs::write(&path, "ab").unwrap();
        let change = next(&watcher).expect("the edit arrives");
        assert_eq!((change.file, change.source.as_str()), (7, "ab"));
        assert!(wakes.load(Ordering::Relaxed) >= 1);
        assert!(next(&watcher).is_none(), "one edit is one change");
    }

    #[test]
    fn a_content_identical_save_is_dropped() {
        let (watcher, path, wakes) = watching("same", "same");
        thread::sleep(POLL * 3);
        std::fs::write(&path, "same").unwrap();
        assert!(next(&watcher).is_none());
        assert_eq!(wakes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn an_edit_before_the_first_poll_still_arrives() {
        let (watcher, path, _) = watching("early", "old");
        std::fs::write(&path, "new").unwrap();
        let change = next(&watcher).expect("the edit arrives");
        assert_eq!(change.source, "new");
    }

    #[test]
    fn an_atomic_save_that_renames_a_new_file_into_place_arrives() {
        let (watcher, path, _) = watching("atomic", "old");
        thread::sleep(POLL * 3);
        let staged = path.with_extension("vs.tmp");
        std::fs::write(&staged, "renamed").unwrap();
        std::fs::rename(&staged, &path).unwrap();
        let change = next(&watcher).expect("the save arrives");
        assert_eq!(change.source, "renamed");
        std::fs::write(&path, "again").unwrap();
        let change = next(&watcher).expect("the replaced file still reports");
        assert_eq!(change.source, "again");
    }

    #[test]
    fn a_polled_file_is_read_once_its_stamp_settles() {
        let dir = std::env::temp_dir().join(format!("viso-watch-{}-polled", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("view.vs");
        std::fs::write(&path, "new").unwrap();
        let mut watched = Watched {
            file: 0,
            path,
            hash: content_hash("old"),
            group: None,
            due: None,
            stamp: None,
            since: None,
        };
        let start = Instant::now();
        assert_eq!(watched.check(start), None, "a new stamp starts the window");
        assert_eq!(watched.check(start + SETTLE / 2), None);
        assert_eq!(watched.check(start + SETTLE).as_deref(), Some("new"));
        assert_eq!(watched.check(start + SETTLE * 3), None, "read once");
    }
}
