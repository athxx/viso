//! A full-screen host (mobile, a browser tab) draws no caption and keeps the
//! app's content clear of system UI: the facade pads the window root by the
//! reported safe area, and its bottom edge by the on-screen keyboard when
//! that is taller.

use std::time::Duration;

use viso::__test_support::{drive_scripted, drive_scripted_full_screen};
use viso::platform::{Insets, RawEvent, WindowId};
use viso::prelude::*;
use viso::ui::{NodeId, NodeStore, Size};

const STEP: Duration = Duration::from_millis(16);
const WINDOW: WindowId = WindowId(1);

struct BareApp;

impl Application for BareApp {
    fn new(_cx: &mut AppCx) -> Self {
        BareApp
    }
    fn build(&mut self, cx: &mut BuildCx<'_>) {
        cx.flex(
            viso::ui::FlexStyle {
                size: Size::fill(),
                ..viso::ui::FlexStyle::default()
            },
            |_cx| {},
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WINDOW }
}

fn resize() -> RawEvent {
    RawEvent::Resized {
        window: WINDOW,
        width: 400,
        height: 800,
    }
}

fn safe_area() -> RawEvent {
    RawEvent::SafeAreaChanged {
        window: WINDOW,
        insets: Insets {
            top: 47.0,
            left: 0.0,
            bottom: 34.0,
            right: 0.0,
        },
    }
}

fn only_child(store: &NodeStore, parent: NodeId) -> NodeId {
    let arena = store.arena();
    let child = arena
        .links(parent)
        .and_then(|l| l.first_child)
        .expect("the root wraps the app's content");
    assert!(arena.links(child).and_then(|l| l.next_sibling).is_none());
    child
}

#[test]
fn content_is_inset_by_the_safe_area_without_a_caption() {
    let app = drive_scripted_full_screen::<BareApp>(vec![resize(), safe_area(), redraw()], STEP);
    let store = app.store();
    let root = app.root().expect("launch window declares a root");
    let body = store.world(only_child(store, root));
    assert!(store.semantics(root).is_none(), "no caption band");
    assert_eq!(body.y, 47.0);
    assert_eq!(body.h, 800.0 - 47.0 - 34.0);
}

#[test]
fn the_keyboard_raises_the_bottom_edge() {
    let keyboard = RawEvent::KeyboardInsetChanged {
        window: WINDOW,
        height: 300.0,
    };
    let app = drive_scripted_full_screen::<BareApp>(
        vec![resize(), safe_area(), redraw(), keyboard, redraw()],
        STEP,
    );
    let store = app.store();
    let root = app.root().expect("launch window declares a root");
    let body = store.world(only_child(store, root));
    assert_eq!(body.y, 47.0);
    assert_eq!(body.h, 800.0 - 47.0 - 300.0);
}

#[test]
fn a_framed_window_ignores_the_safe_area() {
    let app = drive_scripted::<BareApp>(vec![safe_area(), redraw()], STEP);
    let store = app.store();
    let root = app.root().expect("launch window declares a root");
    // The desktop wrap: caption band first, body second.
    let caption = store.arena().links(root).and_then(|l| l.first_child);
    assert!(caption.is_some_and(|c| store.semantics(c).is_some()));
}
