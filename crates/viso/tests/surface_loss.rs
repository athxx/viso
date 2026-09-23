//! A mobile host takes the window's native surface away in the background
//! and hands a new one back on return. The retained tree survives the gap and
//! lays out against whatever size the new surface reports.

use std::time::Duration;

use viso::__test_support::drive_scripted_full_screen;
use viso::platform::{RawEvent, WindowId};
use viso::prelude::*;
use viso::ui::Size;

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

fn resize(width: u32, height: u32) -> RawEvent {
    RawEvent::Resized {
        window: WINDOW,
        width,
        height,
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WINDOW }
}

#[test]
fn the_tree_outlives_its_surface_and_follows_the_new_one() {
    let app = drive_scripted_full_screen::<BareApp>(
        vec![
            resize(400, 800),
            redraw(),
            RawEvent::SurfaceDestroyed { window: WINDOW },
            RawEvent::SurfaceCreated { window: WINDOW },
            resize(800, 400),
            redraw(),
        ],
        STEP,
    );
    let root = app.root().expect("the root survives the surface loss");
    let bounds = app.store().world(root);
    assert_eq!((bounds.w, bounds.h), (800.0, 400.0));
    assert_eq!(app.surface_size_at(0), (800, 400));
}
