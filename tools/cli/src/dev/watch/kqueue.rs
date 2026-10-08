//! kqueue: each watched directory and each watched file in it, as vnodes.
//!
//! A directory reports an entry created, renamed or removed in it (an
//! editor's atomic save, a new file or directory); a file reports a write, a
//! truncate, its deletion or its rename. After an event the files of that
//! directory whose path now names another file are opened again, so a save
//! that replaced the file keeps reporting.

use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// How long a file's events must stop before it is read: a vnode event
/// fires on each write, and a save that truncates then writes is two.
pub(super) const QUIET: Duration = Duration::from_millis(5);

#[cfg(any(target_os = "macos", target_os = "ios"))]
const OPEN: libc::c_int = libc::O_EVTONLY;
#[cfg(target_os = "freebsd")]
const OPEN: libc::c_int = libc::O_RDONLY;

/// The `EVFILT_USER` event [`Wake`] triggers.
const WAKE: usize = 0;

/// The vnode changes a watched file reports.
const FILE_CHANGES: u32 = libc::NOTE_WRITE
    | libc::NOTE_EXTEND
    | libc::NOTE_ATTRIB
    | libc::NOTE_DELETE
    | libc::NOTE_RENAME
    | libc::NOTE_REVOKE;

/// The kernel queue and the vnodes registered on it.
pub(super) struct Events {
    queue: Arc<OwnedFd>,
    dirs: Vec<Dir>,
}

/// A watched directory: its own vnode, and each watched file in it.
struct Dir {
    path: PathBuf,
    _vnode: OwnedFd,
    files: Vec<File>,
}

struct File {
    path: PathBuf,
    /// The open file and the device and inode it is, while the path names one.
    vnode: Option<(OwnedFd, (u64, u64))>,
}

/// Interrupts [`Events::wait`] from another thread.
pub(super) struct Wake(Arc<OwnedFd>);

/// A change to register: `filter` on `ident`, tagged with `group`.
fn change(ident: usize, filter: i16, flags: u16, fflags: u32, group: usize) -> libc::kevent {
    // SAFETY: `kevent` is plain data; every field the kernel reads is set
    // below and the rest (`data`, platform extensions) must be zero.
    let mut event: libc::kevent = unsafe { std::mem::zeroed() };
    event.ident = ident;
    event.filter = filter;
    event.flags = flags;
    event.fflags = fflags;
    event.udata = group as *mut libc::c_void;
    event
}

/// Applies `change` to the queue `queue`.
fn register(queue: &OwnedFd, change: &libc::kevent) -> bool {
    // SAFETY: one change read from a valid reference and no event list
    // written; `queue` is an open kqueue.
    unsafe {
        libc::kevent(
            queue.as_raw_fd(),
            change,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        ) == 0
    }
}

/// `path` opened for events only.
fn open(path: &Path) -> Option<OwnedFd> {
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `path` is NUL-terminated and outlives the call.
    let fd = unsafe { libc::open(path.as_ptr(), OPEN | libc::O_CLOEXEC) };
    // SAFETY: a descriptor `open` returned is owned by nothing else.
    (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The device and inode `path` names.
fn identity(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.dev(), meta.ino()))
}

impl Events {
    /// A new queue and the handle that interrupts its wait, `None` when the
    /// system has no queue to give.
    pub(super) fn new() -> Option<(Events, Wake)> {
        // SAFETY: `kqueue` takes no arguments; a non-negative result is a new
        // descriptor owned by nothing else.
        let fd = unsafe { libc::kqueue() };
        if fd < 0 {
            return None;
        }
        // SAFETY: as above.
        let queue = Arc::new(unsafe { OwnedFd::from_raw_fd(fd) });
        let wake = change(
            WAKE,
            libc::EVFILT_USER,
            libc::EV_ADD | libc::EV_CLEAR,
            0,
            usize::MAX,
        );
        if !register(&queue, &wake) {
            return None;
        }
        let events = Events {
            queue: Arc::clone(&queue),
            dirs: Vec::new(),
        };
        Some((events, Wake(queue)))
    }

    /// Watches the entries of directory `dir`, returning the group its events
    /// report under, `None` when it cannot be watched.
    pub(super) fn add_dir(&mut self, dir: &Path) -> Option<u32> {
        if let Some(group) = self.dirs.iter().position(|d| d.path == dir) {
            return u32::try_from(group).ok();
        }
        let vnode = open(dir)?;
        let group = self.dirs.len();
        let watch = change(
            vnode.as_raw_fd() as usize,
            libc::EVFILT_VNODE,
            libc::EV_ADD | libc::EV_CLEAR,
            libc::NOTE_WRITE,
            group,
        );
        if !register(&self.queue, &watch) {
            return None;
        }
        self.dirs.push(Dir {
            path: dir.to_owned(),
            _vnode: vnode,
            files: Vec::new(),
        });
        u32::try_from(group).ok()
    }

    /// Watches `path`, returning the group its events report under (its
    /// directory's), `None` when its directory cannot be watched.
    pub(super) fn add(&mut self, path: &Path) -> Option<u32> {
        let dir = match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            _ => Path::new("."),
        };
        let group = self.add_dir(dir)? as usize;
        let mut file = File {
            path: path.to_owned(),
            vnode: None,
        };
        arm(&self.queue, group, &mut file);
        self.dirs[group].files.push(file);
        u32::try_from(group).ok()
    }

    /// Blocks until a watched vnode changes, [`Wake::wake`] is called or
    /// `timeout` passes, and pushes the groups that changed to `touched`.
    pub(super) fn wait(&mut self, timeout: Option<Duration>, touched: &mut Vec<u32>) {
        // SAFETY: `kevent` is plain data; the kernel overwrites the entries it
        // reports.
        let mut out: [libc::kevent; 16] = unsafe { std::mem::zeroed() };
        let limit = timeout.map(|t| libc::timespec {
            tv_sec: t.as_secs().min(i32::MAX as u64) as libc::time_t,
            tv_nsec: libc::c_long::from(t.subsec_nanos() as i32),
        });
        // SAFETY: no change list; `out` has room for the 16 events allowed;
        // `limit` lives across the call.
        let count = unsafe {
            libc::kevent(
                self.queue.as_raw_fd(),
                std::ptr::null(),
                0,
                out.as_mut_ptr(),
                out.len() as libc::c_int,
                limit.as_ref().map_or(std::ptr::null(), |limit| limit),
            )
        };
        let count = usize::try_from(count).unwrap_or(0);
        for event in &out[..count] {
            if event.filter != libc::EVFILT_VNODE {
                continue;
            }
            let group = event.udata as usize;
            let Ok(tag) = u32::try_from(group) else {
                continue;
            };
            if touched.contains(&tag) {
                continue;
            }
            touched.push(tag);
            if let Some(dir) = self.dirs.get_mut(group) {
                for file in &mut dir.files {
                    arm(&self.queue, group, file);
                }
            }
        }
    }
}

/// Registers `file`'s vnode under `group`, opening it again when its path
/// names another file than the one open (or none was).
fn arm(queue: &OwnedFd, group: usize, file: &mut File) {
    let now = identity(&file.path);
    if now.is_some() && file.vnode.as_ref().map(|(_, id)| *id) == now {
        return;
    }
    file.vnode = None;
    let (Some(id), Some(vnode)) = (now, open(&file.path)) else {
        return;
    };
    let watch = change(
        vnode.as_raw_fd() as usize,
        libc::EVFILT_VNODE,
        libc::EV_ADD | libc::EV_CLEAR,
        FILE_CHANGES,
        group,
    );
    if register(queue, &watch) {
        file.vnode = Some((vnode, id));
    }
}

impl Wake {
    pub(super) fn wake(&self) {
        let trigger = change(WAKE, libc::EVFILT_USER, 0, libc::NOTE_TRIGGER, usize::MAX);
        register(&self.0, &trigger);
    }
}
