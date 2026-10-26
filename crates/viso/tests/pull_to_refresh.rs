//! Pull to refresh driven through the facade: a finger pulling a list down
//! past its start, far enough, refreshes it and tells its handler on the
//! next frame; the list shows refreshing until the handler, or the work it
//! started, ends it. Short of the mark nothing refreshes.
#![cfg(feature = "headless")]

use std::cell::Cell;
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    Modifiers, PointerButtons, PointerId, PointerKind, PointerPhase, RawEvent, RawPointer, WindowId,
};
use viso::prelude::*;
use viso::ui::{Axis, BoxStyle, Component, FlexStyle, LeafStyle, NodeId, Sizing};
use viso::widgets::{ScrollViewStyle, scroll};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

thread_local! {
    static ASKED: Cell<u32> = const { Cell::new(0) };
    static ENDS_AT_ONCE: Cell<bool> = const { Cell::new(false) };
}

/// A window-filling list that refreshes when pulled.
struct FeedApp;

impl Application for FeedApp {
    fn new(_cx: &mut AppCx) -> Self {
        FeedApp
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        scroll(ScrollViewStyle::default())
            .content(|cx| {
                cx.flex(
                    FlexStyle {
                        axis: Axis::Column,
                        size: Sizing::fixed(200.0, 3000.0),
                        ..FlexStyle::default()
                    },
                    |cx| {
                        cx.leaf(LeafStyle {
                            size: Sizing::fixed(200.0, 40.0),
                            style: BoxStyle::NONE,
                        });
                    },
                );
            })
            .on_refresh(|ev, list| {
                ASKED.with(|a| a.set(a.get() + 1));
                if ENDS_AT_ONCE.with(Cell::get) {
                    ev.set_refreshing(list.id(), false);
                }
            })
            .build(cx);
    }
}

fn finger(y: f32, phase: PointerPhase) -> RawEvent {
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
            100.0,
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

/// A finger pulling the list down `travel` points over ten frames from the
/// top, lifted, then `after` frames.
fn pull(travel: f32, after: usize, ends_at_once: bool) -> DrivenApp<FeedApp> {
    ASKED.with(|a| a.set(0));
    ENDS_AT_ONCE.with(|e| e.set(ends_at_once));
    let mut script = vec![
        redraw(),
        redraw(),
        finger(50.0, PointerPhase::Down),
        redraw(),
    ];
    for k in 1..=10 {
        script.push(finger(50.0 + travel * k as f32 / 10.0, PointerPhase::Moved));
        script.push(redraw());
    }
    script.push(finger(50.0 + travel, PointerPhase::Up));
    script.extend(std::iter::repeat_n(redraw(), after));
    drive_scripted::<FeedApp>(script, STEP)
}

fn list(app: &DrivenApp<FeedApp>) -> NodeId {
    app.root().unwrap()
}

#[test]
fn a_long_pull_refreshes_and_tells_the_handler_once() {
    let app = pull(500.0, 90, false);
    let store = app.store();
    assert!(store.is_refreshable(list(&app)));
    assert!(store.is_refreshing(list(&app)), "until the handler ends it");
    assert_eq!(ASKED.with(Cell::get), 1);
    let (_, spinner) = store.refresh_indicator(list(&app)).unwrap();
    assert!(store.spinner(spinner).unwrap().spinning);
}

#[test]
fn a_handler_ends_the_refresh_it_was_told_of() {
    let app = pull(500.0, 90, true);
    assert_eq!(ASKED.with(Cell::get), 1);
    let store = app.store();
    assert!(!store.is_refreshing(list(&app)));
    assert_eq!(store.overscroll(list(&app)), 0.0);
    assert!(store.scroll(list(&app)).y >= 0.0, "settled back at the top");
}

#[test]
fn a_short_pull_springs_back_without_refreshing() {
    let app = pull(60.0, 90, false);
    assert_eq!(ASKED.with(Cell::get), 0);
    let store = app.store();
    assert!(!store.is_refreshing(list(&app)));
    assert_eq!(store.overscroll(list(&app)), 0.0);
    assert!(!store.is_scroll_moving());
}
