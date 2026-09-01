//! Backend-level tests for the headless [`PlatformApp`] and the raw event pump.
//!
//! These sit *below* the runtime: they drive a [`HeadlessApp`] with a recording
//! [`AppHandler`] and assert the pump's contract directly — boot ordering, that
//! `request_redraw` turns into a prioritized beat, that `ControlFlow::Exit`
//! terminates immediately, and that the [`AcceptCell`] veto handshake works.

use std::cell::RefCell;
use std::rc::Rc;

use viso_platform::backend::headless::HeadlessApp;
use viso_platform::{
    AcceptCell, AppHandler, ControlFlow, PlatformApp, RawEvent, WindowConfig, WindowId,
};

/// An `AppHandler` that records every event and replays a scripted list of
/// [`ControlFlow`] return values (defaulting to `Poll` once exhausted).
struct Recorder {
    seen: Rc<RefCell<Vec<RawEvent>>>,
    flows: Vec<ControlFlow>,
    next: usize,
}

impl Recorder {
    fn new(seen: Rc<RefCell<Vec<RawEvent>>>, flows: Vec<ControlFlow>) -> Self {
        Self {
            seen,
            flows,
            next: 0,
        }
    }
}

impl AppHandler for Recorder {
    fn handle(&mut self, event: RawEvent) -> ControlFlow {
        self.seen.borrow_mut().push(event);
        let flow = self
            .flows
            .get(self.next)
            .copied()
            .unwrap_or(ControlFlow::Poll);
        self.next += 1;
        flow
    }
}

#[test]
fn app_launched_is_delivered_first_and_once() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut app = HeadlessApp::scripted(vec![
        RawEvent::Wakeup,
        RawEvent::WindowClosed {
            window: WindowId(1),
        },
    ]);
    let mut handler = Recorder::new(Rc::clone(&seen), vec![]);
    app.run(&mut handler);

    let seen = seen.borrow();
    assert_eq!(seen[0], RawEvent::AppLaunched, "launch is delivered first");
    assert_eq!(
        seen.iter().filter(|e| **e == RawEvent::AppLaunched).count(),
        1,
        "launch is delivered exactly once"
    );
    // The rest arrive in script order.
    assert_eq!(seen[1], RawEvent::Wakeup);
    assert_eq!(
        seen[2],
        RawEvent::WindowClosed {
            window: WindowId(1)
        }
    );
}

#[test]
fn requested_redraw_is_delivered_as_a_prioritized_beat() {
    // A pending redraw beat must arrive before any scripted event: the pump
    // drains its redraw queue ahead of the script.
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut app = HeadlessApp::scripted(vec![RawEvent::Wakeup]);
    let w = app
        .create_window(WindowConfig::default())
        .expect("headless window creation never fails");
    app.request_redraw(w);

    let mut handler = Recorder::new(Rc::clone(&seen), vec![]);
    app.run(&mut handler);

    let seen = seen.borrow();
    assert_eq!(seen[0], RawEvent::AppLaunched);
    assert_eq!(
        seen[1],
        RawEvent::RedrawRequested { window: w },
        "the requested beat jumps ahead of scripted events"
    );
    assert_eq!(seen[2], RawEvent::Wakeup);
}

#[test]
fn control_flow_exit_terminates_immediately() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut app = HeadlessApp::scripted(vec![
        RawEvent::Wakeup, // handler returns Exit right after this
        RawEvent::WindowClosed {
            window: WindowId(1),
        },
        RawEvent::WindowClosed {
            window: WindowId(2),
        },
    ]);
    // AppLaunched -> Poll, Wakeup -> Exit.
    let mut handler = Recorder::new(Rc::clone(&seen), vec![ControlFlow::Poll, ControlFlow::Exit]);
    app.run(&mut handler);

    let seen = seen.borrow();
    assert_eq!(
        *seen,
        vec![RawEvent::AppLaunched, RawEvent::Wakeup],
        "no event is delivered after the handler returns Exit"
    );
}

#[test]
fn exit_on_launch_delivers_nothing_further() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut app = HeadlessApp::scripted(vec![RawEvent::Wakeup]);
    let mut handler = Recorder::new(Rc::clone(&seen), vec![ControlFlow::Exit]);
    app.run(&mut handler);

    assert_eq!(
        *seen.borrow(),
        vec![RawEvent::AppLaunched],
        "returning Exit from the launch event stops the pump"
    );
}

#[test]
fn accept_cell_veto_handshake() {
    // The platform layer creates the cell accepting; a handler that wants to
    // keep the window open calls deny(). Verify both sides of the handshake.
    let cell = AcceptCell::new();
    assert!(cell.is_accepted(), "cells accept by default");

    let clone = cell.clone();
    clone.deny();
    assert!(
        !cell.is_accepted(),
        "deny on a clone vetoes through the shared cell"
    );

    cell.accept();
    assert!(cell.is_accepted(), "accept restores it");

    // Two distinct cells are not equal; clones of one are.
    assert_eq!(cell, clone, "clones share identity");
    assert_ne!(cell, AcceptCell::new(), "distinct cells differ");
}

#[test]
fn empty_script_drains_after_launch() {
    // With nothing scripted and no redraw requested, the pump delivers only the
    // launch event, then the queue drains and run returns.
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut app = HeadlessApp::new();
    let mut handler = Recorder::new(Rc::clone(&seen), vec![]);
    app.run(&mut handler);

    assert_eq!(*seen.borrow(), vec![RawEvent::AppLaunched]);
}
