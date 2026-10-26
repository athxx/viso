//! Gamepads on the web: the Gamepad API. `gamepadconnected` and
//! `gamepaddisconnected` (watched from the first poll) mark the list stale
//! and wake the loop; `navigator.getGamepads()` is called once a poll only
//! while a pad is connected or the list is stale, and a pad is read only
//! when its `timestamp` moved. Browsers expose a pad only after the page
//! has seen input from it, so a pad already plugged in connects at its
//! first press.
//!
//! A pad with the `"standard"` mapping has its buttons and axes where the
//! Gamepad API places them; any other is read by the same indices, the
//! closest guess the API allows. The standard y axes point down and are
//! turned up here. Rumble plays a `"dual-rumble"` effect on the pad's
//! `vibrationActuator`, where the browser has one.

use super::{GamepadButton, GamepadState};

/// The standard mapping's buttons, by index.
const BUTTONS: [GamepadButton; 17] = [
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
    GamepadButton::LeftThumb,
    GamepadButton::RightThumb,
    GamepadButton::DpadUp,
    GamepadButton::DpadDown,
    GamepadButton::DpadLeft,
    GamepadButton::DpadRight,
    GamepadButton::Home,
];

/// The standard state of a pad whose buttons with index `i` are pressed
/// where bit `i` of `pressed` is set, whose triggers (buttons 6 and 7) read
/// `triggers`, and whose first four axes read `axes`. The triggers' buttons
/// follow their values, not the browser's own threshold.
pub(crate) fn standard(pressed: u32, triggers: [f64; 2], axes: [f64; 4]) -> GamepadState {
    let mut state = GamepadState {
        left_stick: [axes[0] as f32, -axes[1] as f32],
        right_stick: [axes[2] as f32, -axes[3] as f32],
        left_trigger: triggers[0] as f32,
        right_trigger: triggers[1] as f32,
        ..GamepadState::default()
    };
    for (index, button) in BUTTONS.into_iter().enumerate() {
        let trigger = matches!(
            button,
            GamepadButton::LeftTrigger | GamepadButton::RightTrigger
        );
        if !trigger && pressed & (1 << index) != 0 {
            state.set(button, true);
        }
    }
    state
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub(crate) use native::Source;

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod native {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use js_sys::{Array, Function, Object, Reflect};
    use wasm_bindgen::JsCast;
    use wasm_bindgen::prelude::{Closure, JsValue, wasm_bindgen};
    use web_sys::Event;

    use super::super::{GamepadEvent, GamepadId, GamepadState, Pads, Rumble};
    use super::standard;
    use crate::LoopWaker;

    #[wasm_bindgen]
    extern "C" {
        /// A `Gamepad`.
        type Pad;

        #[wasm_bindgen(method, getter)]
        fn index(this: &Pad) -> u32;

        #[wasm_bindgen(method, getter)]
        fn id(this: &Pad) -> String;

        #[wasm_bindgen(method, getter)]
        fn connected(this: &Pad) -> bool;

        #[wasm_bindgen(method, getter)]
        fn timestamp(this: &Pad) -> f64;

        #[wasm_bindgen(method, getter)]
        fn buttons(this: &Pad) -> Array;

        #[wasm_bindgen(method, getter)]
        fn axes(this: &Pad) -> Array;

        #[wasm_bindgen(method, getter, js_name = vibrationActuator)]
        fn vibration_actuator(this: &Pad) -> JsValue;

        /// A `GamepadButton`.
        type PadButton;

        #[wasm_bindgen(method, getter)]
        fn pressed(this: &PadButton) -> bool;

        #[wasm_bindgen(method, getter)]
        fn value(this: &PadButton) -> f64;

        /// A `GamepadHapticActuator`.
        type Actuator;

        #[wasm_bindgen(method, catch, js_name = playEffect)]
        fn play_effect(this: &Actuator, kind: &str, params: &JsValue) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(method, catch)]
        fn reset(this: &Actuator) -> Result<JsValue, JsValue>;
    }

    /// A connected pad: its index in `getGamepads()` (its key) and the
    /// `timestamp` it was last read at.
    struct Slot {
        index: u32,
        timestamp: f64,
    }

    /// An event listener on the window, by its event's name.
    type Listener = (&'static str, Closure<dyn FnMut(Event)>);

    /// What the connection listeners hand the poll.
    #[derive(Default)]
    struct News {
        /// Read the list again.
        stale: Cell<bool>,
        /// The indices `gamepaddisconnected` reported, gone even if a new
        /// pad took the index before the poll.
        departed: RefCell<Vec<u32>>,
    }

    /// The pads the Gamepad API reports.
    pub(crate) struct Source {
        window: web_sys::Window,
        navigator: web_sys::Navigator,
        get: Function,
        pads: Pads,
        slots: Vec<Slot>,
        news: Rc<News>,
        listeners: [Listener; 2],
        /// Swallows a rejected rumble promise.
        ignore: Closure<dyn FnMut(JsValue)>,
    }

    impl Source {
        /// Watches the page's pads, kicking `waker` at each connection;
        /// `None` where the browser lacks the Gamepad API (or forbids it,
        /// outside a secure context).
        pub(crate) fn new(waker: LoopWaker) -> Option<Source> {
            let window = web_sys::window()?;
            let navigator = window.navigator();
            let get = Reflect::get(&navigator, &JsValue::from_str("getGamepads"))
                .ok()?
                .dyn_into::<Function>()
                .ok()?;
            let news = Rc::new(News {
                stale: Cell::new(true),
                departed: RefCell::new(Vec::with_capacity(4)),
            });
            let listener = |departs: bool| {
                let news = news.clone();
                let waker = waker.clone();
                Closure::<dyn FnMut(Event)>::new(move |event: Event| {
                    if departs {
                        let pad = Reflect::get(&event, &JsValue::from_str("gamepad"))
                            .ok()
                            .and_then(|p| Reflect::get(&p, &JsValue::from_str("index")).ok())
                            .and_then(|i| i.as_f64());
                        if let Some(index) = pad {
                            news.departed.borrow_mut().push(index as u32);
                        }
                    }
                    news.stale.set(true);
                    waker.wake();
                })
            };
            let listeners = [
                ("gamepadconnected", listener(false)),
                ("gamepaddisconnected", listener(true)),
            ];
            for (kind, callback) in &listeners {
                let _ = window
                    .add_event_listener_with_callback(kind, callback.as_ref().unchecked_ref());
            }
            Some(Source {
                window,
                navigator,
                get,
                pads: Pads::default(),
                slots: Vec::with_capacity(4),
                news,
                listeners,
                ignore: Closure::new(|_: JsValue| {}),
            })
        }

        /// `navigator.getGamepads()`, empty when it throws.
        fn list(&self) -> Array {
            self.get
                .call0(&self.navigator)
                .ok()
                .and_then(|list| list.dyn_into::<Array>().ok())
                .unwrap_or_default()
        }

        pub(crate) fn poll(&mut self, events: &mut Vec<GamepadEvent>) {
            let stale = self.news.stale.replace(false);
            if !stale && self.slots.is_empty() {
                return;
            }
            for index in self.news.departed.borrow_mut().drain(..) {
                if let Some(at) = self.slots.iter().position(|s| s.index == index) {
                    self.slots.swap_remove(at);
                    self.pads.disconnect(u64::from(index), events);
                }
            }
            let list = self.list();
            let pads = &mut self.pads;
            self.slots.retain_mut(|slot| {
                let Some(pad) = present(&list, slot.index) else {
                    pads.disconnect(u64::from(slot.index), events);
                    return false;
                };
                let timestamp = pad.timestamp();
                if timestamp != slot.timestamp || timestamp == 0.0 {
                    slot.timestamp = timestamp;
                    pads.update(u64::from(slot.index), &read(&pad), events);
                }
                true
            });
            if !stale {
                return;
            }
            for index in 0..list.length() {
                if self.slots.iter().any(|s| s.index == index) {
                    continue;
                }
                let Some(pad) = present(&list, index) else {
                    continue;
                };
                let actuator = pad.vibration_actuator();
                let rumble = !actuator.is_undefined() && !actuator.is_null();
                self.pads
                    .connect(u64::from(index), &pad.id(), rumble, events);
                self.pads.update(u64::from(index), &read(&pad), events);
                self.slots.push(Slot {
                    index,
                    timestamp: pad.timestamp(),
                });
            }
        }

        pub(crate) fn rumble(&mut self, id: GamepadId, rumble: Rumble) -> bool {
            let Some(index) = self.pads.key(id) else {
                return false;
            };
            let Some(pad) = present(&self.list(), index as u32) else {
                return false;
            };
            let actuator = pad.vibration_actuator();
            if actuator.is_undefined() || actuator.is_null() {
                return false;
            }
            let actuator: Actuator = actuator.unchecked_into();
            let rumble = rumble.clamped();
            let played = if (rumble.strong <= 0.0 && rumble.weak <= 0.0)
                || rumble.duration.is_zero()
            {
                actuator.reset()
            } else {
                let params = Object::new();
                for (name, value) in [
                    ("startDelay", 0.0),
                    ("duration", rumble.duration.as_secs_f64() * 1000.0),
                    ("strongMagnitude", f64::from(rumble.strong)),
                    ("weakMagnitude", f64::from(rumble.weak)),
                ] {
                    let _ = Reflect::set(&params, &JsValue::from_str(name), &JsValue::from(value));
                }
                actuator.play_effect("dual-rumble", &params)
            };
            match played {
                Ok(promise) => {
                    if let Ok(promise) = promise.dyn_into::<js_sys::Promise>() {
                        let _ = promise.catch(&self.ignore);
                    }
                    true
                }
                Err(_) => false,
            }
        }
    }

    impl Drop for Source {
        fn drop(&mut self) {
            for (kind, callback) in &self.listeners {
                let _ = self
                    .window
                    .remove_event_listener_with_callback(kind, callback.as_ref().unchecked_ref());
            }
        }
    }

    /// The connected pad at `index` of `list`.
    fn present(list: &Array, index: u32) -> Option<Pad> {
        let pad = list.get(index);
        if pad.is_undefined() || pad.is_null() {
            return None;
        }
        let pad: Pad = pad.unchecked_into();
        pad.connected().then_some(pad)
    }

    /// `pad`'s state.
    fn read(pad: &Pad) -> GamepadState {
        let buttons = pad.buttons();
        let mut pressed = 0u32;
        let mut triggers = [0.0; 2];
        for index in 0..buttons.length().min(17) {
            let button = buttons.get(index);
            if button.is_undefined() || button.is_null() {
                continue;
            }
            let button: PadButton = button.unchecked_into();
            match index {
                6 | 7 => triggers[index as usize - 6] = button.value(),
                _ => {
                    if button.pressed() {
                        pressed |= 1 << index;
                    }
                }
            }
        }
        let list = pad.axes();
        let mut axes = [0.0; 4];
        for (index, axis) in axes.iter_mut().enumerate() {
            *axis = list.get(index as u32).as_f64().unwrap_or(0.0);
        }
        standard(pressed, triggers, axes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_standard_mappings_buttons_and_axes_read_as_the_standard_layout() {
        // South, North, Select, Home, d-pad left, and button 6 (a trigger,
        // left to its value).
        let pressed = 1 | 1 << 3 | 1 << 8 | 1 << 16 | 1 << 14 | 1 << 6;
        let state = standard(pressed, [0.0, 0.75], [-1.0, -1.0, 0.5, 1.0]);
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
        assert_eq!(state.right_stick, [0.5, -1.0]);
        assert_eq!((state.left_trigger, state.right_trigger), (0.0, 0.75));
    }
}
