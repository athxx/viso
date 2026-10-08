//! inotify: each watched directory, for the writes that complete a save and
//! the directories that appear in it.
//!
//! A directory reports a file in it closed after writing (an in-place save)
//! or renamed into it (an atomic save); both mean the content is complete,
//! so a file is read as soon as its event arrives. A directory created in it
//! reports too, so it is scanned; a file's creation does not, as its content
//! is complete only once it is closed.

use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// How long a file's events must stop before it is read: none, as each
/// event marks a complete write.
pub(super) const QUIET: Duration = Duration::ZERO;

/// The directory events a watch reports.
const CHANGES: u32 = libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_CREATE;

/// The inotify instance and the directories watched on it.
pub(super) struct Events {
    inotify: OwnedFd,
    wake: Arc<OwnedFd>,
    /// Each watched directory and its watch descriptor, by group.
    dirs: Vec<(PathBuf, libc::c_int)>,
}

/// Interrupts [`Events::wait`] from another thread.
pub(super) struct Wake(Arc<OwnedFd>);

/// A descriptor a call returned, `None` for a failure.
fn owned(fd: libc::c_int) -> Option<OwnedFd> {
    // SAFETY: a non-negative descriptor a call just returned is owned by
    // nothing else.
    (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
}

impl Events {
    /// A new instance and the handle that interrupts its wait, `None` when
    /// the system has no instance to give.
    pub(super) fn new() -> Option<(Events, Wake)> {
        // SAFETY: plain flags; the result is checked.
        let inotify = owned(unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) })?;
        // SAFETY: as above.
        let wake = Arc::new(owned(unsafe {
            libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC)
        })?);
        let events = Events {
            inotify,
            wake: Arc::clone(&wake),
            dirs: Vec::new(),
        };
        Some((events, Wake(wake)))
    }

    /// Watches `path`, returning the group its events report under (its
    /// directory's), `None` when its directory cannot be watched.
    pub(super) fn add(&mut self, path: &Path) -> Option<u32> {
        let dir = match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            _ => Path::new("."),
        };
        self.add_dir(dir)
    }

    /// Watches the entries of directory `dir`, returning the group its events
    /// report under, `None` when it cannot be watched.
    pub(super) fn add_dir(&mut self, dir: &Path) -> Option<u32> {
        let group = match self.dirs.iter().position(|(d, _)| d == dir) {
            Some(group) => group,
            None => {
                let name = CString::new(dir.as_os_str().as_bytes()).ok()?;
                // SAFETY: `name` is NUL-terminated and outlives the call.
                let watch = unsafe {
                    libc::inotify_add_watch(self.inotify.as_raw_fd(), name.as_ptr(), CHANGES)
                };
                if watch < 0 {
                    return None;
                }
                self.dirs.push((dir.to_owned(), watch));
                self.dirs.len() - 1
            }
        };
        u32::try_from(group).ok()
    }

    /// Blocks until a watched directory reports a save, [`Wake::wake`] is
    /// called or `timeout` passes, and pushes the groups saved into to
    /// `touched`.
    pub(super) fn wait(&mut self, timeout: Option<Duration>, touched: &mut Vec<u32>) {
        let mut fds = [
            libc::pollfd {
                fd: self.inotify.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let limit = timeout.map_or(-1, |t| {
            libc::c_int::try_from(t.as_micros().div_ceil(1000)).unwrap_or(libc::c_int::MAX)
        });
        // SAFETY: `fds` holds the two entries the count names.
        if unsafe { libc::poll(fds.as_mut_ptr(), 2, limit) } <= 0 {
            return;
        }
        if fds[1].revents != 0 {
            let mut count = 0u64;
            // SAFETY: an eventfd read takes exactly the 8 bytes `count` holds.
            unsafe { libc::read(self.wake.as_raw_fd(), (&raw mut count).cast(), 8) };
        }
        if fds[0].revents != 0 {
            self.drain(touched);
        }
    }

    /// Reads every queued event, pushing the group of each.
    fn drain(&self, touched: &mut Vec<u32>) {
        // Aligned for the `inotify_event` headers the kernel writes.
        let mut buffer = [0u64; 512];
        let header = std::mem::size_of::<libc::inotify_event>();
        loop {
            // SAFETY: the kernel writes at most the buffer's byte length.
            let read = unsafe {
                libc::read(
                    self.inotify.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    std::mem::size_of_val(&buffer),
                )
            };
            let Ok(read) = usize::try_from(read) else {
                return;
            };
            if read == 0 {
                return;
            }
            let bytes = buffer.as_ptr().cast::<u8>();
            let mut at = 0;
            while at + header <= read {
                // SAFETY: `at..at + header` lies within the bytes read, and
                // the kernel wrote a whole header there.
                let event = unsafe {
                    std::ptr::read_unaligned(bytes.add(at).cast::<libc::inotify_event>())
                };
                at += header + event.len as usize;
                // A file created is not yet written; its close reports it.
                if event.mask & libc::IN_CREATE != 0 && event.mask & libc::IN_ISDIR == 0 {
                    continue;
                }
                let groups = if event.mask & libc::IN_Q_OVERFLOW != 0 {
                    0..self.dirs.len()
                } else {
                    match self.dirs.iter().position(|&(_, wd)| wd == event.wd) {
                        Some(group) => group..group + 1,
                        None => continue,
                    }
                };
                for group in groups {
                    let tag = group as u32;
                    if !touched.contains(&tag) {
                        touched.push(tag);
                    }
                }
            }
        }
    }
}

impl Wake {
    pub(super) fn wake(&self) {
        let one = 1u64;
        // SAFETY: an eventfd write takes exactly the 8 bytes `one` holds.
        unsafe { libc::write(self.0.as_raw_fd(), (&raw const one).cast(), 8) };
    }
}
