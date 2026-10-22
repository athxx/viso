//! A native node whose displayed value the view drives: the built-in response of
//! a control — a toggle flipping, a slider following the pointer, a tab strip
//! selecting a child, a text field editing — with the change event it reports,
//! and a label showing its text.
//!
//! A node reads its current value and range from pure entries of the view's
//! handler table, evaluated against the states when a sample arrives, so the
//! change a control reports is always relative to what the view holds.
//!
//! A node's role and accessible name are its semantics: the role its
//! `semantics.role` names, else its widget's; the name its `semantics.label`
//! gives, else the text it shows.

use std::rc::Rc;

use viso_behavior::{Aggregate, Fault, FaultKind, Value};

use viso_ui::{
    DispatchPhase, EditIntent, EventCx, ImeEvent, Key, KeyEvent, Motion, NodeId, NodeStore,
    PointerButtons, PointerPhase, TextOffset,
};

use crate::host::ViewHost;
use crate::route::EventRoute;
use crate::scope::Scope;

pub use viso_schema::view::control::{
    Control, ControlInput, ControlKind, LayoutSlot, Look, LookArm, NodeEntries, SOURCE_ROLES,
    SemanticEntries, Switched, TextEntries, When, source_live, source_name, source_role,
    widget_role,
};

/// The deepest condition a [`LookArm`] evaluates.
const WHEN_DEPTH: u32 = 64;

/// A control's response at run time; what it is, is schema data.
pub(crate) trait Drive {
    /// Runs the control's response to the sample under dispatch and returns
    /// the change it reports, if any.
    fn drive(
        &self,
        host: &mut ViewHost,
        scope: &Scope,
        cx: &mut EventCx<'_>,
    ) -> Option<(EventRoute, Value)>;
}

impl Drive for Control {
    /// Runs the control's response to the sample under dispatch and returns the
    /// change it reports, if any. A control responds on the target and bubble
    /// legs of the walk: its own samples, and a tab strip's from its children.
    fn drive(
        &self,
        host: &mut ViewHost,
        scope: &Scope,
        cx: &mut EventCx<'_>,
    ) -> Option<(EventRoute, Value)> {
        if cx.phase() == DispatchPhase::Capture {
            return None;
        }
        match self.kind {
            ControlKind::Check | ControlKind::Switch => toggle(self, host, scope, cx),
            ControlKind::Disclosure => disclose(self, host, scope, cx),
            ControlKind::Dialog => dismiss(self, host, scope, cx),
            ControlKind::Slider => slider(self, host, scope, cx),
            ControlKind::Select => select(self, host, scope, cx),
            ControlKind::TextInput => text_input(cx),
            ControlKind::Viewport => viewport(cx),
            ControlKind::Label
            | ControlKind::Plain
            | ControlKind::Asset
            | ControlKind::Shader
            | ControlKind::Toast => None,
        }
    }
}

/// A look arm's condition at run time.
pub(crate) trait Holds {
    /// Whether its condition holds for `node`: its entries evaluated in
    /// `scope`, its states read from `store`.
    fn holds(
        &self,
        node: NodeId,
        store: &NodeStore,
        host: &mut ViewHost,
        scope: &Scope,
        cells: &dyn crate::host::StateCells,
    ) -> Result<bool, Fault>;
}

impl Holds for LookArm {
    /// Whether its condition holds for `node`: its entries evaluated in
    /// `scope`, its states read from `store`.
    fn holds(
        &self,
        node: NodeId,
        store: &NodeStore,
        host: &mut ViewHost,
        scope: &Scope,
        cells: &dyn crate::host::StateCells,
    ) -> Result<bool, Fault> {
        // A stack of booleans, one bit each.
        let (mut stack, mut depth) = (0u64, 0u32);
        let malformed = || Fault {
            kind: FaultKind::Internal,
            at: None,
            message: "a style condition is malformed".to_owned(),
        };
        for step in self.when.iter() {
            let value = match *step {
                When::State(state) => store.in_state(node, state),
                When::Entry(entry) => {
                    let value = host.evaluate(entry, scope, None, cells)?;
                    value.as_int().is_some_and(|v| v != 0)
                }
                When::Not | When::And | When::Or => {
                    let arity = if *step == When::Not { 1 } else { 2 };
                    if depth < arity {
                        return Err(malformed());
                    }
                    let top = stack & 1 != 0;
                    let value = match step {
                        When::Not => !top,
                        When::And => top && (stack >> 1) & 1 != 0,
                        _ => top || (stack >> 1) & 1 != 0,
                    };
                    stack >>= arity;
                    depth -= arity;
                    value
                }
            };
            if depth == WHEN_DEPTH {
                return Err(malformed());
            }
            stack = (stack << 1) | u64::from(value);
            depth += 1;
        }
        if depth != 1 {
            return Err(malformed());
        }
        Ok(stack & 1 != 0)
    }
}

fn toggle(
    control: &Control,
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
    let checked = read(host, control.value, scope, cx).as_int().unwrap_or(0) != 0;
    Some((EventRoute::Changed, field(Value::bool(!checked))))
}

/// A game viewport's response: a press on it takes the keyboard focus, so
/// the keys reach the game.
fn viewport(cx: &mut EventCx<'_>) -> Option<(EventRoute, Value)> {
    if cx.phase() == DispatchPhase::Target
        && cx.pointer().is_some_and(|p| p.phase == PointerPhase::Down)
        && let Some(node) = cx.node()
    {
        cx.request_focus(node);
    }
    None
}

/// A disclosure's response: a release on its header, its first child, or a
/// fresh Space/Enter on it or its header flips `expanded`.
fn disclose(
    control: &Control,
    host: &mut ViewHost,
    scope: &Scope,
    cx: &mut EventCx<'_>,
) -> Option<(EventRoute, Value)> {
    let on_header = cx.phase() == DispatchPhase::Bubble && cx.child_index() == Some(0);
    let flips = match (cx.pointer(), cx.key()) {
        (Some(p), _) => p.phase == PointerPhase::Up && on_header,
        (_, Some(k)) => activates(k) && (on_header || cx.phase() == DispatchPhase::Target),
        _ => false,
    };
    if !flips {
        return None;
    }
    let expanded = read(host, control.value, scope, cx).as_int().unwrap_or(0) != 0;
    Some((EventRoute::Changed, field(Value::bool(!expanded))))
}

/// A dialog's response: it swallows the pointer on its scrim, its own box
/// outside its content, and a press there or a fresh Escape inside it asks
/// an open dialog to close.
fn dismiss(
    control: &Control,
    host: &mut ViewHost,
    scope: &Scope,
    cx: &mut EventCx<'_>,
) -> Option<(EventRoute, Value)> {
    let escape = cx
        .key()
        .is_some_and(|k| k.pressed && !k.repeat && k.key == Key::Escape);
    let closes = match cx.pointer().map(|p| p.phase) {
        Some(phase) if cx.phase() == DispatchPhase::Target => {
            cx.stop_propagation();
            // The rest of the press stays with the dialog, so its release
            // reaches nothing the dialog uncovers as it closes.
            if phase == PointerPhase::Down
                && let Some(node) = cx.node()
            {
                cx.capture_pointer(node);
            }
            phase == PointerPhase::Down
        }
        _ if escape => {
            cx.stop_propagation();
            true
        }
        _ => false,
    };
    if !closes || read(host, control.value, scope, cx).as_int().unwrap_or(0) == 0 {
        return None;
    }
    Some((EventRoute::Changed, field(Value::bool(false))))
}

fn slider(
    control: &Control,
    host: &mut ViewHost,
    scope: &Scope,
    cx: &mut EventCx<'_>,
) -> Option<(EventRoute, Value)> {
    if cx.phase() != DispatchPhase::Target {
        return None;
    }
    let node = cx.node()?;
    let pointer = cx.pointer().map(|p| (p.phase, p.x, p.y));
    if let Some((phase, ..)) = pointer {
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
        min: float(read(host, control.min, scope, cx), 0.0),
        max: float(read(host, control.max, scope, cx), 1.0),
        step: read(host, control.step, scope, cx)
            .as_float()
            .map(|s| s as f32)
            .filter(|&s| s > 0.0),
    };
    let current = float(read(host, control.value, scope, cx), range.min);
    let next = if let Some((_, x, y)) = pointer {
        let (size, at) = (cx.size()?, cx.local(x, y));
        let t = if size.x > 0.0 {
            (at.x / size.x).clamp(0.0, 1.0)
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
    control: &Control,
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
    let current = read(host, control.value, scope, cx).as_int().unwrap_or(0);
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

/// The fraction of a slider's range one arrow key moves a stepless slider.
const KEY_FRACTION: f32 = 0.01;

/// How many steps Page Up / Page Down move a slider.
const PAGE_STEPS: f32 = 10.0;

/// A text field's response: pointer placement and drag selection, caret
/// motion and deletion keys, IME composition and commit — recorded as edit
/// intents the router applies — and the settled text or an Enter it reports.
fn text_input(cx: &mut EventCx<'_>) -> Option<(EventRoute, Value)> {
    if cx.phase() != DispatchPhase::Target {
        return None;
    }
    let node = cx.node()?;
    if let Some(text) = cx.text_change() {
        return Some((EventRoute::Changed, field(Value::str(text))));
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
}
