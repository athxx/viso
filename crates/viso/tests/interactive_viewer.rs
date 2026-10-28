//! An interactive viewer driven through the facade: a double click zooms it
//! in about where it lands and the frames glide it there, a trackpad's pinch
//! zooms it about the pointer, and a wheel pans it.
#![cfg(feature = "headless")]

use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    GesturePhase, Modifiers, PointerButtons, PointerPhase, RawEvent, RawGesture, RawPointer,
    RawScroll, ScrollPhase, WindowId,
};
use viso::prelude::*;
use viso::ui::{BoxStyle, LeafStyle, NodeId, Sizing, Vec2, WindowConfig};
use viso::widgets::{InteractiveViewerStyle, interactive_viewer};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

/// A window-filling viewer over content that fills it.
struct Photo;

impl Application for Photo {
    fn new(_cx: &mut AppCx) -> Self {
        Photo
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        interactive_viewer(InteractiveViewerStyle::default())
            .label("Photo")
            .content(|cx| {
                cx.leaf(LeafStyle {
                    size: Sizing::fill(),
                    style: BoxStyle::NONE,
                });
            })
            .build(cx);
    }
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

fn gesture(scale: f64, phase: GesturePhase) -> RawEvent {
    RawEvent::Gesture(RawGesture {
        window: WIN,
        x: 100.0,
        y: 50.0,
        scale,
        rotation: 0.0,
        dx: 0.0,
        dy: 0.0,
        modifiers: Modifiers::default(),
        phase,
    })
}

fn run(script: Vec<RawEvent>) -> DrivenApp<Photo> {
    let mut all = vec![redraw(), redraw()];
    all.extend(script);
    // The frames that carry a glide home.
    all.extend(std::iter::repeat_n(redraw(), 60));
    drive_scripted::<Photo>(all, STEP)
}

fn viewer(app: &DrivenApp<Photo>) -> NodeId {
    app.root().unwrap()
}

#[test]
fn a_double_click_zooms_in_about_where_it_lands_and_another_zooms_back() {
    let at = (60.0, 40.0);
    let click = || {
        vec![
            mouse(at.0, at.1, PointerButtons::NONE, PointerPhase::Moved),
            mouse(at.0, at.1, PointerButtons::PRIMARY, PointerPhase::Down),
            mouse(at.0, at.1, PointerButtons::PRIMARY, PointerPhase::Up),
            redraw(),
        ]
    };
    let app = run([click(), click()].concat());
    let node = viewer(&app);
    assert_eq!(app.store().viewer_scale(node), Some(2.5));
    let pan = app.store().viewer_pan(node).unwrap();
    let kept = Vec2::new(60.0, 40.0) * (1.0 - 2.5);
    assert!((pan - kept).length() < 0.01, "{pan:?}");

    let app = run([click(), click(), click(), click()].concat());
    assert_eq!(app.store().viewer_scale(viewer(&app)), Some(1.0));
    assert_eq!(app.store().viewer_pan(viewer(&app)), Some(Vec2::ZERO));
}

#[test]
fn a_trackpad_pinch_zooms_and_a_wheel_pans() {
    let wheel = RawEvent::Scroll(RawScroll {
        window: WIN,
        x: 100.0,
        y: 50.0,
        delta_x: 0.0,
        delta_y: 30.0,
        modifiers: Modifiers::default(),
        phase: ScrollPhase::None,
    });
    let app = run(vec![
        gesture(1.0, GesturePhase::Began),
        gesture(2.0, GesturePhase::Changed),
        gesture(1.0, GesturePhase::Ended),
        redraw(),
        wheel,
        redraw(),
    ]);
    let node = viewer(&app);
    assert_eq!(app.store().viewer_scale(node), Some(2.0));
    let pan = app.store().viewer_pan(node).unwrap();
    let zoomed = Vec2::new(100.0, 50.0) * (1.0 - 2.0);
    assert!(
        (pan - (zoomed - Vec2::new(0.0, 30.0))).length() < 0.01,
        "{pan:?}"
    );
}
