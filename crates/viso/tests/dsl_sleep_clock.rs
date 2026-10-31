//! A task's `time::sleep` under the frame loop is timed by the frame clock:
//! a headless drive's fixed-step clock carries it to its end in as many beats
//! as its duration spans, never by the wall clock, and while it waits the
//! loop keeps its deadline as it keeps a UI timer's.

#![cfg(feature = "headless")]

use std::cell::Cell;
use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::platform::{
    Modifiers, PointerButtons as RawButtons, PointerPhase as RawPhase, RawEvent, RawPointer,
    WindowId,
};
use viso::prelude::*;
use viso::ui::{StateId, StateValue, WindowConfig};

const STEP: Duration = Duration::from_millis(16);

viso::component! {
    import viso::time;

    Napper {
        state slept = 0;
        task blink() -> I64 { await time::sleep(48ms); 7 }
        view {
            Column {
                width: fill;
                height: fill;
                on click {
                    start blink() { success(v) { slept = v; } };
                }
            }
        }
    }
}

thread_local! {
    static SLEPT: Cell<Option<StateId>> = const { Cell::new(None) };
}

struct NapApp;

impl Application for NapApp {
    fn new(_cx: &mut AppCx) -> Self {
        NapApp
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let (Napper { slept }, _) = Napper::build(cx);
        SLEPT.with(|s| s.set(Some(slept)));
    }
}

/// A priming frame, a click that starts the 48ms sleep, then `beats` wakes
/// each with its frame, one step apart.
fn script(beats: usize) -> Vec<RawEvent> {
    let at = |phase| {
        RawEvent::Pointer(RawPointer::mouse(
            WindowId(1),
            40.0,
            40.0,
            RawButtons::PRIMARY,
            Modifiers::default(),
            phase,
        ))
    };
    let redraw = RawEvent::RedrawRequested {
        window: WindowId(1),
    };
    let mut script = vec![
        redraw.clone(),
        at(RawPhase::Down),
        at(RawPhase::Up),
        redraw.clone(),
    ];
    for _ in 0..beats {
        script.push(RawEvent::Wakeup);
        script.push(redraw.clone());
    }
    script
}

fn slept(app: &viso::__test_support::DrivenApp<NapApp>) -> Option<StateValue> {
    app.states().get(SLEPT.with(Cell::get).expect("built"))
}

#[test]
fn a_sleep_ends_on_the_frame_its_duration_carries_the_clock_to() {
    let app = drive_scripted::<NapApp>(script(1), STEP);
    assert_eq!(
        slept(&app),
        Some(StateValue::Int(0)),
        "one step in, it sleeps"
    );
    assert!(app.next_timer_deadline().is_some(), "the loop waits for it");

    let app = drive_scripted::<NapApp>(script(5), STEP);
    assert_eq!(
        slept(&app),
        Some(StateValue::Int(7)),
        "five steps carry it past 48ms"
    );
    assert!(
        app.next_timer_deadline().is_none(),
        "nothing waits once it ends"
    );
}
