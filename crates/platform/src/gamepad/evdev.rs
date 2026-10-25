//! Gamepads on Linux: evdev, read directly. Every `/dev/input/event*` node
//! whose keys include the gamepad buttons (`BTN_SOUTH`) is a pad; it is
//! opened non-blocking and read until it has nothing more at each poll, a
//! report complete at its `SYN_REPORT`. An inotify watch on `/dev/input`
//! finds the nodes that appear (and those whose permissions arrive late);
//! a node that goes away fails its read. No udev.
//!
//! The kernel's gamepad layout names the face buttons by position, except
//! that the `xpad` driver reports an Xbox pad's left button as `BTN_NORTH`
//! and its top one as `BTN_WEST`, so those two swap for it. Sticks are
//! `ABS_X`/`ABS_Y` and `ABS_RX`/`ABS_RY`, triggers `ABS_Z`/`ABS_RZ` (or
//! `ABS_BRAKE`/`ABS_GAS`) — except on a pad without `ABS_RX`, where
//! `ABS_Z`/`ABS_RZ` are the right stick — the d-pad `ABS_HAT0X`/`ABS_HAT0Y`
//! or the `BTN_DPAD_*` keys, and a digital trigger reads fully pulled.
//! Rumble uploads one `FF_RUMBLE` effect per pad and plays it.

use super::{GamepadButton, GamepadState};

pub(crate) const EV_SYN: u16 = 0x00;
pub(crate) const EV_KEY: u16 = 0x01;
pub(crate) const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0x00;
const SYN_DROPPED: u16 = 0x03;

pub(crate) const BTN_SOUTH: u16 = 0x130;
const BTN_EAST: u16 = 0x131;
const BTN_NORTH: u16 = 0x133;
const BTN_WEST: u16 = 0x134;
const BTN_TL: u16 = 0x136;
const BTN_TR: u16 = 0x137;
const BTN_TL2: u16 = 0x138;
const BTN_TR2: u16 = 0x139;
const BTN_SELECT: u16 = 0x13a;
const BTN_START: u16 = 0x13b;
const BTN_MODE: u16 = 0x13c;
const BTN_THUMBL: u16 = 0x13d;
const BTN_THUMBR: u16 = 0x13e;
const BTN_DPAD_UP: u16 = 0x220;
const BTN_DPAD_DOWN: u16 = 0x221;
const BTN_DPAD_LEFT: u16 = 0x222;
const BTN_DPAD_RIGHT: u16 = 0x223;
/// `xpad`'s d-pad, when it reports it as buttons: left, right, up, down.
const BTN_TRIGGER_HAPPY1: u16 = 0x2c0;
pub(crate) const KEY_MAX: u16 = 0x2ff;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_Z: u16 = 0x02;
const ABS_RX: u16 = 0x03;
const ABS_RY: u16 = 0x04;
const ABS_RZ: u16 = 0x05;
const ABS_GAS: u16 = 0x09;
const ABS_BRAKE: u16 = 0x0a;
const ABS_HAT0X: u16 = 0x10;
const ABS_HAT0Y: u16 = 0x11;
pub(crate) const ABS_MAX: u16 = 0x3f;

/// What an absolute axis is to the pad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    LeftX,
    LeftY,
    RightX,
    RightY,
    LeftTrigger,
    RightTrigger,
    HatX,
    HatY,
}

/// An axis's range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Range {
    pub min: i32,
    pub max: i32,
}

impl Range {
    /// `value` across the range, `0..=1`.
    fn unit(self, value: i32) -> f32 {
        let span = (i64::from(self.max) - i64::from(self.min)).max(1) as f32;
        ((i64::from(value) - i64::from(self.min)) as f32 / span).clamp(0.0, 1.0)
    }

    /// `value` across the range, `-1..=1` about its middle: about zero for
    /// a signed range, so that zero reads exactly centred.
    fn centred(self, value: i32) -> f32 {
        if self.min < 0 && self.max > 0 {
            let bound = if value < 0 { -self.min } else { self.max };
            (value as f32 / bound as f32).clamp(-1.0, 1.0)
        } else {
            self.unit(value) * 2.0 - 1.0
        }
    }
}

/// How a device's codes map onto the standard layout, from what it
/// declares.
#[derive(Debug, Clone)]
pub(crate) struct Layout {
    /// The axes the pad uses: code, role and range.
    axes: Vec<(u16, Role, Range)>,
    /// The device is driven by `xpad`.
    xpad: bool,
    /// The triggers have axes, so their buttons do not move them.
    analog: [bool; 2],
}

impl Layout {
    /// The layout of a device declaring `keys` and `abs` (bit sets) with
    /// the ranges `range` reads; `None` when it is not a gamepad.
    pub(crate) fn of(
        keys: &[u8],
        abs: &[u8],
        xpad: bool,
        mut range: impl FnMut(u16) -> Option<Range>,
    ) -> Option<Layout> {
        if !bit(keys, BTN_SOUTH) {
            return None;
        }
        let has = |code: u16| bit(abs, code);
        let right_on_z = !has(ABS_RX) && has(ABS_Z) && has(ABS_RZ);
        let mut roles = vec![(ABS_X, Role::LeftX), (ABS_Y, Role::LeftY)];
        if right_on_z {
            roles.extend([(ABS_Z, Role::RightX), (ABS_RZ, Role::RightY)]);
        } else {
            roles.extend([(ABS_RX, Role::RightX), (ABS_RY, Role::RightY)]);
            roles.extend([(ABS_Z, Role::LeftTrigger), (ABS_RZ, Role::RightTrigger)]);
        }
        roles.extend([
            (ABS_BRAKE, Role::LeftTrigger),
            (ABS_GAS, Role::RightTrigger),
        ]);
        roles.extend([(ABS_HAT0X, Role::HatX), (ABS_HAT0Y, Role::HatY)]);
        let mut analog = [false; 2];
        let mut axes = Vec::with_capacity(roles.len());
        for (code, role) in roles {
            if !has(code) {
                continue;
            }
            let Some(range) = range(code) else { continue };
            match role {
                Role::LeftTrigger if !analog[0] => analog[0] = true,
                Role::RightTrigger if !analog[1] => analog[1] = true,
                Role::LeftTrigger | Role::RightTrigger => continue,
                _ => {}
            }
            axes.push((code, role, range));
        }
        Some(Layout { axes, xpad, analog })
    }

    fn button(&self, code: u16) -> Option<GamepadButton> {
        use GamepadButton as B;
        Some(match code {
            BTN_SOUTH => B::South,
            BTN_EAST => B::East,
            BTN_NORTH if self.xpad => B::West,
            BTN_WEST if self.xpad => B::North,
            BTN_NORTH => B::North,
            BTN_WEST => B::West,
            BTN_TL => B::LeftShoulder,
            BTN_TR => B::RightShoulder,
            BTN_TL2 => B::LeftTrigger,
            BTN_TR2 => B::RightTrigger,
            BTN_SELECT => B::Select,
            BTN_START => B::Start,
            BTN_MODE => B::Home,
            BTN_THUMBL => B::LeftThumb,
            BTN_THUMBR => B::RightThumb,
            BTN_DPAD_UP => B::DpadUp,
            BTN_DPAD_DOWN => B::DpadDown,
            BTN_DPAD_LEFT => B::DpadLeft,
            BTN_DPAD_RIGHT => B::DpadRight,
            c if self.xpad && (BTN_TRIGGER_HAPPY1..BTN_TRIGGER_HAPPY1 + 4).contains(&c) => {
                [B::DpadLeft, B::DpadRight, B::DpadUp, B::DpadDown]
                    [usize::from(c - BTN_TRIGGER_HAPPY1)]
            }
            _ => return None,
        })
    }
}

/// Whether bit `n` of `bits` is set.
pub(crate) fn bit(bits: &[u8], n: u16) -> bool {
    bits.get(usize::from(n / 8))
        .is_some_and(|b| b & (1 << (n % 8)) != 0)
}

/// A pad's state as its events build it.
#[derive(Debug, Clone)]
pub(crate) struct Decoder {
    layout: Layout,
    /// The state as of the last event.
    state: GamepadState,
    /// The digital triggers held, which pull a trigger without an axis.
    triggers: [bool; 2],
    /// Events since a `SYN_DROPPED` are discarded through the next report.
    dropped: bool,
}

/// What one event did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Step {
    /// Nothing yet.
    Pending,
    /// A report completed: the pad reads this.
    Report(GamepadState),
    /// Events were lost: read the device's state whole.
    Resync,
}

impl Decoder {
    pub(crate) fn new(layout: Layout) -> Decoder {
        Decoder {
            layout,
            state: GamepadState::default(),
            triggers: [false; 2],
            dropped: false,
        }
    }

    /// Takes one event.
    pub(crate) fn event(&mut self, kind: u16, code: u16, value: i32) -> Step {
        match (kind, code) {
            (EV_SYN, SYN_DROPPED) => {
                self.dropped = true;
                Step::Pending
            }
            (EV_SYN, SYN_REPORT) if self.dropped => {
                self.dropped = false;
                Step::Resync
            }
            (EV_SYN, SYN_REPORT) => Step::Report(self.state),
            _ if self.dropped => Step::Pending,
            (EV_KEY, code) => {
                self.key(code, value != 0);
                Step::Pending
            }
            (EV_ABS, code) => {
                self.axis(code, value);
                Step::Pending
            }
            _ => Step::Pending,
        }
    }

    fn key(&mut self, code: u16, down: bool) {
        let Some(button) = self.layout.button(code) else {
            return;
        };
        self.state.set(button, down);
        let trigger = match button {
            GamepadButton::LeftTrigger => 0,
            GamepadButton::RightTrigger => 1,
            _ => return,
        };
        self.triggers[trigger] = down;
        if !self.layout.analog[trigger] {
            let value = if down { 1.0 } else { 0.0 };
            if trigger == 0 {
                self.state.left_trigger = value;
            } else {
                self.state.right_trigger = value;
            }
        }
    }

    fn axis(&mut self, code: u16, value: i32) {
        let Some(&(_, role, range)) = self.layout.axes.iter().find(|a| a.0 == code) else {
            return;
        };
        let s = &mut self.state;
        match role {
            Role::LeftX => s.left_stick[0] = range.centred(value),
            Role::LeftY => s.left_stick[1] = -range.centred(value),
            Role::RightX => s.right_stick[0] = range.centred(value),
            Role::RightY => s.right_stick[1] = -range.centred(value),
            Role::LeftTrigger => s.left_trigger = range.unit(value),
            Role::RightTrigger => s.right_trigger = range.unit(value),
            Role::HatX => {
                s.set(GamepadButton::DpadLeft, value < 0);
                s.set(GamepadButton::DpadRight, value > 0);
            }
            Role::HatY => {
                s.set(GamepadButton::DpadUp, value < 0);
                s.set(GamepadButton::DpadDown, value > 0);
            }
        }
    }

    /// Reads the device's whole state: the keys held (a bit set) and each
    /// axis's value.
    pub(crate) fn resync(
        &mut self,
        keys: &[u8],
        mut value: impl FnMut(u16) -> Option<i32>,
    ) -> GamepadState {
        self.state = GamepadState::default();
        self.triggers = [false; 2];
        for code in BTN_SOUTH..=KEY_MAX {
            if bit(keys, code) {
                self.key(code, true);
            }
        }
        for i in 0..self.layout.axes.len() {
            let code = self.layout.axes[i].0;
            if let Some(v) = value(code) {
                self.axis(code, v);
            }
        }
        self.state
    }
}

#[cfg(target_os = "linux")]
pub(crate) use native::Source;

#[cfg(target_os = "linux")]
mod native {
    use std::ffi::CString;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

    use super::super::{GamepadEvent, GamepadId, Pads, Rumble};
    use super::{ABS_MAX, Decoder, EV_ABS, EV_KEY, KEY_MAX, Layout, Range, Step, bit};
    use crate::LoopWaker;

    const INPUT: &str = "/dev/input";
    const EV_FF: u16 = 0x15;
    const FF_RUMBLE: u16 = 0x50;
    const FF_MAX: u16 = 0x7f;

    /// `struct input_event`: the kernel's `__kernel_ulong_t` seconds and
    /// microseconds, then the type, code and value.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RawEvent {
        time: [libc::c_ulong; 2],
        kind: u16,
        code: u16,
        value: i32,
    }

    /// `struct input_absinfo`.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct AbsInfo {
        value: i32,
        minimum: i32,
        maximum: i32,
        fuzz: i32,
        flat: i32,
        resolution: i32,
    }

    const fn ioc(dir: u32, nr: u32, len: usize) -> libc::Ioctl {
        ((dir << 30) | ((len as u32) << 16) | ((b'E' as u32) << 8) | nr) as libc::Ioctl
    }

    const READ: u32 = 2;
    const WRITE: u32 = 1;

    const fn eviocgbit(ev: u16, len: usize) -> libc::Ioctl {
        ioc(READ, 0x20 + ev as u32, len)
    }

    const fn eviocgabs(abs: u16) -> libc::Ioctl {
        ioc(READ, 0x40 + abs as u32, std::mem::size_of::<AbsInfo>())
    }

    const EVIOCGNAME: libc::Ioctl = ioc(READ, 0x06, 128);
    const EVIOCGKEY: libc::Ioctl = ioc(READ, 0x18, KEY_BYTES);
    const EVIOCSFF: libc::Ioctl = ioc(WRITE, 0x80, std::mem::size_of::<libc::ff_effect>());

    const KEY_BYTES: usize = KEY_MAX as usize / 8 + 1;
    const ABS_BYTES: usize = ABS_MAX as usize / 8 + 1;
    const FF_BYTES: usize = FF_MAX as usize / 8 + 1;

    /// An open pad.
    struct Device {
        node: u32,
        fd: OwnedFd,
        decoder: Decoder,
        /// The pad takes rumble: the fd is writable and declares
        /// `FF_RUMBLE`; the effect uploaded, once one is.
        rumble: bool,
        effect: Option<i16>,
    }

    /// The pads under `/dev/input`.
    pub(crate) struct Source {
        watch: Option<OwnedFd>,
        /// Whether the nodes present were looked at.
        scanned: bool,
        devices: Vec<Device>,
        pads: Pads,
    }

    /// `ioctl` reading into `arg`; whether it succeeded.
    fn ioctl<T>(fd: RawFd, request: libc::Ioctl, arg: &mut T) -> bool {
        // SAFETY: each request named here writes at most `size_of::<T>()`
        // bytes, the size encoded in it, into `arg`.
        unsafe { libc::ioctl(fd, request, std::ptr::from_mut(arg)) >= 0 }
    }

    /// The `eventN` node number `name` names.
    fn node_of(name: &[u8]) -> Option<u32> {
        std::str::from_utf8(name.strip_prefix(b"event")?)
            .ok()?
            .parse()
            .ok()
    }

    impl Source {
        pub(crate) fn new(_waker: LoopWaker) -> Option<Source> {
            // SAFETY: plain flags; the descriptor returned is owned below.
            let watch = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
            let watch = (watch >= 0).then(|| {
                // SAFETY: a fresh descriptor nothing else owns.
                unsafe { OwnedFd::from_raw_fd(watch) }
            });
            if let Some(fd) = &watch {
                let path = CString::new(INPUT).expect("no NUL");
                // SAFETY: a NUL-terminated path and plain flags.
                unsafe {
                    libc::inotify_add_watch(
                        fd.as_raw_fd(),
                        path.as_ptr(),
                        libc::IN_CREATE | libc::IN_ATTRIB | libc::IN_DELETE,
                    );
                }
            }
            Some(Source {
                watch,
                scanned: false,
                devices: Vec::with_capacity(4),
                pads: Pads::default(),
            })
        }

        /// Opens the pads among the nodes present.
        fn scan(&mut self, events: &mut Vec<GamepadEvent>) {
            let mut nodes: Vec<u32> = std::fs::read_dir(INPUT)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| node_of(e.file_name().as_encoded_bytes()))
                .collect();
            nodes.sort_unstable();
            for node in nodes {
                self.open(node, events);
            }
        }

        /// Opens node `node` if it is a pad not yet open, connecting it.
        fn open(&mut self, node: u32, events: &mut Vec<GamepadEvent>) {
            if self.devices.iter().any(|d| d.node == node) {
                return;
            }
            let Some(mut device) = Device::open(node) else {
                return;
            };
            let name = device.name();
            self.pads
                .connect(u64::from(node), &name, device.rumble, events);
            if let Some(state) = device.read_whole() {
                self.pads.update(u64::from(node), &state, events);
            }
            self.devices.push(device);
        }

        fn close(&mut self, node: u32, events: &mut Vec<GamepadEvent>) {
            if let Some(i) = self.devices.iter().position(|d| d.node == node) {
                self.devices.remove(i);
                self.pads.disconnect(u64::from(node), events);
            }
        }

        pub(crate) fn poll(&mut self, events: &mut Vec<GamepadEvent>) {
            if !self.scanned {
                self.scanned = true;
                self.scan(events);
            }
            self.watch(events);
            let mut i = 0;
            while i < self.devices.len() {
                if self.devices[i].read(&mut self.pads, events) {
                    i += 1;
                } else {
                    let node = self.devices[i].node;
                    self.close(node, events);
                }
            }
        }

        /// Takes what the inotify watch saw: nodes created, given access or
        /// removed.
        fn watch(&mut self, events: &mut Vec<GamepadEvent>) {
            let Some(fd) = self.watch.as_ref().map(AsRawFd::as_raw_fd) else {
                return;
            };
            let mut buffer = [0u64; 512];
            loop {
                // SAFETY: reading into a live, aligned buffer of its size.
                let n = unsafe {
                    libc::read(
                        fd,
                        buffer.as_mut_ptr().cast(),
                        std::mem::size_of_val(&buffer),
                    )
                };
                if n <= 0 {
                    return;
                }
                // SAFETY: the buffer is plain bytes.
                let bytes =
                    unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), n as usize) };
                let head = std::mem::size_of::<libc::inotify_event>();
                let mut at = 0;
                while at + head <= bytes.len() {
                    // SAFETY: the kernel wrote a whole `inotify_event` here.
                    let event = unsafe {
                        bytes[at..]
                            .as_ptr()
                            .cast::<libc::inotify_event>()
                            .read_unaligned()
                    };
                    let name = &bytes[at + head..(at + head + event.len as usize).min(bytes.len())];
                    let name = name.split(|&b| b == 0).next().unwrap_or_default();
                    at += head + event.len as usize;
                    let Some(node) = node_of(name) else { continue };
                    if event.mask & libc::IN_DELETE != 0 {
                        self.close(node, events);
                    } else {
                        self.open(node, events);
                    }
                }
            }
        }

        pub(crate) fn rumble(&mut self, id: GamepadId, rumble: Rumble) -> bool {
            let Some(node) = self.pads.key(id) else {
                return false;
            };
            self.devices
                .iter_mut()
                .find(|d| u64::from(d.node) == node)
                .is_some_and(|d| d.rumble(rumble.clamped()))
        }
    }

    impl Device {
        fn open(node: u32) -> Option<Device> {
            let path = CString::new(format!("{INPUT}/event{node}")).ok()?;
            let flags = libc::O_NONBLOCK | libc::O_CLOEXEC;
            // SAFETY: a NUL-terminated path and plain flags; each
            // descriptor returned is owned below.
            let (fd, writable) = unsafe {
                let fd = libc::open(path.as_ptr(), libc::O_RDWR | flags);
                if fd >= 0 {
                    (fd, true)
                } else {
                    (libc::open(path.as_ptr(), libc::O_RDONLY | flags), false)
                }
            };
            if fd < 0 {
                return None;
            }
            // SAFETY: a fresh descriptor nothing else owns.
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            let raw = fd.as_raw_fd();
            let mut keys = [0u8; KEY_BYTES];
            let mut abs = [0u8; ABS_BYTES];
            let mut ff = [0u8; FF_BYTES];
            if !ioctl(raw, eviocgbit(EV_KEY, KEY_BYTES), &mut keys) {
                return None;
            }
            ioctl(raw, eviocgbit(EV_ABS, ABS_BYTES), &mut abs);
            let rumble =
                writable && ioctl(raw, eviocgbit(EV_FF, FF_BYTES), &mut ff) && bit(&ff, FF_RUMBLE);
            let driver =
                std::fs::read_link(format!("/sys/class/input/event{node}/device/device/driver"))
                    .ok();
            let xpad = driver
                .as_ref()
                .and_then(|d| d.file_name())
                .is_some_and(|d| d == "xpad" || d.to_string_lossy().starts_with("xone"));
            let layout = Layout::of(&keys, &abs, xpad, |code| {
                let mut info = AbsInfo::default();
                ioctl(raw, eviocgabs(code), &mut info).then_some(Range {
                    min: info.minimum,
                    max: info.maximum,
                })
            })?;
            Some(Device {
                node,
                fd,
                decoder: Decoder::new(layout),
                rumble,
                effect: None,
            })
        }

        fn name(&self) -> String {
            let mut name = [0u8; 128];
            if !ioctl(self.fd.as_raw_fd(), EVIOCGNAME, &mut name) {
                return String::new();
            }
            let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
            String::from_utf8_lossy(&name[..end]).into_owned()
        }

        /// The whole state, read through the ioctls.
        fn read_whole(&mut self) -> Option<super::super::GamepadState> {
            let raw = self.fd.as_raw_fd();
            let mut keys = [0u8; KEY_BYTES];
            if !ioctl(raw, EVIOCGKEY, &mut keys) {
                return None;
            }
            Some(self.decoder.resync(&keys, |code| {
                let mut info = AbsInfo::default();
                ioctl(raw, eviocgabs(code), &mut info).then_some(info.value)
            }))
        }

        /// Reads every event queued, reporting each completed report;
        /// `false` once the device is gone.
        fn read(&mut self, pads: &mut Pads, events: &mut Vec<GamepadEvent>) -> bool {
            let key = u64::from(self.node);
            let mut buffer = [RawEvent::default(); 64];
            loop {
                // SAFETY: reading into a live buffer of `input_event`s of
                // its size.
                let n = unsafe {
                    libc::read(
                        self.fd.as_raw_fd(),
                        buffer.as_mut_ptr().cast(),
                        std::mem::size_of_val(&buffer),
                    )
                };
                if n < 0 {
                    let error = io::Error::last_os_error();
                    return error.kind() == io::ErrorKind::WouldBlock
                        || error.kind() == io::ErrorKind::Interrupted;
                }
                if n == 0 {
                    return false;
                }
                let count = n as usize / std::mem::size_of::<RawEvent>();
                for event in &buffer[..count] {
                    match self.decoder.event(event.kind, event.code, event.value) {
                        Step::Pending => {}
                        Step::Report(state) => pads.update(key, &state, events),
                        Step::Resync => {
                            if let Some(state) = self.read_whole() {
                                pads.update(key, &state, events);
                            }
                        }
                    }
                }
                if count < buffer.len() {
                    return true;
                }
            }
        }

        fn rumble(&mut self, rumble: Rumble) -> bool {
            if !self.rumble {
                return false;
            }
            let raw = self.fd.as_raw_fd();
            let stop = (rumble.strong <= 0.0 && rumble.weak <= 0.0) || rumble.duration.is_zero();
            if !stop {
                // SAFETY: an all-zero `ff_effect` is a valid value.
                let mut effect: libc::ff_effect = unsafe { std::mem::zeroed() };
                effect.type_ = FF_RUMBLE;
                effect.id = self.effect.unwrap_or(-1);
                effect.replay.length = rumble.duration.as_millis().min(u128::from(u16::MAX)) as u16;
                // SAFETY: the effect's union starts with the rumble
                // member's two magnitudes, and has room for them.
                unsafe {
                    effect
                        .u
                        .as_mut_ptr()
                        .cast::<libc::ff_rumble_effect>()
                        .write_unaligned(libc::ff_rumble_effect {
                            strong_magnitude: (rumble.strong * 65535.0) as u16,
                            weak_magnitude: (rumble.weak * 65535.0) as u16,
                        });
                }
                if !ioctl(raw, EVIOCSFF, &mut effect) {
                    return false;
                }
                self.effect = Some(effect.id);
            }
            let Some(id) = self.effect else {
                return true;
            };
            let play = RawEvent {
                kind: EV_FF,
                code: id as u16,
                value: i32::from(!stop),
                ..RawEvent::default()
            };
            // SAFETY: writing one live `input_event` to the device.
            let n = unsafe {
                libc::write(
                    raw,
                    std::ptr::from_ref(&play).cast(),
                    std::mem::size_of::<RawEvent>(),
                )
            };
            n == std::mem::size_of::<RawEvent>() as isize
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_ioctl_numbers_match_the_kernel_headers() {
            assert_eq!(eviocgabs(0x00), 0x8018_4540 as libc::Ioctl);
            assert_eq!(EVIOCGNAME, 0x8080_4506 as libc::Ioctl);
            assert_eq!(eviocgbit(EV_KEY, KEY_BYTES), 0x8060_4521 as libc::Ioctl);
            assert_eq!(EVIOCGKEY, 0x8060_4518 as libc::Ioctl);
            #[cfg(target_pointer_width = "64")]
            assert_eq!(EVIOCSFF, 0x4030_4580 as libc::Ioctl);
        }

        /// A virtual pad made through `/dev/uinput`, gone when dropped.
        struct Virtual(OwnedFd);

        impl Virtual {
            fn new() -> Virtual {
                const UI_SET_EVBIT: libc::Ioctl = 0x4004_5564 as libc::Ioctl;
                const UI_SET_KEYBIT: libc::Ioctl = 0x4004_5565 as libc::Ioctl;
                const UI_SET_ABSBIT: libc::Ioctl = 0x4004_5567 as libc::Ioctl;
                const UI_SET_FFBIT: libc::Ioctl = 0x4004_556b as libc::Ioctl;
                const UI_ABS_SETUP: libc::Ioctl = 0x401c_5504 as libc::Ioctl;
                const UI_DEV_SETUP: libc::Ioctl = 0x405c_5503 as libc::Ioctl;
                const UI_DEV_CREATE: libc::Ioctl = 0x5501 as libc::Ioctl;
                #[repr(C)]
                struct AbsSetup {
                    code: u16,
                    info: AbsInfo,
                }
                #[repr(C)]
                struct Setup {
                    id: [u16; 4],
                    name: [u8; 80],
                    ff_effects_max: u32,
                }
                let path = CString::new("/dev/uinput").expect("no NUL");
                // SAFETY: a NUL-terminated path; the descriptor is owned
                // below, and each ioctl takes the argument the uinput
                // interface defines for it.
                unsafe {
                    let fd = libc::open(path.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK);
                    assert!(fd >= 0, "/dev/uinput: {}", io::Error::last_os_error());
                    let int = |request, value: i32| {
                        assert!(libc::ioctl(fd, request, value) >= 0);
                    };
                    for ev in [super::super::EV_SYN, EV_KEY, EV_ABS, EV_FF] {
                        int(UI_SET_EVBIT, i32::from(ev));
                    }
                    for key in [0x130, 0x131, 0x133, 0x134, 0x13b] {
                        int(UI_SET_KEYBIT, key);
                    }
                    int(UI_SET_FFBIT, i32::from(FF_RUMBLE));
                    for code in [0x00u16, 0x01, 0x02, 0x03, 0x04, 0x05] {
                        int(UI_SET_ABSBIT, i32::from(code));
                        let trigger = code == 0x02 || code == 0x05;
                        let mut setup = AbsSetup {
                            code,
                            info: AbsInfo {
                                minimum: if trigger { 0 } else { -32768 },
                                maximum: if trigger { 255 } else { 32767 },
                                ..AbsInfo::default()
                            },
                        };
                        assert!(libc::ioctl(fd, UI_ABS_SETUP, &raw mut setup) >= 0);
                    }
                    let mut setup = Setup {
                        id: [0x03, 0x1234, 0x5678, 1],
                        name: [0; 80],
                        ff_effects_max: 4,
                    };
                    setup.name[..12].copy_from_slice(b"Viso Testpad");
                    assert!(libc::ioctl(fd, UI_DEV_SETUP, &raw mut setup) >= 0);
                    assert!(libc::ioctl(fd, UI_DEV_CREATE) >= 0);
                    Virtual(OwnedFd::from_raw_fd(fd))
                }
            }

            fn emit(&self, events: &[(u16, u16, i32)]) {
                for &(kind, code, value) in events.iter().chain(&[(0, 0, 0)]) {
                    let event = RawEvent {
                        kind,
                        code,
                        value,
                        ..RawEvent::default()
                    };
                    // SAFETY: writing one live `input_event`.
                    let n = unsafe {
                        libc::write(
                            self.0.as_raw_fd(),
                            std::ptr::from_ref(&event).cast(),
                            std::mem::size_of::<RawEvent>(),
                        )
                    };
                    assert_eq!(n, std::mem::size_of::<RawEvent>() as isize);
                }
            }
        }

        /// Answers the effect uploads the kernel forwards to the pad's
        /// maker, as a force-feedback driver does, and records the effects
        /// played, until `stop`.
        fn serve(
            fd: RawFd,
            stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        ) -> std::thread::JoinHandle<Vec<(u16, i32)>> {
            const EV_UINPUT: u16 = 0x0101;
            const UI_FF_UPLOAD: u16 = 1;
            const UI_BEGIN_FF_UPLOAD: libc::Ioctl = 0xc068_55c8_u32 as libc::Ioctl;
            const UI_END_FF_UPLOAD: libc::Ioctl = 0x4068_55c9 as libc::Ioctl;
            #[repr(C)]
            struct Upload {
                request_id: u32,
                retval: i32,
                effect: libc::ff_effect,
                old: libc::ff_effect,
            }
            // SAFETY: duplicating a live descriptor; the copy is owned by
            // the thread.
            let fd = unsafe { OwnedFd::from_raw_fd(libc::dup(fd)) };
            std::thread::spawn(move || {
                let mut played = Vec::new();
                while !stop.load(std::sync::atomic::Ordering::Acquire) {
                    let mut event = RawEvent::default();
                    // SAFETY: reading one `input_event` into a live local.
                    let n = unsafe {
                        libc::read(
                            fd.as_raw_fd(),
                            std::ptr::from_mut(&mut event).cast(),
                            std::mem::size_of::<RawEvent>(),
                        )
                    };
                    if n != std::mem::size_of::<RawEvent>() as isize {
                        std::thread::yield_now();
                        continue;
                    }
                    if event.kind == EV_UINPUT && event.code == UI_FF_UPLOAD {
                        // SAFETY: an all-zero upload is a valid value, and
                        // the two ioctls take it as uinput defines.
                        unsafe {
                            let mut upload: Upload = std::mem::zeroed();
                            upload.request_id = event.value as u32;
                            libc::ioctl(fd.as_raw_fd(), UI_BEGIN_FF_UPLOAD, &raw mut upload);
                            upload.retval = 0;
                            libc::ioctl(fd.as_raw_fd(), UI_END_FF_UPLOAD, &raw mut upload);
                        }
                    } else if event.kind == EV_FF {
                        played.push((event.code, event.value));
                    }
                }
                played
            })
        }

        /// Polls `source` until `done` holds of what it reported, for at
        /// most two seconds.
        fn poll_until(
            source: &mut Source,
            mut done: impl FnMut(&[GamepadEvent]) -> bool,
        ) -> Vec<GamepadEvent> {
            let mut events = Vec::new();
            let start = std::time::Instant::now();
            while !done(&events) && start.elapsed() < std::time::Duration::from_secs(2) {
                source.poll(&mut events);
            }
            events
        }

        #[test]
        #[ignore = "needs /dev/uinput and /dev/input access"]
        fn a_virtual_pad_connects_reports_and_disconnects() {
            use super::super::super::{GamepadButton, GamepadInfo};
            let mut source = Source::new(LoopWaker::inert()).expect("a source");
            let mut events = Vec::new();
            source.poll(&mut events);
            assert!(events.is_empty(), "no pad yet: {events:?}");

            let pad = Virtual::new();
            let connected = |e: &[GamepadEvent]| {
                e.iter().find_map(|e| match e {
                    GamepadEvent::Connected(GamepadInfo { id, name, rumble })
                        if name == "Viso Testpad" =>
                    {
                        Some((*id, *rumble))
                    }
                    _ => None,
                })
            };
            let events = poll_until(&mut source, |e| connected(e).is_some());
            let (id, rumble) = connected(&events).expect("hotplugged through inotify");
            assert!(rumble, "it declares FF_RUMBLE");

            pad.emit(&[
                (EV_KEY, 0x130, 1),
                (EV_ABS, 0x00, 32767),
                (EV_ABS, 0x05, 255),
            ]);
            let events = poll_until(&mut source, |e| !e.is_empty());
            let Some(GamepadEvent::Changed(changed, state)) = events.last() else {
                panic!("a change: {events:?}");
            };
            assert_eq!(*changed, id);
            assert!(state.held(GamepadButton::South));
            assert!(state.held(GamepadButton::RightTrigger));
            assert_eq!(state.left_stick, [1.0, 0.0]);
            assert_eq!(state.right_trigger, 1.0);

            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let server = serve(pad.0.as_raw_fd(), stop.clone());
            assert!(source.rumble(
                id,
                Rumble {
                    strong: 1.0,
                    weak: 0.5,
                    duration: std::time::Duration::from_millis(100),
                }
            ));
            let start = std::time::Instant::now();
            while start.elapsed() < std::time::Duration::from_millis(300) {
                std::thread::yield_now();
            }
            stop.store(true, std::sync::atomic::Ordering::Release);
            let played = server.join().expect("the server");
            assert_eq!(played.len(), 1, "one effect played: {played:?}");
            assert_eq!(played[0].1, 1, "started");

            drop(pad);
            let events = poll_until(&mut source, |e| e.contains(&GamepadEvent::Disconnected(id)));
            assert!(
                events.contains(&GamepadEvent::Disconnected(id)),
                "{events:?}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(bits: &mut [u8], n: u16) {
        bits[usize::from(n / 8)] |= 1 << (n % 8);
    }

    fn layout(xpad: bool, abs_codes: &[u16]) -> Option<Layout> {
        let mut keys = [0u8; KEY_MAX as usize / 8 + 1];
        for code in [BTN_SOUTH, BTN_EAST, BTN_NORTH, BTN_WEST, BTN_TL2, BTN_START] {
            set(&mut keys, code);
        }
        let mut abs = [0u8; ABS_MAX as usize / 8 + 1];
        for &code in abs_codes {
            set(&mut abs, code);
        }
        Layout::of(&keys, &abs, xpad, |code| {
            Some(match code {
                ABS_Z | ABS_RZ | ABS_BRAKE | ABS_GAS => Range { min: 0, max: 255 },
                ABS_HAT0X | ABS_HAT0Y => Range { min: -1, max: 1 },
                _ => Range {
                    min: -32768,
                    max: 32767,
                },
            })
        })
    }

    /// Feeds `events` and returns the reports.
    fn feed(decoder: &mut Decoder, events: &[(u16, u16, i32)]) -> Vec<Step> {
        events
            .iter()
            .map(|&(k, c, v)| decoder.event(k, c, v))
            .filter(|s| *s != Step::Pending)
            .collect()
    }

    #[test]
    fn a_device_without_the_gamepad_buttons_is_no_pad() {
        let keys = [0u8; KEY_MAX as usize / 8 + 1];
        assert!(Layout::of(&keys, &[], false, |_| None).is_none());
    }

    #[test]
    fn an_xbox_pads_recorded_report_reads_as_the_standard_layout() {
        let pad = layout(
            true,
            &[
                ABS_X, ABS_Y, ABS_Z, ABS_RX, ABS_RY, ABS_RZ, ABS_HAT0X, ABS_HAT0Y,
            ],
        )
        .expect("a pad");
        let mut decoder = Decoder::new(pad);
        let steps = feed(
            &mut decoder,
            &[
                (EV_KEY, BTN_SOUTH, 1),
                (EV_KEY, BTN_NORTH, 1),
                (EV_ABS, ABS_X, 32767),
                (EV_ABS, ABS_Y, -32768),
                (EV_ABS, ABS_RZ, 255),
                (EV_ABS, ABS_HAT0Y, -1),
                (EV_KEY, BTN_TL2, 1),
                (EV_SYN, SYN_REPORT, 0),
            ],
        );
        let [Step::Report(state)] = steps[..] else {
            panic!("one report: {steps:?}");
        };
        let held: Vec<_> = GamepadButton::ALL
            .into_iter()
            .filter(|b| state.held(*b))
            .collect();
        assert_eq!(
            held,
            [
                GamepadButton::South,
                GamepadButton::West,
                GamepadButton::LeftTrigger,
                GamepadButton::DpadUp
            ],
            "xpad's BTN_NORTH is the left button"
        );
        assert_eq!(state.left_stick, [1.0, 1.0], "y is up");
        assert_eq!(state.right_trigger, 1.0);
        assert_eq!(
            state.left_trigger, 0.0,
            "an analog trigger's button pulls nothing"
        );
    }

    #[test]
    fn a_generic_pad_has_its_right_stick_on_z_and_digital_triggers() {
        let pad = layout(false, &[ABS_X, ABS_Y, ABS_Z, ABS_RZ]).expect("a pad");
        let mut decoder = Decoder::new(pad);
        let steps = feed(
            &mut decoder,
            &[
                (EV_KEY, BTN_NORTH, 1),
                (EV_ABS, ABS_Z, 32767),
                (EV_ABS, ABS_RZ, 32767),
                (EV_KEY, BTN_TL2, 1),
                (EV_SYN, SYN_REPORT, 0),
            ],
        );
        let [Step::Report(state)] = steps[..] else {
            panic!("one report: {steps:?}");
        };
        assert!(state.held(GamepadButton::North), "positional");
        assert_eq!(state.right_stick, [1.0, -1.0]);
        assert_eq!(
            state.left_trigger, 1.0,
            "a digital trigger reads fully pulled"
        );
    }

    #[test]
    fn a_dropped_report_is_discarded_and_resynced() {
        let pad = layout(false, &[ABS_X, ABS_Y]).expect("a pad");
        let mut decoder = Decoder::new(pad);
        let steps = feed(
            &mut decoder,
            &[
                (EV_SYN, SYN_DROPPED, 0),
                (EV_KEY, BTN_SOUTH, 1),
                (EV_SYN, SYN_REPORT, 0),
            ],
        );
        assert_eq!(steps, [Step::Resync]);
        let mut keys = [0u8; KEY_MAX as usize / 8 + 1];
        set(&mut keys, BTN_EAST);
        let state = decoder.resync(&keys, |code| (code == ABS_X).then_some(-32768));
        assert!(state.held(GamepadButton::East) && !state.held(GamepadButton::South));
        assert_eq!(state.left_stick, [-1.0, 0.0]);
    }
}
