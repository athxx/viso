//! Gamepads: the controllers connected to the device, read in one standard
//! layout.
//!
//! Every backend reports the same [`GamepadState`]: four face buttons by
//! position, the d-pad, two shoulders, two analog triggers, two sticks and
//! the select, start and home buttons. The runtime polls once per frame
//! ([`PlatformApp::poll_gamepads`](crate::PlatformApp::poll_gamepads)) and
//! receives a [`GamepadEvent`] only for what changed since the last poll: a
//! pad that connected, one that went away, or a pad whose state moved. A
//! steady state costs no allocation; only a connection carries a name.
//!
//! The sticks pass a radial dead zone of [`STICK_REST`]: inside it a stick
//! reads exactly centred, so a pad at rest raises no events, and outside it
//! the value is the device's own, unscaled, so a game's dead zone keeps its
//! meaning. A trigger past [`TRIGGER_PRESS`] holds its button bit until it
//! falls back under [`TRIGGER_RELEASE`].
//!
//! macOS and iOS read `GameController.framework`, Windows XInput, Linux
//! evdev, Android the activity's gamepad key and motion events and the web
//! the Gamepad API; the headless backend is fed by a test through
//! [`HeadlessGamepads`](crate::backend::headless::HeadlessGamepads).

use std::time::Duration;

#[cfg(any(target_os = "android", test))]
pub(crate) mod android;
#[cfg(target_vendor = "apple")]
mod apple;
#[cfg(any(target_os = "linux", test))]
mod evdev;
#[cfg(any(all(target_arch = "wasm32", target_os = "unknown"), test))]
mod web;
#[cfg(any(target_os = "windows", test))]
mod xinput;

#[cfg(target_vendor = "apple")]
use apple as imp;
#[cfg(target_os = "linux")]
use evdev as imp;
#[cfg(not(any(
    target_vendor = "apple",
    target_os = "windows",
    target_os = "linux",
    target_os = "android",
    target_family = "wasm"
)))]
use none as imp;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
use web as imp;
#[cfg(target_os = "windows")]
use xinput as imp;

#[cfg(not(any(
    target_os = "android",
    all(target_family = "wasm", not(target_os = "unknown"))
)))]
use crate::LoopWaker;

/// A connected gamepad's id: unique to its connection, never reused while
/// the process runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GamepadId(pub u32);

/// A gamepad button, by its position in the standard layout: `South` is the
/// bottom face button (A on an Xbox pad, cross on a PlayStation pad).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum GamepadButton {
    South,
    East,
    West,
    North,
    LeftShoulder,
    RightShoulder,
    LeftTrigger,
    RightTrigger,
    Select,
    Start,
    Home,
    LeftThumb,
    RightThumb,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
}

impl GamepadButton {
    /// Every button, in bit order.
    pub const ALL: [GamepadButton; 17] = [
        GamepadButton::South,
        GamepadButton::East,
        GamepadButton::West,
        GamepadButton::North,
        GamepadButton::LeftShoulder,
        GamepadButton::RightShoulder,
        GamepadButton::LeftTrigger,
        GamepadButton::RightTrigger,
        GamepadButton::Select,
        GamepadButton::Start,
        GamepadButton::Home,
        GamepadButton::LeftThumb,
        GamepadButton::RightThumb,
        GamepadButton::DpadUp,
        GamepadButton::DpadDown,
        GamepadButton::DpadLeft,
        GamepadButton::DpadRight,
    ];

    /// Its bit in [`GamepadState::buttons`].
    pub const fn bit(self) -> u32 {
        1 << self as u32
    }
}

/// A gamepad's state in the standard layout.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct GamepadState {
    /// The buttons held, one [`GamepadButton::bit`] each.
    pub buttons: u32,
    /// The left stick, each axis in `[-1, 1]`, `x` right and `y` up.
    pub left_stick: [f32; 2],
    /// The right stick, as the left.
    pub right_stick: [f32; 2],
    /// How far the left trigger is pulled, `0` released to `1` fully.
    pub left_trigger: f32,
    /// How far the right trigger is pulled.
    pub right_trigger: f32,
}

impl GamepadState {
    /// Whether `button` is held.
    pub fn held(&self, button: GamepadButton) -> bool {
        self.buttons & button.bit() != 0
    }

    /// Sets whether `button` is held.
    pub fn set(&mut self, button: GamepadButton, held: bool) {
        if held {
            self.buttons |= button.bit();
        } else {
            self.buttons &= !button.bit();
        }
    }
}

/// A gamepad that connected.
#[derive(Debug, Clone, PartialEq)]
pub struct GamepadInfo {
    pub id: GamepadId,
    /// The name the device gives, or a generic one.
    pub name: String,
    /// Whether [`PlatformApp::rumble_gamepad`](crate::PlatformApp::rumble_gamepad)
    /// can shake it.
    pub rumble: bool,
}

/// What changed about the gamepads since the last poll.
#[derive(Debug, Clone, PartialEq)]
pub enum GamepadEvent {
    /// A gamepad connected, its state all released. One that was connected
    /// at launch is reported at the first poll.
    Connected(GamepadInfo),
    /// The gamepad's state changed; it carries the whole new state.
    Changed(GamepadId, GamepadState),
    /// The gamepad went away; what it held is released.
    Disconnected(GamepadId),
}

/// A rumble: the strong (low-frequency) and weak (high-frequency) motors'
/// strength, each `0..=1`, for `duration`. A new rumble replaces one still
/// playing; zero strength stops it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rumble {
    pub strong: f32,
    pub weak: f32,
    pub duration: Duration,
}

impl Rumble {
    /// The rumble with both strengths clamped to `0..=1` (non-finite is 0).
    pub(crate) fn clamped(self) -> Rumble {
        let unit = |v: f32| {
            if v.is_finite() {
                v.clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        Rumble {
            strong: unit(self.strong),
            weak: unit(self.weak),
            duration: self.duration,
        }
    }
}

/// The radius inside which a stick reads exactly centred.
pub const STICK_REST: f32 = 0.1;

/// The travel at which a trigger's button goes down.
pub const TRIGGER_PRESS: f32 = 0.25;

/// The travel under which a held trigger's button goes up.
pub const TRIGGER_RELEASE: f32 = 0.15;

/// A trigger's travel that reads as released: the noise of one at rest.
const TRIGGER_REST: f32 = 0.02;

/// The smallest analog change reported.
const ANALOG_STEP: f32 = 1.0 / 512.0;

/// The pads a backend has connected, by the backend's own key for each (a
/// slot, a device node, an object), turning what the device reads into
/// standard states and the events of what changed.
#[derive(Debug)]
pub(crate) struct Pads {
    next: u32,
    pads: Vec<Pad>,
}

#[derive(Debug)]
struct Pad {
    key: u64,
    id: GamepadId,
    /// The state last reported.
    state: GamepadState,
}

impl Default for Pads {
    fn default() -> Self {
        Pads {
            next: 1,
            pads: Vec::with_capacity(4),
        }
    }
}

impl Pads {
    /// Connects the pad the backend knows as `key`, reporting it; a key
    /// already connected keeps its id and reports nothing.
    pub(crate) fn connect(
        &mut self,
        key: u64,
        name: &str,
        rumble: bool,
        events: &mut Vec<GamepadEvent>,
    ) -> GamepadId {
        if let Some(id) = self.id(key) {
            return id;
        }
        let id = GamepadId(self.next);
        self.next += 1;
        self.pads.push(Pad {
            key,
            id,
            state: GamepadState::default(),
        });
        let name = name.trim();
        events.push(GamepadEvent::Connected(GamepadInfo {
            id,
            name: if name.is_empty() { "Gamepad" } else { name }.to_owned(),
            rumble,
        }));
        id
    }

    /// Disconnects the pad keyed `key`, if connected, reporting it.
    pub(crate) fn disconnect(&mut self, key: u64, events: &mut Vec<GamepadEvent>) {
        if let Some(i) = self.pads.iter().position(|p| p.key == key) {
            let pad = self.pads.remove(i);
            events.push(GamepadEvent::Disconnected(pad.id));
        }
    }

    /// Reads `raw`, what the pad keyed `key` reports now, reporting the
    /// standard state it gives if that differs from the last reported.
    pub(crate) fn update(&mut self, key: u64, raw: &GamepadState, events: &mut Vec<GamepadEvent>) {
        let Some(pad) = self.pads.iter_mut().find(|p| p.key == key) else {
            return;
        };
        let next = standard(raw, &pad.state);
        if differs(&next, &pad.state) {
            pad.state = next;
            events.push(GamepadEvent::Changed(pad.id, next));
        }
    }

    /// The id of the pad keyed `key`.
    pub(crate) fn id(&self, key: u64) -> Option<GamepadId> {
        self.pads.iter().find(|p| p.key == key).map(|p| p.id)
    }

    /// The key of the pad `id`.
    pub(crate) fn key(&self, id: GamepadId) -> Option<u64> {
        self.pads.iter().find(|p| p.id == id).map(|p| p.key)
    }
}

/// A backend's gamepads, started at the first poll. Android feeds its own
/// from the events its activity delivers.
#[cfg(not(any(
    target_os = "android",
    all(target_family = "wasm", not(target_os = "unknown"))
)))]
#[derive(Default)]
pub(crate) struct Gamepads(Option<Option<imp::Source>>);

#[cfg(not(any(
    target_os = "android",
    all(target_family = "wasm", not(target_os = "unknown"))
)))]
impl Gamepads {
    /// Whether the source was started (or found missing).
    pub(crate) fn started(&self) -> bool {
        self.0.is_some()
    }

    /// Starts the source, its connections kicking `waker`.
    pub(crate) fn start(&mut self, waker: LoopWaker) {
        if self.0.is_none() {
            self.0 = Some(imp::Source::new(waker));
        }
    }

    pub(crate) fn poll(&mut self, events: &mut Vec<GamepadEvent>) {
        if let Some(Some(source)) = &mut self.0 {
            source.poll(events);
        }
    }

    pub(crate) fn rumble(&mut self, id: GamepadId, rumble: Rumble) -> bool {
        match &mut self.0 {
            Some(Some(source)) => source.rumble(id, rumble),
            _ => false,
        }
    }

    /// When the earliest rumble playing is due to stop, on a backend whose
    /// device does not stop it itself: the pump wakes for it.
    #[cfg(target_os = "windows")]
    pub(crate) fn deadline(&self) -> Option<crate::Instant> {
        match &self.0 {
            Some(Some(source)) => source.deadline(),
            _ => None,
        }
    }
}

/// A target with no gamepad support.
#[cfg(not(any(
    target_vendor = "apple",
    target_os = "windows",
    target_os = "linux",
    target_os = "android",
    target_family = "wasm"
)))]
mod none {
    use super::{GamepadEvent, GamepadId, Rumble};
    use crate::LoopWaker;

    pub(crate) struct Source;

    impl Source {
        pub(crate) fn new(_waker: LoopWaker) -> Option<Source> {
            None
        }

        pub(crate) fn poll(&mut self, _events: &mut Vec<GamepadEvent>) {}

        pub(crate) fn rumble(&mut self, _id: GamepadId, _rumble: Rumble) -> bool {
            false
        }
    }
}

/// `raw` in the standard form, `last` the state last reported: sticks
/// inside their rest radius centred, triggers in `0..=1` holding their
/// buttons with hysteresis.
fn standard(raw: &GamepadState, last: &GamepadState) -> GamepadState {
    let stick = |[x, y]: [f32; 2]| {
        let axis = |v: f32| {
            if v.is_finite() {
                v.clamp(-1.0, 1.0)
            } else {
                0.0
            }
        };
        let (x, y) = (axis(x), axis(y));
        if x * x + y * y <= STICK_REST * STICK_REST {
            [0.0, 0.0]
        } else {
            [x, y]
        }
    };
    let trigger = |v: f32| {
        let v = if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if v <= TRIGGER_REST { 0.0 } else { v }
    };
    let mut state = GamepadState {
        buttons: raw.buttons & ((1 << GamepadButton::ALL.len()) - 1),
        left_stick: stick(raw.left_stick),
        right_stick: stick(raw.right_stick),
        left_trigger: trigger(raw.left_trigger),
        right_trigger: trigger(raw.right_trigger),
    };
    for (button, value) in [
        (GamepadButton::LeftTrigger, state.left_trigger),
        (GamepadButton::RightTrigger, state.right_trigger),
    ] {
        let threshold = if last.held(button) {
            TRIGGER_RELEASE
        } else {
            TRIGGER_PRESS
        };
        if value >= threshold {
            state.set(button, true);
        }
    }
    state
}

/// Whether `next` is worth reporting over `last`: a button changed, or an
/// analog value moved a step, or reached rest or full travel.
fn differs(next: &GamepadState, last: &GamepadState) -> bool {
    let moved =
        |a: f32, b: f32| (a - b).abs() >= ANALOG_STEP || (a != b && (a == 0.0 || a.abs() == 1.0));
    next.buttons != last.buttons
        || moved(next.left_stick[0], last.left_stick[0])
        || moved(next.left_stick[1], last.left_stick[1])
        || moved(next.right_stick[0], last.right_stick[0])
        || moved(next.right_stick[1], last.right_stick[1])
        || moved(next.left_trigger, last.left_trigger)
        || moved(next.right_trigger, last.right_trigger)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changed(events: &[GamepadEvent]) -> Vec<GamepadState> {
        events
            .iter()
            .filter_map(|e| match e {
                GamepadEvent::Changed(_, s) => Some(*s),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_pad_reports_its_connection_changes_and_departure_once_each() {
        let mut pads = Pads::default();
        let mut events = Vec::new();
        let id = pads.connect(7, "  Pad  ", true, &mut events);
        assert_eq!(pads.connect(7, "Pad", true, &mut events), id, "same key");
        assert_eq!(
            events,
            [GamepadEvent::Connected(GamepadInfo {
                id,
                name: "Pad".into(),
                rumble: true
            })]
        );
        events.clear();

        let mut raw = GamepadState::default();
        raw.set(GamepadButton::South, true);
        pads.update(7, &raw, &mut events);
        pads.update(7, &raw, &mut events);
        assert_eq!(changed(&events).len(), 1, "only the change");
        events.clear();

        pads.disconnect(7, &mut events);
        pads.disconnect(7, &mut events);
        assert_eq!(events, [GamepadEvent::Disconnected(id)]);
        let again = pads.connect(7, "", false, &mut events);
        assert_ne!(again, id, "a new connection, a new id");
        assert!(matches!(&events[1], GamepadEvent::Connected(i) if i.name == "Gamepad"));
    }

    #[test]
    fn a_stick_at_rest_reads_centred_and_noise_raises_nothing() {
        let mut pads = Pads::default();
        let mut events = Vec::new();
        pads.connect(1, "Pad", false, &mut events);
        events.clear();
        for jitter in [0.03, -0.05, 0.07] {
            let raw = GamepadState {
                left_stick: [jitter, 0.04],
                right_stick: [0.0, -jitter],
                left_trigger: 0.01,
                ..GamepadState::default()
            };
            pads.update(1, &raw, &mut events);
        }
        assert!(events.is_empty(), "rest noise: {events:?}");

        let raw = GamepadState {
            left_stick: [0.5, f32::NAN],
            right_stick: [-3.0, 0.0],
            ..GamepadState::default()
        };
        pads.update(1, &raw, &mut events);
        let state = changed(&events)[0];
        assert_eq!(
            state.left_stick,
            [0.5, 0.0],
            "unscaled past the rest radius"
        );
        assert_eq!(state.right_stick, [-1.0, 0.0], "clamped");
        events.clear();
        let raw = GamepadState {
            left_stick: [0.5005, 0.0],
            right_stick: [-1.0, 0.0],
            ..GamepadState::default()
        };
        pads.update(1, &raw, &mut events);
        assert!(events.is_empty(), "under a step");
    }

    #[test]
    fn a_trigger_holds_its_button_with_hysteresis() {
        let mut pads = Pads::default();
        let mut events = Vec::new();
        pads.connect(1, "Pad", false, &mut events);
        let held = |pads: &mut Pads, v: f32| {
            let mut events = Vec::new();
            let raw = GamepadState {
                right_trigger: v,
                ..GamepadState::default()
            };
            pads.update(1, &raw, &mut events);
            let pad = pads.pads.iter().find(|p| p.key == 1).expect("connected");
            pad.state.held(GamepadButton::RightTrigger)
        };
        assert!(!held(&mut pads, 0.2), "under the press threshold");
        assert!(held(&mut pads, 0.3));
        assert!(
            held(&mut pads, 0.2),
            "held until under the release threshold"
        );
        assert!(!held(&mut pads, 0.1));
    }

    #[test]
    fn a_rumble_clamps_its_strengths() {
        let r = Rumble {
            strong: 2.0,
            weak: f32::NAN,
            duration: Duration::from_millis(100),
        }
        .clamped();
        assert_eq!((r.strong, r.weak), (1.0, 0.0));
    }
}
