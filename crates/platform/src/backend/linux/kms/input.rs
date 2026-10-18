//! evdev input devices read directly: keyboards and buttons, mice, rotary
//! encoders, touchscreens (multi-touch protocol B, or single-touch), absolute
//! pointers (tablets, virtual machines' pointers) and touchpads, each decoded
//! into frame-complete [`Input`]s at the device's `SYN_REPORT`.

use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};

use super::seat::{Device, Seat};
use crate::event::{PointerButtons, PointerPhase};

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;

const SYN_REPORT: u16 = 0x00;
const SYN_DROPPED: u16 = 0x03;

const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_HWHEEL: u16 = 0x06;
const REL_DIAL: u16 = 0x07;
const REL_WHEEL: u16 = 0x08;
const REL_WHEEL_HI_RES: u16 = 0x0b;
const REL_HWHEEL_HI_RES: u16 = 0x0c;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_PRESSURE: u16 = 0x18;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
const ABS_MT_PRESSURE: u16 = 0x3a;

const BTN_MISC: u16 = 0x100;
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_JOYSTICK: u16 = 0x120;
const BTN_TOOL_FINGER: u16 = 0x145;
const BTN_TOUCH: u16 = 0x14a;
const KEY_OK: u16 = 0x160;
const KEY_MAX: u16 = 0x2ff;

const INPUT_PROP_DIRECT: u16 = 0x01;

/// Slots tracked on a multi-touch device; contacts beyond are ignored.
const MAX_SLOTS: usize = 16;

/// A touchpad's travel in pixels per millimetre of finger movement.
const TOUCHPAD_PX_PER_MM: f64 = 6.0;

const fn eviocg(nr: u32, len: usize) -> libc::Ioctl {
    ((2u32 << 30) | ((len as u32) << 16) | ((b'E' as u32) << 8) | nr) as libc::Ioctl
}

/// `struct input_event`: the kernel's `__kernel_ulong_t` seconds and
/// microseconds, then the type, code and value.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RawInputEvent {
    time: [libc::c_ulong; 2],
    kind: u16,
    code: u16,
    value: i32,
}

/// `struct input_absinfo`.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct AbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

/// One decoded input, complete as of its device's frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Input {
    /// An evdev key code went up (`0`), down (`1`) or repeated (`2`).
    Key { code: u16, value: i32 },
    Button {
        button: PointerButtons,
        pressed: bool,
    },
    /// Relative motion in pixels.
    Motion { dx: f64, dy: f64 },
    /// An absolute pointer's position, each axis `0..=1` across the panel.
    Position { x: f64, y: f64 },
    /// Wheel notches: positive is away from the user, and to the right.
    Wheel { x: f64, y: f64 },
    /// Smooth scrolling in pixels (fingers on a touchpad): positive the way
    /// a wheel turned toward the user and to the right scrolls.
    Scroll { dx: f64, dy: f64 },
    /// Encoder detents: positive is clockwise.
    Dial { steps: i32 },
    /// A touch contact, positioned `0..=1` across the panel.
    Touch {
        id: u32,
        phase: PointerPhase,
        x: f64,
        y: f64,
        pressure: f32,
    },
}

/// What a device is, from the event codes it declares.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Caps {
    pub keys: bool,
    pub mouse: bool,
    pub encoder: bool,
    pub touchscreen: bool,
    pub touchpad: bool,
    pub absolute: bool,
    /// The device reports multi-touch slots.
    multitouch: bool,
    hires_wheel: bool,
}

impl Caps {
    /// A device worth reading.
    pub fn any(&self) -> bool {
        self.keys
            || self.mouse
            || self.encoder
            || self.touchscreen
            || self.touchpad
            || self.absolute
    }

    /// Whether the device moves an on-screen pointer.
    pub fn pointer(&self) -> bool {
        self.mouse || self.touchpad || self.absolute
    }

    /// Whether libinput reads the device, when installed: pointers and
    /// touchscreens, not keyboards (the kernel's repeat) or encoders.
    pub fn libinput(&self) -> bool {
        (self.pointer() || self.touchscreen) && !self.keys && !self.encoder
    }

    fn classify(ev: &[u8], key: &[u8], rel: &[u8], abs: &[u8], prop: &[u8]) -> Caps {
        let bit = |bits: &[u8], n: u16| {
            bits.get(n as usize / 8)
                .is_some_and(|b| b & (1 << (n % 8)) != 0)
        };
        let has_key = bit(ev, EV_KEY);
        let has_rel = bit(ev, EV_REL);
        let has_abs = bit(ev, EV_ABS);
        let left = has_key && bit(key, BTN_LEFT);
        let touch = has_key && bit(key, BTN_TOUCH);
        let finger = has_key && bit(key, BTN_TOOL_FINGER);
        let direct = bit(prop, INPUT_PROP_DIRECT);
        let multitouch = has_abs && bit(abs, ABS_MT_POSITION_X) && bit(abs, ABS_MT_POSITION_Y);
        let single = has_abs && bit(abs, ABS_X) && bit(abs, ABS_Y);
        let keys = has_key
            && ((1..BTN_MISC).any(|k| bit(key, k)) || (KEY_OK..=KEY_MAX).any(|k| bit(key, k)));
        let rel_xy = has_rel && bit(rel, REL_X) && bit(rel, REL_Y);
        let mouse = rel_xy && left;
        let encoder = has_rel
            && !left
            && [REL_X, REL_Y, REL_DIAL, REL_WHEEL]
                .iter()
                .any(|&a| bit(rel, a));
        let touchscreen = (multitouch || (single && touch)) && (direct || !finger);
        let touchpad = (multitouch || single) && finger && !direct;
        let absolute = single && left && !touchscreen && !touchpad;
        // A joystick's buttons and axes are not a pointer or a keyboard.
        let joystick = has_key && bit(key, BTN_JOYSTICK) && !keys && !mouse;
        if joystick {
            return Caps::default();
        }
        Caps {
            keys,
            mouse,
            encoder,
            touchscreen,
            touchpad,
            absolute,
            multitouch: (touchscreen || touchpad) && multitouch,
            hires_wheel: has_rel && bit(rel, REL_WHEEL_HI_RES),
        }
    }
}

/// An axis's range, mapped onto `0..=1`.
#[derive(Debug, Clone, Copy)]
struct Range {
    min: f64,
    span: f64,
    /// Units per millimetre, when the device says.
    resolution: f64,
}

impl Range {
    fn of(info: AbsInfo) -> Range {
        Range {
            min: f64::from(info.minimum),
            span: f64::from(info.maximum - info.minimum).max(1.0),
            resolution: f64::from(info.resolution),
        }
    }

    fn unit(&self, value: i32) -> f64 {
        ((f64::from(value) - self.min) / self.span).clamp(0.0, 1.0)
    }
}

impl Default for Range {
    fn default() -> Self {
        Range {
            min: 0.0,
            span: 1.0,
            resolution: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Slot {
    /// The contact's tracking id; `-1` when lifted.
    tracking: i32,
    x: i32,
    y: i32,
    pressure: i32,
    /// The tracking id as of the last frame.
    reported: i32,
    changed: bool,
}

/// The per-device state that turns raw events into [`Input`]s.
pub(super) struct Decoder {
    caps: Caps,
    x: Range,
    y: Range,
    pressure: Option<Range>,
    slots: [Slot; MAX_SLOTS],
    slot: usize,
    /// Relative motion, wheel and dial accumulated this frame.
    rel: (i32, i32),
    wheel: (f64, f64),
    dial: i32,
    /// An absolute position changed this frame.
    moved: bool,
    abs: (i32, i32),
    /// Key and button transitions this frame, in order.
    keys: Vec<(u16, i32)>,
    /// Events since a `SYN_DROPPED` are discarded through the next report.
    dropped: bool,
    /// A touchpad's previous position, for deltas.
    last_pad: Option<(i32, i32)>,
}

impl Decoder {
    fn new(caps: Caps, x: AbsInfo, y: AbsInfo, pressure: Option<AbsInfo>) -> Decoder {
        let slot = Slot {
            tracking: -1,
            reported: -1,
            ..Slot::default()
        };
        Decoder {
            caps,
            x: Range::of(x),
            y: Range::of(y),
            pressure: pressure.filter(|p| p.maximum > p.minimum).map(Range::of),
            slots: [slot; MAX_SLOTS],
            slot: 0,
            rel: (0, 0),
            wheel: (0.0, 0.0),
            dial: 0,
            moved: false,
            abs: (0, 0),
            keys: Vec::new(),
            dropped: false,
            last_pad: None,
        }
    }

    /// Feeds one raw event; a report flushes the frame into `out`. Returns
    /// `true` when the device's slot state must be read back
    /// ([`resync`](Self::resync)) after a dropped stretch.
    fn feed(&mut self, kind: u16, code: u16, value: i32, out: &mut Vec<Input>) -> bool {
        if kind == EV_SYN {
            match code {
                SYN_DROPPED => {
                    self.dropped = true;
                    self.clear_frame();
                }
                SYN_REPORT if self.dropped => {
                    self.dropped = false;
                    self.clear_frame();
                    return self.caps.multitouch;
                }
                SYN_REPORT => self.flush(out),
                _ => {}
            }
            return false;
        }
        if self.dropped {
            return false;
        }
        match kind {
            EV_KEY => self.key(code, value),
            EV_REL => match code {
                REL_X => self.rel.0 += value,
                REL_Y => self.rel.1 += value,
                REL_DIAL => self.dial += value,
                REL_WHEEL if !self.caps.hires_wheel => self.wheel.1 += f64::from(value),
                REL_HWHEEL if !self.caps.hires_wheel => self.wheel.0 += f64::from(value),
                REL_WHEEL_HI_RES => self.wheel.1 += f64::from(value) / 120.0,
                REL_HWHEEL_HI_RES => self.wheel.0 += f64::from(value) / 120.0,
                _ => {}
            },
            EV_ABS => self.abs(code, value),
            _ => {}
        }
        false
    }

    fn key(&mut self, code: u16, value: i32) {
        if code == BTN_TOUCH && !self.caps.multitouch {
            let s = &mut self.slots[0];
            s.tracking = if value != 0 { 0 } else { -1 };
            s.changed = true;
            return;
        }
        if code == BTN_TOUCH || code == BTN_TOOL_FINGER || (0x140..0x150).contains(&code) {
            return;
        }
        self.keys.push((code, value));
    }

    fn abs(&mut self, code: u16, value: i32) {
        let touch = self.caps.touchscreen || self.caps.touchpad;
        match code {
            ABS_MT_SLOT if self.caps.multitouch => {
                self.slot = usize::try_from(value).unwrap_or(MAX_SLOTS);
            }
            ABS_MT_TRACKING_ID | ABS_MT_POSITION_X | ABS_MT_POSITION_Y | ABS_MT_PRESSURE
                if self.caps.multitouch =>
            {
                let Some(s) = self.slots.get_mut(self.slot) else {
                    return;
                };
                match code {
                    ABS_MT_TRACKING_ID => s.tracking = value,
                    ABS_MT_POSITION_X => s.x = value,
                    ABS_MT_POSITION_Y => s.y = value,
                    _ => s.pressure = value,
                }
                s.changed = true;
            }
            ABS_X | ABS_Y | ABS_PRESSURE if touch && !self.caps.multitouch => {
                let s = &mut self.slots[0];
                match code {
                    ABS_X => s.x = value,
                    ABS_Y => s.y = value,
                    _ => s.pressure = value,
                }
                s.changed = true;
            }
            ABS_X if self.caps.absolute => {
                self.abs.0 = value;
                self.moved = true;
            }
            ABS_Y if self.caps.absolute => {
                self.abs.1 = value;
                self.moved = true;
            }
            _ => {}
        }
    }

    fn clear_frame(&mut self) {
        self.rel = (0, 0);
        self.wheel = (0.0, 0.0);
        self.dial = 0;
        self.moved = false;
        self.keys.clear();
    }

    /// Reads the slot state back after a drop: contacts that ended meanwhile
    /// end, new ones begin, the rest move to where they are now.
    fn resync(&mut self, fd: RawFd, out: &mut Vec<Input>) {
        for code in [ABS_MT_TRACKING_ID, ABS_MT_POSITION_X, ABS_MT_POSITION_Y] {
            let mut buf = [0i32; MAX_SLOTS + 1];
            buf[0] = i32::from(code);
            // SAFETY: `EVIOCGMTSLOTS` fills the code's value for as many slots
            // as fit in the buffer whose byte length the request encodes.
            let ok =
                unsafe { libc::ioctl(fd, eviocg(0x0a, size_of_val(&buf)), buf.as_mut_ptr()) } >= 0;
            if !ok {
                continue;
            }
            for (s, &v) in self.slots.iter_mut().zip(&buf[1..]) {
                match code {
                    ABS_MT_TRACKING_ID => s.tracking = v,
                    ABS_MT_POSITION_X => s.x = v,
                    _ => s.y = v,
                }
                s.changed = true;
            }
        }
        self.flush(out);
    }

    fn flush(&mut self, out: &mut Vec<Input>) {
        if self.caps.touchpad {
            self.flush_touchpad();
        }
        let (dx, dy) = std::mem::take(&mut self.rel);
        if self.caps.encoder && !self.caps.mouse {
            self.dial += dx + dy + self.wheel.1 as i32;
            self.wheel = (0.0, 0.0);
        } else if (dx, dy) != (0, 0) {
            out.push(Input::Motion {
                dx: f64::from(dx),
                dy: f64::from(dy),
            });
        }
        if std::mem::take(&mut self.moved) {
            out.push(Input::Position {
                x: self.x.unit(self.abs.0),
                y: self.y.unit(self.abs.1),
            });
        }
        let wheel = std::mem::take(&mut self.wheel);
        if wheel != (0.0, 0.0) {
            out.push(Input::Wheel {
                x: wheel.0,
                y: wheel.1,
            });
        }
        let steps = std::mem::take(&mut self.dial);
        if steps != 0 {
            out.push(Input::Dial { steps });
        }
        for (code, value) in self.keys.drain(..) {
            let button = match code {
                BTN_LEFT => Some(PointerButtons::PRIMARY),
                BTN_RIGHT => Some(PointerButtons::SECONDARY),
                BTN_MIDDLE => Some(PointerButtons::MIDDLE),
                _ => None,
            };
            match button {
                Some(button) if value != 2 => out.push(Input::Button {
                    button,
                    pressed: value != 0,
                }),
                Some(_) => {}
                None => out.push(Input::Key { code, value }),
            }
        }
        if self.caps.touchscreen {
            self.flush_touches(out);
        }
    }

    fn flush_touches(&mut self, out: &mut Vec<Input>) {
        for s in &mut self.slots {
            if !std::mem::take(&mut s.changed) {
                continue;
            }
            let (x, y) = (self.x.unit(s.x), self.y.unit(s.y));
            let pressure = self.pressure.map_or(0.5, |p| p.unit(s.pressure) as f32);
            let ended = s.reported >= 0 && s.tracking != s.reported;
            if ended {
                out.push(Input::Touch {
                    id: s.reported as u32,
                    phase: PointerPhase::Up,
                    x,
                    y,
                    pressure: 0.0,
                });
            }
            if s.tracking >= 0 {
                let phase = if s.reported == s.tracking {
                    PointerPhase::Moved
                } else {
                    PointerPhase::Down
                };
                out.push(Input::Touch {
                    id: s.tracking as u32,
                    phase,
                    x,
                    y,
                    pressure,
                });
            }
            s.reported = s.tracking;
        }
    }

    /// A touchpad moves the pointer by its first contact's travel.
    fn flush_touchpad(&mut self) {
        let s = &mut self.slots[0];
        s.changed = false;
        if s.tracking < 0 {
            self.last_pad = None;
            return;
        }
        let at = (s.x, s.y);
        if let Some(last) = self.last_pad.replace(at) {
            let mm = |r: Range, d: i32| {
                let per_mm = if r.resolution > 0.0 {
                    r.resolution
                } else {
                    r.span / 100.0
                };
                f64::from(d) / per_mm * TOUCHPAD_PX_PER_MM
            };
            let (dx, dy) = (mm(self.x, at.0 - last.0), mm(self.y, at.1 - last.1));
            self.rel.0 += dx.round() as i32;
            self.rel.1 += dy.round() as i32;
        }
    }

    /// Every contact still down, lifted: the device went away or the session
    /// paused mid-touch.
    pub fn cancel(&mut self, out: &mut Vec<Input>) {
        for s in &mut self.slots {
            if s.reported >= 0 && self.caps.touchscreen {
                out.push(Input::Touch {
                    id: s.reported as u32,
                    phase: PointerPhase::Cancel,
                    x: self.x.unit(s.x),
                    y: self.y.unit(s.y),
                    pressure: 0.0,
                });
            }
            s.reported = -1;
            s.tracking = -1;
        }
        self.last_pad = None;
    }
}

/// An open input device.
pub(super) struct InputDevice {
    pub path: PathBuf,
    pub caps: Caps,
    device: Device,
    decoder: Decoder,
}

impl InputDevice {
    /// Opens `path` and reads what it is; `None` when it is nothing the
    /// display uses.
    pub fn open(seat: &mut Seat, path: &Path) -> io::Result<Option<InputDevice>> {
        let device = seat.open_device(path, libc::O_RDONLY)?;
        let fd = device.fd.as_raw_fd();
        let bits = |ev: u32, len: usize| {
            let mut buf = vec![0u8; len];
            // SAFETY: `EVIOCGBIT` / `EVIOCGPROP` write at most the buffer's
            // length, which the request encodes.
            let n = unsafe { libc::ioctl(fd, eviocg(ev, len), buf.as_mut_ptr()) };
            buf.truncate(usize::try_from(n).unwrap_or(0));
            buf
        };
        let ev = bits(0x20, 4);
        let key = bits(0x20 + u32::from(EV_KEY), usize::from(KEY_MAX) / 8 + 1);
        let rel = bits(0x20 + u32::from(EV_REL), 2);
        let abs = bits(0x20 + u32::from(EV_ABS), 8);
        let prop = bits(0x09, 4);
        let caps = Caps::classify(&ev, &key, &rel, &abs, &prop);
        if !caps.any() {
            seat.close_device(device);
            return Ok(None);
        }
        let info = |code: u16| {
            let mut info = AbsInfo::default();
            // SAFETY: `EVIOCGABS` writes one `input_absinfo`.
            let ok = unsafe {
                libc::ioctl(
                    fd,
                    eviocg(0x40 + u32::from(code), size_of::<AbsInfo>()),
                    &mut info,
                )
            } >= 0;
            ok.then_some(info)
        };
        let (x, y, pressure) = if caps.multitouch {
            (
                info(ABS_MT_POSITION_X),
                info(ABS_MT_POSITION_Y),
                info(ABS_MT_PRESSURE),
            )
        } else {
            (info(ABS_X), info(ABS_Y), info(ABS_PRESSURE))
        };
        let decoder = Decoder::new(caps, x.unwrap_or_default(), y.unwrap_or_default(), pressure);
        Ok(Some(InputDevice {
            path: path.to_owned(),
            caps,
            device,
            decoder,
        }))
    }

    pub fn fd(&self) -> RawFd {
        self.device.fd.as_raw_fd()
    }

    /// Reads everything the device has, decoding complete frames into `out`;
    /// `false` once the device is gone.
    pub fn read(&mut self, out: &mut Vec<Input>) -> bool {
        let mut buf = [RawInputEvent::default(); 64];
        loop {
            // SAFETY: reading whole events into a live local array of its
            // byte length from the device's non-blocking descriptor.
            let n = unsafe { libc::read(self.fd(), buf.as_mut_ptr().cast(), size_of_val(&buf)) };
            if n < 0 {
                let e = io::Error::last_os_error();
                return matches!(e.raw_os_error(), Some(libc::EAGAIN | libc::EINTR));
            }
            if n == 0 {
                return false;
            }
            let count = n as usize / size_of::<RawInputEvent>();
            for e in &buf[..count] {
                if self.decoder.feed(e.kind, e.code, e.value, out) {
                    self.decoder.resync(self.fd(), out);
                }
            }
            if count < buf.len() {
                return true;
            }
        }
    }

    pub fn cancel(&mut self, out: &mut Vec<Input>) {
        self.decoder.cancel(out);
    }

    pub fn close(self, seat: &mut Seat) {
        seat.close_device(self.device);
    }
}

/// The input device nodes present now.
pub(super) fn event_nodes() -> Vec<PathBuf> {
    let mut nodes: Vec<PathBuf> = std::fs::read_dir("/dev/input")
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("event"))
        })
        .collect();
    nodes.sort();
    nodes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(codes: &[u16], len: usize) -> Vec<u8> {
        let mut b = vec![0u8; len];
        for &c in codes {
            b[c as usize / 8] |= 1 << (c % 8);
        }
        b
    }

    fn caps(ev: &[u16], key: &[u16], rel: &[u16], abs: &[u16], prop: &[u16]) -> Caps {
        Caps::classify(
            &bits(ev, 4),
            &bits(key, 96),
            &bits(rel, 2),
            &bits(abs, 8),
            &bits(prop, 4),
        )
    }

    fn range(max: i32) -> AbsInfo {
        AbsInfo {
            maximum: max,
            ..AbsInfo::default()
        }
    }

    fn run(d: &mut Decoder, events: &[(u16, u16, i32)]) -> Vec<Input> {
        let mut out = Vec::new();
        for &(k, c, v) in events {
            d.feed(k, c, v, &mut out);
        }
        out
    }

    #[test]
    fn the_input_event_layout_matches_the_kernel() {
        assert_eq!(
            size_of::<RawInputEvent>(),
            8 + 2 * size_of::<libc::c_ulong>()
        );
        assert_eq!(size_of::<AbsInfo>(), 24);
        assert_eq!(eviocg(0x40, 24) as u64, 0x8018_4540);
    }

    #[test]
    fn devices_classify_by_their_codes() {
        let keyboard = caps(&[EV_KEY], &[1, 30, 57], &[], &[], &[]);
        assert!(keyboard.keys && !keyboard.pointer());
        let mouse = caps(
            &[EV_KEY, EV_REL],
            &[BTN_LEFT],
            &[REL_X, REL_Y, REL_WHEEL],
            &[],
            &[],
        );
        assert!(mouse.mouse && !mouse.encoder);
        let encoder = caps(&[EV_REL], &[], &[REL_X], &[], &[]);
        assert!(encoder.encoder && !encoder.mouse);
        let screen = caps(
            &[EV_KEY, EV_ABS],
            &[BTN_TOUCH],
            &[],
            &[
                ABS_X,
                ABS_Y,
                ABS_MT_SLOT,
                ABS_MT_POSITION_X,
                ABS_MT_POSITION_Y,
                ABS_MT_TRACKING_ID,
            ],
            &[INPUT_PROP_DIRECT],
        );
        assert!(screen.touchscreen && screen.multitouch && !screen.touchpad);
        let pad = caps(
            &[EV_KEY, EV_ABS],
            &[BTN_LEFT, BTN_TOUCH, BTN_TOOL_FINGER],
            &[],
            &[ABS_X, ABS_Y, ABS_MT_POSITION_X, ABS_MT_POSITION_Y],
            &[],
        );
        assert!(pad.touchpad && !pad.touchscreen && !pad.absolute);
        let tablet = caps(&[EV_KEY, EV_ABS], &[BTN_LEFT], &[], &[ABS_X, ABS_Y], &[]);
        assert!(tablet.absolute);
        let stick = caps(
            &[EV_KEY, EV_ABS],
            &[BTN_JOYSTICK],
            &[],
            &[ABS_X, ABS_Y],
            &[],
        );
        assert!(!stick.any());
    }

    #[test]
    fn multitouch_slots_become_contacts() {
        let c = Caps {
            touchscreen: true,
            multitouch: true,
            ..Caps::default()
        };
        let mut d = Decoder::new(c, range(1000), range(500), None);
        let out = run(
            &mut d,
            &[
                (EV_ABS, ABS_MT_SLOT, 0),
                (EV_ABS, ABS_MT_TRACKING_ID, 7),
                (EV_ABS, ABS_MT_POSITION_X, 500),
                (EV_ABS, ABS_MT_POSITION_Y, 250),
                (EV_ABS, ABS_MT_SLOT, 1),
                (EV_ABS, ABS_MT_TRACKING_ID, 8),
                (EV_ABS, ABS_MT_POSITION_X, 1000),
                (EV_SYN, SYN_REPORT, 0),
            ],
        );
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[0],
            Input::Touch {
                id: 7,
                phase: PointerPhase::Down,
                x: 0.5,
                y: 0.5,
                pressure: 0.5
            }
        );
        assert!(
            matches!(out[1], Input::Touch { id: 8, phase: PointerPhase::Down, x, .. } if x == 1.0)
        );
        let out = run(
            &mut d,
            &[
                (EV_ABS, ABS_MT_SLOT, 0),
                (EV_ABS, ABS_MT_POSITION_X, 0),
                (EV_ABS, ABS_MT_SLOT, 1),
                (EV_ABS, ABS_MT_TRACKING_ID, -1),
                (EV_SYN, SYN_REPORT, 0),
            ],
        );
        assert!(
            matches!(out[0], Input::Touch { id: 7, phase: PointerPhase::Moved, x, .. } if x == 0.0)
        );
        assert!(matches!(
            out[1],
            Input::Touch {
                id: 8,
                phase: PointerPhase::Up,
                ..
            }
        ));
        let mut out = Vec::new();
        d.cancel(&mut out);
        assert!(matches!(
            out[..],
            [Input::Touch {
                id: 7,
                phase: PointerPhase::Cancel,
                ..
            }]
        ));
    }

    #[test]
    fn a_reused_slot_ends_the_old_contact_first() {
        let c = Caps {
            touchscreen: true,
            multitouch: true,
            ..Caps::default()
        };
        let mut d = Decoder::new(c, range(100), range(100), None);
        run(
            &mut d,
            &[(EV_ABS, ABS_MT_TRACKING_ID, 1), (EV_SYN, SYN_REPORT, 0)],
        );
        let out = run(
            &mut d,
            &[(EV_ABS, ABS_MT_TRACKING_ID, 2), (EV_SYN, SYN_REPORT, 0)],
        );
        assert!(matches!(
            out[0],
            Input::Touch {
                id: 1,
                phase: PointerPhase::Up,
                ..
            }
        ));
        assert!(matches!(
            out[1],
            Input::Touch {
                id: 2,
                phase: PointerPhase::Down,
                ..
            }
        ));
    }

    #[test]
    fn single_touch_reports_through_btn_touch() {
        let c = Caps {
            touchscreen: true,
            ..Caps::default()
        };
        let mut d = Decoder::new(c, range(100), range(100), None);
        let out = run(
            &mut d,
            &[
                (EV_KEY, BTN_TOUCH, 1),
                (EV_ABS, ABS_X, 25),
                (EV_SYN, SYN_REPORT, 0),
            ],
        );
        assert!(
            matches!(out[..], [Input::Touch { id: 0, phase: PointerPhase::Down, x, .. }] if x == 0.25)
        );
        let out = run(&mut d, &[(EV_KEY, BTN_TOUCH, 0), (EV_SYN, SYN_REPORT, 0)]);
        assert!(matches!(
            out[..],
            [Input::Touch {
                phase: PointerPhase::Up,
                ..
            }]
        ));
    }

    #[test]
    fn a_mouse_frame_moves_before_it_clicks() {
        let c = Caps {
            mouse: true,
            hires_wheel: true,
            ..Caps::default()
        };
        let mut d = Decoder::new(c, AbsInfo::default(), AbsInfo::default(), None);
        let out = run(
            &mut d,
            &[
                (EV_KEY, BTN_LEFT, 1),
                (EV_REL, REL_X, 3),
                (EV_REL, REL_Y, -2),
                (EV_REL, REL_WHEEL, 1),
                (EV_REL, REL_WHEEL_HI_RES, 60),
                (EV_SYN, SYN_REPORT, 0),
            ],
        );
        assert_eq!(
            out,
            [
                Input::Motion { dx: 3.0, dy: -2.0 },
                Input::Wheel { x: 0.0, y: 0.5 },
                Input::Button {
                    button: PointerButtons::PRIMARY,
                    pressed: true
                },
            ]
        );
    }

    #[test]
    fn an_encoder_turns_into_detents_and_a_drop_discards_the_frame() {
        let c = Caps {
            encoder: true,
            ..Caps::default()
        };
        let mut d = Decoder::new(c, AbsInfo::default(), AbsInfo::default(), None);
        let out = run(&mut d, &[(EV_REL, REL_X, -2), (EV_SYN, SYN_REPORT, 0)]);
        assert_eq!(out, [Input::Dial { steps: -2 }]);
        let out = run(
            &mut d,
            &[
                (EV_REL, REL_X, 1),
                (EV_SYN, SYN_DROPPED, 0),
                (EV_REL, REL_X, 1),
                (EV_SYN, SYN_REPORT, 0),
                (EV_KEY, 28, 1),
                (EV_SYN, SYN_REPORT, 0),
            ],
        );
        assert_eq!(out, [Input::Key { code: 28, value: 1 }]);
    }
}
