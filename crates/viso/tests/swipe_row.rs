//! Swipe rows driven through the facade: a drag across a row opens it on
//! its actions, a press on one runs it and shuts the row, a press elsewhere
//! in the list only shuts it, and a full swipe runs the outermost action.
#![cfg(feature = "headless")]

use std::cell::RefCell;
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{Modifiers, PointerButtons, PointerPhase, RawEvent, RawPointer, WindowId};
use viso::prelude::*;
use viso::ui::{Axis, BoxStyle, Component, FlexStyle, LeafStyle, NodeId, Size, WindowConfig};
use viso::widgets::{ScrollViewStyle, button, scroll, swipe_action, swipe_row};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

thread_local! {
    static HEARD: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn heard(what: &str) {
    HEARD.with(|h| h.borrow_mut().push(what.to_owned()));
}

fn take_heard() -> Vec<String> {
    HEARD.with(|h| std::mem::take(&mut *h.borrow_mut()))
}

/// A list of three 300×40 rows, each with a "Delete" and a "Flag" action
/// behind it; the second row's content is a button.
struct Inbox;

impl Application for Inbox {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        Inbox
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        cx.flex(FlexStyle::default(), list);
    }
}

/// The list, in a column so it keeps its own size.
fn list(cx: &mut BuildCx<'_>) {
    scroll(ScrollViewStyle {
        size: Size::fixed(300.0, 100.0),
        ..ScrollViewStyle::default()
    })
    .content(|cx| {
        cx.flex(
            FlexStyle {
                axis: Axis::Column,
                size: Size {
                    width: viso::ui::Length::fill(),
                    height: viso::ui::Length::Fit,
                },
                ..FlexStyle::default()
            },
            |cx| {
                for row in 0..3 {
                    swipe_row(move |cx| {
                        if row == 1 {
                            button("Open")
                                .size(Size::fixed(300.0, 40.0))
                                .on_click(|_| heard("open"))
                                .build(cx);
                        } else {
                            cx.leaf(LeafStyle {
                                size: Size::fixed(300.0, 40.0),
                                style: BoxStyle::NONE,
                            });
                        }
                    })
                    .trailing([
                        swipe_action("Delete")
                            .destructive()
                            .on_tap(move |_| heard(&format!("delete {row}"))),
                        swipe_action("Flag").on_tap(move |_| heard(&format!("flag {row}"))),
                    ])
                    .build(cx);
                }
            },
        );
    })
    .build(cx);
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn mouse(x: f64, y: f64, buttons: PointerButtons, phase: PointerPhase) -> RawEvent {
    RawEvent::Pointer(RawPointer::mouse(
        WIN,
        x,
        y,
        buttons,
        Modifiers::default(),
        phase,
    ))
}

/// A mouse press at `from` dragged across to `to` x in steps, then lifted.
fn drag(from: (f64, f64), to: f64) -> Vec<RawEvent> {
    let mut out = vec![
        mouse(from.0, from.1, PointerButtons::NONE, PointerPhase::Moved),
        mouse(from.0, from.1, PointerButtons::PRIMARY, PointerPhase::Down),
    ];
    let steps = 10;
    for k in 1..=steps {
        let x = from.0 + (to - from.0) * f64::from(k) / f64::from(steps);
        out.push(mouse(
            x,
            from.1,
            PointerButtons::PRIMARY,
            PointerPhase::Moved,
        ));
    }
    out.push(mouse(to, from.1, PointerButtons::PRIMARY, PointerPhase::Up));
    out.push(redraw());
    out
}

fn click(at: (f64, f64)) -> Vec<RawEvent> {
    vec![
        mouse(at.0, at.1, PointerButtons::NONE, PointerPhase::Moved),
        mouse(at.0, at.1, PointerButtons::PRIMARY, PointerPhase::Down),
        mouse(at.0, at.1, PointerButtons::PRIMARY, PointerPhase::Up),
        redraw(),
    ]
}

fn run(steps: &[Vec<RawEvent>]) -> DrivenApp<Inbox> {
    take_heard();
    let mut script = vec![redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
        script.extend(std::iter::repeat_n(redraw(), 60));
    }
    drive_scripted::<Inbox>(script, STEP)
}

/// The swipe rows in tree order.
fn rows(app: &DrivenApp<Inbox>) -> Vec<NodeId> {
    fn walk(app: &DrivenApp<Inbox>, node: NodeId, out: &mut Vec<NodeId>) {
        if app.store().is_swipe_row(node) {
            out.push(node);
        }
        for child in app.store().arena().children(node).collect::<Vec<_>>() {
            walk(app, child, out);
        }
    }
    let mut out = Vec::new();
    walk(app, app.root().unwrap(), &mut out);
    out
}

#[test]
fn a_drag_across_opens_a_row_and_its_action_runs_and_shuts_it() {
    let app = run(&[drag((250.0, 20.0), 150.0)]);
    let row = rows(&app)[0];
    assert_eq!(
        app.store().swipe_open(row),
        Some(viso::ui::SwipeEdge::Trailing)
    );
    let open = app.store().swipe_offset(row).unwrap();
    assert!(open < -140.0, "open on both actions: {open}");

    // The outermost action sits at the row's right end.
    let app = run(&[drag((250.0, 20.0), 150.0), click((290.0, 20.0))]);
    assert_eq!(take_heard(), ["delete 0"]);
    assert_eq!(app.store().swipe_offset(rows(&app)[0]), Some(0.0));
}

#[test]
fn a_press_elsewhere_in_the_list_only_shuts_the_open_row() {
    let app = run(&[drag((250.0, 20.0), 150.0), click((150.0, 60.0))]);
    assert_eq!(
        take_heard(),
        Vec::<String>::new(),
        "the button heard nothing"
    );
    assert_eq!(app.store().swipe_offset(rows(&app)[0]), Some(0.0));
    let app = run(&[click((150.0, 60.0))]);
    assert_eq!(take_heard(), ["open"], "with no row open it is a click");
    let _ = app;
}

#[test]
fn a_full_swipe_runs_the_outermost_action() {
    let app = run(&[drag((290.0, 20.0), 20.0)]);
    assert_eq!(take_heard(), ["delete 0"]);
    assert_eq!(
        app.store().swipe_offset(rows(&app)[0]),
        Some(0.0),
        "and springs back"
    );
}
