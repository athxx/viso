//! Scroll bars driven through the facade: a window's viewport builds its bar
//! in the system's style, a wheel shows it, and the user asking to always
//! see scroll bars gives them their room beside the content.
#![cfg(feature = "headless")]

use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{Appearance, Modifiers, RawEvent, RawScroll, ScrollPhase, WindowId};
use viso::prelude::*;
use viso::ui::{Axis, FlexStyle, NodeId, ScrollBarStyle, ScrollStyle, Size};

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
                        size: Size {
                            width: viso::ui::Length::Fill { weight: 1.0 },
                            height: viso::ui::Length::Fixed(5000.0),
                        },
                        ..FlexStyle::default()
                    },
                    |_| {},
                );
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn run(mut script: Vec<RawEvent>) -> DrivenApp<ListApp> {
    script.insert(0, redraw());
    script.insert(0, redraw());
    script.push(redraw());
    drive_scripted::<ListApp>(script, STEP)
}

fn content(app: &DrivenApp<ListApp>) -> NodeId {
    let root = app.root().unwrap();
    let store = app.store();
    store
        .arena()
        .children(root)
        .find(|&c| !store.is_chrome(c))
        .unwrap()
}

#[test]
fn a_viewport_wears_the_systems_bar_and_a_wheel_shows_it() {
    let app = run(vec![]);
    let root = app.root().unwrap();
    let store = app.store();
    assert_eq!(store.scroll_bar_style(), ScrollBarStyle::native(false));
    assert!(store.scroll_thumb(root).is_some());
    assert!(!store.scroll_bar_shown(root));
    assert_eq!(store.bounds(content(&app)).w, store.bounds(root).w);

    let app = run(vec![
        RawEvent::Scroll(RawScroll {
            window: WIN,
            x: 10.0,
            y: 10.0,
            delta_x: 0.0,
            delta_y: 120.0,
            modifiers: Modifiers::default(),
            phase: ScrollPhase::None,
        }),
        redraw(),
    ]);
    let root = app.root().unwrap();
    assert!(app.store().scroll(root).y > 0.0);
    assert!(app.store().scroll_bar_shown(root));
}

#[test]
fn always_shown_scroll_bars_keep_their_room() {
    let app = run(vec![
        RawEvent::AppearanceChanged(Appearance {
            overlay_scroll_bars: false,
            ..Appearance::default()
        }),
        redraw(),
    ]);
    let root = app.root().unwrap();
    let store = app.store();
    let classic = ScrollBarStyle::classic(false);
    assert_eq!(store.scroll_bar_style(), classic);
    assert_eq!(
        store.bounds(content(&app)).w,
        store.bounds(root).w - classic.track
    );
    assert!(
        store.scroll_bar_shown(root),
        "while there is anything to scroll"
    );
}
