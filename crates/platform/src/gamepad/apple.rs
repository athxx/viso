//! Gamepads on macOS and iOS: `GameController.framework`, loaded on the
//! first poll. Each controller with an extended gamepad profile is a pad;
//! its elements are looked up once at connection, so a poll only reads
//! their values. The connect and disconnect notifications mark the list
//! stale and wake the loop; the list is read again only then. Rumble plays
//! a continuous haptic event through the controller's CoreHaptics engine.

use std::cell::Cell;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use objc2::msg_send;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2_foundation::{NSArray, NSNotification, NSNotificationCenter, NSString};

use super::{GamepadButton, GamepadEvent, GamepadId, GamepadState, Pads, Rumble};
use crate::LoopWaker;

const RTLD_NOW: c_int = 0x2;
const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

const FRAMEWORK: &CStr = c"/System/Library/Frameworks/GameController.framework/GameController";

/// The `NSString *const` the image exports as `name`.
fn constant(name: &CStr) -> Option<&'static NSString> {
    // SAFETY: `dlsym` takes a NUL-terminated name; the symbol, when found,
    // is a framework constant holding an immortal string.
    unsafe {
        let slot = dlsym(RTLD_DEFAULT, name.as_ptr()).cast::<*const NSString>();
        if slot.is_null() {
            return None;
        }
        (*slot).as_ref()
    }
}

/// The selectors of a pad's buttons, in [`GamepadButton`] order.
const BUTTONS: [&CStr; 13] = [
    c"buttonA",
    c"buttonB",
    c"buttonX",
    c"buttonY",
    c"leftShoulder",
    c"rightShoulder",
    c"leftTrigger",
    c"rightTrigger",
    c"buttonOptions",
    c"buttonMenu",
    c"buttonHome",
    c"leftThumbstickButton",
    c"rightThumbstickButton",
];

/// A connected controller and the elements of its extended gamepad.
struct Controller {
    object: Retained<AnyObject>,
    /// The inputs of each [`GamepadButton`] the profile has; the d-pad's
    /// four last.
    buttons: [Option<Retained<AnyObject>>; 17],
    /// The sticks' x and y axes, left then right.
    axes: [Option<Retained<AnyObject>>; 4],
    haptics: Option<Haptics>,
}

/// A controller's haptic engine and the rumble it plays.
struct Haptics {
    engine: Retained<AnyObject>,
    player: Option<Retained<AnyObject>>,
}

/// The pads `GameController.framework` sees.
pub(crate) struct Source {
    class: &'static AnyClass,
    pads: Pads,
    controllers: Vec<Controller>,
    /// Set by the connection notifications: read the list again.
    stale: Rc<Cell<bool>>,
    observers: Vec<Retained<AnyObject>>,
}

impl Source {
    /// Loads the framework and watches its connections, kicking `waker`
    /// at each; `None` where it is missing.
    pub(crate) fn new(waker: LoopWaker) -> Option<Source> {
        // SAFETY: the path is NUL-terminated; loading a system framework
        // runs only its own initializers.
        let image = unsafe { dlopen(FRAMEWORK.as_ptr(), RTLD_NOW) };
        if image.is_null() {
            return None;
        }
        let class = AnyClass::get(c"GCController")?;
        let stale = Rc::new(Cell::new(true));
        let center = NSNotificationCenter::defaultCenter();
        let mut observers = Vec::with_capacity(2);
        for name in [
            c"GCControllerDidConnectNotification",
            c"GCControllerDidDisconnectNotification",
        ] {
            let Some(name) = constant(name) else {
                continue;
            };
            let flag = stale.clone();
            let waker = waker.clone();
            let block = RcBlock::new(move |_: NonNull<NSNotification>| {
                flag.set(true);
                waker.wake();
            });
            // SAFETY: `addObserverForName:object:queue:usingBlock:` takes a
            // name, optional object and queue, and a block of one
            // notification; with no queue the block runs on the posting
            // thread, the main thread for these. The center copies the
            // block; the returned token is kept to remove it.
            let token: Option<Retained<AnyObject>> = unsafe {
                msg_send![
                    &*center,
                    addObserverForName: name,
                    object: None::<&AnyObject>,
                    queue: None::<&AnyObject>,
                    usingBlock: &*block
                ]
            };
            observers.extend(token);
        }
        Some(Source {
            class,
            pads: Pads::default(),
            controllers: Vec::with_capacity(4),
            stale,
            observers,
        })
    }

    pub(crate) fn poll(&mut self, events: &mut Vec<GamepadEvent>) {
        if self.stale.replace(false) {
            objc2::rc::autoreleasepool(|_| self.refresh(events));
        }
        for controller in &self.controllers {
            let key = Retained::as_ptr(&controller.object) as u64;
            self.pads.update(key, &controller.read(), events);
        }
    }

    /// Reads the connected controllers again, connecting the new and
    /// disconnecting the gone.
    fn refresh(&mut self, events: &mut Vec<GamepadEvent>) {
        // SAFETY: `+[GCController controllers]` returns an array of
        // controllers.
        let list: Option<Retained<NSArray<AnyObject>>> =
            unsafe { msg_send![self.class, controllers] };
        let list: Vec<Retained<AnyObject>> = list.map(|l| l.to_vec()).unwrap_or_default();
        let key = |o: &Retained<AnyObject>| Retained::as_ptr(o) as u64;
        let pads = &mut self.pads;
        self.controllers.retain(|c| {
            let here = list.iter().any(|o| key(o) == key(&c.object));
            if !here {
                pads.disconnect(key(&c.object), events);
            }
            here
        });
        for object in list {
            if self
                .controllers
                .iter()
                .any(|c| key(&c.object) == key(&object))
            {
                continue;
            }
            let Some(controller) = Controller::new(object) else {
                continue;
            };
            let name = controller.name();
            let rumble = controller.has_haptics();
            pads.connect(key(&controller.object), &name, rumble, events);
            self.controllers.push(controller);
        }
    }

    pub(crate) fn rumble(&mut self, id: GamepadId, rumble: Rumble) -> bool {
        let Some(key) = self.pads.key(id) else {
            return false;
        };
        let Some(controller) = self
            .controllers
            .iter_mut()
            .find(|c| Retained::as_ptr(&c.object) as u64 == key)
        else {
            return false;
        };
        objc2::rc::autoreleasepool(|_| controller.rumble(rumble.clamped()))
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        let center = NSNotificationCenter::defaultCenter();
        for token in self.observers.drain(..) {
            // SAFETY: each token came from `addObserverForName:…`.
            let () = unsafe { msg_send![&*center, removeObserver: &*token] };
        }
        for controller in &mut self.controllers {
            controller.stop();
        }
    }
}

/// `object`'s property `name`, an object or nil.
fn property(object: &AnyObject, name: &CStr) -> Option<Retained<AnyObject>> {
    let selector = objc2::runtime::Sel::register(name);
    // SAFETY: `respondsToSelector:` is NSObject's; a property getter of
    // these profiles takes no argument and returns an object.
    unsafe {
        let responds: bool = msg_send![object, respondsToSelector: selector];
        if !responds {
            return None;
        }
        let value: *mut AnyObject = msg_send![object, performSelector: selector];
        Retained::retain(value)
    }
}

impl Controller {
    /// `object`, if it has an extended gamepad profile.
    fn new(object: Retained<AnyObject>) -> Option<Controller> {
        let profile = property(&object, c"extendedGamepad")?;
        let mut buttons: [Option<Retained<AnyObject>>; 17] = Default::default();
        for (slot, name) in buttons.iter_mut().zip(BUTTONS) {
            *slot = property(&profile, name);
        }
        let dpad = property(&profile, c"dpad");
        if let Some(dpad) = &dpad {
            for (i, name) in [c"up", c"down", c"left", c"right"].into_iter().enumerate() {
                buttons[GamepadButton::DpadUp as usize + i] = property(dpad, name);
            }
        }
        let mut axes: [Option<Retained<AnyObject>>; 4] = Default::default();
        for (i, stick) in [c"leftThumbstick", c"rightThumbstick"]
            .into_iter()
            .enumerate()
        {
            if let Some(stick) = property(&profile, stick) {
                axes[i * 2] = property(&stick, c"xAxis");
                axes[i * 2 + 1] = property(&stick, c"yAxis");
            }
        }
        Some(Controller {
            object,
            buttons,
            axes,
            haptics: None,
        })
    }

    fn name(&self) -> String {
        property(&self.object, c"vendorName")
            .or_else(|| property(&self.object, c"productCategory"))
            .and_then(|name| name.downcast::<NSString>().ok())
            .map(|name| name.to_string())
            .unwrap_or_default()
    }

    fn has_haptics(&self) -> bool {
        property(&self.object, c"haptics").is_some()
    }

    /// The pad's state.
    fn read(&self) -> GamepadState {
        let mut state = GamepadState::default();
        for (i, input) in self.buttons.iter().enumerate() {
            let Some(input) = input else { continue };
            let button = GamepadButton::ALL[i];
            match button {
                GamepadButton::LeftTrigger | GamepadButton::RightTrigger => {
                    // SAFETY: a button input's `value` is a float.
                    let value: f32 = unsafe { msg_send![&**input, value] };
                    if button == GamepadButton::LeftTrigger {
                        state.left_trigger = value;
                    } else {
                        state.right_trigger = value;
                    }
                }
                _ => {
                    // SAFETY: a button input's `isPressed` is a BOOL.
                    let pressed: bool = unsafe { msg_send![&**input, isPressed] };
                    state.set(button, pressed);
                }
            }
        }
        let mut axes = [0.0f32; 4];
        for (value, axis) in axes.iter_mut().zip(&self.axes) {
            if let Some(axis) = axis {
                // SAFETY: an axis input's `value` is a float.
                *value = unsafe { msg_send![&**axis, value] };
            }
        }
        state.left_stick = [axes[0], axes[1]];
        state.right_stick = [axes[2], axes[3]];
        state
    }

    /// Plays `rumble`, replacing the one playing; whether the controller
    /// has haptics.
    fn rumble(&mut self, rumble: Rumble) -> bool {
        if self.haptics.is_none() {
            self.haptics = self.engine();
        }
        let Some(haptics) = &mut self.haptics else {
            return false;
        };
        if let Some(player) = haptics.player.take() {
            // SAFETY: `stopAtTime:error:` takes a time and an error out
            // parameter; time 0 is now.
            let _: Result<(), Retained<NSObject>> =
                unsafe { msg_send![&*player, stopAtTime: 0.0f64, error: _] };
        }
        let strength = rumble.strong.max(rumble.weak);
        if strength <= 0.0 || rumble.duration.is_zero() {
            return true;
        }
        let sharpness = rumble.weak / (rumble.strong + rumble.weak);
        if let Some(player) = continuous(&haptics.engine, strength, sharpness, rumble.duration) {
            haptics.player = Some(player);
        }
        true
    }

    /// The controller's started haptic engine for all its actuators.
    fn engine(&self) -> Option<Haptics> {
        let haptics = property(&self.object, c"haptics")?;
        let locality = constant(c"GCHapticsLocalityDefault")?;
        // SAFETY: `createEngineWithLocality:` takes a locality name and
        // returns an engine or nil; `startAndReturnError:` starts it.
        unsafe {
            let engine: Option<Retained<AnyObject>> =
                msg_send![&*haptics, createEngineWithLocality: locality];
            let engine = engine?;
            let started: Result<(), Retained<NSObject>> =
                msg_send![&*engine, startAndReturnError: _];
            started.ok()?;
            Some(Haptics {
                engine,
                player: None,
            })
        }
    }

    fn stop(&mut self) {
        if let Some(haptics) = self.haptics.take() {
            // SAFETY: `stopWithCompletionHandler:` takes an optional block.
            let none = None::<&block2::Block<dyn Fn(*mut AnyObject)>>;
            let () = unsafe { msg_send![&*haptics.engine, stopWithCompletionHandler: none] };
        }
    }
}

/// Starts on `engine` a continuous haptic event of `intensity` and
/// `sharpness` lasting `duration`, returning its player.
fn continuous(
    engine: &AnyObject,
    intensity: f32,
    sharpness: f32,
    duration: std::time::Duration,
) -> Option<Retained<AnyObject>> {
    let kind = constant(c"CHHapticEventTypeHapticContinuous")?;
    let intensity_id = constant(c"CHHapticEventParameterIDHapticIntensity")?;
    let sharpness_id = constant(c"CHHapticEventParameterIDHapticSharpness")?;
    let parameter_class = AnyClass::get(c"CHHapticEventParameter")?;
    let event_class = AnyClass::get(c"CHHapticEvent")?;
    let pattern_class = AnyClass::get(c"CHHapticPattern")?;
    // SAFETY: each initializer is CoreHaptics' own, taking the argument
    // types written here (an `NSString` id and a float, an event type, an
    // array of parameters and two time intervals, an array of events and
    // one of dynamic parameters with an error out parameter), consuming the
    // object `alloc` returned.
    unsafe {
        let parameter = |id: &NSString, value: f32| -> Option<Retained<AnyObject>> {
            let object: Allocated<AnyObject> = msg_send![parameter_class, alloc];
            msg_send![object, initWithParameterID: id, value: value]
        };
        let parameters = NSArray::from_retained_slice(&[
            parameter(intensity_id, intensity)?,
            parameter(sharpness_id, sharpness)?,
        ]);
        let event: Allocated<AnyObject> = msg_send![event_class, alloc];
        let event: Option<Retained<AnyObject>> = msg_send![
            event,
            initWithEventType: kind,
            parameters: &*parameters,
            relativeTime: 0.0f64,
            duration: duration.as_secs_f64()
        ];
        let events = NSArray::from_retained_slice(&[event?]);
        let none = NSArray::<AnyObject>::new();
        let pattern: Allocated<AnyObject> = msg_send![pattern_class, alloc];
        let pattern: Result<Retained<AnyObject>, Retained<NSObject>> = msg_send![
            pattern,
            initWithEvents: &*events,
            parameters: &*none,
            error: _
        ];
        let pattern = pattern.ok()?;
        let player: Result<Retained<AnyObject>, Retained<NSObject>> =
            msg_send![engine, createPlayerWithPattern: &*pattern, error: _];
        let player = player.ok()?;
        let started: Result<(), Retained<NSObject>> =
            msg_send![&*player, startAtTime: 0.0f64, error: _];
        started.ok()?;
        Some(player)
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn the_framework_loads_and_a_poll_reports_the_controllers_connected() {
        let mut source = Source::new(LoopWaker::inert()).expect("GameController.framework");
        assert_eq!(source.observers.len(), 2, "both notifications watched");
        let mut events = Vec::new();
        source.poll(&mut events);
        let connected = events
            .iter()
            .filter(|e| matches!(e, GamepadEvent::Connected(_)))
            .count();
        assert_eq!(connected, source.controllers.len());
        events.clear();
        source.poll(&mut events);
        assert!(
            events
                .iter()
                .all(|e| matches!(e, GamepadEvent::Changed(..))),
            "no connection twice: {events:?}"
        );
        assert!(!source.rumble(
            GamepadId(u32::MAX),
            Rumble {
                strong: 1.0,
                weak: 1.0,
                duration: std::time::Duration::from_millis(10),
            }
        ));
    }

    /// Sets `object`'s element `name` (a button) to `value`.
    fn press(profile: &AnyObject, name: &CStr, value: f32) {
        let input = property(profile, name).expect("the element");
        // SAFETY: a snapshot controller's button input takes `setValue:`.
        let () = unsafe { msg_send![&*input, setValue: value] };
    }

    #[test]
    fn a_controllers_elements_read_as_the_standard_layout() {
        Source::new(LoopWaker::inert()).expect("GameController.framework");
        let class = AnyClass::get(c"GCController").expect("the class");
        // SAFETY: `+[GCController controllerWithExtendedGamepad]` makes a
        // controller whose elements the app sets.
        let object: Option<Retained<AnyObject>> =
            unsafe { msg_send![class, controllerWithExtendedGamepad] };
        let controller = Controller::new(object.expect("a controller")).expect("extended");
        let profile = property(&controller.object, c"extendedGamepad").expect("the profile");
        press(&profile, c"buttonA", 1.0);
        press(&profile, c"buttonY", 1.0);
        press(&profile, c"leftTrigger", 0.75);
        let dpad = property(&profile, c"dpad").expect("the d-pad");
        let stick = property(&profile, c"rightThumbstick").expect("the stick");
        // SAFETY: a snapshot controller's direction pads take
        // `setValueForXAxis:yAxis:`.
        unsafe {
            let () = msg_send![&*dpad, setValueForXAxis: -1.0f32, yAxis: 0.0f32];
            let () = msg_send![&*stick, setValueForXAxis: 0.5f32, yAxis: -1.0f32];
        }
        let state = controller.read();
        let held: Vec<_> = GamepadButton::ALL
            .into_iter()
            .filter(|b| state.held(*b))
            .collect();
        assert_eq!(
            held,
            [
                GamepadButton::South,
                GamepadButton::North,
                GamepadButton::DpadLeft
            ]
        );
        assert_eq!(state.left_trigger, 0.75);
        assert_eq!(state.right_stick, [0.5, -1.0]);
        assert_eq!(state.left_stick, [0.0, 0.0]);

        press(&profile, c"buttonA", 0.0);
        assert!(!controller.read().held(GamepadButton::South));
    }
}
