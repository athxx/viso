//! Which node input sample delivers which DSL event, and its payload value.
//!
//! Payload records are built in the VM's representation of the prelude types:
//! a record is an aggregate of its fields in declaration order, `Bool` and a unit
//! enum variant are integers, `Dp` is a float, `Some(x)` is `x`. The field and
//! variant orders below are the prelude's; the compiler checks them against it.

use std::rc::Rc;

use viso_behavior::{Aggregate, Value};
use viso_ui::{EventCx, Key, KeyEvent, Modifiers, PointerButtons, PointerId, PointerPhase};

/// A DSL node event the runtime delivers, and the input sample it comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EventRoute {
    /// `click` (`ClickEvent`): a pointer release on the node.
    Click = 0,
    /// `tap` (`TapEvent`): a pointer release on the node.
    Tap = 1,
    /// `pointer_down` (`PointerEvent`).
    PointerDown = 2,
    /// `pointer_move` (`PointerEvent`).
    PointerMove = 3,
    /// `pointer_up` (`PointerEvent`).
    PointerUp = 4,
    /// `hover_enter` (`HoverEvent`): the pointer entered the node.
    HoverEnter = 5,
    /// `hover_leave` (`HoverEvent`): the pointer left the node.
    HoverLeave = 6,
    /// `key_down` (`KeyEvent`): a key press routed to the node.
    KeyDown = 7,
    /// `key_up` (`KeyEvent`): a key release routed to the node.
    KeyUp = 8,
    /// A control's `changed`: the value it reports after a toggle, a slide or
    /// a settled edit.
    Changed = 9,
    /// A text field's `submitted`: Enter pressed in it.
    Submitted = 10,
    /// A tab strip's or radio group's `selected_changed`.
    SelectedChanged = 11,
}

impl EventRoute {
    /// Every route, by discriminant.
    pub const ALL: [EventRoute; 12] = [
        EventRoute::Click,
        EventRoute::Tap,
        EventRoute::PointerDown,
        EventRoute::PointerMove,
        EventRoute::PointerUp,
        EventRoute::HoverEnter,
        EventRoute::HoverLeave,
        EventRoute::KeyDown,
        EventRoute::KeyUp,
        EventRoute::Changed,
        EventRoute::Submitted,
        EventRoute::SelectedChanged,
    ];

    /// The route of standard DSL event `event`, `None` for an event the runtime
    /// does not deliver. A control's own events route through
    /// [`ControlKind::route`](crate::ControlKind::route).
    pub fn of(event: &str) -> Option<EventRoute> {
        EventRoute::ALL
            .into_iter()
            .find(|route| !route.is_control() && route.event() == event)
    }

    /// The DSL event name.
    pub fn event(self) -> &'static str {
        match self {
            EventRoute::Click => "click",
            EventRoute::Tap => "tap",
            EventRoute::PointerDown => "pointer_down",
            EventRoute::PointerMove => "pointer_move",
            EventRoute::PointerUp => "pointer_up",
            EventRoute::HoverEnter => "hover_enter",
            EventRoute::HoverLeave => "hover_leave",
            EventRoute::KeyDown => "key_down",
            EventRoute::KeyUp => "key_up",
            EventRoute::Changed => "changed",
            EventRoute::Submitted => "submitted",
            EventRoute::SelectedChanged => "selected_changed",
        }
    }

    /// The Rust variant name, for code that names a route in tokens.
    pub fn variant(self) -> &'static str {
        match self {
            EventRoute::Click => "Click",
            EventRoute::Tap => "Tap",
            EventRoute::PointerDown => "PointerDown",
            EventRoute::PointerMove => "PointerMove",
            EventRoute::PointerUp => "PointerUp",
            EventRoute::HoverEnter => "HoverEnter",
            EventRoute::HoverLeave => "HoverLeave",
            EventRoute::KeyDown => "KeyDown",
            EventRoute::KeyUp => "KeyUp",
            EventRoute::Changed => "Changed",
            EventRoute::Submitted => "Submitted",
            EventRoute::SelectedChanged => "SelectedChanged",
        }
    }

    /// The route with discriminant `raw`.
    pub fn from_u8(raw: u8) -> Option<EventRoute> {
        EventRoute::ALL.get(usize::from(raw)).copied()
    }

    /// Whether a key sample delivers it; otherwise a pointer sample does.
    pub fn is_key(self) -> bool {
        matches!(self, EventRoute::KeyDown | EventRoute::KeyUp)
    }

    /// Whether a control reports it, from whichever sample drives the control.
    pub fn is_control(self) -> bool {
        matches!(
            self,
            EventRoute::Changed | EventRoute::Submitted | EventRoute::SelectedChanged
        )
    }

    /// The payload this dispatch delivers for a standard route, `None` when the
    /// sample under dispatch is not one the route fires on. A pointer
    /// position is local to the dispatching node.
    pub fn payload(self, cx: &EventCx<'_>) -> Option<Value> {
        if self.is_control() {
            return None;
        }
        if self.is_key() {
            let key = cx.key()?;
            return (key.pressed == (self == EventRoute::KeyDown)).then(|| key_event(key));
        }
        let pointer = cx.pointer()?;
        let phase = match self {
            EventRoute::Click | EventRoute::Tap | EventRoute::PointerUp => PointerPhase::Up,
            EventRoute::PointerDown => PointerPhase::Down,
            EventRoute::PointerMove => PointerPhase::Move,
            EventRoute::HoverEnter => PointerPhase::Enter,
            EventRoute::HoverLeave => PointerPhase::Leave,
            _ => return None,
        };
        if pointer.phase != phase {
            return None;
        }
        let (x, y) = cx.rect().map_or((0.0, 0.0), |r| (r.x, r.y));
        let position = point(pointer.x - x, pointer.y - y);
        let kind = pointer_kind(cx.pointer_id());
        Some(match self {
            EventRoute::Click => record([position, modifiers(pointer.modifiers)]),
            EventRoute::Tap | EventRoute::HoverEnter | EventRoute::HoverLeave => {
                record([position, kind])
            }
            _ => record([
                position,
                Value::Int(button(pointer.buttons)),
                buttons(pointer.buttons),
                kind,
                modifiers(pointer.modifiers),
            ]),
        })
    }
}

/// The prelude records a payload is built from, with their fields in order.
pub const PAYLOAD_RECORDS: &[(&str, &[&str])] = &[
    ("Point", &["x", "y"]),
    ("Modifiers", &["shift", "control", "alt", "logo"]),
    ("PointerButtons", &["primary", "secondary", "middle"]),
    ("ClickEvent", &["position", "modifiers"]),
    ("TapEvent", &["position", "pointer_kind"]),
    ("HoverEvent", &["position", "pointer_kind"]),
    (
        "PointerEvent",
        &["position", "button", "buttons", "pointer_kind", "modifiers"],
    ),
    ("KeyEvent", &["key", "repeat", "modifiers"]),
];

/// The prelude enums a payload names a variant of, with their variants in order.
pub const PAYLOAD_ENUMS: &[(&str, &[&str])] = &[
    ("PointerButton", &["primary", "secondary", "middle"]),
    ("PointerKind", &["mouse", "touch", "pen"]),
    (
        "Key",
        &[
            "char",
            "enter",
            "escape",
            "tab",
            "backspace",
            "delete",
            "space",
            "arrow_up",
            "arrow_down",
            "arrow_left",
            "arrow_right",
            "home",
            "end",
            "page_up",
            "page_down",
            "function",
            "unidentified",
        ],
    ),
];

/// A record aggregate of `fields`.
fn record<const N: usize>(fields: [Value; N]) -> Value {
    Value::Agg(Rc::new(Aggregate {
        tag: 0,
        fields: Box::new(fields),
    }))
}

/// A payload enum variant `tag` carrying `field`.
fn variant(tag: u32, field: Value) -> Value {
    Value::Agg(Rc::new(Aggregate {
        tag,
        fields: Box::new([field]),
    }))
}

/// A `Point`.
fn point(x: f32, y: f32) -> Value {
    record([Value::Float(f64::from(x)), Value::Float(f64::from(y))])
}

/// A `Modifiers`.
fn modifiers(m: Modifiers) -> Value {
    record([
        Value::bool(m.shift),
        Value::bool(m.control),
        Value::bool(m.alt),
        Value::bool(m.logo),
    ])
}

/// A `PointerButtons`.
fn buttons(b: PointerButtons) -> Value {
    record([
        Value::bool(b.contains(PointerButtons::PRIMARY)),
        Value::bool(b.contains(PointerButtons::SECONDARY)),
        Value::bool(b.contains(PointerButtons::MIDDLE)),
    ])
}

/// The `PointerButton` a sample names: the lowest pressed button, primary for a
/// release (which has none pressed).
fn button(b: PointerButtons) -> i64 {
    if b.contains(PointerButtons::SECONDARY) && !b.contains(PointerButtons::PRIMARY) {
        1
    } else if b.contains(PointerButtons::MIDDLE) && !b.contains(PointerButtons::PRIMARY) {
        2
    } else {
        0
    }
}

/// The `PointerKind` of the pointer under dispatch.
fn pointer_kind(id: PointerId) -> Value {
    Value::Int(if id == PointerId::MOUSE { 0 } else { 1 })
}

/// A `KeyEvent`.
fn key_event(event: &KeyEvent) -> Value {
    record([
        key(event.key, event.modifiers.shift),
        Value::bool(event.repeat),
        modifiers(event.modifiers),
    ])
}

/// The prelude `Key` of a physical key: a letter, digit or US-layout symbol is
/// `char`, upper-case under shift for a letter.
fn key(key: Key, shift: bool) -> Value {
    const CHAR: u32 = 0;
    const FUNCTION: u32 = 15;
    const UNIDENTIFIED: i64 = 16;
    let unit = |tag: i64| Value::Int(tag);
    let char = |c: char| variant(CHAR, Value::Int(i64::from(u32::from(c))));
    let function = |n: i64| variant(FUNCTION, Value::Int(n));
    match key {
        Key::Enter => unit(1),
        Key::Escape => unit(2),
        Key::Tab => unit(3),
        Key::Backspace => unit(4),
        Key::Delete => unit(5),
        Key::Space => unit(6),
        Key::Up => unit(7),
        Key::Down => unit(8),
        Key::Left => unit(9),
        Key::Right => unit(10),
        Key::Home => unit(11),
        Key::End => unit(12),
        Key::PageUp => unit(13),
        Key::PageDown => unit(14),
        _ => {
            if let Some(c) = letter(key) {
                char(if shift { c.to_ascii_uppercase() } else { c })
            } else if let Some(c) = symbol(key) {
                char(c)
            } else if let Some(n) = function_number(key) {
                function(n)
            } else {
                unit(UNIDENTIFIED)
            }
        }
    }
}

/// The lower-case letter a letter key names.
fn letter(key: Key) -> Option<char> {
    const LETTERS: [Key; 26] = [
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::E,
        Key::F,
        Key::G,
        Key::H,
        Key::I,
        Key::J,
        Key::K,
        Key::L,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::Q,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
    ];
    let index = LETTERS.iter().position(|&k| k == key)?;
    char::from_u32(u32::from(b'a') + index as u32)
}

/// The character a digit or US-layout symbol key produces unshifted.
fn symbol(key: Key) -> Option<char> {
    Some(match key {
        Key::Digit0 | Key::Numpad0 => '0',
        Key::Digit1 | Key::Numpad1 => '1',
        Key::Digit2 | Key::Numpad2 => '2',
        Key::Digit3 | Key::Numpad3 => '3',
        Key::Digit4 | Key::Numpad4 => '4',
        Key::Digit5 | Key::Numpad5 => '5',
        Key::Digit6 | Key::Numpad6 => '6',
        Key::Digit7 | Key::Numpad7 => '7',
        Key::Digit8 | Key::Numpad8 => '8',
        Key::Digit9 | Key::Numpad9 => '9',
        Key::Backquote => '`',
        Key::Minus | Key::NumpadSubtract => '-',
        Key::Equal | Key::NumpadEqual => '=',
        Key::BracketLeft => '[',
        Key::BracketRight => ']',
        Key::Backslash => '\\',
        Key::Semicolon => ';',
        Key::Quote => '\'',
        Key::Comma | Key::NumpadComma => ',',
        Key::Period | Key::NumpadDecimal => '.',
        Key::Slash | Key::NumpadDivide => '/',
        Key::NumpadAdd => '+',
        Key::NumpadMultiply => '*',
        _ => return None,
    })
}

/// The number of a function key.
fn function_number(key: Key) -> Option<i64> {
    const FUNCTION_KEYS: [Key; 24] = [
        Key::F1,
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
        Key::F10,
        Key::F11,
        Key::F12,
        Key::F13,
        Key::F14,
        Key::F15,
        Key::F16,
        Key::F17,
        Key::F18,
        Key::F19,
        Key::F20,
        Key::F21,
        Key::F22,
        Key::F23,
        Key::F24,
    ];
    let index = FUNCTION_KEYS.iter().position(|&k| k == key)?;
    Some(index as i64 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_route_round_trips_through_its_name_and_discriminant() {
        for route in EventRoute::ALL {
            let named = (!route.is_control()).then_some(route);
            assert_eq!(EventRoute::of(route.event()), named);
            assert_eq!(EventRoute::from_u8(route as u8), Some(route));
        }
        assert_eq!(EventRoute::of("drag_start"), None);
        assert_eq!(EventRoute::of("changed"), None);
        assert_eq!(EventRoute::from_u8(12), None);
    }

    #[test]
    fn keys_map_to_the_prelude_variants() {
        assert_eq!(key(Key::Enter, false), Value::Int(1));
        assert_eq!(key(Key::A, false), variant(0, Value::Int('a' as i64)));
        assert_eq!(key(Key::A, true), variant(0, Value::Int('A' as i64)));
        assert_eq!(key(Key::Digit7, true), variant(0, Value::Int('7' as i64)));
        assert_eq!(key(Key::F12, false), variant(15, Value::Int(12)));
        assert_eq!(key(Key::CapsLock, false), Value::Int(16));
    }
}
