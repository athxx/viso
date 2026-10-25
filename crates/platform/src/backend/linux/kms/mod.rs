//! The embedded Linux backend: one fullscreen window on a display the app
//! drives itself, with no display server. The card is set up through
//! DRM/KMS (atomic modesetting, the legacy calls where a driver has no
//! atomic), frames reach it as page flips paced by their completion events,
//! input is read from evdev (pointers and touchscreens through libinput
//! where it is installed), and hotplug arrives on the kernel's uevent
//! socket.
//!
//! Everything waits in one epoll: the card, every input device (and
//! libinput's one descriptor), the uevent socket, the seat, the loop's waker
//! and a timerfd for deadlines. An idle app sleeps there with nothing armed,
//! so it never wakes.
//!
//! What a desktop gives is not here: one window, an in-app clipboard, no
//! menu bar (a menu's accelerators still work), no portals, no input method
//! ([`PlatformCaps::FULLSCREEN`]). Rotary encoders turn into focus
//! navigation (Tab / Shift+Tab), so a knob drives the same focus path as a
//! keyboard.
//!
//! The environment tunes it: `VISO_KMS_DEVICE` (the card, else the first
//! `/dev/dri/card*` with a connected panel), `VISO_KMS_MODE` (`1280x720` or
//! `1280x720@60`), `VISO_KMS_FORMAT` (`xrgb8888`, `rgb565`),
//! `VISO_KMS_ROTATION` and `VISO_KMS_TOUCH_ROTATION` (`0`, `90`, `180`,
//! `270`), `VISO_KMS_SEAT=direct` (skip libseat), `VISO_KMS_LEGACY` (no
//! atomic), `VISO_KMS_INPUT=evdev` (no libinput),
//! `VISO_KMS_TOUCH_CALIBRATION` (libinput's six-number matrix) and
//! `VISO_SCALE`. The mode, format and rotation a device target's
//! `display` declares are set the same way when the app is built, and are
//! its defaults ([`setting`]).

mod drm;
mod gbm;
mod input;
mod libinput;
mod scanout;
mod seat;
mod uevent;

use std::cell::RefCell;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use self::drm::Card;
use self::input::{Input, InputDevice};
use self::scanout::{Change, Output, Rotation};
use self::seat::Seat;
use self::uevent::{Hotplug, Uevents};
use super::xkb::{KeyText, Keyboard};
use super::{After, Chore, Display, Pump, WakeReceiver, Waker, translate, wake_channel};
use crate::control::{PlatformCaps, PlatformError, WindowConfig, WindowId};
use crate::event::{
    Appearance, CursorIcon, KeyCode, Modifiers, PointerButtons, PointerId, PointerKind,
    PointerPhase, RawEvent, RawPointer, RawScroll, RawText,
};
use crate::handler::AppHandler;
use crate::menu::Menu;
use crate::panel::setting;
use crate::{Instant, PlatformApp, RawWindowHandle, TextEntry, Window};

/// The display's one window.
const WINDOW: WindowId = WindowId(1);

const TOUCH_BIT: u64 = 1 << 32;

const TAG_CARD: u64 = 1;
const TAG_SEAT: u64 = 2;
const TAG_UEVENT: u64 = 3;
const TAG_TIMER: u64 = 4;
const TAG_WAKE: u64 = 5;
const TAG_LIBINPUT: u64 = 6;
/// An input device: the tag's low half is its descriptor.
const TAG_INPUT: u64 = 1 << 32;

/// A device just plugged in may not have its permissions yet: it is tried
/// again this often, this many times.
const RETRY_EVERY: Duration = Duration::from_millis(100);
const RETRIES: u8 = 20;

fn backend_error(e: impl std::fmt::Display) -> PlatformError {
    PlatformError::Backend(e.to_string())
}

/// The loop's one epoll set.
struct Epoll(OwnedFd);

impl Epoll {
    fn new() -> io::Result<Epoll> {
        // SAFETY: creating an epoll set; the descriptor is owned below.
        let fd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh descriptor nothing else owns.
        Ok(Epoll(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    fn add(&self, fd: RawFd, tag: u64) {
        let mut ev = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: tag,
        };
        // SAFETY: registering a live descriptor with an initialized event.
        unsafe { libc::epoll_ctl(self.0.as_raw_fd(), libc::EPOLL_CTL_ADD, fd, &mut ev) };
    }

    fn remove(&self, fd: RawFd) {
        // SAFETY: unregistering a descriptor; the event is ignored for a delete.
        unsafe {
            libc::epoll_ctl(
                self.0.as_raw_fd(),
                libc::EPOLL_CTL_DEL,
                fd,
                std::ptr::null_mut(),
            )
        };
    }

    /// The tags of the descriptors ready within `timeout_ms` (`-1` waits
    /// indefinitely).
    fn wait(&self, timeout_ms: i32, out: &mut Vec<u64>) {
        let mut events = [libc::epoll_event { events: 0, u64: 0 }; 32];
        // SAFETY: waiting into a live local array of its length.
        let n = unsafe {
            libc::epoll_wait(
                self.0.as_raw_fd(),
                events.as_mut_ptr(),
                events.len() as i32,
                timeout_ms,
            )
        };
        for ev in &events[..usize::try_from(n).unwrap_or(0)] {
            let tag = ev.u64;
            out.push(tag);
        }
    }
}

/// A one-shot monotonic timer for the loop's deadline.
struct Timer(OwnedFd);

impl Timer {
    fn new() -> io::Result<Timer> {
        // SAFETY: creating a timer; the descriptor is owned below.
        let fd = unsafe {
            libc::timerfd_create(
                libc::CLOCK_MONOTONIC,
                libc::TFD_NONBLOCK | libc::TFD_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh descriptor nothing else owns.
        Ok(Timer(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    /// Fires once after `after`; `None` disarms it.
    fn arm(&self, after: Option<Duration>) {
        let value = after.map_or(
            libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            |d| {
                // A zero value disarms: a deadline already past fires at once.
                let d = d.max(Duration::from_nanos(1));
                libc::timespec {
                    tv_sec: d.as_secs() as libc::time_t,
                    tv_nsec: libc::c_long::from(d.subsec_nanos() as i32),
                }
            },
        );
        let spec = libc::itimerspec {
            it_interval: libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            it_value: value,
        };
        // SAFETY: setting a live timer from an initialized spec.
        unsafe { libc::timerfd_settime(self.0.as_raw_fd(), 0, &spec, std::ptr::null_mut()) };
    }

    fn clear(&self) {
        let mut expirations = 0u64;
        // SAFETY: reading the 8-byte expiration count into a live local.
        unsafe { libc::read(self.0.as_raw_fd(), (&raw mut expirations).cast(), 8) };
    }
}

/// The modifiers from evdev key codes, when no keymap tracks them.
#[derive(Default)]
struct Held {
    shift: u8,
    control: u8,
    alt: u8,
    logo: u8,
}

impl Held {
    fn update(&mut self, code: u16, pressed: bool) {
        let slot = match code {
            42 | 54 => &mut self.shift,
            29 | 97 => &mut self.control,
            56 | 100 => &mut self.alt,
            125 | 126 => &mut self.logo,
            _ => return,
        };
        *slot = if pressed {
            slot.saturating_add(1)
        } else {
            slot.saturating_sub(1)
        };
    }

    fn modifiers(&self) -> Modifiers {
        Modifiers {
            shift: self.shift > 0,
            control: self.control > 0,
            alt: self.alt > 0,
            logo: self.logo > 0,
        }
    }
}

/// The text a key types on a US layout, where no keymap is installed.
fn us_text(code: u16, shift: bool) -> Option<char> {
    const ROWS: [(u16, &str, &str); 4] = [
        (2, "1234567890-=", "!@#$%^&*()_+"),
        (16, "qwertyuiop[]", "QWERTYUIOP{}"),
        (30, "asdfghjkl;'`", "ASDFGHJKL:\"~"),
        (43, "\\zxcvbnm,./", "|ZXCVBNM<>?"),
    ];
    if code == 57 {
        return Some(' ');
    }
    ROWS.iter().find_map(|&(first, plain, shifted)| {
        let i = usize::from(code.checked_sub(first)?);
        let row = if shift { shifted } else { plain };
        row.chars().nth(i)
    })
}

/// Everything the loop reads and the handler's calls update.
pub(crate) struct KmsDisplay {
    pump: Pump,
    output: Rc<Output>,
    /// Mice, touchpads and touchscreens, when libinput is installed; it
    /// opens them through `seat`, so it is dropped first.
    libinput: Option<libinput::LibInput>,
    /// Boxed: libinput keeps its address.
    seat: Box<Seat>,
    epoll: Epoll,
    timer: Timer,
    uevents: Option<Uevents>,
    wake_registered: bool,
    inputs: Vec<InputDevice>,
    /// Devices to open again: path, next try, tries left.
    retry: Vec<(PathBuf, Instant, u8)>,
    keyboard: Option<Keyboard>,
    held: Held,
    /// Keys whose press was delivered, released if the session pauses.
    pressed: Vec<u16>,
    window: Option<WindowId>,
    /// A redraw waits for the flip in flight, or for the panel to return.
    wants_redraw: bool,
    paused: bool,
    connected: bool,
    scale: f64,
    touch_rotation: Rotation,
    /// The mouse's position in image pixels.
    mouse: (f64, f64),
    buttons: PointerButtons,
    cursor: CursorIcon,
    /// The mouse moved since the last touch: the cursor is up.
    cursor_on: bool,
    clipboard: String,
    tags: Vec<u64>,
    inputs_scratch: Vec<Input>,
    libinput_scratch: Vec<(u32, Input)>,
    hotplug_scratch: Vec<Hotplug>,
}

impl KmsDisplay {
    fn new() -> Result<KmsDisplay, PlatformError> {
        let mut seat = Box::new(Seat::open());
        let output = open_output(&mut seat)?;
        let libinput = libinput::LibInput::open(&mut seat);
        let epoll = Epoll::new().map_err(backend_error)?;
        let timer = Timer::new().map_err(backend_error)?;
        epoll.add(output.card().as_raw_fd(), TAG_CARD);
        epoll.add(timer.0.as_raw_fd(), TAG_TIMER);
        if let Some(fd) = seat.fd() {
            epoll.add(fd, TAG_SEAT);
        }
        let uevents = Uevents::open().ok();
        if let Some(u) = &uevents {
            epoll.add(u.as_raw_fd(), TAG_UEVENT);
        }
        if let Some(li) = &libinput {
            epoll.add(li.fd(), TAG_LIBINPUT);
        }
        let keyboard = Keyboard::new().and_then(|mut k| k.set_keymap_default().then_some(k));
        let scale = output.scale();
        let touch_rotation = setting(
            "VISO_KMS_TOUCH_ROTATION",
            option_env!("VISO_KMS_TOUCH_ROTATION"),
        )
        .and_then(|r| Rotation::parse(&r))
        .unwrap_or(output.rotation());
        let (w, h) = output.logical_size();
        let mut display = KmsDisplay {
            pump: Pump::default(),
            output: Rc::new(output),
            libinput,
            seat,
            epoll,
            timer,
            uevents,
            wake_registered: false,
            inputs: Vec::new(),
            retry: Vec::new(),
            keyboard,
            held: Held::default(),
            pressed: Vec::new(),
            window: None,
            wants_redraw: false,
            paused: false,
            connected: true,
            scale,
            touch_rotation,
            mouse: (f64::from(w) / 2.0, f64::from(h) / 2.0),
            buttons: PointerButtons::NONE,
            cursor: CursorIcon::Default,
            cursor_on: false,
            clipboard: String::new(),
            tags: Vec::new(),
            inputs_scratch: Vec::new(),
            libinput_scratch: Vec::new(),
            hotplug_scratch: Vec::new(),
        };
        display.open_inputs();
        Ok(display)
    }

    fn visible(&self) -> bool {
        !self.paused && self.connected
    }

    fn request_redraw(&mut self, window: WindowId) {
        if self.window != Some(window) {
            return;
        }
        if self.output.flip_pending() || !self.visible() {
            self.wants_redraw = true;
        } else {
            self.pump.push_redraw(window);
        }
    }

    // ---- devices -------------------------------------------------------

    fn open_inputs(&mut self) {
        for path in input::event_nodes() {
            self.add_input(&path, false);
        }
    }

    /// Opens `path` if it is a device the display uses; a hotplugged one
    /// that cannot be opened yet is tried again shortly.
    fn add_input(&mut self, path: &Path, hotplugged: bool) {
        if self.inputs.iter().any(|d| d.path == path) {
            return;
        }
        if self.libinput.as_ref().is_some_and(|li| li.reads(path)) {
            return;
        }
        match InputDevice::open(&mut self.seat, path) {
            // Pointers and touchscreens go to libinput when it takes them.
            Ok(Some(device)) if self.libinput.is_some() && device.caps.libinput() => {
                let caps = device.caps;
                device.close(&mut self.seat);
                let taken = self.libinput.as_mut().is_some_and(|li| li.add(path));
                if taken {
                    self.retry.retain(|(p, _, _)| p != path);
                } else if let Ok(Some(device)) = InputDevice::open(&mut self.seat, path) {
                    debug_assert_eq!(device.caps, caps);
                    self.epoll
                        .add(device.fd(), TAG_INPUT | device.fd() as u32 as u64);
                    self.inputs.push(device);
                    self.retry.retain(|(p, _, _)| p != path);
                }
            }
            Ok(Some(device)) => {
                self.epoll
                    .add(device.fd(), TAG_INPUT | device.fd() as u32 as u64);
                self.inputs.push(device);
                self.retry.retain(|(p, _, _)| p != path);
            }
            Ok(None) => self.retry.retain(|(p, _, _)| p != path),
            Err(_) if hotplugged => {
                if !self.retry.iter().any(|(p, _, _)| p == path) {
                    self.retry
                        .push((path.to_owned(), Instant::now() + RETRY_EVERY, RETRIES));
                }
            }
            Err(_) => {}
        }
    }

    fn remove_input(&mut self, at: usize) {
        let mut device = self.inputs.swap_remove(at);
        self.epoll.remove(device.fd());
        let mut lifted = std::mem::take(&mut self.inputs_scratch);
        device.cancel(&mut lifted);
        let fd = device.fd();
        for input in lifted.drain(..) {
            self.input(fd, input);
        }
        self.inputs_scratch = lifted;
        device.close(&mut self.seat);
        self.pointer_left();
    }

    /// Hides the cursor when no device can move it any more.
    fn pointer_left(&mut self) {
        let pointer = self.inputs.iter().any(|d| d.caps.pointer())
            || self.libinput.as_ref().is_some_and(|li| li.has_pointer());
        if !pointer && self.cursor_on {
            self.cursor_on = false;
            self.refresh_cursor();
        }
    }

    fn close_inputs(&mut self) {
        while !self.inputs.is_empty() {
            self.remove_input(self.inputs.len() - 1);
        }
    }

    fn retry_due(&mut self) {
        let now = Instant::now();
        let due: Vec<PathBuf> = self
            .retry
            .iter()
            .filter(|(_, at, _)| *at <= now)
            .map(|(p, _, _)| p.clone())
            .collect();
        for path in due {
            let Some(i) = self.retry.iter().position(|(p, _, _)| *p == path) else {
                continue;
            };
            let (_, at, left) = &mut self.retry[i];
            if *left == 0 {
                self.retry.swap_remove(i);
                continue;
            }
            *left -= 1;
            *at = now + RETRY_EVERY;
            self.add_input(&path, true);
        }
    }

    // ---- the loop ------------------------------------------------------

    /// Waits up to `timeout_ms` for the loop's descriptors and handles what
    /// is ready.
    fn poll(&mut self, timeout_ms: i32) {
        let mut tags = std::mem::take(&mut self.tags);
        self.epoll.wait(timeout_ms, &mut tags);
        for tag in tags.drain(..) {
            match tag {
                TAG_CARD => self.output.read_flips(),
                TAG_SEAT => self.seat_changed(),
                TAG_UEVENT => self.hotplug(),
                TAG_TIMER => self.timer.clear(),
                TAG_WAKE => {}
                TAG_LIBINPUT => self.read_libinput(),
                _ => self.read_input((tag & 0xffff_ffff) as RawFd),
            }
        }
        self.tags = tags;
        self.retry_due();
        if self.output.take_flipped() && self.wants_redraw && self.visible() {
            self.wants_redraw = false;
            if let Some(window) = self.window {
                self.pump.push_redraw(window);
            }
        }
    }

    fn read_input(&mut self, fd: RawFd) {
        let Some(at) = self.inputs.iter().position(|d| d.fd() == fd) else {
            return;
        };
        let mut inputs = std::mem::take(&mut self.inputs_scratch);
        let alive = self.inputs[at].read(&mut inputs);
        for input in inputs.drain(..) {
            self.input(fd, input);
        }
        self.inputs_scratch = inputs;
        if !alive {
            self.remove_input(at);
        }
    }

    fn read_libinput(&mut self) {
        let Some(li) = &mut self.libinput else {
            return;
        };
        let mut read = std::mem::take(&mut self.libinput_scratch);
        li.read(&mut read);
        for (serial, input) in read.drain(..) {
            self.input(serial as RawFd, input);
        }
        self.libinput_scratch = read;
    }

    fn hotplug(&mut self) {
        let mut events = std::mem::take(&mut self.hotplug_scratch);
        if let Some(u) = &self.uevents {
            u.read(&mut events);
        }
        for event in events.drain(..) {
            match event {
                Hotplug::InputAdded(path) => self.add_input(&path, true),
                Hotplug::InputRemoved(path) => {
                    self.retry.retain(|(p, _, _)| *p != path);
                    if self.libinput.as_mut().is_some_and(|li| li.remove(&path)) {
                        self.pointer_left();
                    }
                    if let Some(at) = self.inputs.iter().position(|d| d.path == path) {
                        self.remove_input(at);
                    }
                }
                Hotplug::Display => self.display_changed(),
            }
        }
        self.hotplug_scratch = events;
    }

    fn display_changed(&mut self) {
        let Some(change) = self.output.reprobe() else {
            return;
        };
        let was = self.visible();
        self.connected = change != Change::Disconnected;
        if change == Change::Resized
            && let Some(window) = self.window
        {
            let (w, h) = self.output.logical_size();
            self.scale = self.output.scale();
            self.mouse = (
                self.mouse.0.min(f64::from(w)),
                self.mouse.1.min(f64::from(h)),
            );
            self.pump.push(RawEvent::SurfaceDestroyed { window });
            self.pump.push(RawEvent::ScaleFactorChanged {
                window,
                scale: self.scale,
                width: w,
                height: h,
            });
            self.pump.push(RawEvent::SurfaceCreated { window });
        }
        self.visibility_changed(was);
    }

    fn seat_changed(&mut self) {
        let change = self.seat.dispatch();
        let was = self.visible();
        if change.paused {
            self.output.pause();
            self.paused = true;
            self.release_all();
            if let Some(li) = &mut self.libinput {
                li.suspend();
            }
        }
        if change.resumed {
            self.paused = false;
            self.output.resume();
            // A session away may have had its devices revoked.
            self.close_inputs();
            if let Some(li) = &mut self.libinput {
                li.resume();
            }
            self.open_inputs();
            if let Some(k) = &mut self.keyboard {
                k.set_keymap_default();
            }
            self.held = Held::default();
            if self.cursor_on {
                self.refresh_cursor();
            }
        }
        self.visibility_changed(was);
    }

    /// Reports the display going out of sight or coming back.
    fn visibility_changed(&mut self, was: bool) {
        let now = self.visible();
        if was == now {
            return;
        }
        self.pump.push(if now {
            RawEvent::Resumed
        } else {
            RawEvent::Suspended
        });
        if now
            && std::mem::take(&mut self.wants_redraw)
            && let Some(window) = self.window
        {
            self.pump.push_redraw(window);
        }
    }

    /// Lifts every key, button and contact: the session paused.
    fn release_all(&mut self) {
        let Some(window) = self.window else {
            return;
        };
        let modifiers = self.modifiers();
        for code in std::mem::take(&mut self.pressed) {
            let kc = u32::from(code) + translate::XKB_EVDEV_OFFSET;
            self.pump.key(
                window,
                translate::key_code(kc),
                None,
                modifiers,
                false,
                false,
            );
        }
        let mut lifted = std::mem::take(&mut self.inputs_scratch);
        for at in 0..self.inputs.len() {
            let fd = self.inputs[at].fd();
            self.inputs[at].cancel(&mut lifted);
            for input in lifted.drain(..) {
                self.input(fd, input);
            }
        }
        self.inputs_scratch = lifted;
        if !self.buttons.is_empty() {
            self.buttons = PointerButtons::NONE;
            self.mouse_event(PointerPhase::Cancel);
        }
    }

    // ---- input ---------------------------------------------------------

    fn modifiers(&self) -> Modifiers {
        match &self.keyboard {
            Some(k) if k.has_keymap() => k.modifiers(),
            _ => self.held.modifiers(),
        }
    }

    fn input(&mut self, device: RawFd, input: Input) {
        if self.window.is_none() {
            return;
        }
        match input {
            Input::Key { code, value } => self.key(code, value),
            Input::Button { button, pressed } => {
                if pressed {
                    self.buttons.0 |= button.0;
                } else {
                    self.buttons.0 &= !button.0;
                }
                self.show_cursor();
                self.mouse_event(if pressed {
                    PointerPhase::Down
                } else {
                    PointerPhase::Up
                });
            }
            Input::Motion { dx, dy } => {
                let (w, h) = self.output.logical_size();
                self.mouse = (
                    (self.mouse.0 + dx).clamp(0.0, f64::from(w.max(1) - 1)),
                    (self.mouse.1 + dy).clamp(0.0, f64::from(h.max(1) - 1)),
                );
                self.show_cursor();
                self.mouse_event(PointerPhase::Moved);
            }
            Input::Position { x, y } => {
                let (w, h) = self.output.logical_size();
                let (u, v) = self.touch_rotation.to_logical(x, y);
                self.mouse = (u * f64::from(w), v * f64::from(h));
                self.show_cursor();
                self.mouse_event(PointerPhase::Moved);
            }
            Input::Wheel { x, y } => {
                let Some(window) = self.window else {
                    return;
                };
                let s = self.scale;
                self.pump.push(RawEvent::Scroll(RawScroll {
                    window,
                    x: self.mouse.0 / s,
                    y: self.mouse.1 / s,
                    delta_x: translate::wheel_notches(x),
                    delta_y: -translate::wheel_notches(y),
                    modifiers: self.modifiers(),
                    phase: crate::ScrollPhase::None,
                }));
            }
            Input::Scroll { dx, dy } => {
                let Some(window) = self.window else {
                    return;
                };
                let s = self.scale;
                self.pump.push(RawEvent::Scroll(RawScroll {
                    window,
                    x: self.mouse.0 / s,
                    y: self.mouse.1 / s,
                    delta_x: dx / s,
                    delta_y: dy / s,
                    modifiers: self.modifiers(),
                    phase: crate::ScrollPhase::None,
                }));
            }
            Input::Dial { steps } => self.dial(steps),
            Input::Touch {
                id,
                phase,
                x,
                y,
                pressure,
            } => {
                let Some(window) = self.window else {
                    return;
                };
                if self.cursor_on {
                    self.cursor_on = false;
                    self.refresh_cursor();
                }
                let (w, h) = self.output.logical_size();
                let (u, v) = self.touch_rotation.to_logical(x, y);
                let s = self.scale;
                let down = matches!(phase, PointerPhase::Down | PointerPhase::Moved);
                self.pump.push(RawEvent::Pointer(RawPointer {
                    window,
                    pointer: PointerId(
                        TOUCH_BIT | (u64::from(device as u32) << 20) | u64::from(id & 0xf_ffff),
                    ),
                    kind: PointerKind::Touch,
                    x: u * f64::from(w) / s,
                    y: v * f64::from(h) / s,
                    pressure,
                    buttons: if down {
                        PointerButtons::PRIMARY
                    } else {
                        PointerButtons::NONE
                    },
                    modifiers: self.modifiers(),
                    phase,
                }));
            }
        }
    }

    fn mouse_event(&mut self, phase: PointerPhase) {
        let Some(window) = self.window else {
            return;
        };
        let s = self.scale;
        self.pump.push(RawEvent::Pointer(RawPointer {
            window,
            pointer: PointerId::MOUSE,
            kind: PointerKind::Mouse,
            x: self.mouse.0 / s,
            y: self.mouse.1 / s,
            pressure: if self.buttons.is_empty() { 0.0 } else { 0.5 },
            buttons: self.buttons,
            modifiers: self.modifiers(),
            phase,
        }));
        self.move_cursor();
    }

    fn key(&mut self, code: u16, value: i32) {
        let Some(window) = self.window else {
            return;
        };
        let kc = u32::from(code) + translate::XKB_EVDEV_OFFSET;
        let pressed = value != 0;
        let repeat = value == 2;
        if !repeat {
            if pressed {
                if !self.pressed.contains(&code) {
                    self.pressed.push(code);
                }
            } else if let Some(at) = self.pressed.iter().position(|c| *c == code) {
                self.pressed.swap_remove(at);
            } else {
                return;
            }
        }
        let modifiers = self.modifiers();
        let keymap = self.keyboard.as_ref().filter(|k| k.has_keymap());
        let base = keymap.map_or_else(|| us_text(code, false), |k| k.base_char(kc));
        let route = self.pump.key(
            window,
            translate::key_code(kc),
            base,
            modifiers,
            pressed,
            repeat,
        );
        if let Some(Chore::Paste(window)) = route.chore {
            self.paste(window);
        }
        if pressed && !route.consumed && !(modifiers.control || modifiers.alt || modifiers.logo) {
            let text = match &mut self.keyboard {
                Some(k) if k.has_keymap() => match k.press_text(kc) {
                    KeyText::Text(text) => Some(text),
                    _ => None,
                },
                _ => us_text(code, modifiers.shift).map(String::from),
            };
            if let Some(text) = text {
                self.pump.push(RawEvent::Text(RawText { window, text }));
            }
        }
        if !repeat {
            if let Some(k) = &mut self.keyboard {
                k.update_key(kc, pressed);
            }
            self.held.update(code, pressed);
        }
    }

    /// An encoder's detents step the focus: forward clockwise, back the
    /// other way.
    fn dial(&mut self, steps: i32) {
        let Some(window) = self.window else {
            return;
        };
        let modifiers = Modifiers {
            shift: steps < 0,
            ..self.modifiers()
        };
        for _ in 0..steps.unsigned_abs().min(16) {
            for pressed in [true, false] {
                self.pump
                    .key(window, KeyCode::Tab, None, modifiers, pressed, false);
            }
        }
    }

    fn paste(&mut self, window: WindowId) {
        if !self.clipboard.is_empty() {
            self.pump.push(RawEvent::Paste {
                window,
                text: self.clipboard.clone(),
            });
        }
    }

    // ---- cursor --------------------------------------------------------

    fn show_cursor(&mut self) {
        if !self.cursor_on {
            self.cursor_on = true;
            self.refresh_cursor();
        }
    }

    fn refresh_cursor(&mut self) {
        let icon = (self.cursor_on && self.cursor != CursorIcon::Hidden).then_some(self.cursor);
        self.output.set_cursor(icon, self.scale);
        self.move_cursor();
    }

    fn move_cursor(&self) {
        if !self.cursor_on {
            return;
        }
        let (w, h) = self.output.logical_size();
        let (nw, nh) = self.output.native_size();
        let (u, v) = self.output.rotation().to_native(
            self.mouse.0 / f64::from(w.max(1)),
            self.mouse.1 / f64::from(h.max(1)),
        );
        self.output
            .move_cursor(u * f64::from(nw), v * f64::from(nh));
    }
}

impl Display for KmsDisplay {
    fn pump(&mut self) -> &mut Pump {
        &mut self.pump
    }

    fn pump_ref(&self) -> &Pump {
        &self.pump
    }

    fn dispatch(&mut self) -> bool {
        self.poll(0);
        true
    }

    fn wait(&mut self, wake_fd: RawFd, deadline: Option<Instant>) -> bool {
        if !self.wake_registered {
            self.epoll.add(wake_fd, TAG_WAKE);
            self.wake_registered = true;
        }
        let retry = self.retry.iter().map(|(_, at, _)| *at).min();
        let deadline = match (deadline, retry) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let now = Instant::now();
        self.timer
            .arm(deadline.map(|d| d.saturating_duration_since(now)));
        self.poll(-1);
        true
    }

    fn before_redraw(&mut self, _window: WindowId) {}

    fn finish(&mut self, after: After) {
        match after {
            After::Copy(text) => self.clipboard = text,
            After::Close(window) => {
                if self.window == Some(window) {
                    self.window = None;
                    self.pump.forget_window(window);
                    self.pump.push(RawEvent::WindowClosed { window });
                }
            }
            // A panel has no other clients to drag from.
            After::Redrawn(_) | After::Dropped(..) => {}
        }
    }
}

/// The card to drive: `VISO_KMS_DEVICE`, else the first card with a
/// connected panel.
fn open_output(seat: &mut Seat) -> Result<Output, PlatformError> {
    let paths: Vec<PathBuf> = match std::env::var_os("VISO_KMS_DEVICE") {
        Some(path) => vec![PathBuf::from(path)],
        None => {
            let mut cards: Vec<PathBuf> = std::fs::read_dir("/dev/dri")
                .map_err(backend_error)?
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("card"))
                })
                .collect();
            cards.sort();
            cards
        }
    };
    let mut last = PlatformError::Backend("no DRM card with a connected display".to_owned());
    for path in paths {
        let device = match seat.open_device(&path, libc::O_RDWR) {
            Ok(device) => device,
            Err(e) => {
                last = backend_error(format!("{}: {e}", path.display()));
                continue;
            }
        };
        let id = device.id();
        let output = Card::from_fd(device.fd).and_then(Output::open);
        match output {
            Ok(Some(output)) => return Ok(output),
            Ok(None) => {}
            Err(e) => last = backend_error(format!("{}: {e}", path.display())),
        }
        seat.release(id);
    }
    Err(last)
}

pub(crate) struct KmsApp {
    display: Rc<RefCell<KmsDisplay>>,
    wakes: Rc<WakeReceiver>,
    waker: Waker,
    window: Option<KmsWindow>,
}

impl KmsApp {
    pub(crate) fn new() -> Result<Self, PlatformError> {
        let (waker, wakes) = wake_channel().map_err(backend_error)?;
        let display = KmsDisplay::new()?;
        Ok(KmsApp {
            display: Rc::new(RefCell::new(display)),
            wakes: Rc::new(wakes),
            waker,
            window: None,
        })
    }
}

impl PlatformApp for KmsApp {
    fn create_window(&mut self, _config: WindowConfig) -> Result<WindowId, PlatformError> {
        let mut d = self.display.borrow_mut();
        if d.window.is_some() {
            return Err(PlatformError::WindowCreation(
                "a KMS display shows one window".to_owned(),
            ));
        }
        d.window = Some(WINDOW);
        d.pump.push(RawEvent::WindowFocused {
            window: WINDOW,
            focused: true,
        });
        let output = d.output.clone();
        drop(d);
        self.window = Some(KmsWindow {
            id: WINDOW,
            output,
            display: self.display.clone(),
        });
        Ok(WINDOW)
    }

    fn run(&mut self, handler: &mut dyn AppHandler) {
        let display = self.display.clone();
        let wakes = self.wakes.clone();
        let waker = self.waker.clone();
        super::run(self, &display, waker, &wakes, handler);
    }

    fn window(&self, id: WindowId) -> Option<&dyn Window> {
        let open = self.display.borrow().window == Some(id);
        self.window
            .as_ref()
            .filter(|w| open && w.id == id)
            .map(|w| w as &dyn Window)
    }

    fn request_redraw(&mut self, window: WindowId) {
        self.display.borrow_mut().request_redraw(window);
    }

    fn set_menu(&mut self, menu: &Menu) {
        self.display.borrow_mut().pump.accels = translate::AccelTable::build(menu);
    }

    fn close_window(&mut self, window: WindowId) {
        self.display.borrow_mut().finish(After::Close(window));
    }

    fn set_clipboard_text(&mut self, text: &str) {
        self.display.borrow_mut().clipboard = text.to_owned();
    }

    fn request_paste(&mut self, window: WindowId) {
        self.display.borrow_mut().paste(window);
    }

    fn set_cursor(&mut self, _window: WindowId, icon: CursorIcon) {
        let mut d = self.display.borrow_mut();
        if d.cursor != icon {
            d.cursor = icon;
            if d.cursor_on {
                d.refresh_cursor();
            }
        }
    }

    fn set_text_entry(&mut self, _window: WindowId, _entry: Option<TextEntry>) {}

    fn show_soft_keyboard(&mut self, _window: WindowId, _show: bool) {}

    fn appearance(&self) -> Appearance {
        self.display.borrow().pump.appearance
    }

    fn locale(&self) -> String {
        crate::locale::from_environment(|name| std::env::var(name).ok())
    }

    fn framed_windows(&self) -> bool {
        false
    }

    fn capabilities(&self) -> PlatformCaps {
        PlatformCaps::FULLSCREEN
    }

    fn loop_waker(&self) -> crate::LoopWaker {
        self.waker.loop_waker()
    }
}

/// The display's window: the whole panel.
pub(crate) struct KmsWindow {
    id: WindowId,
    output: Rc<Output>,
    display: Rc<RefCell<KmsDisplay>>,
}

impl Window for KmsWindow {
    fn id(&self) -> WindowId {
        self.id
    }

    fn request_redraw(&self) {
        self.display.borrow_mut().request_redraw(self.id);
    }

    fn set_title(&mut self, _title: &str) {}

    fn scale_factor(&self) -> f64 {
        self.display.borrow().scale
    }

    fn inner_size(&self) -> (u32, u32) {
        self.output.logical_size()
    }

    fn raw_handle(&self) -> RawWindowHandle {
        let scanout: *const Output = Rc::as_ptr(&self.output);
        RawWindowHandle::Scanout {
            scanout: scanout as *const dyn viso_handle::Scanout,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_us_fallback_types_letters_digits_and_symbols() {
        assert_eq!(us_text(30, false), Some('a'));
        assert_eq!(us_text(30, true), Some('A'));
        assert_eq!(us_text(2, true), Some('!'));
        assert_eq!(us_text(13, false), Some('='));
        assert_eq!(us_text(41, true), Some('~'));
        assert_eq!(us_text(43, false), Some('\\'));
        assert_eq!(us_text(53, true), Some('?'));
        assert_eq!(us_text(57, false), Some(' '));
        assert_eq!(us_text(28, false), None, "Enter types no text");
        assert_eq!(us_text(1, false), None, "Escape types no text");
    }

    #[test]
    fn held_modifiers_count_both_sides() {
        let mut held = Held::default();
        held.update(42, true);
        held.update(54, true);
        held.update(42, false);
        assert!(held.modifiers().shift);
        held.update(54, false);
        assert!(!held.modifiers().shift);
        held.update(29, true);
        assert!(held.modifiers().control);
    }
}
