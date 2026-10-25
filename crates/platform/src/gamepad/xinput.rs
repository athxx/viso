//! Gamepads on Windows: XInput, loaded on the first poll (`xinput1_4.dll`,
//! else `xinput1_3.dll` or `xinput9_1_0.dll`). Its four slots are the pads:
//! a connected slot is read once a poll and its state used only when the
//! packet number moved; an empty slot is probed one at a time, a quarter
//! second apart, because probing an empty slot is slow. The hidden
//! `XInputGetStateEx` export, where present, adds the guide button. Rumble
//! sets the two motors and stops them at the poll after it is due; the pump
//! wakes for that deadline when nothing else does.

use super::{GamepadButton, GamepadState};

/// `XINPUT_GAMEPAD`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct XGamepad {
    pub buttons: u16,
    pub left_trigger: u8,
    pub right_trigger: u8,
    pub thumb_lx: i16,
    pub thumb_ly: i16,
    pub thumb_rx: i16,
    pub thumb_ry: i16,
}

/// XInput's button bits, by the [`GamepadButton`] each is.
const BITS: [(u16, GamepadButton); 15] = [
    (0x1000, GamepadButton::South),
    (0x2000, GamepadButton::East),
    (0x4000, GamepadButton::West),
    (0x8000, GamepadButton::North),
    (0x0100, GamepadButton::LeftShoulder),
    (0x0200, GamepadButton::RightShoulder),
    (0x0020, GamepadButton::Select),
    (0x0010, GamepadButton::Start),
    (0x0400, GamepadButton::Home),
    (0x0040, GamepadButton::LeftThumb),
    (0x0080, GamepadButton::RightThumb),
    (0x0001, GamepadButton::DpadUp),
    (0x0002, GamepadButton::DpadDown),
    (0x0004, GamepadButton::DpadLeft),
    (0x0008, GamepadButton::DpadRight),
];

/// The standard state of what XInput reads.
pub(crate) fn standard(pad: &XGamepad) -> GamepadState {
    let axis = |v: i16| (f32::from(v) / 32767.0).max(-1.0);
    let mut state = GamepadState {
        left_stick: [axis(pad.thumb_lx), axis(pad.thumb_ly)],
        right_stick: [axis(pad.thumb_rx), axis(pad.thumb_ry)],
        left_trigger: f32::from(pad.left_trigger) / 255.0,
        right_trigger: f32::from(pad.right_trigger) / 255.0,
        ..GamepadState::default()
    };
    for (bit, button) in BITS {
        state.set(button, pad.buttons & bit != 0);
    }
    state
}

#[cfg(target_os = "windows")]
pub(crate) use native::Source;

#[cfg(target_os = "windows")]
mod native {
    use std::time::Duration;

    use windows::Win32::Foundation::{FARPROC, HMODULE};
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::core::{PCSTR, PCWSTR, w};

    use super::super::{GamepadEvent, GamepadId, Pads, Rumble};
    use super::{XGamepad, standard};
    use crate::{Instant, LoopWaker};

    const SLOTS: usize = 4;
    const ERROR_SUCCESS: u32 = 0;
    /// How long after probing one empty slot the next is probed.
    const PROBE_EVERY: Duration = Duration::from_millis(250);

    /// `XINPUT_STATE`, with the trailing word `XInputGetStateEx` writes.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct XState {
        packet: u32,
        gamepad: XGamepad,
        reserved: u32,
    }

    /// `XINPUT_VIBRATION`.
    #[repr(C)]
    struct XVibration {
        left: u16,
        right: u16,
    }

    type GetState = unsafe extern "system" fn(u32, *mut XState) -> u32;
    type SetState = unsafe extern "system" fn(u32, *mut XVibration) -> u32;

    #[derive(Clone, Copy, Default)]
    struct Slot {
        connected: bool,
        packet: u32,
        /// When the rumble playing stops.
        rumble_until: Option<Instant>,
    }

    /// The pads XInput's slots hold.
    pub(crate) struct Source {
        get: GetState,
        set: Option<SetState>,
        pads: Pads,
        slots: [Slot; SLOTS],
        /// The empty slot probed next, and when.
        probe: usize,
        probe_at: Option<Instant>,
    }

    /// The export `name` of `module`, as `F`.
    fn export<F: Copy>(module: HMODULE, name: PCSTR) -> Option<F> {
        // SAFETY: `module` is a loaded library and `name` a NUL-terminated
        // name or an ordinal; the caller names the export's signature.
        let found: FARPROC = unsafe { GetProcAddress(module, name) };
        let found = found?;
        // SAFETY: a function pointer, reinterpreted as the signature the
        // caller states for it.
        Some(unsafe { std::mem::transmute_copy::<_, F>(&found) })
    }

    impl Source {
        /// Loads XInput; `None` without it.
        pub(crate) fn new(_waker: LoopWaker) -> Option<Source> {
            let libraries: [(PCWSTR, bool); 3] = [
                (w!("xinput1_4.dll"), true),
                (w!("xinput1_3.dll"), true),
                (w!("xinput9_1_0.dll"), false),
            ];
            let (module, guide) = libraries.into_iter().find_map(|(name, guide)| {
                // SAFETY: a NUL-terminated system library name.
                unsafe { LoadLibraryW(name) }.ok().map(|m| (m, guide))
            })?;
            // The guide button's state is behind the export ordinal 100.
            let get = guide
                .then(|| export::<GetState>(module, PCSTR(100 as *const u8)))
                .flatten()
                .or_else(|| export(module, windows::core::s!("XInputGetState")))?;
            let set = export(module, windows::core::s!("XInputSetState"));
            Some(Source {
                get,
                set,
                pads: Pads::default(),
                slots: [Slot::default(); SLOTS],
                probe: 0,
                probe_at: None,
            })
        }

        fn read(&self, slot: usize) -> Option<XState> {
            let mut state = XState::default();
            // SAFETY: `slot` is below four and `state` a writable
            // `XINPUT_STATE` with room for the extended form.
            let status = unsafe { (self.get)(slot as u32, &mut state) };
            (status == ERROR_SUCCESS).then_some(state)
        }

        pub(crate) fn poll(&mut self, events: &mut Vec<GamepadEvent>) {
            let mut now = None;
            let mut clock = || *now.get_or_insert_with(Instant::now);
            for slot in 0..SLOTS {
                if !self.slots[slot].connected {
                    continue;
                }
                match self.read(slot) {
                    Some(state) => {
                        if state.packet != self.slots[slot].packet {
                            self.slots[slot].packet = state.packet;
                            self.pads
                                .update(slot as u64, &standard(&state.gamepad), events);
                        }
                    }
                    None => {
                        self.slots[slot] = Slot::default();
                        self.pads.disconnect(slot as u64, events);
                    }
                }
                if self.slots[slot].rumble_until.is_some_and(|t| clock() >= t) {
                    self.vibrate(slot, 0.0, 0.0);
                    self.slots[slot].rumble_until = None;
                }
            }
            // The first poll probes every slot; later ones one empty slot
            // at a time.
            let first = self.probe_at.is_none();
            if self.slots.iter().all(|s| s.connected)
                || self.probe_at.is_some_and(|at| clock() < at)
            {
                return;
            }
            for _ in 0..SLOTS {
                let slot = self.probe;
                self.probe = (self.probe + 1) % SLOTS;
                if self.slots[slot].connected {
                    continue;
                }
                if let Some(state) = self.read(slot) {
                    self.slots[slot] = Slot {
                        connected: true,
                        packet: state.packet,
                        rumble_until: None,
                    };
                    let name = format!("Xbox Controller {}", slot + 1);
                    self.pads
                        .connect(slot as u64, &name, self.set.is_some(), events);
                    self.pads
                        .update(slot as u64, &standard(&state.gamepad), events);
                }
                if !first {
                    break;
                }
            }
            self.probe_at = Some(clock() + PROBE_EVERY);
        }

        fn vibrate(&self, slot: usize, strong: f32, weak: f32) -> bool {
            let Some(set) = self.set else {
                return false;
            };
            let mut vibration = XVibration {
                left: (strong * 65535.0) as u16,
                right: (weak * 65535.0) as u16,
            };
            // SAFETY: `slot` is below four and `vibration` a valid
            // `XINPUT_VIBRATION`.
            unsafe { set(slot as u32, &mut vibration) == ERROR_SUCCESS }
        }

        pub(crate) fn rumble(&mut self, id: GamepadId, rumble: Rumble) -> bool {
            let Some(slot) = self.pads.key(id).map(|k| k as usize) else {
                return false;
            };
            let rumble = rumble.clamped();
            let stop = (rumble.strong <= 0.0 && rumble.weak <= 0.0) || rumble.duration.is_zero();
            let (strong, weak) = if stop {
                (0.0, 0.0)
            } else {
                (rumble.strong, rumble.weak)
            };
            if !self.vibrate(slot, strong, weak) {
                return false;
            }
            self.slots[slot].rumble_until = (!stop).then(|| Instant::now() + rumble.duration);
            true
        }

        /// When the earliest rumble playing is due to stop.
        pub(crate) fn deadline(&self) -> Option<Instant> {
            self.slots.iter().filter_map(|s| s.rumble_until).min()
        }
    }

    impl Drop for Source {
        fn drop(&mut self) {
            for slot in 0..SLOTS {
                if self.slots[slot].rumble_until.is_some() {
                    self.vibrate(slot, 0.0, 0.0);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xinputs_buttons_and_axes_read_as_the_standard_layout() {
        let pad = XGamepad {
            buttons: 0x1000 | 0x8000 | 0x0400 | 0x0004 | 0x0020,
            left_trigger: 255,
            right_trigger: 0,
            thumb_lx: -32768,
            thumb_ly: 32767,
            thumb_rx: 16384,
            thumb_ry: 0,
        };
        let state = standard(&pad);
        let held: Vec<_> = GamepadButton::ALL
            .into_iter()
            .filter(|b| state.held(*b))
            .collect();
        assert_eq!(
            held,
            [
                GamepadButton::South,
                GamepadButton::North,
                GamepadButton::Select,
                GamepadButton::Home,
                GamepadButton::DpadLeft
            ]
        );
        assert_eq!(state.left_stick, [-1.0, 1.0], "y is up");
        assert!((state.right_stick[0] - 0.5).abs() < 1e-4);
        assert_eq!((state.left_trigger, state.right_trigger), (1.0, 0.0));
    }

    #[test]
    fn the_struct_layouts_are_xinputs() {
        assert_eq!(std::mem::size_of::<XGamepad>(), 12);
    }
}
