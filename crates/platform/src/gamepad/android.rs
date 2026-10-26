//! Gamepads on Android: the activity reports the input devices whose
//! sources are a gamepad or joystick (an `InputManager` listener, started at
//! the first poll), and while it does, routes their button keys and
//! joystick motion here instead of to the view. A device's state builds up
//! from them on the loop thread and is reported at the next poll.
//!
//! Android's layout is positional: `BUTTON_A` is the bottom face button.
//! The left stick is `AXIS_X`/`AXIS_Y`, the right `AXIS_Z`/`AXIS_RZ`, the
//! d-pad `AXIS_HAT_X`/`AXIS_HAT_Y` or the `DPAD_*` keys, and a trigger the
//! larger of `AXIS_LTRIGGER`/`AXIS_BRAKE` (left) and
//! `AXIS_RTRIGGER`/`AXIS_GAS` (right), or full while its button is held on
//! a pad without the axis.

use super::{GamepadButton, GamepadEvent, GamepadId, GamepadState, Pads};

const KEYCODE_DPAD_UP: i32 = 19;
const KEYCODE_DPAD_DOWN: i32 = 20;
const KEYCODE_DPAD_LEFT: i32 = 21;
const KEYCODE_DPAD_RIGHT: i32 = 22;
const KEYCODE_BACK: i32 = 4;
const KEYCODE_BUTTON_A: i32 = 96;
const KEYCODE_BUTTON_B: i32 = 97;
const KEYCODE_BUTTON_X: i32 = 99;
const KEYCODE_BUTTON_Y: i32 = 100;
const KEYCODE_BUTTON_L1: i32 = 102;
const KEYCODE_BUTTON_R1: i32 = 103;
const KEYCODE_BUTTON_L2: i32 = 104;
const KEYCODE_BUTTON_R2: i32 = 105;
const KEYCODE_BUTTON_THUMBL: i32 = 106;
const KEYCODE_BUTTON_THUMBR: i32 = 107;
const KEYCODE_BUTTON_START: i32 = 108;
const KEYCODE_BUTTON_SELECT: i32 = 109;
const KEYCODE_BUTTON_MODE: i32 = 110;

/// One motion sample's axes: the sticks (x, y, z, rz), the hat (x, y) and
/// the triggers (left, right, brake, gas).
pub(crate) type Axes = [f32; 10];

/// The button a gamepad key code is.
fn button(code: i32) -> Option<GamepadButton> {
    use GamepadButton as B;
    Some(match code {
        KEYCODE_BUTTON_A => B::South,
        KEYCODE_BUTTON_B => B::East,
        KEYCODE_BUTTON_X => B::West,
        KEYCODE_BUTTON_Y => B::North,
        KEYCODE_BUTTON_L1 => B::LeftShoulder,
        KEYCODE_BUTTON_R1 => B::RightShoulder,
        KEYCODE_BUTTON_L2 => B::LeftTrigger,
        KEYCODE_BUTTON_R2 => B::RightTrigger,
        KEYCODE_BUTTON_SELECT | KEYCODE_BACK => B::Select,
        KEYCODE_BUTTON_START => B::Start,
        KEYCODE_BUTTON_MODE => B::Home,
        KEYCODE_BUTTON_THUMBL => B::LeftThumb,
        KEYCODE_BUTTON_THUMBR => B::RightThumb,
        KEYCODE_DPAD_UP => B::DpadUp,
        KEYCODE_DPAD_DOWN => B::DpadDown,
        KEYCODE_DPAD_LEFT => B::DpadLeft,
        KEYCODE_DPAD_RIGHT => B::DpadRight,
        _ => return None,
    })
}

#[derive(Debug)]
struct Device {
    id: i32,
    /// The buttons its keys hold.
    keys: u32,
    axes: Axes,
    /// Something changed since the last poll.
    dirty: bool,
}

impl Device {
    fn state(&self) -> GamepadState {
        let [x, y, z, rz, hat_x, hat_y, left, right, brake, gas] = self.axes;
        let mut state = GamepadState {
            buttons: self.keys,
            left_stick: [x, -y],
            right_stick: [z, -rz],
            left_trigger: left.max(brake),
            right_trigger: right.max(gas),
        };
        for (button, on) in [
            (GamepadButton::DpadLeft, hat_x < -0.5),
            (GamepadButton::DpadRight, hat_x > 0.5),
            (GamepadButton::DpadUp, hat_y < -0.5),
            (GamepadButton::DpadDown, hat_y > 0.5),
        ] {
            if on {
                state.set(button, true);
            }
        }
        if state.held(GamepadButton::LeftTrigger) && state.left_trigger == 0.0 {
            state.left_trigger = 1.0;
        }
        if state.held(GamepadButton::RightTrigger) && state.right_trigger == 0.0 {
            state.right_trigger = 1.0;
        }
        state
    }
}

/// The pads the activity reports, built up from its events.
#[derive(Debug, Default)]
pub(crate) struct Feed {
    pads: Pads,
    devices: Vec<Device>,
    /// The connections and departures since the last poll.
    events: Vec<GamepadEvent>,
}

impl Feed {
    /// Input device `id`, a gamepad named `name`, connected (or changed).
    pub(crate) fn added(&mut self, id: i32, name: &str, rumble: bool) {
        if self.devices.iter().any(|d| d.id == id) {
            return;
        }
        self.pads.connect(id as u64, name, rumble, &mut self.events);
        self.devices.push(Device {
            id,
            keys: 0,
            axes: [0.0; 10],
            dirty: false,
        });
    }

    /// Input device `id` went away.
    pub(crate) fn removed(&mut self, id: i32) {
        self.devices.retain(|d| d.id != id);
        self.pads.disconnect(id as u64, &mut self.events);
    }

    /// A key of input device `id`; whether the feed has news since the
    /// last poll that it did not have before.
    pub(crate) fn key(&mut self, id: i32, code: i32, pressed: bool) -> bool {
        let (Some(device), Some(button)) =
            (self.devices.iter_mut().find(|d| d.id == id), button(code))
        else {
            return false;
        };
        if pressed {
            device.keys |= button.bit();
        } else {
            device.keys &= !button.bit();
        }
        !std::mem::replace(&mut device.dirty, true)
    }

    /// A motion sample of input device `id`; whether the feed has news
    /// since the last poll that it did not have before.
    pub(crate) fn motion(&mut self, id: i32, axes: Axes) -> bool {
        let Some(device) = self.devices.iter_mut().find(|d| d.id == id) else {
            return false;
        };
        device.axes = axes;
        !std::mem::replace(&mut device.dirty, true)
    }

    /// Appends what changed since the last poll.
    pub(crate) fn poll(&mut self, events: &mut Vec<GamepadEvent>) {
        events.append(&mut self.events);
        for device in &mut self.devices {
            if std::mem::take(&mut device.dirty) {
                self.pads.update(device.id as u64, &device.state(), events);
            }
        }
    }

    /// The input device pad `id` is.
    pub(crate) fn device(&self, id: GamepadId) -> Option<i32> {
        self.pads.key(id).map(|k| k as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_devices_keys_and_motion_read_as_the_standard_layout() {
        let mut feed = Feed::default();
        let mut events = Vec::new();
        assert!(!feed.key(9, KEYCODE_BUTTON_A, true));
        feed.poll(&mut events);
        assert!(events.is_empty(), "an unknown device is ignored");

        feed.added(9, "Pad", true);
        assert!(feed.key(9, KEYCODE_BUTTON_A, true), "the first news wakes");
        assert!(
            !feed.key(9, KEYCODE_BUTTON_L2, true),
            "the rest waits for it"
        );
        assert!(!feed.motion(9, [0.5, -1.0, 0.0, 1.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.8]));
        feed.poll(&mut events);
        let [
            GamepadEvent::Connected(info),
            GamepadEvent::Changed(id, state),
        ] = &events[..]
        else {
            panic!("a connection, then a state: {events:?}");
        };
        let (id, state) = (*id, *state);
        assert_eq!(info.id, id);
        assert_eq!(feed.device(id), Some(9));
        let held: Vec<_> = GamepadButton::ALL
            .into_iter()
            .filter(|b| state.held(*b))
            .collect();
        assert_eq!(
            held,
            [
                GamepadButton::South,
                GamepadButton::LeftTrigger,
                GamepadButton::RightTrigger,
                GamepadButton::DpadLeft
            ]
        );
        assert_eq!(state.left_stick, [0.5, 1.0], "y is up");
        assert_eq!(state.right_stick, [0.0, -1.0]);
        assert_eq!(state.left_trigger, 1.0, "a held trigger without its axis");
        assert_eq!(state.right_trigger, 0.8, "the gas axis");

        events.clear();
        feed.poll(&mut events);
        assert!(events.is_empty(), "nothing new");
        assert!(feed.motion(9, [0.0; 10]), "news after a poll wakes again");
        feed.poll(&mut events);
        events.clear();
        feed.removed(9);
        feed.poll(&mut events);
        assert_eq!(events, [GamepadEvent::Disconnected(id)]);
    }
}
