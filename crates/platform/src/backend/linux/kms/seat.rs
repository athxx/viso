//! Access to the card and the input devices: through `libseat` (seatd or
//! logind, loaded at run time) when a seat is available, which hands out DRM
//! master and pauses the session across VT switches; otherwise by opening the
//! nodes directly, as a dedicated user in the `video` and `input` groups,
//! with the controlling VT put in graphics mode for the run.

use std::cell::Cell;
use std::ffi::{CString, c_char, c_int, c_void};
use std::io;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::path::Path;

use super::gbm::Library;

const KDSETMODE: libc::Ioctl = 0x4b3a as libc::Ioctl;
const KDGETMODE: libc::Ioctl = 0x4b3b as libc::Ioctl;
const KDGKBMODE: libc::Ioctl = 0x4b44 as libc::Ioctl;
const KDSKBMODE: libc::Ioctl = 0x4b45 as libc::Ioctl;
const KD_GRAPHICS: c_int = 1;
const K_OFF: c_int = 4;

/// `struct libseat_seat_listener`.
#[repr(C)]
struct Listener {
    enable_seat: unsafe extern "C" fn(*mut c_void, *mut c_void),
    disable_seat: unsafe extern "C" fn(*mut c_void, *mut c_void),
}

/// What the seat's callbacks report, read by the loop after a dispatch.
#[derive(Default)]
struct Flags {
    active: Cell<bool>,
    /// The seat was disabled and must be acknowledged.
    disabled: Cell<bool>,
    /// The seat was enabled again after being disabled.
    enabled: Cell<bool>,
}

unsafe extern "C" fn enable_seat(_seat: *mut c_void, data: *mut c_void) {
    // SAFETY: `data` is the boxed `Flags` the seat was opened with, alive
    // until the seat is closed.
    let flags = unsafe { &*data.cast::<Flags>() };
    flags.active.set(true);
    flags.enabled.set(true);
}

unsafe extern "C" fn disable_seat(_seat: *mut c_void, data: *mut c_void) {
    // SAFETY: as in `enable_seat`.
    let flags = unsafe { &*data.cast::<Flags>() };
    flags.active.set(false);
    flags.disabled.set(true);
}

static LISTENER: Listener = Listener {
    enable_seat,
    disable_seat,
};

struct Libseat {
    seat: *mut c_void,
    flags: Box<Flags>,
    disable: unsafe extern "C" fn(*mut c_void) -> c_int,
    close_seat: unsafe extern "C" fn(*mut c_void) -> c_int,
    open_device: unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_int) -> c_int,
    close_device: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    get_fd: unsafe extern "C" fn(*mut c_void) -> c_int,
    dispatch: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    _lib: Library,
}

impl Libseat {
    fn open() -> Option<Libseat> {
        let lib = Library::open(&[c"libseat.so.1", c"libseat.so"])?;
        let sym = |name: &std::ffi::CStr| {
            let p = lib.symbol(name);
            (!p.is_null()).then_some(p)
        };
        let (open, disable, close_seat, open_device, close_device, get_fd, dispatch) = (
            sym(c"libseat_open_seat")?,
            sym(c"libseat_disable_seat")?,
            sym(c"libseat_close_seat")?,
            sym(c"libseat_open_device")?,
            sym(c"libseat_close_device")?,
            sym(c"libseat_get_fd")?,
            sym(c"libseat_dispatch")?,
        );
        type Open = unsafe extern "C" fn(*const Listener, *mut c_void) -> *mut c_void;
        type SeatInt = unsafe extern "C" fn(*mut c_void) -> c_int;
        type OpenDevice = unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_int) -> c_int;
        type SeatIntInt = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
        // SAFETY: libseat exports these symbols with these signatures
        // (`libseat.h`).
        let (open, disable, close_seat, open_device, close_device, get_fd, dispatch) = unsafe {
            (
                std::mem::transmute::<*mut c_void, Open>(open),
                std::mem::transmute::<*mut c_void, SeatInt>(disable),
                std::mem::transmute::<*mut c_void, SeatInt>(close_seat),
                std::mem::transmute::<*mut c_void, OpenDevice>(open_device),
                std::mem::transmute::<*mut c_void, SeatIntInt>(close_device),
                std::mem::transmute::<*mut c_void, SeatInt>(get_fd),
                std::mem::transmute::<*mut c_void, SeatIntInt>(dispatch),
            )
        };
        let flags = Box::new(Flags::default());
        // SAFETY: the listener is static and the flags are boxed, both alive
        // as long as the seat.
        let seat = unsafe { open(&LISTENER, std::ptr::from_ref(&*flags).cast_mut().cast()) };
        if seat.is_null() {
            return None;
        }
        let s = Libseat {
            seat,
            flags,
            disable,
            close_seat,
            open_device,
            close_device,
            get_fd,
            dispatch,
            _lib: lib,
        };
        // The seat is usable once it is enabled, which takes a round trip.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !s.flags.active.get() && std::time::Instant::now() < deadline {
            // SAFETY: a live seat; a 100 ms wait.
            if unsafe { (s.dispatch)(s.seat, 100) } < 0 {
                return None;
            }
        }
        s.flags.enabled.set(false);
        s.flags.active.get().then_some(s)
    }
}

impl Drop for Libseat {
    fn drop(&mut self) {
        // SAFETY: the live seat, closed once; its devices were closed first.
        unsafe { (self.close_seat)(self.seat) };
    }
}

/// The controlling VT, switched to graphics mode with its keyboard off for
/// the run, so the console neither draws over the display nor echoes keys.
struct Vt {
    fd: RawFd,
    keyboard_mode: c_int,
}

impl Vt {
    fn take() -> Option<Vt> {
        let fd = libc::STDIN_FILENO;
        let mut mode: c_int = 0;
        let mut keyboard_mode: c_int = 0;
        // SAFETY: querying stdin with ioctls that write one `int`; both fail
        // harmlessly when stdin is not a VT.
        let is_vt = unsafe {
            libc::ioctl(fd, KDGETMODE, &mut mode) == 0
                && libc::ioctl(fd, KDGKBMODE, &mut keyboard_mode) == 0
        };
        if !is_vt {
            return None;
        }
        // SAFETY: switching our own VT's modes; `Drop` restores them.
        unsafe {
            libc::ioctl(fd, KDSETMODE, KD_GRAPHICS);
            libc::ioctl(fd, KDSKBMODE, K_OFF);
        }
        Some(Vt { fd, keyboard_mode })
    }
}

impl Drop for Vt {
    fn drop(&mut self) {
        // SAFETY: restoring the modes `take` changed on the same VT.
        unsafe {
            libc::ioctl(self.fd, KDSKBMODE, self.keyboard_mode);
            libc::ioctl(self.fd, KDSETMODE, 0);
        }
    }
}

/// What a seat dispatch changed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct SeatChange {
    /// The session went to the background: stop touching the card.
    pub paused: bool,
    /// The session is back: devices must be reopened and the mode set again.
    pub resumed: bool,
}

/// An open device node and the seat's id for it.
pub(super) struct Device {
    pub fd: OwnedFd,
    id: Option<c_int>,
}

impl Device {
    /// The seat's id for the device, to [`release`](Seat::release) once its
    /// descriptor has gone elsewhere.
    pub fn id(&self) -> Option<c_int> {
        self.id
    }
}

pub(super) struct Seat {
    libseat: Option<Libseat>,
    _vt: Option<Vt>,
}

impl Seat {
    /// `libseat` unless `VISO_KMS_SEAT=direct` or no seat answers.
    pub fn open() -> Seat {
        let direct = std::env::var("VISO_KMS_SEAT").is_ok_and(|v| v == "direct");
        let libseat = if direct { None } else { Libseat::open() };
        let vt = if libseat.is_none() { Vt::take() } else { None };
        Seat { libseat, _vt: vt }
    }

    pub fn open_device(&mut self, path: &Path, flags: c_int) -> io::Result<Device> {
        let c = CString::new(path.as_os_str().as_encoded_bytes()).map_err(io::Error::other)?;
        if let Some(s) = &self.libseat {
            let mut fd: c_int = -1;
            // SAFETY: a live seat, a NUL-terminated path and a writable fd slot.
            let id = unsafe { (s.open_device)(s.seat, c.as_ptr(), &mut fd) };
            if id < 0 || fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // The seat opens blocking descriptors; the loop wants them not to
            // block.
            // SAFETY: `fd` is the descriptor the seat handed over, now ours.
            unsafe {
                let fl = libc::fcntl(fd, libc::F_GETFL);
                libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
            return Ok(Device {
                // SAFETY: as above.
                fd: unsafe { OwnedFd::from_raw_fd(fd) },
                id: Some(id),
            });
        }
        // SAFETY: opening a NUL-terminated path; the descriptor is owned below.
        let fd = unsafe { libc::open(c.as_ptr(), flags | libc::O_CLOEXEC | libc::O_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Device {
            // SAFETY: a fresh descriptor nothing else owns.
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
            id: None,
        })
    }

    /// Gives `device` back to the seat; its descriptor closes with it.
    pub fn close_device(&mut self, device: Device) {
        self.release(device.id);
    }

    /// Gives a device back to the seat by its id.
    pub fn release(&mut self, id: Option<c_int>) {
        if let (Some(s), Some(id)) = (&self.libseat, id) {
            // SAFETY: an id this seat handed out, closed once.
            unsafe { (s.close_device)(s.seat, id) };
        }
    }

    /// The seat's descriptor for the loop's poll, if it has one.
    pub fn fd(&self) -> Option<RawFd> {
        self.libseat.as_ref().map(|s| {
            // SAFETY: a live seat.
            unsafe { (s.get_fd)(s.seat) }
        })
    }

    /// Reads the seat's pending messages; a pause is acknowledged at once,
    /// so the caller must stop using the card before polling again.
    pub fn dispatch(&mut self) -> SeatChange {
        let Some(s) = &self.libseat else {
            return SeatChange::default();
        };
        // SAFETY: a live seat; no wait.
        unsafe { (s.dispatch)(s.seat, 0) };
        let change = SeatChange {
            paused: s.flags.disabled.replace(false),
            resumed: s.flags.enabled.replace(false),
        };
        if change.paused {
            // SAFETY: a live seat, acknowledging the disable it asked for.
            unsafe { (s.disable)(s.seat) };
        }
        change
    }
}
