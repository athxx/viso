//! Pointer lock driven through the facade: a press asks for the lock, the
//! platform engages it, the mouse's motion reaches the node as deltas in
//! logical points and its presses reach the node wherever it was, until
//! Escape, the window losing focus or the platform ends it; a host without
//! pointer lock answers the ask with the lock ended.

use std::cell::RefCell;
use std::time::Duration;

use viso::__test_support::{drive_scripted, drive_scripted_with_caps};
use viso::platform::{
    KeyCode as RawKeyCode, Modifiers as RawModifiers, PlatformCaps, PointerButtons as RawButtons,
    PointerPhase as RawPhase, RawEvent, RawKey, RawPointer, WindowId,
};
use viso::prelude::*;
use viso::ui::{Axis, BoxStyle, FlexStyle, LeafStyle, PointerPhase, Sizing};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

/// What the locking node heard, in order.
#[derive(Debug, Clone, PartialEq)]
enum Heard {
    Locked(bool),
    Motion(f32, f32),
    Press,
}

thread_local! {
    static HEARD: RefCell<Vec<Heard>> = const { RefCell::new(Vec::new()) };
}

fn heard() -> Vec<Heard> {
    HEARD.with(|h| h.borrow_mut().drain(..).collect())
}

/// A 100 × 100 view that locks the pointer when pressed, beside a 100 × 100
/// leaf that does nothing.
struct LockingApp;

impl Application for LockingApp {
    fn new(_cx: &mut AppCx) -> Self {
        LockingApp
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        cx.flex(
            FlexStyle {
                size: Sizing::fill(),
                axis: Axis::Row,
                ..FlexStyle::default()
            },
            |cx| {
                let view = cx.leaf(LeafStyle {
                    size: Sizing::fixed(100.0, 100.0),
                    style: BoxStyle::NONE,
                });
                let id = view.id();
                cx.on_pointer(view, move |cx| {
                    if cx.pointer().is_some_and(|p| p.phase == PointerPhase::Down) {
                        HEARD.with(|h| h.borrow_mut().push(Heard::Press));
                        cx.lock_pointer(id);
                    }
                });
                cx.on_key(view, |cx| {
                    let heard = if let Some(d) = cx.pointer_motion() {
                        Heard::Motion(d.x, d.y)
                    } else if let Some(locked) = cx.pointer_lock_change() {
                        Heard::Locked(locked)
                    } else {
                        return;
                    };
                    HEARD.with(|h| h.borrow_mut().push(heard));
                });
                cx.leaf(LeafStyle {
                    size: Sizing::fixed(100.0, 100.0),
                    style: BoxStyle::NONE,
                });
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn mouse(x: f64, y: f64, phase: RawPhase) -> RawEvent {
    let buttons = match phase {
        RawPhase::Down => RawButtons::PRIMARY,
        _ => RawButtons::NONE,
    };
    RawEvent::Pointer(RawPointer::mouse(
        WIN,
        x,
        y,
        buttons,
        RawModifiers::default(),
        phase,
    ))
}

fn motion(dx: f64, dy: f64) -> RawEvent {
    RawEvent::PointerMotion {
        window: WIN,
        dx,
        dy,
    }
}

fn escape() -> RawEvent {
    RawEvent::Key(RawKey {
        window: WIN,
        code: RawKeyCode::Escape,
        pressed: true,
        repeat: false,
        modifiers: RawModifiers::default(),
    })
}

#[test]
fn a_press_locks_the_pointer_and_motion_reaches_the_node() {
    heard();
    let app = drive_scripted::<LockingApp>(
        vec![
            redraw(),
            // Motion before any lock reaches nobody.
            motion(9.0, 9.0),
            mouse(50.0, 50.0, RawPhase::Down),
            mouse(50.0, 50.0, RawPhase::Up),
            motion(3.0, -2.0),
            motion(0.5, 0.0),
            // A press over the other leaf while locked still reaches the view.
            mouse(150.0, 50.0, RawPhase::Down),
            redraw(),
        ],
        STEP,
    );
    assert_eq!(
        heard(),
        vec![
            Heard::Press,
            Heard::Locked(true),
            Heard::Motion(3.0, -2.0),
            Heard::Motion(0.5, 0.0),
            Heard::Press,
        ]
    );
    assert!(app.store().pointer_lock().is_some());
}

#[test]
fn escape_and_focus_loss_end_the_lock() {
    heard();
    let app = drive_scripted::<LockingApp>(
        vec![
            redraw(),
            mouse(50.0, 50.0, RawPhase::Down),
            escape(),
            motion(1.0, 1.0),
            mouse(50.0, 50.0, RawPhase::Down),
            RawEvent::WindowFocused {
                window: WIN,
                focused: false,
            },
            motion(1.0, 1.0),
            redraw(),
        ],
        STEP,
    );
    assert_eq!(
        heard(),
        vec![
            Heard::Press,
            Heard::Locked(true),
            Heard::Locked(false),
            Heard::Press,
            Heard::Locked(true),
            Heard::Locked(false),
        ]
    );
    assert_eq!(app.store().pointer_lock(), None);
}

#[test]
fn a_host_without_pointer_lock_answers_with_the_lock_ended() {
    heard();
    let app = drive_scripted_with_caps::<LockingApp>(
        vec![
            redraw(),
            mouse(50.0, 50.0, RawPhase::Down),
            motion(1.0, 1.0),
        ],
        STEP,
        PlatformCaps::MOBILE,
    );
    assert_eq!(heard(), vec![Heard::Press, Heard::Locked(false)]);
    assert_eq!(app.store().pointer_lock(), None);
}
