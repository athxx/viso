//! Section 66 facade integration pack for the one-shot timer clock — the
//! frame-loop wiring the Toast control (and every future auto-dismiss/delay)
//! rides on. Like the animation pack, it drives the *real* [`AppDriver`] frame
//! loop end to end through a scripted headless pump with a self-stepping
//! deterministic clock, so it exercises the same path `viso::run` takes:
//!
//! - **a timer fires once through the loop** — a scripted pointer press asks a
//!   handler to arm a timer whose `on_fire` hides a marker node; after enough
//!   `STEP`-sized beats carry the clock past the deadline, the marker is hidden,
//!   proving `arm_request` → `fire_due` ran exactly once at the right frame;
//! - **a timer costs no frame while it waits** — unlike an animation, an armed
//!   timer never sets `wants_animation`, so the pump does not self-reschedule for
//!   it. It fires only on a beat the *script* supplies (a real backend supplies
//!   that beat by honoring `WaitUntil` at the deadline); before the deadline the
//!   marker is still shown and `next_timer_deadline` is `Some`;
//! - **the loop halts when the timer fires** — once the registry empties,
//!   `next_timer_deadline` is `None`: the timer-side zero-CPU-when-idle contract,
//!   the counterpart to `wants_animation` for the slide clock.
//!
//! The clock is a `FixedStepClock`: each frame the pump runs observes exactly
//! one `STEP`, so a known number of beats deterministically crosses the timer's
//! deadline. Because an idle timer does not keep the pump beating, the script
//! must supply every beat itself (a real backend's `WaitUntil` timeout does this
//! at the deadline instead).

use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::platform::{
    Modifiers, PointerButtons as RawButtons, PointerPhase as RawPhase, RawEvent, RawPointer,
    WindowId,
};
use viso::prelude::*;
use viso::ui::{BuildCx, FlexStyle, LeafStyle, PointerButtons, PointerPhase, Size};

/// The launch surface (`WindowConfig::default` logical size, scale 1.0). A
/// center sample lands on the fill root.
const SURFACE_W: f32 = 800.0;
const SURFACE_H: f32 = 600.0;

/// A fixed frame delta. The timer's delay is a small multiple of it, so a
/// handful of beats crosses the deadline.
const STEP: Duration = Duration::from_millis(16);
/// The timer delay: three steps out, so two post-arm beats leave it pending and
/// four cross it.
const DELAY: Duration = Duration::from_millis(48);

/// An app whose root is a surface-filling leaf holding one child marker leaf. A
/// primary pointer-down on the root arms a one-shot timer (the same
/// `EventCx::request_timer` seam the Toast control will use) whose `on_fire`
/// hides the marker — a store mutation the test observes to prove the timer
/// fired exactly once, at the frame the deadline is crossed.
struct TimerApp;

impl Application for TimerApp {
    fn new(_cx: &mut AppCx) -> Self {
        TimerApp
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        // A surface-filling flex root holding one child marker leaf. The child is
        // declared inside the `flex` closure (the only child-building form —
        // there is no `child_leaf`); its handle is captured out of the closure.
        let mut marker_id = None;
        let root = cx.flex(
            FlexStyle {
                size: Size::fill(),
                ..FlexStyle::default()
            },
            |c| {
                let marker = c.leaf(LeafStyle {
                    size: Size::fixed(10.0, 10.0),
                    ..LeafStyle::default()
                });
                marker_id = Some(marker.id());
            },
        );
        cx.focusable(root, true);
        let marker_id = marker_id.expect("the flex closure built the marker leaf");
        cx.on_pointer(root, move |ev| {
            let Some(p) = ev.pointer() else { return };
            if p.phase == PointerPhase::Down && p.buttons.contains(PointerButtons::PRIMARY) {
                ev.request_timer(marker_id, DELAY, move |store| {
                    store.set_hidden(marker_id, true);
                });
            }
        });
    }
}

/// One priming redraw (runs the first layout so the fill root gets a world box
/// the pointer can hit), a primary pointer-down that arms the timer, then
/// `beats` wake beats. An armed timer never sets a redraw reason (it blocks, it
/// does not spin), so a bare `RedrawRequested` after the arm frame finds the
/// pump idle and runs no frame — exactly the zero-CPU-when-idle contract. In
/// production the backend honors the timer's `WaitUntil(deadline)` and, when it
/// times out, delivers a `RawEvent::Wakeup` (which adds an `AsyncCompletion`
/// reason) followed by a beat that drains it. This script models that wakeup
/// explicitly: each beat is a `Wakeup` (arm the reason) then a `RedrawRequested`
/// (run the frame, stepping the `FixedStepClock` by one `STEP`), so `beats`
/// controls how far past the arm frame the clock advances.
fn timer_script(beats: usize) -> Vec<RawEvent> {
    let mut script = vec![
        RawEvent::RedrawRequested {
            window: WindowId(1),
        },
        RawEvent::Pointer(RawPointer {
            window: WindowId(1),
            x: SURFACE_W as f64 / 2.0,
            y: SURFACE_H as f64 / 2.0,
            buttons: RawButtons::PRIMARY,
            modifiers: Modifiers::default(),
            phase: RawPhase::Down,
        }),
        // The input beat that arms the timer: `InputDirty` from the pointer drives
        // this frame, which drains the request and calls `arm_request`.
        RawEvent::RedrawRequested {
            window: WindowId(1),
        },
    ];
    for _ in 0..beats {
        script.push(RawEvent::Wakeup);
        script.push(RawEvent::RedrawRequested {
            window: WindowId(1),
        });
    }
    script
}

#[test]
fn timer_fires_once_and_the_loop_halts() {
    // The timer arms at t = 2 * STEP = 32ms (launch, priming, then the arm frame),
    // so its deadline is 32 + 48 = 80ms. Each wake beat steps the clock one STEP
    // past the arm frame: beats land at 48, 64, 80(=deadline, fires), 96, 112ms.
    // Five beats carry the clock to 112ms, well past the deadline.
    let app = drive_scripted::<TimerApp>(timer_script(5), STEP);
    let root = app.root().expect("TimerApp declares a root");
    let store = app.store();

    // The marker is the root's first child.
    let marker = store
        .arena()
        .links(root)
        .and_then(|l| l.first_child)
        .expect("TimerApp builds a marker child");

    // The timer's `on_fire` ran: the marker is hidden. It runs at most once
    // (fire_due removes a fired timer), so a hidden marker is one fire.
    assert!(
        store.hidden(marker),
        "the timer fired and hid the marker through the facade loop"
    );

    // The registry emptied when the timer fired, so no deadline is pending — the
    // loop is free to idle (the timer-side frame-halt / zero-CPU-when-idle
    // contract, the counterpart to `wants_animation` for the slide clock).
    assert!(
        app.next_timer_deadline().is_none(),
        "no timer deadline remains once the one-shot timer has fired"
    );
}

#[test]
fn a_timer_costs_no_frame_and_stays_pending_before_its_deadline() {
    // The timer arms at t = 32ms with an 80ms deadline. Two wake beats land at
    // 48 and 64ms — both short of 80ms — so the deadline is not yet crossed.
    let app = drive_scripted::<TimerApp>(timer_script(2), STEP);
    let root = app.root().expect("TimerApp declares a root");
    let store = app.store();
    let marker = store
        .arena()
        .links(root)
        .and_then(|l| l.first_child)
        .expect("TimerApp builds a marker child");

    // Before the deadline the timer has not fired: the marker is still shown.
    assert!(
        !store.hidden(marker),
        "the timer has not fired before its deadline (marker still shown)"
    );

    // And the deadline is still pending — the driver surfaces `Some`, which the
    // scheduler turns into a `WaitUntil` (a real backend blocks on it rather than
    // spinning frames). An armed timer never set `wants_animation`, so the pump
    // did not beat frames on its own to reach here.
    assert!(
        !app.wants_animation(),
        "an armed timer does not request animation frames (it blocks, not spins)"
    );
    assert!(
        app.next_timer_deadline().is_some(),
        "the timer's deadline is still pending before it is crossed"
    );
}
