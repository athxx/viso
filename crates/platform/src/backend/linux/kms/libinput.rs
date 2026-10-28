//! The libinput adapter, used when libinput is installed: mice, touchpads,
//! touchscreens and absolute pointers read through it (loaded at run time)
//! for its pointer acceleration, touchpad handling (tapping, two-finger and
//! edge scrolling, pinches and swipes, palm rejection) and touchscreen
//! calibration. Keyboards and
//! rotary encoders stay with the evdev decoder, which keeps the kernel's key
//! repeat and turns encoder detents into focus steps; so does every device
//! libinput declines. `VISO_KMS_INPUT=evdev` keeps every device on the
//! decoder; `VISO_KMS_TOUCH_CALIBRATION` is a touchscreen's calibration
//! matrix (six numbers, libinput's row-major 2×3, in panel units).

use std::ffi::{CStr, CString, OsStr, c_char, c_int, c_void};
use std::os::fd::{IntoRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use super::gbm::Library;
use super::input::Input;
use super::seat::Seat;
use crate::event::{GesturePhase, PointerButtons, PointerPhase};

const DEVICE_ADDED: c_int = 1;
const KEYBOARD_KEY: c_int = 300;
const POINTER_MOTION: c_int = 400;
const POINTER_MOTION_ABSOLUTE: c_int = 401;
const POINTER_BUTTON: c_int = 402;
const POINTER_AXIS: c_int = 403;
const POINTER_SCROLL_WHEEL: c_int = 404;
const POINTER_SCROLL_FINGER: c_int = 405;
const POINTER_SCROLL_CONTINUOUS: c_int = 406;
const TOUCH_DOWN: c_int = 500;
const TOUCH_UP: c_int = 501;
const TOUCH_MOTION: c_int = 502;
const TOUCH_CANCEL: c_int = 503;
const GESTURE_SWIPE_BEGIN: c_int = 800;
const GESTURE_SWIPE_UPDATE: c_int = 801;
const GESTURE_SWIPE_END: c_int = 802;
const GESTURE_PINCH_BEGIN: c_int = 803;
const GESTURE_PINCH_UPDATE: c_int = 804;
const GESTURE_PINCH_END: c_int = 805;

const AXIS_VERTICAL: c_int = 0;
const AXIS_HORIZONTAL: c_int = 1;
/// `LIBINPUT_POINTER_AXIS_SOURCE_WHEEL`.
const SOURCE_WHEEL: c_int = 1;
const CAP_TOUCH: c_int = 2;

const CAP_POINTER: c_int = 1;

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

type Ctx = *mut c_void;
type Dev = *mut c_void;
type Ev = *mut c_void;

/// `struct libinput_interface`.
#[repr(C)]
struct Interface {
    open_restricted: unsafe extern "C" fn(*const c_char, c_int, *mut c_void) -> c_int,
    close_restricted: unsafe extern "C" fn(c_int, *mut c_void),
}

static INTERFACE: Interface = Interface {
    open_restricted,
    close_restricted,
};

/// What the callbacks reach: the display's seat, which outlives the
/// context, and the seat's id for each descriptor they opened.
struct Restricted {
    seat: *mut Seat,
    opened: Vec<(RawFd, Option<c_int>)>,
}

unsafe extern "C" fn open_restricted(
    path: *const c_char,
    flags: c_int,
    data: *mut c_void,
) -> c_int {
    // SAFETY: `data` is the boxed `Restricted` the context was created with,
    // alive until the context is unreferenced; `seat` points at the boxed
    // seat of the display that owns both; `path` is a NUL-terminated path.
    let (r, path) = unsafe { (&mut *data.cast::<Restricted>(), CStr::from_ptr(path)) };
    // SAFETY: as above.
    let seat = unsafe { &mut *r.seat };
    match seat.open_device(Path::new(OsStr::from_bytes(path.to_bytes())), flags) {
        Ok(device) => {
            let id = device.id();
            let fd = device.fd.into_raw_fd();
            r.opened.push((fd, id));
            fd
        }
        Err(e) => -e.raw_os_error().unwrap_or(libc::EIO),
    }
}

unsafe extern "C" fn close_restricted(fd: c_int, data: *mut c_void) {
    // SAFETY: as in `open_restricted`.
    let r = unsafe { &mut *data.cast::<Restricted>() };
    let id = r
        .opened
        .iter()
        .position(|&(f, _)| f == fd)
        .and_then(|i| r.opened.swap_remove(i).1);
    // SAFETY: `fd` is a descriptor `open_restricted` handed libinput, which
    // gives it back here once.
    unsafe { libc::close(fd) };
    // SAFETY: as in `open_restricted`.
    unsafe { (*r.seat).release(id) };
}

macro_rules! api {
    ($($name:ident: fn($($arg:ty),*) $(-> $ret:ty)?;)*) => {
        #[allow(non_snake_case, reason = "the C names")]
        struct Api {
            $($name: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)*
            libinput_event_pointer_get_scroll_value_v120: Option<unsafe extern "C" fn(Ev, c_int) -> f64>,
            libinput_event_pointer_get_scroll_value: Option<unsafe extern "C" fn(Ev, c_int) -> f64>,
            libinput_device_config_calibration_set_matrix: Option<unsafe extern "C" fn(Dev, *const f32) -> c_int>,
            _lib: Library,
        }

        impl Api {
            fn load() -> Option<Api> {
                let lib = Library::open(&[c"libinput.so.10", c"libinput.so"])?;
                let sym = |name: &str| {
                    let name = CString::new(name).ok()?;
                    let p = lib.symbol(&name);
                    (!p.is_null()).then_some(p)
                };
                // SAFETY: libinput exports each symbol with the signature
                // declared here (`libinput.h`); the optional ones arrived in
                // later versions and stay `None` where missing.
                unsafe {
                    Some(Api {
                        $($name: std::mem::transmute::<*mut c_void, unsafe extern "C" fn($($arg),*) $(-> $ret)?>(sym(stringify!($name))?),)*
                        libinput_event_pointer_get_scroll_value_v120: sym("libinput_event_pointer_get_scroll_value_v120")
                            .map(|p| std::mem::transmute::<*mut c_void, unsafe extern "C" fn(Ev, c_int) -> f64>(p)),
                        libinput_event_pointer_get_scroll_value: sym("libinput_event_pointer_get_scroll_value")
                            .map(|p| std::mem::transmute::<*mut c_void, unsafe extern "C" fn(Ev, c_int) -> f64>(p)),
                        libinput_device_config_calibration_set_matrix: sym("libinput_device_config_calibration_set_matrix")
                            .map(|p| std::mem::transmute::<*mut c_void, unsafe extern "C" fn(Dev, *const f32) -> c_int>(p)),
                        _lib: lib,
                    })
                }
            }
        }
    };
}

api! {
    libinput_path_create_context: fn(*const Interface, *mut c_void) -> Ctx;
    libinput_path_add_device: fn(Ctx, *const c_char) -> Dev;
    libinput_path_remove_device: fn(Dev);
    libinput_unref: fn(Ctx) -> Ctx;
    libinput_get_fd: fn(Ctx) -> c_int;
    libinput_dispatch: fn(Ctx) -> c_int;
    libinput_suspend: fn(Ctx);
    libinput_resume: fn(Ctx) -> c_int;
    libinput_get_event: fn(Ctx) -> Ev;
    libinput_event_get_type: fn(Ev) -> c_int;
    libinput_event_get_device: fn(Ev) -> Dev;
    libinput_event_destroy: fn(Ev);
    libinput_device_ref: fn(Dev) -> Dev;
    libinput_device_unref: fn(Dev) -> Dev;
    libinput_device_set_user_data: fn(Dev, *mut c_void);
    libinput_device_get_user_data: fn(Dev) -> *mut c_void;
    libinput_device_has_capability: fn(Dev, c_int) -> c_int;
    libinput_device_config_calibration_has_matrix: fn(Dev) -> c_int;
    libinput_event_get_keyboard_event: fn(Ev) -> Ev;
    libinput_event_keyboard_get_key: fn(Ev) -> u32;
    libinput_event_keyboard_get_key_state: fn(Ev) -> c_int;
    libinput_event_get_pointer_event: fn(Ev) -> Ev;
    libinput_event_pointer_get_dx: fn(Ev) -> f64;
    libinput_event_pointer_get_dy: fn(Ev) -> f64;
    libinput_event_pointer_get_absolute_x_transformed: fn(Ev, u32) -> f64;
    libinput_event_pointer_get_absolute_y_transformed: fn(Ev, u32) -> f64;
    libinput_event_pointer_get_button: fn(Ev) -> u32;
    libinput_event_pointer_get_button_state: fn(Ev) -> c_int;
    libinput_event_pointer_has_axis: fn(Ev, c_int) -> c_int;
    libinput_event_pointer_get_axis_value: fn(Ev, c_int) -> f64;
    libinput_event_pointer_get_axis_value_discrete: fn(Ev, c_int) -> f64;
    libinput_event_pointer_get_axis_source: fn(Ev) -> c_int;
    libinput_event_get_touch_event: fn(Ev) -> Ev;
    libinput_event_touch_get_seat_slot: fn(Ev) -> i32;
    libinput_event_touch_get_x_transformed: fn(Ev, u32) -> f64;
    libinput_event_touch_get_y_transformed: fn(Ev, u32) -> f64;
    libinput_event_get_gesture_event: fn(Ev) -> Ev;
    libinput_event_gesture_get_dx: fn(Ev) -> f64;
    libinput_event_gesture_get_dy: fn(Ev) -> f64;
    libinput_event_gesture_get_scale: fn(Ev) -> f64;
    libinput_event_gesture_get_angle_delta: fn(Ev) -> f64;
}

/// A device libinput reads for the display.
struct Taken {
    path: PathBuf,
    device: Dev,
}

/// A libinput context over the devices the display gives it.
pub(super) struct LibInput {
    api: Api,
    ctx: Ctx,
    /// What the callbacks read, alive as long as the context.
    _restricted: Box<Restricted>,
    devices: Vec<Taken>,
    next_serial: u32,
    calibration: Option<[f32; 6]>,
    /// Where each contact down is: serial, slot, position. A lift carries
    /// no position of its own.
    contacts: Vec<(u32, u32, f64, f64)>,
    gestures: Gestures,
}

/// The first serial: above any descriptor number, so a contact's pointer id
/// never collides with one from an evdev device.
const FIRST_SERIAL: u32 = 1 << 16;

impl LibInput {
    /// A context, unless libinput is not installed or `VISO_KMS_INPUT=evdev`.
    /// `seat` must outlive it.
    pub fn open(seat: &mut Seat) -> Option<LibInput> {
        if std::env::var("VISO_KMS_INPUT").is_ok_and(|v| v == "evdev") {
            return None;
        }
        let api = Api::load()?;
        let mut restricted = Box::new(Restricted {
            seat: std::ptr::from_mut(seat),
            opened: Vec::new(),
        });
        // SAFETY: the interface is static and the user data boxed, both alive
        // as long as the context.
        let ctx = unsafe {
            (api.libinput_path_create_context)(
                &INTERFACE,
                std::ptr::from_mut(&mut *restricted).cast(),
            )
        };
        if ctx.is_null() {
            return None;
        }
        let calibration = std::env::var("VISO_KMS_TOUCH_CALIBRATION")
            .ok()
            .and_then(|m| parse_calibration(&m));
        Some(LibInput {
            api,
            ctx,
            _restricted: restricted,
            devices: Vec::new(),
            next_serial: FIRST_SERIAL,
            calibration,
            contacts: Vec::new(),
            gestures: Gestures::default(),
        })
    }

    pub fn fd(&self) -> RawFd {
        // SAFETY: a live context.
        unsafe { (self.api.libinput_get_fd)(self.ctx) }
    }

    /// Hands libinput the device at `path`; `false` when it declines it.
    pub fn add(&mut self, path: &Path) -> bool {
        if self.devices.iter().any(|d| d.path == path) {
            return true;
        }
        let Ok(c) = CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: a live context and a NUL-terminated path; the device is
        // referenced below and unreferenced when removed.
        let device = unsafe { (self.api.libinput_path_add_device)(self.ctx, c.as_ptr()) };
        if device.is_null() {
            return false;
        }
        let serial = self.next_serial;
        self.next_serial = self.next_serial.wrapping_add(1).max(FIRST_SERIAL);
        // SAFETY: a device libinput just returned.
        unsafe {
            (self.api.libinput_device_ref)(device);
            (self.api.libinput_device_set_user_data)(device, serial as usize as *mut c_void);
            if let (Some(matrix), Some(set)) = (
                &self.calibration,
                self.api.libinput_device_config_calibration_set_matrix,
            ) && (self.api.libinput_device_has_capability)(device, CAP_TOUCH) != 0
                && (self.api.libinput_device_config_calibration_has_matrix)(device) != 0
            {
                set(device, matrix.as_ptr());
            }
        }
        self.devices.push(Taken {
            path: path.to_owned(),
            device,
        });
        true
    }

    /// Whether libinput reads the device at `path`.
    pub fn reads(&self, path: &Path) -> bool {
        self.devices.iter().any(|d| d.path == path)
    }

    /// Takes the device at `path` back from libinput; `false` when it does
    /// not read it.
    pub fn remove(&mut self, path: &Path) -> bool {
        let Some(at) = self.devices.iter().position(|d| d.path == path) else {
            return false;
        };
        let taken = self.devices.swap_remove(at);
        // SAFETY: a device added to this context and referenced by `add`.
        unsafe {
            (self.api.libinput_path_remove_device)(taken.device);
            (self.api.libinput_device_unref)(taken.device);
        }
        true
    }

    /// Whether libinput reads a device that moves the pointer.
    pub fn has_pointer(&self) -> bool {
        // SAFETY: devices referenced by this context.
        self.devices.iter().any(
            |d| unsafe { (self.api.libinput_device_has_capability)(d.device, CAP_POINTER) } != 0,
        )
    }

    /// The session went away: libinput lets go of its devices.
    pub fn suspend(&mut self) {
        // SAFETY: a live context.
        unsafe { (self.api.libinput_suspend)(self.ctx) };
    }

    /// The session is back: libinput opens its devices again.
    pub fn resume(&mut self) {
        // SAFETY: a live context.
        unsafe { (self.api.libinput_resume)(self.ctx) };
    }

    /// Reads what is pending, each input with its device's serial.
    pub fn read(&mut self, out: &mut Vec<(u32, Input)>) {
        let api = &self.api;
        // SAFETY: a live context; every event is read through the accessors
        // of its type and destroyed once.
        unsafe {
            (api.libinput_dispatch)(self.ctx);
            loop {
                let ev = (api.libinput_get_event)(self.ctx);
                if ev.is_null() {
                    break;
                }
                let device = (api.libinput_event_get_device)(ev);
                let serial = (api.libinput_device_get_user_data)(device) as usize as u32;
                let kind = (api.libinput_event_get_type)(ev);
                if (GESTURE_SWIPE_BEGIN..=GESTURE_PINCH_END).contains(&kind) {
                    let g = (api.libinput_event_get_gesture_event)(ev);
                    let mut gesture = Gesture {
                        kind,
                        dx: (api.libinput_event_gesture_get_dx)(g),
                        dy: (api.libinput_event_gesture_get_dy)(g),
                        scale: 1.0,
                        angle: 0.0,
                    };
                    if kind == GESTURE_PINCH_UPDATE {
                        gesture.scale = (api.libinput_event_gesture_get_scale)(g);
                        gesture.angle = (api.libinput_event_gesture_get_angle_delta)(g);
                    }
                    if let Some(input) = self.gestures.hear(gesture) {
                        out.push((serial, input));
                    }
                    (api.libinput_event_destroy)(ev);
                    continue;
                }
                for mut input in translate(api, kind, ev).into_iter().flatten() {
                    if let Input::Touch {
                        id, phase, x, y, ..
                    } = &mut input
                    {
                        let at = self
                            .contacts
                            .iter()
                            .position(|c| (c.0, c.1) == (serial, *id));
                        match (*phase, at) {
                            (PointerPhase::Down | PointerPhase::Moved, Some(at)) => {
                                self.contacts[at] = (serial, *id, *x, *y);
                            }
                            (PointerPhase::Down | PointerPhase::Moved, None) => {
                                self.contacts.push((serial, *id, *x, *y));
                            }
                            (_, Some(at)) => {
                                let (.., cx, cy) = self.contacts.swap_remove(at);
                                (*x, *y) = (cx, cy);
                            }
                            (_, None) => continue,
                        }
                    }
                    out.push((serial, input));
                }
                (api.libinput_event_destroy)(ev);
            }
        }
    }
}

/// The inputs one event carries (a scroll may move both axes).
///
/// # Safety
/// `ev` is a live event of type `kind`.
unsafe fn translate(api: &Api, kind: c_int, ev: Ev) -> [Option<Input>; 2] {
    // SAFETY: each accessor matches the event's type, per the caller.
    unsafe {
        match kind {
            DEVICE_ADDED => [None, None],
            KEYBOARD_KEY => {
                let k = (api.libinput_event_get_keyboard_event)(ev);
                [
                    Some(Input::Key {
                        code: (api.libinput_event_keyboard_get_key)(k) as u16,
                        value: (api.libinput_event_keyboard_get_key_state)(k),
                    }),
                    None,
                ]
            }
            POINTER_MOTION => {
                let p = (api.libinput_event_get_pointer_event)(ev);
                [
                    Some(Input::Motion {
                        dx: (api.libinput_event_pointer_get_dx)(p),
                        dy: (api.libinput_event_pointer_get_dy)(p),
                    }),
                    None,
                ]
            }
            POINTER_MOTION_ABSOLUTE => {
                let p = (api.libinput_event_get_pointer_event)(ev);
                [
                    Some(Input::Position {
                        x: (api.libinput_event_pointer_get_absolute_x_transformed)(p, 1),
                        y: (api.libinput_event_pointer_get_absolute_y_transformed)(p, 1),
                    }),
                    None,
                ]
            }
            POINTER_BUTTON => {
                let p = (api.libinput_event_get_pointer_event)(ev);
                let button = match (api.libinput_event_pointer_get_button)(p) {
                    BTN_LEFT => PointerButtons::PRIMARY,
                    BTN_RIGHT => PointerButtons::SECONDARY,
                    BTN_MIDDLE => PointerButtons::MIDDLE,
                    _ => return [None, None],
                };
                [
                    Some(Input::Button {
                        button,
                        pressed: (api.libinput_event_pointer_get_button_state)(p) != 0,
                    }),
                    None,
                ]
            }
            POINTER_SCROLL_WHEEL | POINTER_SCROLL_FINGER | POINTER_SCROLL_CONTINUOUS => {
                let (Some(v120), Some(value)) = (
                    api.libinput_event_pointer_get_scroll_value_v120,
                    api.libinput_event_pointer_get_scroll_value,
                ) else {
                    return [None, None];
                };
                let p = (api.libinput_event_get_pointer_event)(ev);
                let axis = |a: c_int, read: unsafe extern "C" fn(Ev, c_int) -> f64| {
                    if (api.libinput_event_pointer_has_axis)(p, a) != 0 {
                        read(p, a)
                    } else {
                        0.0
                    }
                };
                if kind == POINTER_SCROLL_WHEEL {
                    // Notches, positive away from the user and to the right.
                    let y = -axis(AXIS_VERTICAL, v120) / 120.0;
                    let x = axis(AXIS_HORIZONTAL, v120) / 120.0;
                    [Some(Input::Wheel { x, y }), None]
                } else {
                    [
                        Some(Input::Scroll {
                            dx: axis(AXIS_HORIZONTAL, value),
                            dy: axis(AXIS_VERTICAL, value),
                        }),
                        None,
                    ]
                }
            }
            // A libinput too old for the scroll events above reports scrolling
            // only this way; a newer one reports both, and this copy is skipped.
            POINTER_AXIS if api.libinput_event_pointer_get_scroll_value_v120.is_none() => {
                let p = (api.libinput_event_get_pointer_event)(ev);
                let has = |a| (api.libinput_event_pointer_has_axis)(p, a) != 0;
                if (api.libinput_event_pointer_get_axis_source)(p) == SOURCE_WHEEL {
                    let notches = |a| {
                        if has(a) {
                            (api.libinput_event_pointer_get_axis_value_discrete)(p, a)
                        } else {
                            0.0
                        }
                    };
                    [
                        Some(Input::Wheel {
                            x: notches(AXIS_HORIZONTAL),
                            y: -notches(AXIS_VERTICAL),
                        }),
                        None,
                    ]
                } else {
                    let value = |a| {
                        if has(a) {
                            (api.libinput_event_pointer_get_axis_value)(p, a)
                        } else {
                            0.0
                        }
                    };
                    [
                        Some(Input::Scroll {
                            dx: value(AXIS_HORIZONTAL),
                            dy: value(AXIS_VERTICAL),
                        }),
                        None,
                    ]
                }
            }
            TOUCH_DOWN | TOUCH_MOTION | TOUCH_UP | TOUCH_CANCEL => {
                let t = (api.libinput_event_get_touch_event)(ev);
                let id = (api.libinput_event_touch_get_seat_slot)(t).max(0) as u32;
                let phase = match kind {
                    TOUCH_DOWN => PointerPhase::Down,
                    TOUCH_MOTION => PointerPhase::Moved,
                    TOUCH_UP => PointerPhase::Up,
                    _ => PointerPhase::Cancel,
                };
                // Up and cancel carry no position: the contact lifts where it
                // last was, which the display remembers.
                let (x, y) = if matches!(kind, TOUCH_DOWN | TOUCH_MOTION) {
                    (
                        (api.libinput_event_touch_get_x_transformed)(t, 1),
                        (api.libinput_event_touch_get_y_transformed)(t, 1),
                    )
                } else {
                    (f64::NAN, f64::NAN)
                };
                [
                    Some(Input::Touch {
                        id,
                        phase,
                        x,
                        y,
                        pressure: 1.0,
                    }),
                    None,
                ]
            }
            _ => [None, None],
        }
    }
}

/// A touchpad's gesture event, as libinput reports it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Gesture {
    kind: c_int,
    /// How far the fingers' middle moved since the last event, in pixels.
    dx: f64,
    dy: f64,
    /// A pinch update's spread, as a factor of the spread at its begin.
    scale: f64,
    /// A pinch update's turn since the last, in degrees, clockwise.
    angle: f64,
}

/// Turns a touchpad's gesture events into inputs, each a step from the last.
#[derive(Debug, Default)]
struct Gestures {
    /// The spread the running pinch reached by its last update.
    scale: f64,
}

impl Gestures {
    fn hear(&mut self, g: Gesture) -> Option<Input> {
        let (phase, scale, rotation, dx, dy) = match g.kind {
            GESTURE_SWIPE_BEGIN | GESTURE_PINCH_BEGIN => {
                self.scale = 1.0;
                (GesturePhase::Began, 1.0, 0.0, 0.0, 0.0)
            }
            GESTURE_SWIPE_UPDATE => (GesturePhase::Changed, 1.0, 0.0, g.dx, g.dy),
            GESTURE_PINCH_UPDATE => {
                let by = if g.scale > 0.0 && self.scale > 0.0 {
                    g.scale / self.scale
                } else {
                    1.0
                };
                self.scale = g.scale;
                (GesturePhase::Changed, by, g.angle.to_radians(), g.dx, g.dy)
            }
            // Lifted or cancelled alike: whatever it did stays done.
            GESTURE_SWIPE_END | GESTURE_PINCH_END => (GesturePhase::Ended, 1.0, 0.0, 0.0, 0.0),
            _ => return None,
        };
        Some(Input::Gesture {
            phase,
            scale,
            rotation,
            dx,
            dy,
        })
    }
}

impl Drop for LibInput {
    fn drop(&mut self) {
        for taken in self.devices.drain(..) {
            // SAFETY: devices referenced by `add`, unreferenced once.
            unsafe { (self.api.libinput_device_unref)(taken.device) };
        }
        // SAFETY: the context, unreferenced once; it closes its devices
        // through `close_restricted` while `restricted` is still alive.
        unsafe { (self.api.libinput_unref)(self.ctx) };
    }
}

/// Six numbers, separated by commas or spaces.
fn parse_calibration(text: &str) -> Option<[f32; 6]> {
    let mut matrix = [0.0; 6];
    let mut values = text
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|v| !v.is_empty());
    for slot in &mut matrix {
        *slot = values
            .next()?
            .parse()
            .ok()
            .filter(|v: &f32| v.is_finite())?;
    }
    values.next().is_none().then_some(matrix)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gesture events in `libinput debug-events` output, as libinput
    /// would hand them over.
    fn recorded(text: &str) -> Vec<Gesture> {
        let mut out = Vec::new();
        for line in text.lines() {
            let mut words = line.split_whitespace();
            let (Some(_device), Some(name)) = (words.next(), words.next()) else {
                continue;
            };
            let kind = match name {
                "GESTURE_SWIPE_BEGIN" => GESTURE_SWIPE_BEGIN,
                "GESTURE_SWIPE_UPDATE" => GESTURE_SWIPE_UPDATE,
                "GESTURE_SWIPE_END" => GESTURE_SWIPE_END,
                "GESTURE_PINCH_BEGIN" => GESTURE_PINCH_BEGIN,
                "GESTURE_PINCH_UPDATE" => GESTURE_PINCH_UPDATE,
                "GESTURE_PINCH_END" => GESTURE_PINCH_END,
                _ => continue,
            };
            // The time, then the finger count, then an update's motion
            // (accelerated, then raw in parentheses) and a pinch's scale
            // `@` turn.
            let rest = words.skip(2).collect::<Vec<_>>().join(" ");
            let mut g = Gesture {
                kind,
                dx: 0.0,
                dy: 0.0,
                scale: 1.0,
                angle: 0.0,
            };
            if let Some((motion, tail)) = rest.split_once('(') {
                let motion: Vec<f64> = motion
                    .split(['/', ' '])
                    .filter_map(|v| v.parse().ok())
                    .collect();
                (g.dx, g.dy) = (motion[0], motion[1]);
                let tail = tail.split_once(')').map_or("", |(_, t)| t);
                if let Some((scale, angle)) = tail.split_once('@') {
                    g.scale = scale.trim().parse().unwrap();
                    g.angle = angle.trim().parse().unwrap();
                }
            }
            out.push(g);
        }
        out
    }

    #[test]
    fn recorded_pinches_and_swipes_come_out_as_steps() {
        let events = recorded(include_str!("fixtures/libinput-gestures.txt"));
        assert_eq!(events.len(), 12);
        let mut gestures = Gestures::default();
        let inputs: Vec<_> = events
            .into_iter()
            .filter_map(|g| gestures.hear(g))
            .collect();
        let steps = |range: std::ops::Range<usize>| {
            let (mut scale, mut rotation, mut dx, mut dy) = (1.0, 0.0, 0.0, 0.0);
            for input in &inputs[range] {
                let Input::Gesture {
                    scale: s,
                    rotation: r,
                    dx: x,
                    dy: y,
                    ..
                } = *input
                else {
                    panic!("{input:?}");
                };
                (scale, rotation, dx, dy) = (scale * s, rotation + r, dx + x, dy + y);
            }
            (scale, rotation, dx, dy)
        };
        let phase = |i: usize| match inputs[i] {
            Input::Gesture { phase, .. } => phase,
            _ => unreachable!(),
        };
        let phases: Vec<_> = (0..inputs.len()).map(phase).collect();
        use GesturePhase::{Began, Changed, Ended};
        assert_eq!(
            phases,
            [
                Began, Changed, Changed, Changed, Ended, Began, Changed, Changed, Ended, Began,
                Changed, Ended
            ]
        );

        // A pinch's steps multiply to the spread libinput reports since its
        // begin, and its turns add up.
        let (scale, rotation, dx, dy) = steps(0..5);
        assert!((scale - 1.5).abs() < 1e-9, "{scale}");
        assert!((rotation - 2.0f64.to_radians()).abs() < 1e-9, "{rotation}");
        assert!((dx - 2.0).abs() < 1e-9 && (dy - 0.79).abs() < 1e-9);

        // A swipe moves and neither scales nor turns.
        let (scale, rotation, dx, dy) = steps(5..9);
        assert_eq!((scale, rotation), (1.0, 0.0));
        assert!((dx - 32.0).abs() < 1e-9 && dy.abs() < 1e-9);

        // A pinch begun anew starts from its own spread.
        let (scale, ..) = steps(9..12);
        assert!((scale - 0.8).abs() < 1e-9, "{scale}");
    }

    #[test]
    fn a_calibration_is_six_finite_numbers() {
        assert_eq!(
            parse_calibration("1, 0, 0, 0 1 0"),
            Some([1.0, 0.0, 0.0, 0.0, 1.0, 0.0])
        );
        assert_eq!(
            parse_calibration("0 -1 1 1 0 0"),
            Some([0.0, -1.0, 1.0, 1.0, 0.0, 0.0])
        );
        assert_eq!(parse_calibration("1 0 0 0 1"), None, "five");
        assert_eq!(parse_calibration("1 0 0 0 1 0 0"), None, "seven");
        assert_eq!(parse_calibration("1 0 0 0 1 nan"), None);
    }
}
