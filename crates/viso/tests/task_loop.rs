//! UI tasks through the real frame loop (ADR 0032): a handler spawns a task
//! that awaits a service reply, and its continuation writes state at a frame
//! boundary.
//!
//! - **a service reply writes back** — a click opens a file dialog on a mock
//!   registry and `spawn_then`s the reply; the frame after the click polls the
//!   task and its continuation records how many files came back.
//! - **a reply from another thread wakes the loop** — the reply is completed on
//!   a foreign thread and the script supplies no beat after the click: the
//!   frame that runs the continuation is the one `on_wakeup` requested.

use std::cell::Cell;
use std::path::PathBuf;
use std::time::Duration;

use viso::__test_support::{drive_scripted, drive_scripted_with_services};
use viso::platform::{
    Modifiers, PointerButtons as RawButtons, PointerPhase as RawPhase, RawEvent, RawPointer,
    WindowId,
};
use viso::prelude::*;
use viso::services::{Call, Mock, OpenOptions, PickedFile, reply};
use viso::ui::{BuildCx, FlexStyle, PointerPhase, Size, WindowConfig};

const STEP: Duration = Duration::from_millis(16);

thread_local! {
    /// The cell the app's continuation writes, published by `build`.
    static PICKED: Cell<Option<StateId>> = const { Cell::new(None) };
}

/// A surface-filling root; a press runs `on_press` with the state cell.
fn build_root(cx: &mut BuildCx<'_>, on_press: fn(&mut viso::ui::EventCx<'_>, StateId)) {
    let picked = cx.state(StateValue::Int(-1));
    PICKED.with(|cell| cell.set(Some(picked)));
    let root = cx.flex(
        FlexStyle {
            size: Size::fill(),
            ..FlexStyle::default()
        },
        |_| {},
    );
    cx.on_pointer(root, move |ev| {
        if ev.pointer().is_some_and(|p| p.phase == PointerPhase::Down) {
            on_press(ev, picked);
        }
    });
}

fn unframed() -> WindowConfig {
    WindowConfig {
        caption: false,
        ..Default::default()
    }
}

/// Opens a file dialog and records how many files the reply carried.
struct OpenApp;

impl Application for OpenApp {
    fn new(_cx: &mut AppCx) -> Self {
        OpenApp
    }

    fn window_config(&self) -> WindowConfig {
        unframed()
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        build_root(cx, |ev, picked| {
            let files = ev.services().files().open(OpenOptions::default());
            ev.spawn_then(files, move |cx, files| {
                let n = files.map_or(0, |f| f.len() as i32);
                cx.set(picked, StateValue::Int(n));
            });
        });
    }
}

/// Awaits a reply that another thread completes.
struct ThreadApp;

impl Application for ThreadApp {
    fn new(_cx: &mut AppCx) -> Self {
        ThreadApp
    }

    fn window_config(&self) -> WindowConfig {
        unframed()
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        build_root(cx, |ev, picked| {
            let (completer, answer) = reply::<i32>();
            ev.spawn_then(answer, move |cx, value| {
                cx.set(picked, StateValue::Int(value.unwrap_or(0)));
            });
            std::thread::spawn(move || completer.complete(Ok(7)))
                .join()
                .unwrap();
        });
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested {
        window: WindowId(1),
    }
}

fn press() -> RawEvent {
    RawEvent::Pointer(RawPointer::mouse(
        WindowId(1),
        400.0,
        300.0,
        RawButtons::PRIMARY,
        Modifiers::default(),
        RawPhase::Down,
    ))
}

fn picked() -> StateId {
    PICKED.with(Cell::get).expect("build published the cell")
}

#[test]
fn a_service_reply_writes_back_through_the_continuation() {
    let mock = Mock::new();
    let file = |name: &str| PickedFile {
        name: name.to_owned(),
        path: Some(PathBuf::from("/tmp").join(name)),
        contents: Vec::new(),
    };
    mock.answer_open(Ok(vec![file("a.txt"), file("b.txt")]));
    let app = drive_scripted_with_services::<OpenApp>(
        vec![redraw(), press(), redraw()],
        STEP,
        mock.services(),
    );
    assert!(matches!(mock.calls().as_slice(), [Call::Open(_)]));
    assert_eq!(app.states().get(picked()), Some(StateValue::Int(2)));
    assert_eq!(
        app.store().task_count(),
        0,
        "a finished task leaves the store"
    );
}

#[test]
fn a_reply_completed_on_another_thread_wakes_the_loop() {
    // No beat follows the press: the continuation runs only if the kick
    // reached `on_wakeup` and the redraw it requested ran a frame.
    let app = drive_scripted::<ThreadApp>(vec![redraw(), press()], STEP);
    assert_eq!(app.states().get(picked()), Some(StateValue::Int(7)));
}
