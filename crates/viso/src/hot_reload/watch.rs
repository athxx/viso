//! The development watcher: one thread that polls the mounted `.vs` files and
//! hands each settled, content-changed save to the loop.
//!
//! Each poll costs one `stat` per watched file. A changed stamp (length and
//! modification time) starts a settle window; the file is read only once its
//! stamp has held for the window, so an editor's truncate-then-write or
//! write-then-rename burst arrives as one change. A read whose content hash
//! equals the last one delivered (a save that changed nothing, a touch) is
//! dropped on the thread.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use viso_platform::LoopWaker;

/// How often the thread polls the watched files.
const POLL: Duration = Duration::from_millis(25);

/// How long a changed stamp must hold before the file is read.
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

/// A file the thread polls.
struct Watched {
    file: usize,
    path: PathBuf,
    /// The hash of the content last delivered (or embedded at build).
    hash: u64,
    /// The stamp last seen, `None` while the file cannot be read.
    stamp: Option<(u64, SystemTime)>,
    /// When the stamp last changed, while a read is pending.
    since: Option<Instant>,
}

impl Watched {
    /// Polls the file at `now`, returning its content once a change has
    /// settled and its hash differs from the last delivered.
    fn poll(&mut self, now: Instant) -> Option<String> {
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
        let source = std::fs::read_to_string(&self.path).ok()?;
        let hash = content_hash(&source);
        if hash == self.hash {
            return None;
        }
        self.hash = hash;
        Some(source)
    }
}

/// The handle of the watcher thread. Dropping it stops and joins the thread.
pub(crate) struct Watcher {
    files: Option<Sender<Watched>>,
    changes: Receiver<Change>,
    thread: Option<JoinHandle<()>>,
}

impl Watcher {
    /// Starts the thread; each change it delivers wakes the loop through
    /// `waker`.
    pub(crate) fn spawn(waker: LoopWaker) -> Self {
        let (files, watch) = mpsc::channel();
        let (deliver, changes) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("viso-hot-reload".into())
            .spawn(move || run(&watch, &deliver, &waker))
            .ok();
        Self {
            files: Some(files),
            changes,
            thread,
        }
    }

    /// Starts polling `path` as file `file`, whose content the running build
    /// holds hashes to `hash`. The first poll reads it, so an edit made between
    /// the build and the launch still arrives.
    pub(crate) fn watch(&self, file: usize, path: PathBuf, hash: u64) {
        if let Some(files) = &self.files {
            let _ = files.send(Watched {
                file,
                path,
                hash,
                stamp: None,
                since: None,
            });
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
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

/// The thread's loop: it ends when the handle is dropped.
fn run(watch: &Receiver<Watched>, deliver: &Sender<Change>, waker: &LoopWaker) {
    let mut files: Vec<Watched> = Vec::new();
    loop {
        loop {
            match watch.try_recv() {
                Ok(file) => files.push(file),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        let now = Instant::now();
        let mut woke = false;
        for watched in &mut files {
            if let Some(source) = watched.poll(now) {
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
        thread::park_timeout(POLL);
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
}
