//! Kinetic scrolling driven through the facade: a finger flicking a list
//! lifts, and the list coasts on frame by frame until it slows to a stop;
//! a finger that held still before lifting leaves it where it is.
#![cfg(feature = "headless")]

use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    Modifiers, PointerButtons, PointerId, PointerKind, PointerPhase, RawEvent, RawPointer, WindowId,
};
use viso::prelude::*;
use viso::ui::{Axis, BoxStyle, FlexStyle, LeafStyle, NodeId, ScrollPhysics, ScrollStyle, Size};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

/// A window-filling vertical list 5000 points tall.
struct ListApp;

impl Application for ListApp {
    fn new(_cx: &mut AppCx) -> Self {
        ListApp
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        cx.scroll(
            ScrollStyle {
                axis: Axis::Column,
                size: Size::fill(),
                ..Default::default()
            },
            |cx| {
                cx.flex(
                    FlexStyle {
                        axis: Axis::Column,
                        size: Size::fixed(200.0, 5000.0),
                        ..FlexStyle::default()
                    },
                    |cx| {
                        cx.leaf(LeafStyle {
                            size: Size::fixed(200.0, 40.0),
                            style: BoxStyle::NONE,
                        });
                    },
                );
            },
        );
    }
}

fn finger(x: f32, y: f32, phase: PointerPhase) -> RawEvent {
    let buttons = if phase == PointerPhase::Up {
        PointerButtons::NONE
    } else {
        PointerButtons::PRIMARY
    };
    RawEvent::Pointer(RawPointer {
        pointer: PointerId(3),
        kind: PointerKind::Touch,
        ..RawPointer::mouse(
            WIN,
            f64::from(x),
            f64::from(y),
            buttons,
            Modifiers::default(),
            phase,
        )
    })
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

/// A finger rising `per_frame` points a frame for `frames` frames from
/// (100, 400), then held still `still` frames, then lifted.
fn flick(per_frame: f32, frames: usize, still: usize) -> DrivenApp<ListApp> {
    let mut script = vec![redraw(), redraw(), finger(100.0, 400.0, PointerPhase::Down)];
    script.push(redraw());
    let mut y = 400.0;
    for _ in 0..frames {
        y -= per_frame;
        script.push(finger(100.0, y, PointerPhase::Moved));
        script.push(redraw());
    }
    for _ in 0..still {
        script.push(finger(100.0, y, PointerPhase::Moved));
        script.push(redraw());
    }
    script.push(finger(100.0, y, PointerPhase::Up));
    script.push(redraw());
    drive_scripted::<ListApp>(script, STEP)
}

fn offset(app: &DrivenApp<ListApp>) -> f32 {
    let root: NodeId = app.root().unwrap();
    app.store().scroll(root).y
}

#[test]
fn a_flick_coasts_on_after_the_lift_and_comes_to_rest() {
    let app = flick(16.0, 6, 0);
    let panned = 16.0 * 6.0;
    let rest = offset(&app);
    assert!(app.store().scroll_physics() == ScrollPhysics::native());
    // A thousand points a second carries on hundreds of points.
    assert!(rest > panned + 200.0, "{rest}");
    assert!(!app.store().is_scroll_moving(), "it came to rest");
}

#[test]
fn a_finger_held_still_before_the_lift_does_not_coast() {
    let app = flick(16.0, 6, 10);
    assert_eq!(offset(&app), 16.0 * 6.0);
}

/// A finger pulling the list down from its start by `travel` points over
/// six frames, lifted or not.
fn pull(travel: f32, lift: bool) -> DrivenApp<ListApp> {
    let mut script = vec![
        redraw(),
        redraw(),
        finger(100.0, 100.0, PointerPhase::Down),
        redraw(),
    ];
    for k in 1..=6 {
        script.push(finger(
            100.0,
            100.0 + travel * k as f32 / 6.0,
            PointerPhase::Moved,
        ));
        script.push(redraw());
    }
    if lift {
        script.push(finger(100.0, 100.0 + travel, PointerPhase::Up));
        script.push(redraw());
    }
    drive_scripted::<ListApp>(script, STEP)
}

#[test]
fn a_pull_past_the_start_shows_and_springs_back_when_let_go() {
    let app = pull(120.0, false);
    let root = app.root().unwrap();
    let store = app.store();
    let shown = store.overscroll(root);
    assert!(shown < -10.0, "{shown}");
    assert_eq!(store.scroll(root).y, 0.0);
    let content = store.arena().children(root).next().unwrap();
    let moved = store.world(content).y - store.world(root).y;
    match store.scroll_physics().overscroll {
        viso::ui::OverscrollStyle::Bounce => {
            assert!((moved + shown).abs() < 1.0, "{moved} {shown}")
        }
        _ => assert_eq!(moved, 0.0),
    }

    let app = pull(120.0, true);
    let root = app.root().unwrap();
    assert_eq!(app.store().overscroll(root), 0.0, "settled");
    assert!(!app.store().is_scroll_moving());
}
