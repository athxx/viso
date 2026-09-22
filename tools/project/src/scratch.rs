//! A self-cleaning temporary directory for this crate's own tests.
//!
//! Discovery, fingerprinting and locking are all filesystem behavior, so testing
//! them against a real directory tree is the only honest option. A dependency for
//! thirty lines would be the wrong trade at the bottom of the tools graph, and this
//! is the whole of what those tests need.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A unique temporary directory, removed when dropped.
pub struct Scratch {
    path: PathBuf,
}

/// Distinguishes directories created within one process; the pid separates
/// concurrent test binaries.
static COUNTER: AtomicU64 = AtomicU64::new(0);

impl Scratch {
    /// Creates a fresh directory under the system temporary directory.
    pub fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("viso-project-{label}-{}-{n}", std::process::id()));
        // A leftover from a previous run with the same pid would make a test see
        // files it did not write, which is worse than failing to clean up.
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create scratch directory");
        Self { path }
    }

    /// The directory root.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes a file, creating parent directories. Returns its absolute path.
    pub fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.path.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent directory");
        }
        std::fs::write(&path, contents).expect("write scratch file");
        path
    }

    /// Creates a directory, including parents. Returns its absolute path.
    pub fn dir(&self, relative: &str) -> PathBuf {
        let path = self.path.join(relative);
        std::fs::create_dir_all(&path).expect("create scratch directory");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // A failed cleanup must not mask the test's own result.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
