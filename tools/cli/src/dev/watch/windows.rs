//! `ReadDirectoryChangesW`: each watched directory, read asynchronously so
//! one wait covers every directory and the wake event.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY,
    FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
    FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    ReadDirectoryChangesW,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::{
    CreateEventW, INFINITE, ResetEvent, SetEvent, WaitForMultipleObjects,
};
use windows_core::HSTRING;

/// How long a file's events must stop before it is read: a directory
/// reports each write, and a save that truncates then writes is two.
pub(super) const QUIET: Duration = Duration::from_millis(5);

/// The most directories one wait covers, beside the wake event.
const MAX_DIRS: usize = 63;

/// An owned kernel handle.
struct Handle(HANDLE);

// SAFETY: a kernel handle names the same object from any thread, and
// `SetEvent`, the only call made through a shared one, is thread-safe.
unsafe impl Send for Handle {}
// SAFETY: as above.
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle is open and owned here.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// The watched directories and the event that interrupts the wait.
pub(super) struct Events {
    wake: Arc<Handle>,
    dirs: Vec<Dir>,
}

/// A watched directory with its read in flight.
struct Dir {
    path: PathBuf,
    handle: Handle,
    /// Signaled when the read completes; `overlapped` names it.
    _event: Handle,
    overlapped: Box<OVERLAPPED>,
    /// The notifications the read fills; never parsed, as any change marks
    /// every file of the directory.
    buffer: Box<[u32; 1024]>,
    /// Whether a read is in flight.
    pending: bool,
}

// SAFETY: a directory's handles and the buffers its read fills are owned by
// the one thread that waits on them.
unsafe impl Send for Dir {}

impl Dir {
    /// Starts the next read of the directory's changes.
    fn arm(&mut self) -> bool {
        // SAFETY: the buffer and the overlapped record are boxed, so they keep
        // their address while the read is in flight, and `Drop` cancels and
        // awaits the read before freeing them.
        self.pending = unsafe {
            ReadDirectoryChangesW(
                self.handle.0,
                self.buffer.as_mut_ptr().cast(),
                std::mem::size_of_val(&*self.buffer) as u32,
                false,
                FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_DIR_NAME
                    | FILE_NOTIFY_CHANGE_LAST_WRITE
                    | FILE_NOTIFY_CHANGE_SIZE,
                None,
                Some(&mut *self.overlapped),
                None,
            )
        }
        .is_ok();
        self.pending
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        if !self.pending {
            return;
        }
        let mut moved = 0;
        // SAFETY: cancels the read this directory started and waits for it to
        // stop writing into the buffer before the fields drop.
        unsafe {
            let _ = CancelIoEx(self.handle.0, Some(&*self.overlapped));
            let _ = GetOverlappedResult(self.handle.0, &*self.overlapped, &mut moved, true);
        }
    }
}

/// Interrupts [`Events::wait`] from another thread.
pub(super) struct Wake(Arc<Handle>);

/// A new unnamed event, reset by a wait when `manual` is false.
fn event(manual: bool) -> Option<Handle> {
    // SAFETY: no attributes and no name.
    unsafe { CreateEventW(None, manual, false, None) }
        .ok()
        .map(Handle)
}

impl Events {
    /// The wait's state and the handle that interrupts it.
    pub(super) fn new() -> Option<(Events, Wake)> {
        let wake = Arc::new(event(false)?);
        let events = Events {
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
        if let Some(group) = self.dirs.iter().position(|d| d.path == dir) {
            return u32::try_from(group).ok();
        }
        if self.dirs.len() == MAX_DIRS {
            return None;
        }
        // SAFETY: the name is a valid wide string for the call; the handle
        // returned is owned below.
        let handle = unsafe {
            CreateFileW(
                &HSTRING::from(dir),
                FILE_LIST_DIRECTORY.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                None,
            )
        }
        .ok()?;
        let handle = Handle(handle);
        let done = event(true)?;
        let mut dir = Dir {
            path: dir.to_owned(),
            overlapped: Box::new(OVERLAPPED {
                hEvent: done.0,
                ..OVERLAPPED::default()
            }),
            handle,
            _event: done,
            buffer: Box::new([0; 1024]),
            pending: false,
        };
        if !dir.arm() {
            return None;
        }
        self.dirs.push(dir);
        u32::try_from(self.dirs.len() - 1).ok()
    }

    /// Blocks until a watched directory changes, [`Wake::wake`] is called or
    /// `timeout` passes, and pushes the groups that changed to `touched`.
    pub(super) fn wait(&mut self, timeout: Option<Duration>, touched: &mut Vec<u32>) {
        let mut handles = Vec::with_capacity(self.dirs.len() + 1);
        handles.push(self.wake.0);
        handles.extend(self.dirs.iter().map(|dir| dir.overlapped.hEvent));
        let limit = timeout.map_or(INFINITE, |t| {
            u32::try_from(t.as_micros().div_ceil(1000)).unwrap_or(INFINITE - 1)
        });
        // SAFETY: every handle is an open event owned by `self`.
        let signaled = unsafe { WaitForMultipleObjects(&handles, false, limit) };
        if signaled.0.wrapping_sub(WAIT_OBJECT_0.0) > self.dirs.len() as u32 {
            return;
        }
        for (group, dir) in self.dirs.iter_mut().enumerate() {
            if !dir.pending {
                continue;
            }
            let mut moved = 0;
            // SAFETY: polls the read this directory started without waiting.
            let done =
                unsafe { GetOverlappedResult(dir.handle.0, &*dir.overlapped, &mut moved, false) };
            if done.is_err() {
                continue;
            }
            // SAFETY: the manual-reset event belongs to this directory.
            let _ = unsafe { ResetEvent(dir.overlapped.hEvent) };
            dir.pending = false;
            touched.push(group as u32);
            dir.arm();
        }
    }
}

impl Wake {
    pub(super) fn wake(&self) {
        // SAFETY: the event is open while the `Arc` holds it.
        let _ = unsafe { SetEvent(self.0.0) };
    }
}
