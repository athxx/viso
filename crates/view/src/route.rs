//! Which node input sample delivers which DSL event, and its payload value.
//!
//! Payload records are built in the VM's representation of the prelude types:
//! a record is an aggregate of its fields in declaration order, `Bool` and a unit
//! enum variant are integers, `Dp` is a float, `Some(x)` is `x`. The field and
//! variant orders below are the prelude's; the compiler checks them against it.

use std::rc::Rc;

use viso_behavior::{Aggregate, Value};
use viso_ui::{
    DragPhase, EventCx, Interaction, Key, KeyEvent, LookValue, Modifiers, NodeId, NodeStore,
    Notice, PinchPhase, PointerButtons, PointerId, PointerPhase, SwipeDirection, Vec2,
};

use crate::layout_values::length;

pub use viso_schema::view::route::{EventRoute, PAYLOAD_ENUMS, PAYLOAD_RECORDS};

/// The payload this dispatch delivers for a standard route, `None` when the
/// sample under dispatch is not one the route fires on. A pointer
/// position is local to the dispatching node.
pub fn payload(route: EventRoute, cx: &EventCx<'_>) -> Option<Value> {
    if route.is_control() {
        return None;
    }
    if route == EventRoute::Link {
        return cx.link().map(|url| record([Value::str(url)]));
    }
    if route == EventRoute::Refresh {
        return cx.refresh().then_some(Value::Nil);
    }
    if route == EventRoute::SnapChanged {
        return cx.snapped().map(|index| record([Value::Int(index as i64)]));
    }
    if route.is_key() {
        let key = cx.key()?;
        return (key.pressed == (route == EventRoute::KeyDown)).then(|| key_event(key));
    }
    if route.is_drag() {
        return drag_event(route, cx);
    }
    if route.is_pinch() {
        return pinch_event(route, cx);
    }
    if route == EventRoute::LongPress {
        let held = cx.long_press()?;
        let at = cx.local(held.at.x, held.at.y);
        return Some(record([point(at.x, at.y), pointer_kind(cx.pointer_id())]));
    }
    let pointer = cx.pointer()?;
    let phase = match route {
        EventRoute::Click | EventRoute::Tap | EventRoute::DoubleTap | EventRoute::PointerUp => {
            PointerPhase::Up
        }
        EventRoute::PointerDown => PointerPhase::Down,
        EventRoute::PointerMove => PointerPhase::Move,
        EventRoute::HoverEnter => PointerPhase::Enter,
        EventRoute::HoverLeave => PointerPhase::Leave,
        _ => return None,
    };
    if pointer.phase != phase {
        return None;
    }
    // An activation is a release on the node the press began in, with the
    // pointer still inside it and no gesture having taken the contact.
    if matches!(
        route,
        EventRoute::Click | EventRoute::Tap | EventRoute::DoubleTap
    ) && !cx.in_state(Interaction::Pressed)
    {
        return None;
    }
    // A double tap is the release of a run's second press.
    if route == EventRoute::DoubleTap && cx.clicks() != 2 {
        return None;
    }
    let at = cx.local(pointer.x, pointer.y);
    let position = point(at.x, at.y);
    let kind = pointer_kind(cx.pointer_id());
    Some(match route {
        EventRoute::Click => record([position, modifiers(pointer.modifiers)]),
        EventRoute::Tap
        | EventRoute::DoubleTap
        | EventRoute::HoverEnter
        | EventRoute::HoverLeave => record([position, kind]),
        _ => record([
            position,
            Value::Int(button(pointer.buttons)),
            buttons(pointer.buttons),
            kind,
            modifiers(pointer.modifiers),
        ]),
    })
}

/// The `ClickEvent` a keyboard activation of the dispatching node delivers:
/// at the node's middle, with the keys' modifiers.
pub(crate) fn key_click(cx: &EventCx<'_>, held: Modifiers) -> Value {
    let size = cx.size().unwrap_or_default();
    record([point(size.x * 0.5, size.y * 0.5), modifiers(held)])
}

/// The `DragEvent` of `route` when the sample under dispatch is that sample of
/// a drag, or the `SwipeEvent` of the end that swipes. Its position is local
/// to the dispatching node, its travel and velocity in window axes.
fn drag_event(route: EventRoute, cx: &EventCx<'_>) -> Option<Value> {
    let drag = cx.drag()?;
    let phase = match route {
        EventRoute::DragStart => DragPhase::Start,
        EventRoute::DragMove => DragPhase::Move,
        _ => DragPhase::End,
    };
    if drag.phase != phase {
        return None;
    }
    let at = cx.local(drag.at.x, drag.at.y);
    let offset = |v: Vec2| record([mixed(v.x), mixed(v.y)]);
    if route == EventRoute::Swipe {
        let direction = match drag.swipe()? {
            SwipeDirection::Left => 0,
            SwipeDirection::Right => 1,
            SwipeDirection::Up => 2,
            SwipeDirection::Down => 3,
        };
        return Some(record([
            Value::Int(direction),
            point(at.x, at.y),
            offset(drag.velocity),
        ]));
    }
    Some(record([
        point(at.x, at.y),
        offset(drag.delta),
        offset(drag.total),
        offset(drag.velocity),
    ]))
}

/// The `PinchEvent` of `route` when the dispatch carries that sample of a
/// pinch. Its position is local to the dispatching node, its pan in window
/// axes.
fn pinch_event(route: EventRoute, cx: &EventCx<'_>) -> Option<Value> {
    let pinch = cx.pinch()?;
    let phase = match route {
        EventRoute::PinchStart => PinchPhase::Start,
        EventRoute::PinchMove => PinchPhase::Move,
        _ => PinchPhase::End,
    };
    if pinch.phase != phase {
        return None;
    }
    let at = cx.local(pinch.focal.x, pinch.focal.y);
    Some(record([
        point(at.x, at.y),
        Value::Float(f64::from(pinch.scale)),
        Value::Float(f64::from(pinch.rotation.to_degrees())),
        Value::Float(f64::from(pinch.scale_delta)),
        Value::Float(f64::from(pinch.rotation_delta.to_degrees())),
        record([mixed(pinch.pan.x), mixed(pinch.pan.y)]),
    ]))
}

/// The route a notice runs, and its payload.
pub fn notice_payload(notice: &Notice) -> (EventRoute, Value) {
    match *notice {
        Notice::Scroll {
            offset,
            viewport,
            content,
        } => (
            EventRoute::ScrollChanged,
            record([
                record([mixed(offset.x), mixed(offset.y)]),
                size(viewport),
                size(content),
            ]),
        ),
        Notice::VisibleRange { first, last, count } => (
            EventRoute::VisibleRangeChanged,
            record([first, last, count].map(|n| Value::Int(n as i64))),
        ),
        Notice::AnimationEnd { target, finished } => (
            EventRoute::AnimationEnd,
            record([animate(target), Value::Int(i64::from(finished))]),
        ),
    }
}

/// The `Animate` an animation moved toward `target`.
fn animate(target: LookValue) -> Value {
    let float = |v: f32| Value::Float(f64::from(v));
    match target {
        LookValue::Translate(v) => variant(0, record([mixed(v.x), mixed(v.y)])),
        LookValue::Scale(v) => variant(1, float(v)),
        LookValue::Rotation(v) => variant(2, float(v.to_degrees())),
        LookValue::Opacity(v) => variant(3, float(v)),
        _ => Value::Nil,
    }
}

/// The value an `Animate` names, folded at `node`.
pub(crate) fn look_of(store: &NodeStore, node: NodeId, target: &Value) -> Option<LookValue> {
    let Value::Agg(agg) = target else {
        return None;
    };
    let float = |v: &Value| v.as_float().map(|v| v as f32);
    Some(match (agg.tag, agg.fields.first()?) {
        (0, Value::Agg(offset)) => {
            let [x, y] = &*offset.fields else {
                return None;
            };
            LookValue::Translate(store.fold_offset(node, [length(x)?, length(y)?]))
        }
        (1, v) => LookValue::Scale(float(v)?),
        (2, v) => LookValue::Rotation(float(v)?.to_radians()),
        (3, v) => LookValue::Opacity(float(v)?),
        _ => return None,
    })
}

/// A `MixedLength` of `dp` alone: its `dp`, `px`, `sp`, `em` and `pct` terms.
fn mixed(dp: f32) -> Value {
    record([f64::from(dp), 0.0, 0.0, 0.0, 0.0].map(Value::Float))
}

/// A `SizeDp`.
fn size(v: Vec2) -> Value {
    record([Value::Float(f64::from(v.x)), Value::Float(f64::from(v.y))])
}

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
        assert_eq!(EventRoute::of("double_click"), None);
        assert_eq!(EventRoute::of("changed"), None);
        assert_eq!(EventRoute::from_u8(EventRoute::ALL.len() as u8), None);
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
