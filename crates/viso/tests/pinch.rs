//! Pinches driven through the facade: a trackpad's gesture and a wheel turned
//! with Control held reach the node that takes pinches under the pointer, in
//! logical points, and a wheel's pinch ends once it rests.
#![cfg(feature = "headless")]

use std::cell::RefCell;
use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::platform::{
    GesturePhase, Modifiers, RawEvent, RawGesture, RawScroll, ScrollPhase, WindowId,
};
use viso::prelude::*;
use viso::ui::{LeafStyle, PinchPhase, Sizing, WindowConfig};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

thread_local! {
    static SEEN: RefCell<Vec<(PinchPhase, f32, f32)>> = const { RefCell::new(Vec::new()) };
}

/// A window-filling node that takes pinches and notes each sample: its
/// phase, scale and turn.
struct Pinched;

impl Application for Pinched {
    fn new(_cx: &mut AppCx) -> Self {
        Pinched
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let root = cx.leaf(LeafStyle {
            size: Sizing::fill(),
            ..LeafStyle::default()
        });
        cx.pinchable(root, true);
        cx.on_pointer(root, |ev| {
            if let Some(p) = ev.pinch() {
                SEEN.with(|s| s.borrow_mut().push((p.phase, p.scale, p.rotation)));
            }
        });
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn gesture(scale: f64, rotation: f64, phase: GesturePhase) -> RawEvent {
    RawEvent::Gesture(RawGesture {
        window: WIN,
        x: 50.0,
        y: 50.0,
        scale,
        rotation,
        modifiers: Modifiers::default(),
        phase,
    })
}

fn run(script: Vec<RawEvent>) -> Vec<(PinchPhase, f32, f32)> {
    SEEN.with(|s| s.borrow_mut().clear());
    let mut all = vec![redraw(), redraw()];
    all.extend(script);
    drive_scripted::<Pinched>(all, STEP);
    SEEN.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

#[test]
fn a_trackpad_s_pinch_reaches_the_node_under_the_pointer() {
    let seen = run(vec![
        gesture(1.0, 0.0, GesturePhase::Began),
        gesture(2.0, 0.5, GesturePhase::Changed),
        gesture(1.0, 0.0, GesturePhase::Ended),
        redraw(),
    ]);
    assert_eq!(
        seen,
        [
            (PinchPhase::Start, 1.0, 0.0),
            (PinchPhase::Move, 2.0, 0.5),
            (PinchPhase::End, 2.0, 0.5)
        ]
    );
}

#[test]
fn a_wheel_with_control_zooms_and_rests() {
    let wheel = RawEvent::Scroll(RawScroll {
        window: WIN,
        x: 50.0,
        y: 50.0,
        delta_x: 0.0,
        delta_y: -100.0,
        modifiers: Modifiers {
            control: true,
            ..Modifiers::default()
        },
        phase: ScrollPhase::None,
    });
    let mut script = vec![wheel, redraw()];
    // The beats that carry the clock past its rest.
    for _ in 0..20 {
        script.extend([RawEvent::Wakeup, redraw()]);
    }
    let seen = run(script);
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[0].0, PinchPhase::Start);
    assert!((seen[0].1 - std::f32::consts::E).abs() < 1e-4);
    assert_eq!(seen[1].0, PinchPhase::End, "it rests");
}
