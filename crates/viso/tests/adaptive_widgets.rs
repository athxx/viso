//! The adaptive widgets under the frame loop: a window resized across size
//! classes re-arranges an adaptive navigation once the loop settles the
//! environment against its layout.

use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::platform::{RawEvent, WindowId};
use viso::prelude::*;
use viso::ui::{NodeId, Role};
use viso::widgets::adaptive_navigation;

struct App;

impl Application for App {
    fn new(_cx: &mut AppCx) -> Self {
        App
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        adaptive_navigation()
            .destination("Home", |_| {})
            .destination("Search", |_| {})
            .build(cx);
    }
}

/// The width of the navigation's strip after the window resizes to `width`
/// physical pixels, and whether it comes before the content.
fn strip_after(width: u32) -> (f32, bool) {
    let window = WindowId(1);
    let app = drive_scripted::<App>(
        vec![
            RawEvent::RedrawRequested { window },
            RawEvent::Resized {
                window,
                width,
                height: 600,
            },
            RawEvent::RedrawRequested { window },
            RawEvent::RedrawRequested { window },
        ],
        Duration::from_millis(16),
    );
    let store = app.store();
    let root = app.root().expect("a root");
    let children: Vec<NodeId> = store.arena().children(root).collect();
    let at = children
        .iter()
        .position(|&c| store.semantics(c).map(|s| s.role) == Some(Role::TabList))
        .expect("a tablist");
    (store.world(children[at]).w, at == 0)
}

#[test]
fn a_resized_window_re_arranges_an_adaptive_navigation() {
    let (width, first) = strip_after(400);
    assert!(!first && width > 300.0, "a bar below the content: {width}");
    assert_eq!(strip_after(700), (80.0, true), "a rail");
    assert_eq!(strip_after(1000), (240.0, true), "a sidebar");
}
