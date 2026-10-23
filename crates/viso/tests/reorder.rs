//! Reorderable rows driven through the facade: a drag on a row's handle
//! carries it to another place and the list hears where, a pointer held at
//! the viewport's end scrolls it, and the keyboard lifts, moves and drops a
//! row.
#![cfg(feature = "headless")]

use std::cell::RefCell;
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    KeyCode, Modifiers, PointerButtons, PointerPhase, RawEvent, RawKey, RawPointer, WindowId,
};
use viso::prelude::*;
use viso::ui::{Axis, BoxStyle, Component, FlexStyle, LeafStyle, Length, Size, WindowConfig};
use viso::widgets::{
    ReorderHandleStyle, ScrollViewStyle, VirtualListViewStyle, reorder_handle, scroll, virtual_list,
};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

thread_local! {
    static HEARD: RefCell<Vec<(usize, usize)>> = const { RefCell::new(Vec::new()) };
}

fn take_heard() -> Vec<(usize, usize)> {
    HEARD.with(|h| std::mem::take(&mut *h.borrow_mut()))
}

/// A 300×100 viewport over six 300×40 rows, each a filler and a 40×40
/// handle at its end.
struct Groceries;

impl Application for Groceries {
    fn new(_cx: &mut AppCx) -> Self {
        Groceries
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

fn list(cx: &mut BuildCx<'_>) {
    scroll(ScrollViewStyle {
        size: Size::fixed(300.0, 100.0),
        ..ScrollViewStyle::default()
    })
    .on_reorder(|_, _, from, to| HEARD.with(|h| h.borrow_mut().push((from, to))))
    .content(|cx| {
        cx.flex(
            FlexStyle {
                axis: Axis::Column,
                size: Size {
                    width: Length::fill(),
                    height: Length::Fit,
                },
                ..FlexStyle::default()
            },
            |cx| {
                for _ in 0..6 {
                    row(cx);
                }
            },
        );
    })
    .build(cx);
}

/// A 300×40 row: a filler and a 40×40 handle at its end.
fn row(cx: &mut BuildCx<'_>) {
    cx.flex(
        FlexStyle {
            axis: Axis::Row,
            size: Size::fixed(300.0, 40.0),
            ..FlexStyle::default()
        },
        |cx| {
            cx.leaf(LeafStyle {
                size: Size::fixed(260.0, 40.0),
                style: BoxStyle::NONE,
            });
            reorder_handle()
                .style(ReorderHandleStyle {
                    size: Size::fixed(40.0, 40.0),
                    ..Default::default()
                })
                .build(cx);
        },
    );
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

/// A press at `from` on a handle dragged down to `to` y in steps, held
/// `hold` frames, then lifted.
fn drag(from: (f64, f64), to: f64, hold: usize) -> Vec<RawEvent> {
    let mut out = vec![
        mouse(from.0, from.1, PointerButtons::NONE, PointerPhase::Moved),
        mouse(from.0, from.1, PointerButtons::PRIMARY, PointerPhase::Down),
    ];
    for k in 1..=10 {
        let y = from.1 + (to - from.1) * f64::from(k) / 10.0;
        out.push(mouse(
            from.0,
            y,
            PointerButtons::PRIMARY,
            PointerPhase::Moved,
        ));
        out.push(redraw());
    }
    out.extend(std::iter::repeat_n(redraw(), hold));
    out.push(mouse(from.0, to, PointerButtons::PRIMARY, PointerPhase::Up));
    out.push(redraw());
    out
}

fn key(code: KeyCode) -> Vec<RawEvent> {
    let at = |pressed| {
        RawEvent::Key(RawKey {
            window: WIN,
            code,
            pressed,
            repeat: false,
            modifiers: Modifiers::default(),
        })
    };
    vec![at(true), at(false), redraw()]
}

fn run(steps: &[Vec<RawEvent>]) -> DrivenApp<Groceries> {
    take_heard();
    drive_scripted::<Groceries>(script(steps), STEP)
}

fn script(steps: &[Vec<RawEvent>]) -> Vec<RawEvent> {
    let mut script = vec![redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
        script.extend(std::iter::repeat_n(redraw(), 60));
    }
    script
}

/// A 300×100 virtual list of two hundred 300×40 rows like the others.
struct Long;

impl Application for Long {
    fn new(_cx: &mut AppCx) -> Self {
        Long
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        cx.flex(FlexStyle::default(), |cx| {
            virtual_list(VirtualListViewStyle {
                size: Size::fixed(300.0, 100.0),
                estimated_row: 40.0,
                ..VirtualListViewStyle::default()
            })
            .on_reorder(|_, _, from, to| HEARD.with(|h| h.borrow_mut().push((from, to))))
            .items(200, |_, cx| row(cx))
            .build(cx);
        });
    }
}

#[test]
fn a_handle_drag_carries_its_row_and_the_list_hears_where() {
    let _ = run(&[drag((280.0, 20.0), 66.0, 0)]);
    assert_eq!(take_heard(), [(0, 1)]);
}

#[test]
fn a_pointer_held_at_the_end_scrolls_the_list_on() {
    let _ = run(&[drag((280.0, 20.0), 96.0, 60)]);
    let heard = take_heard();
    assert_eq!(heard.len(), 1);
    assert_eq!(heard[0].0, 0);
    assert!(
        heard[0].1 >= 3,
        "carried past the rows first in sight: {heard:?}"
    );
}

#[test]
fn the_keyboard_lifts_moves_and_drops_a_row() {
    let _ = run(&[
        key(KeyCode::Tab),
        key(KeyCode::Space),
        key(KeyCode::Down),
        key(KeyCode::Down),
        key(KeyCode::Enter),
    ]);
    assert_eq!(take_heard(), [(0, 2)]);
}

#[test]
fn a_virtual_list_keeps_the_lifted_row_as_it_scrolls_far() {
    take_heard();
    let _ = drive_scripted::<Long>(script(&[drag((280.0, 20.0), 99.0, 120)]), STEP);
    let heard = take_heard();
    assert_eq!(
        heard.len(),
        1,
        "the lift outlived its place leaving the window"
    );
    assert_eq!(heard[0].0, 0);
    assert!(heard[0].1 >= 20, "carried far down: {heard:?}");
}
