//! A stepper: a pill split in two, a minus that steps a number down and a
//! plus that steps it up.
//!
//! A stepper is a node's content ([`Content::Stepper`]). It holds no number:
//! whoever drives it — the `Stepper` widget, a view's `Stepper` — keeps the
//! number and its bounds, and tells the stepper which halves can still step
//! ([`NodeStore::show_stepper`]), which it draws dimmed when they cannot. A
//! press on a half steps once and holds it down: after a pause the store
//! steps it again and again, faster the longer it is held, by waking the
//! node's key handler with the half's token ([`StepHalf::of_token`]) until
//! the press lifts, leaves the stepper or the half can step no further.
//! Read right to left, the minus is on the right.
//!
//! [`stepper_input`] reads what a sample asks of a stepper, and
//! [`StepInput::apply`] what that makes of a number, so every driver steps
//! the same way: a press or a wake by one step, the arrows by one, Page
//! Up/Down by ten, Home and End to the bounds.

use core::time::Duration;

use viso_render::{Border, LineCap, Path, PathCmd, Point, Primitive, Quad, Rect, Rgba, Stroke};

use crate::adaptive::LayoutDirection;
use crate::component::NodeStore;
use crate::content::Content;
use crate::context::EventCx;
use crate::dirty::DirtyClass;
use crate::input::{Key, PointerButtons, PointerPhase};
use crate::layout::Vec2;
use crate::node::NodeId;
use crate::timer::TimerRequest;

/// How long a press holds before its half steps on, how long between the
/// first steps on, and between the steps once it has been held a while.
const HOLD: Duration = Duration::from_millis(500);
const REPEAT: Duration = Duration::from_millis(120);
const REPEAT_FAST: Duration = Duration::from_millis(50);
/// How many steps on run at [`REPEAT`] before they quicken.
const QUICKEN: u32 = 8;

/// How many steps Page Up and Page Down take.
const PAGE: f64 = 10.0;

/// A stepper's size unless its node sizes it.
pub const STEPPER_SIZE: Vec2 = Vec2 { x: 96.0, y: 32.0 };

/// One half of a stepper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepHalf {
    /// The minus: steps down.
    Down,
    /// The plus: steps up.
    Up,
}

impl StepHalf {
    /// The wake token a held half steps on with.
    pub const fn token(self) -> u32 {
        match self {
            StepHalf::Down => 0x5354_0000,
            StepHalf::Up => 0x5354_0001,
        }
    }

    /// The half whose steps on wake with `token`.
    pub fn of_token(token: u32) -> Option<StepHalf> {
        [StepHalf::Down, StepHalf::Up]
            .into_iter()
            .find(|half| half.token() == token)
    }

    fn sign(self) -> f64 {
        match self {
            StepHalf::Down => -1.0,
            StepHalf::Up => 1.0,
        }
    }
}

/// What a stepper draws.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stepper {
    /// The minus, the plus and the line between them.
    pub ink: Rgba,
    /// The pill.
    pub fill: Rgba,
    /// Whether the minus can step.
    pub lower: bool,
    /// Whether the plus can step.
    pub upper: bool,
    /// The half a press holds down.
    pub held: Option<StepHalf>,
    /// Counts holds, so a step on of a hold that lifted is let go.
    serial: u32,
    /// Its size unless its node sizes it.
    pub natural: Vec2,
}

impl Stepper {
    /// A stepper inked `ink` on a `fill` pill, both halves stepping.
    pub fn new(ink: Rgba, fill: Rgba) -> Self {
        Stepper {
            ink,
            fill,
            lower: true,
            upper: true,
            held: None,
            serial: 0,
            natural: STEPPER_SIZE,
        }
    }

    /// The half a point `x` across a stepper `width` wide lands on.
    pub fn half_at(&self, x: f32, width: f32, rtl: bool) -> StepHalf {
        if (x < width * 0.5) != rtl {
            StepHalf::Down
        } else {
            StepHalf::Up
        }
    }

    /// Whether `half` can step.
    pub fn steps(&self, half: StepHalf) -> bool {
        match half {
            StepHalf::Down => self.lower,
            StepHalf::Up => self.upper,
        }
    }

    /// Draws it into `rect` (world space): the pill, a held half pressed in,
    /// the line between the halves, and the minus and the plus, dimmed when
    /// they cannot step; mirrored right to left.
    pub(crate) fn paint(&self, rect: Rect, rtl: bool, out: &mut Vec<Primitive>) {
        let h = rect.h;
        let half_w = rect.w * 0.5;
        let radius = (h * 0.25).min(half_w);
        out.push(Primitive::Quad(Quad {
            rect,
            color: self.fill,
            radius,
            border: Border::NONE,
        }));
        let left_of = |half: StepHalf| (half == StepHalf::Down) != rtl;
        let box_of = |half: StepHalf| Rect {
            x: if left_of(half) {
                rect.x
            } else {
                rect.x + half_w
            },
            w: half_w,
            ..rect
        };
        if let Some(held) = self.held.filter(|&half| self.steps(half)) {
            let inset = (h * 0.0625).max(1.0);
            let b = box_of(held);
            out.push(Primitive::Quad(Quad {
                rect: Rect::new(
                    b.x + inset,
                    b.y + inset,
                    b.w - inset * 2.0,
                    b.h - inset * 2.0,
                ),
                color: faded(self.ink, 0.14),
                radius: (radius - inset).max(0.0),
                border: Border::NONE,
            }));
        }
        let line = (h / 16.0).max(1.0);
        out.push(Primitive::Quad(Quad {
            rect: Rect::new(
                rect.x + half_w - line * 0.5,
                rect.y + h * 0.25,
                line,
                h * 0.5,
            ),
            color: faded(self.ink, 0.25),
            radius: 0.0,
            border: Border::NONE,
        }));
        let arm = h * 0.1875;
        for half in [StepHalf::Down, StepHalf::Up] {
            let b = box_of(half);
            let (cx, cy) = (b.x + b.w * 0.5, b.y + b.h * 0.5);
            let at = |x: f32, y: f32| Point {
                x: cx + x,
                y: cy + y,
            };
            let mut cmds = vec![
                PathCmd::MoveTo(at(-arm, 0.0)),
                PathCmd::LineTo(at(arm, 0.0)),
            ];
            if half == StepHalf::Up {
                cmds.extend([
                    PathCmd::MoveTo(at(0.0, -arm)),
                    PathCmd::LineTo(at(0.0, arm)),
                ]);
            }
            let ink = faded(self.ink, if self.steps(half) { 1.0 } else { 0.3 });
            let mut stroke = Stroke::new(line * 1.5, ink);
            stroke.cap = LineCap::Round;
            out.push(Primitive::Path(Box::new(Path {
                cmds,
                fill: None,
                stroke: Some(stroke),
                shadow: None,
            })));
        }
    }
}

/// `color` at `alpha` of its opacity.
fn faded(color: Rgba, alpha: f32) -> Rgba {
    Rgba {
        a: color.a * alpha,
        ..color
    }
}

/// What a sample asks of a stepper.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StepInput {
    /// Step `times` steps toward the half.
    Step(StepHalf, f64),
    /// Go to the least number.
    ToMin,
    /// Go to the greatest number.
    ToMax,
}

impl StepInput {
    /// What it makes of `value` stepping by `step` within `min..=max`: a
    /// step past a bound stops at it, or comes round to the other when
    /// `wrap` and both are given; a step lands on the grid of `step` from
    /// the least number (from zero with none). `None` when it changes
    /// nothing.
    pub fn apply(
        self,
        value: f64,
        step: f64,
        min: Option<f64>,
        max: Option<f64>,
        wrap: bool,
    ) -> Option<f64> {
        let (lo, hi) = (
            min.unwrap_or(f64::NEG_INFINITY),
            max.unwrap_or(f64::INFINITY),
        );
        let next = match self {
            StepInput::ToMin => lo,
            StepInput::ToMax => hi,
            StepInput::Step(half, times) => {
                let step = if step > 0.0 { step } else { 1.0 };
                let base = min.unwrap_or(0.0);
                let raw = value + half.sign() * step * times;
                let snapped = base + ((raw - base) / step).round() * step;
                match (wrap, min, max) {
                    (true, Some(lo), Some(hi)) if value >= hi && half == StepHalf::Up => lo,
                    (true, Some(lo), Some(hi)) if value <= lo && half == StepHalf::Down => hi,
                    _ => snapped,
                }
            }
        };
        let next = next.clamp(lo.min(hi), hi.max(lo));
        (next.is_finite() && next != value).then_some(next)
    }
}

/// Whether a number `value` within `min..=max` can step down and up: one at
/// a bound cannot step past it, unless it wraps round to the other.
pub fn can_step(value: f64, min: Option<f64>, max: Option<f64>, wrap: bool) -> (bool, bool) {
    if wrap && min.is_some() && max.is_some() {
        return (true, true);
    }
    (
        min.is_none_or(|lo| value > lo),
        max.is_none_or(|hi| value < hi),
    )
}

/// What the sample under dispatch asks of stepper `node`, holding a half
/// down while a press is on it: a press steps its half once and holds it,
/// a step on of the hold steps it again, the arrows step one (Left and Right
/// mirrored right to left), Page Up and Page Down ten, Home and End go to
/// the bounds. A key it takes goes no further.
pub fn stepper_input(ev: &mut EventCx<'_>, node: NodeId) -> Option<StepInput> {
    if let Some(p) = ev.pointer().copied() {
        let stepper = ev.stepper(node)?;
        let size = ev.size()?;
        let at = ev.local(p.x, p.y);
        match p.phase {
            PointerPhase::Down if p.buttons.contains(PointerButtons::PRIMARY) => {
                let half = stepper.half_at(at.x, size.x, ev.reads_rtl());
                if !stepper.steps(half) {
                    return None;
                }
                ev.hold_step(node, Some(half));
                return Some(StepInput::Step(half, 1.0));
            }
            PointerPhase::Move
                if stepper.held.is_some()
                    && (at.x < 0.0 || at.y < 0.0 || at.x > size.x || at.y > size.y) =>
            {
                ev.hold_step(node, None);
            }
            PointerPhase::Up | PointerPhase::Leave if stepper.held.is_some() => {
                ev.hold_step(node, None);
            }
            _ => {}
        }
        return None;
    }
    // The store wakes a half only while it is held.
    if let Some(half) = ev.woken().and_then(StepHalf::of_token) {
        return Some(StepInput::Step(half, 1.0));
    }
    let k = ev.key().copied().filter(|k| k.pressed)?;
    let rtl = ev.reads_rtl();
    let input = match k.key {
        Key::Up => StepInput::Step(StepHalf::Up, 1.0),
        Key::Down => StepInput::Step(StepHalf::Down, 1.0),
        Key::Right if !rtl => StepInput::Step(StepHalf::Up, 1.0),
        Key::Left if rtl => StepInput::Step(StepHalf::Up, 1.0),
        Key::Left | Key::Right => StepInput::Step(StepHalf::Down, 1.0),
        Key::PageUp => StepInput::Step(StepHalf::Up, PAGE),
        Key::PageDown => StepInput::Step(StepHalf::Down, PAGE),
        Key::Home => StepInput::ToMin,
        Key::End => StepInput::ToMax,
        _ => return None,
    };
    ev.stop_propagation();
    Some(input)
}

impl NodeStore {
    /// Node `id`'s stepper, if it draws one.
    pub fn stepper(&self, id: NodeId) -> Option<Stepper> {
        match self.content_payload(id) {
            Some(Content::Stepper(stepper)) => Some(*stepper),
            _ => None,
        }
    }

    fn stepper_mut(&mut self, id: NodeId) -> Option<&mut Stepper> {
        if !self.arena().is_live(id) {
            return None;
        }
        match self.content_payload_mut(id) {
            Some(Content::Stepper(stepper)) => Some(stepper),
            _ => None,
        }
    }

    /// Tell stepper `id` whether its minus (`lower`) and its plus (`upper`)
    /// can step; a half that no longer can lets go of its hold. Repaints it
    /// when that changes.
    pub fn show_stepper(&mut self, id: NodeId, lower: bool, upper: bool) {
        let Some(stepper) = self.stepper_mut(id) else {
            return;
        };
        let was = *stepper;
        stepper.lower = lower;
        stepper.upper = upper;
        if stepper.held.is_some_and(|half| !stepper.steps(half)) {
            stepper.held = None;
        }
        if *stepper != was {
            self.mark_dirty(id, DirtyClass::PAINT);
        }
    }

    /// Hold half `half` of stepper `id` down, stepping it on after a pause
    /// and faster the longer it is held, or let go of its hold.
    pub(crate) fn hold_step(&mut self, id: NodeId, half: Option<StepHalf>) {
        let Some(stepper) = self.stepper_mut(id) else {
            return;
        };
        if stepper.held == half {
            return;
        }
        stepper.held = half;
        stepper.serial = stepper.serial.wrapping_add(1);
        let serial = stepper.serial;
        self.mark_dirty(id, DirtyClass::PAINT);
        if let Some(half) = half {
            self.step_on(id, half, serial, HOLD, 0);
        }
    }

    /// Arm the `steps`-th step on of hold `serial` of `half` of stepper `id`,
    /// `delay` from now.
    fn step_on(&mut self, id: NodeId, half: StepHalf, serial: u32, delay: Duration, steps: u32) {
        self.queue_timer(TimerRequest::new(id, delay, move |store| {
            let Some(stepper) = store.stepper(id) else {
                return;
            };
            if stepper.held != Some(half) || stepper.serial != serial || !stepper.steps(half) {
                return;
            }
            store.queue_wake(id, half.token());
            let next = if steps < QUICKEN { REPEAT } else { REPEAT_FAST };
            store.step_on(id, half, serial, next, steps + 1);
        }));
    }

    /// Whether node `id` reads right to left.
    pub(crate) fn reads_rtl(&self, id: NodeId) -> bool {
        self.layout_direction(id) == LayoutDirection::Rtl
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_stops_at_a_bound_or_wraps_round_and_keeps_to_its_grid() {
        let up = StepInput::Step(StepHalf::Up, 1.0);
        let down = StepInput::Step(StepHalf::Down, 1.0);
        assert_eq!(up.apply(1.0, 1.0, Some(0.0), Some(3.0), false), Some(2.0));
        assert_eq!(up.apply(3.0, 1.0, Some(0.0), Some(3.0), false), None);
        assert_eq!(up.apply(3.0, 1.0, Some(0.0), Some(3.0), true), Some(0.0));
        assert_eq!(down.apply(0.0, 1.0, Some(0.0), Some(3.0), true), Some(3.0));
        assert_eq!(up.apply(2.5, 1.0, Some(0.0), Some(3.0), false), Some(3.0));
        assert_eq!(up.apply(0.1, 0.1, None, None, false), Some(0.2));
        let page = StepInput::Step(StepHalf::Down, PAGE);
        assert_eq!(page.apply(5.0, 1.0, Some(0.0), None, false), Some(0.0));
        assert_eq!(
            StepInput::ToMax.apply(5.0, 1.0, Some(0.0), Some(9.0), false),
            Some(9.0)
        );
        assert_eq!(StepInput::ToMax.apply(5.0, 1.0, None, None, false), None);
        assert_eq!(can_step(0.0, Some(0.0), Some(3.0), false), (false, true));
        assert_eq!(can_step(3.0, Some(0.0), Some(3.0), true), (true, true));
    }

    #[test]
    fn a_half_is_the_side_a_point_lands_on_mirrored_right_to_left() {
        let s = Stepper::new(Rgba::new(1.0, 1.0, 1.0, 1.0), Rgba::TRANSPARENT);
        assert_eq!(s.half_at(10.0, 96.0, false), StepHalf::Down);
        assert_eq!(s.half_at(60.0, 96.0, false), StepHalf::Up);
        assert_eq!(s.half_at(10.0, 96.0, true), StepHalf::Up);
        assert_eq!(StepHalf::of_token(StepHalf::Up.token()), Some(StepHalf::Up));
        assert_eq!(StepHalf::of_token(7), None);
    }

    #[test]
    fn a_stepper_dims_a_half_that_cannot_step_and_presses_in_a_held_one() {
        let mut s = Stepper::new(Rgba::new(1.0, 1.0, 1.0, 1.0), Rgba::new(1.0, 1.0, 1.0, 0.1));
        s.lower = false;
        s.held = Some(StepHalf::Up);
        let mut out = Vec::new();
        s.paint(Rect::new(0.0, 0.0, 96.0, 32.0), false, &mut out);
        let [
            Primitive::Quad(pill),
            Primitive::Quad(pressed),
            Primitive::Quad(line),
            Primitive::Path(minus),
            Primitive::Path(plus),
        ] = &out[..]
        else {
            panic!("{out:?}")
        };
        assert_eq!((pill.radius, pill.color.a), (8.0, 0.1));
        assert!(pressed.rect.x > 48.0, "the plus is pressed in");
        assert_eq!(line.rect.x + line.rect.w * 0.5, 48.0);
        assert!((minus.stroke.as_ref().unwrap().color.a - 0.3).abs() < 1e-6);
        assert_eq!(minus.cmds.len(), 2);
        assert_eq!(plus.cmds.len(), 4);
        assert_eq!(plus.stroke.as_ref().unwrap().color.a, 1.0);
    }
}
