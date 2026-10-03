//! A native node whose displayed value the view drives: the built-in response of
//! a control — a toggle flipping, a slider following the pointer, a tab strip
//! selecting a child, a text field editing — with the change event it reports,
//! and a label showing its text.
//!
//! A node reads its current value and range from pure entries of the view's
//! handler table, evaluated against the states when a sample arrives, so the
//! change a control reports is always relative to what the view holds.

use std::rc::Rc;

use viso_behavior::{Aggregate, Value};
use viso_ende::{Decode, DecodeError, Decoder, Encode, Encoder};
use viso_ui::{
    DispatchPhase, EditIntent, EventCx, ImeEvent, Key, KeyEvent, Motion, PointerButtons,
    PointerPhase, TextOffset,
};

use crate::host::ViewHost;
use crate::route::EventRoute;
use crate::scope::Scope;

/// Which view-driven native node a node is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ControlKind {
    /// `Toggle` and `CheckBox`: a release or Space/Enter flips `checked`.
    Toggle = 0,
    /// `Slider`: a press or a drag sets `value` from the pointer's position
    /// across the node; arrows, Page Up/Down and Home/End step it.
    Slider = 1,
    /// `Tabs` and `RadioGroup`: a release selects the child it lands in;
    /// arrows and Home/End move the selection.
    Select = 2,
    /// `TextInput`: pointer, key and IME samples edit its text; every settled
    /// edit reports the new text, and Enter submits. Its `value` seeds the text.
    TextInput = 3,
    /// `Text` and `Button`: shows the text of its `text`; it has no built-in
    /// response.
    Label = 4,
    /// Any other native node: it shows only its [`Look`] and has no built-in
    /// response.
    Plain = 5,
}

impl ControlKind {
    /// Every kind, by discriminant.
    pub const ALL: [ControlKind; 6] = [
        ControlKind::Toggle,
        ControlKind::Slider,
        ControlKind::Select,
        ControlKind::TextInput,
        ControlKind::Label,
        ControlKind::Plain,
    ];

    /// The kind native widget `widget` is.
    pub fn of(widget: &str) -> ControlKind {
        match widget {
            "Toggle" | "CheckBox" => ControlKind::Toggle,
            "Slider" => ControlKind::Slider,
            "Tabs" | "RadioGroup" => ControlKind::Select,
            "TextInput" => ControlKind::TextInput,
            "Text" | "Button" => ControlKind::Label,
            _ => ControlKind::Plain,
        }
    }

    /// The route of the control's event `event`, `None` for one it does not
    /// report.
    pub fn route(self, event: &str) -> Option<EventRoute> {
        let route = match event {
            "changed" => EventRoute::Changed,
            "submitted" => EventRoute::Submitted,
            "selected_changed" => EventRoute::SelectedChanged,
            _ => return None,
        };
        self.reports(route).then_some(route)
    }

    /// Whether the node responds to pointer and key samples itself.
    pub fn responds(self) -> bool {
        !matches!(self, ControlKind::Label | ControlKind::Plain)
    }

    /// Whether the control reports `route`.
    pub fn reports(self, route: EventRoute) -> bool {
        matches!(
            (self, route),
            (
                ControlKind::Toggle | ControlKind::Slider | ControlKind::TextInput,
                EventRoute::Changed
            ) | (ControlKind::TextInput, EventRoute::Submitted)
                | (ControlKind::Select, EventRoute::SelectedChanged)
        )
    }

    /// Which of [`Control`]'s entries the widget property `property` fills; a
    /// group member is named by its dotted path (`transition.opacity`).
    pub fn input(self, property: &str) -> Option<ControlInput> {
        Some(match (self, property) {
            (_, "background") => ControlInput::Background,
            (_, "opacity") => ControlInput::Opacity,
            (_, "transition.background") => ControlInput::BackgroundTransition,
            (_, "transition.opacity") => ControlInput::OpacityTransition,
            (ControlKind::Toggle, "checked")
            | (ControlKind::Slider, "value")
            | (ControlKind::Select, "selected")
            | (ControlKind::TextInput, "value")
            | (ControlKind::Label, "text") => ControlInput::Value,
            (ControlKind::Slider, "min") => ControlInput::Min,
            (ControlKind::Slider, "max") => ControlInput::Max,
            (ControlKind::Slider, "step") => ControlInput::Step,
            _ => return None,
        })
    }

    /// The Rust variant name, for code that names a kind in tokens.
    pub fn variant(self) -> &'static str {
        match self {
            ControlKind::Toggle => "Toggle",
            ControlKind::Slider => "Slider",
            ControlKind::Select => "Select",
            ControlKind::TextInput => "TextInput",
            ControlKind::Label => "Label",
            ControlKind::Plain => "Plain",
        }
    }

    /// The kind with discriminant `raw`.
    pub fn from_u8(raw: u8) -> Option<ControlKind> {
        ControlKind::ALL.get(usize::from(raw)).copied()
    }
}

/// A value a control reads from the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlInput {
    /// Its current value: `checked`, `value`, `selected` or a label's `text`.
    Value,
    /// A slider's lower bound.
    Min,
    /// A slider's upper bound.
    Max,
    /// A slider's step.
    Step,
    /// The node's `background`.
    Background,
    /// The node's `opacity`.
    Opacity,
    /// The node's `transition.background`.
    BackgroundTransition,
    /// The node's `transition.opacity`.
    OpacityTransition,
}

/// A view-driven native node: its kind and the handler-table entries that
/// evaluate its current value and range. An absent entry reads the property's
/// default: `false`, `0`, an empty text, a `0..1` range and no step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Control {
    /// Which control it is.
    pub kind: ControlKind,
    /// The entry of its current value.
    pub value: Option<u32>,
    /// The entry of a slider's lower bound.
    pub min: Option<u32>,
    /// The entry of a slider's upper bound.
    pub max: Option<u32>,
    /// The entry of a slider's step.
    pub step: Option<u32>,
    /// The entries of the look it shows.
    pub look: Look,
}

/// The handler-table entries of the look a node shows: an absent entry leaves
/// the node as it was built (no fill, opaque), and a value with no transition
/// shows at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Look {
    /// Its `background`: a color, or `None` for no fill.
    pub background: Option<u32>,
    /// Its `opacity`.
    pub opacity: Option<u32>,
    /// Its `transition.background`: how a new background moves in.
    pub background_transition: Option<u32>,
    /// Its `transition.opacity`: how a new opacity moves in.
    pub opacity_transition: Option<u32>,
}

/// The fraction of a slider's range one arrow key moves a stepless slider.
const KEY_FRACTION: f32 = 0.01;

/// How many steps Page Up / Page Down move a slider.
const PAGE_STEPS: f32 = 10.0;

impl Control {
    /// A control of `kind` reading only defaults.
    pub fn new(kind: ControlKind) -> Control {
        Control {
            kind,
            value: None,
            min: None,
            max: None,
            step: None,
            look: Look::default(),
        }
    }

    /// The entry `input` reads.
    pub fn entry(&self, input: ControlInput) -> Option<u32> {
        match input {
            ControlInput::Value => self.value,
            ControlInput::Min => self.min,
            ControlInput::Max => self.max,
            ControlInput::Step => self.step,
            ControlInput::Background => self.look.background,
            ControlInput::Opacity => self.look.opacity,
            ControlInput::BackgroundTransition => self.look.background_transition,
            ControlInput::OpacityTransition => self.look.opacity_transition,
        }
    }

    /// Points `input` at entry `entry`.
    pub fn set_entry(&mut self, input: ControlInput, entry: u32) {
        let slot = match input {
            ControlInput::Value => &mut self.value,
            ControlInput::Min => &mut self.min,
            ControlInput::Max => &mut self.max,
            ControlInput::Step => &mut self.step,
            ControlInput::Background => &mut self.look.background,
            ControlInput::Opacity => &mut self.look.opacity,
            ControlInput::BackgroundTransition => &mut self.look.background_transition,
            ControlInput::OpacityTransition => &mut self.look.opacity_transition,
        };
        *slot = Some(entry);
    }

    /// Runs the control's response to the sample under dispatch and returns the
    /// change it reports, if any. A control responds on the target and bubble
    /// legs of the walk: its own samples, and a tab strip's from its children.
    pub(crate) fn drive(
        &self,
        host: &mut ViewHost,
        scope: &Scope,
        cx: &mut EventCx<'_>,
    ) -> Option<(EventRoute, Value)> {
        if cx.phase() == DispatchPhase::Capture {
            return None;
        }
        match self.kind {
            ControlKind::Toggle => self.toggle(host, scope, cx),
            ControlKind::Slider => self.slider(host, scope, cx),
            ControlKind::Select => self.select(host, scope, cx),
            ControlKind::TextInput => text_input(cx),
            ControlKind::Label | ControlKind::Plain => None,
        }
    }

    fn toggle(
        &self,
        host: &mut ViewHost,
        scope: &Scope,
        cx: &mut EventCx<'_>,
    ) -> Option<(EventRoute, Value)> {
        let flips = match (cx.pointer(), cx.key()) {
            (Some(p), _) => p.phase == PointerPhase::Up,
            (_, Some(k)) => activates(k),
            _ => false,
        };
        if !flips {
            return None;
        }
        let checked = read(host, self.value, scope, cx).as_int().unwrap_or(0) != 0;
        Some((EventRoute::Changed, field(Value::bool(!checked))))
    }

    fn slider(
        &self,
        host: &mut ViewHost,
        scope: &Scope,
        cx: &mut EventCx<'_>,
    ) -> Option<(EventRoute, Value)> {
        if cx.phase() != DispatchPhase::Target {
            return None;
        }
        let node = cx.node()?;
        let pointer = cx.pointer().map(|p| (p.phase, p.x));
        if let Some((phase, _)) = pointer {
            let held = cx
                .pointer()
                .is_some_and(|p| p.buttons.contains(PointerButtons::PRIMARY));
            match phase {
                PointerPhase::Down if held => {
                    cx.request_focus(node);
                    cx.capture_pointer(node);
                }
                PointerPhase::Move if held => {}
                PointerPhase::Up => {
                    cx.release_pointer();
                    return None;
                }
                _ => return None,
            }
        } else if !cx.key().is_some_and(|k| k.pressed) {
            return None;
        }
        let range = Range {
            min: float(read(host, self.min, scope, cx), 0.0),
            max: float(read(host, self.max, scope, cx), 1.0),
            step: read(host, self.step, scope, cx)
                .as_float()
                .map(|s| s as f32)
                .filter(|&s| s > 0.0),
        };
        let current = float(read(host, self.value, scope, cx), range.min);
        let next = if let Some((_, x)) = pointer {
            let rect = cx.rect()?;
            let t = if rect.w > 0.0 {
                ((x - rect.x) / rect.w).clamp(0.0, 1.0)
            } else {
                0.0
            };
            range.snap(range.min + t * (range.max - range.min))
        } else {
            let k = cx.key()?;
            let unit = range.step.unwrap_or((range.max - range.min) * KEY_FRACTION);
            match k.key {
                Key::Right | Key::Up => range.snap(current + unit),
                Key::Left | Key::Down => range.snap(current - unit),
                Key::PageUp => range.snap(current + unit * PAGE_STEPS),
                Key::PageDown => range.snap(current - unit * PAGE_STEPS),
                Key::Home => range.min,
                Key::End => range.max,
                _ => return None,
            }
        };
        (next != current).then(|| (EventRoute::Changed, field(Value::Float(f64::from(next)))))
    }

    fn select(
        &self,
        host: &mut ViewHost,
        scope: &Scope,
        cx: &mut EventCx<'_>,
    ) -> Option<(EventRoute, Value)> {
        if let Some(p) = cx.pointer() {
            if p.phase != PointerPhase::Up {
                return None;
            }
        } else if !cx.key().is_some_and(|k| k.pressed) || cx.phase() != DispatchPhase::Target {
            return None;
        }
        let count = cx.child_count();
        if count == 0 {
            return None;
        }
        let current = read(host, self.value, scope, cx).as_int().unwrap_or(0);
        let next = if cx.pointer().is_some() {
            i64::from(cx.child_index()?)
        } else {
            let k = cx.key()?;
            let last = i64::from(count - 1);
            match k.key {
                Key::Right | Key::Down => (current + 1).min(last),
                Key::Left | Key::Up => (current - 1).max(0),
                Key::Home => 0,
                Key::End => last,
                _ => return None,
            }
        };
        (next != current).then(|| (EventRoute::SelectedChanged, field(Value::Int(next))))
    }
}

impl Encode for Control {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_u8(self.kind as u8);
        let look = self.look;
        for entry in [
            self.value,
            self.min,
            self.max,
            self.step,
            look.background,
            look.opacity,
            look.background_transition,
            look.opacity_transition,
        ] {
            enc.write_varint(entry.map_or(0, |e| u64::from(e) + 1));
        }
    }
}

impl Decode for Control {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let offset = dec.position();
        let kind = ControlKind::from_u8(dec.read_u8()?).ok_or(DecodeError::Malformed { offset })?;
        let mut entry = || -> Result<Option<u32>, DecodeError> {
            let offset = dec.position();
            let raw =
                u32::try_from(dec.read_varint()?).map_err(|_| DecodeError::Malformed { offset })?;
            Ok(raw.checked_sub(1))
        };
        Ok(Control {
            kind,
            value: entry()?,
            min: entry()?,
            max: entry()?,
            step: entry()?,
            look: Look {
                background: entry()?,
                opacity: entry()?,
                background_transition: entry()?,
                opacity_transition: entry()?,
            },
        })
    }
}

/// A text field's response: pointer placement and drag selection, caret
/// motion and deletion keys, IME composition and commit — recorded as edit
/// intents the router applies — and the settled text or an Enter it reports.
fn text_input(cx: &mut EventCx<'_>) -> Option<(EventRoute, Value)> {
    if cx.phase() != DispatchPhase::Target {
        return None;
    }
    let node = cx.node()?;
    if let Some(text) = cx.text_change() {
        return Some((
            EventRoute::Changed,
            field(Value::Str(Rc::new(text.to_owned()))),
        ));
    }
    if let Some(p) = cx.pointer() {
        let held = p.buttons.contains(PointerButtons::PRIMARY);
        let (x, y, extend) = (p.x, p.y, p.modifiers.shift);
        match p.phase {
            PointerPhase::Down if held => {
                cx.request_focus(node);
                cx.capture_pointer(node);
                cx.record_edit(EditIntent::PlaceAt { x, y, extend });
            }
            PointerPhase::Move if held => {
                cx.record_edit(EditIntent::PlaceAt { x, y, extend: true });
            }
            PointerPhase::Up => cx.release_pointer(),
            _ => {}
        }
        return None;
    }
    if let Some(k) = cx.key() {
        if !k.pressed {
            return None;
        }
        let extend = k.modifiers.shift;
        let motion = |motion| EditIntent::Move { motion, extend };
        let intent = match k.key {
            Key::Enter if !k.repeat => return Some((EventRoute::Submitted, Value::Nil)),
            Key::Left => motion(Motion::Left),
            Key::Right => motion(Motion::Right),
            Key::Home => motion(Motion::Home),
            Key::End => motion(Motion::End),
            Key::Backspace => EditIntent::Backspace,
            Key::Delete => EditIntent::Delete,
            _ => return None,
        };
        cx.record_edit(intent);
        return None;
    }
    let intent = match cx.ime()? {
        ImeEvent::Preedit { text, caret } => EditIntent::Compose {
            text: text.clone(),
            caret: TextOffset(*caret),
        },
        ImeEvent::Commit { text } => EditIntent::CommitCompose(text.clone()),
    };
    cx.record_edit(intent);
    None
}

/// A slider's range and step.
struct Range {
    min: f32,
    max: f32,
    step: Option<f32>,
}

impl Range {
    /// `value` on the nearest step from `min`, within the range.
    fn snap(&self, value: f32) -> f32 {
        let (low, high) = if self.min <= self.max {
            (self.min, self.max)
        } else {
            (self.max, self.min)
        };
        let stepped = match self.step {
            Some(step) => self.min + ((value - self.min) / step).round() * step,
            None => value,
        };
        stepped.clamp(low, high)
    }
}

/// Whether a key sample activates a toggle: a fresh Space or Enter press.
fn activates(k: &KeyEvent) -> bool {
    k.pressed && !k.repeat && matches!(k.key, Key::Space | Key::Enter)
}

/// The value of entry `entry`, `Nil` for an absent entry or one that faults.
fn read(host: &mut ViewHost, entry: Option<u32>, scope: &Scope, cx: &EventCx<'_>) -> Value {
    entry
        .and_then(|entry| host.evaluate(entry, scope, None, cx).ok())
        .unwrap_or(Value::Nil)
}

/// An `F32` value as `f32`, `default` for any other.
fn float(value: Value, default: f32) -> f32 {
    value.as_float().map_or(default, |v| v as f32)
}

/// A widget change payload: a record of the one field `value`.
fn field(value: Value) -> Value {
    Value::Agg(Rc::new(Aggregate {
        tag: 0,
        fields: Box::new([value]),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slider_snaps_to_its_step_within_its_range() {
        let range = Range {
            min: 0.0,
            max: 10.0,
            step: Some(2.5),
        };
        assert_eq!(range.snap(3.6), 2.5);
        assert_eq!(range.snap(3.8), 5.0);
        assert_eq!(range.snap(12.0), 10.0);
        assert_eq!(range.snap(-1.0), 0.0);
    }

    #[test]
    fn every_kind_round_trips_through_its_discriminant() {
        for kind in ControlKind::ALL {
            assert_eq!(ControlKind::from_u8(kind as u8), Some(kind));
        }
        assert_eq!(ControlKind::from_u8(6), None);
        assert_eq!(ControlKind::of("CheckBox"), ControlKind::Toggle);
        assert_eq!(ControlKind::of("Button"), ControlKind::Label);
        assert_eq!(ControlKind::of("Column"), ControlKind::Plain);
        assert!(!ControlKind::Label.responds());
        assert!(!ControlKind::Plain.responds());
        assert_eq!(
            ControlKind::Plain.input("opacity"),
            Some(ControlInput::Opacity)
        );
        assert_eq!(ControlKind::Plain.input("text"), None);
        assert_eq!(
            ControlKind::Label.input("transition.opacity"),
            Some(ControlInput::OpacityTransition)
        );
        assert_eq!(ControlKind::Label.input("transition.scale"), None);
        assert_eq!(
            ControlKind::TextInput.input("value"),
            Some(ControlInput::Value)
        );
        assert_eq!(
            ControlKind::Select.route("selected_changed"),
            Some(EventRoute::SelectedChanged)
        );
        assert_eq!(ControlKind::Select.route("changed"), None);
    }

    #[test]
    fn a_control_round_trips_and_rejects_a_bad_kind() {
        let control = Control {
            kind: ControlKind::Slider,
            value: Some(0),
            min: None,
            max: Some(7),
            step: None,
            look: Look {
                background: Some(3),
                opacity: None,
                background_transition: None,
                opacity_transition: Some(4),
            },
        };
        let bytes = control.encode_to_vec();
        assert_eq!(Control::decode_from_slice(&bytes), Ok(control));
        let mut bad = bytes;
        bad[0] = 9;
        assert!(Control::decode_from_slice(&bad).is_err());
    }
}
