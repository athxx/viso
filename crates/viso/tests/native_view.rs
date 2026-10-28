//! Native views over nodes: a platform view hears where its node is after
//! layout — moved by a scroll, cut by the viewport's clip, gone once it
//! scrolls away — goes with the node, and reports back from any thread.
#![cfg(feature = "headless")]

use std::cell::RefCell;
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{Modifiers, RawEvent, RawScroll, ScrollPhase, WindowId};
use viso::prelude::*;
use viso::ui::native::{NativePlace, NativeSender, NativeView};
use viso::ui::{Axis, FlexStyle, LeafStyle, Length, ScrollStyle, Sizing};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

thread_local! {
    static PLACES: RefCell<Vec<NativePlace>> = const { RefCell::new(Vec::new()) };
    static DROPPED: RefCell<u32> = const { RefCell::new(0) };
    static HEARD: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
    static SENDER: RefCell<Option<NativeSender<u32>>> = const { RefCell::new(None) };
}

struct Recorder;

impl NativeView for Recorder {
    fn place(&mut self, place: &NativePlace) {
        PLACES.with(|p| p.borrow_mut().push(*place));
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        DROPPED.with(|d| *d.borrow_mut() += 1);
    }
}

/// A viewport scrolling a 200-point spacer, then a 100-point box with a
/// native view, then room to scroll it away.
struct HostApp;

impl Application for HostApp {
    fn new(_cx: &mut AppCx) -> Self {
        HostApp
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
                size: Sizing::fill(),
                ..Default::default()
            },
            |cx| {
                cx.flex(
                    FlexStyle {
                        axis: Axis::Column,
                        size: Sizing {
                            width: Length::Fill { weight: 1.0 },
                            height: Length::Fixed(5000.0),
                        },
                        ..FlexStyle::default()
                    },
                    |cx| {
                        cx.leaf(LeafStyle {
                            size: Sizing::fixed(100.0, 200.0),
                            ..Default::default()
                        });
                        let host = cx.leaf(LeafStyle {
                            size: Sizing::fixed(100.0, 100.0),
                            ..Default::default()
                        });
                        cx.native_view(host, Recorder);
                        let sender = cx.native_events(host, |n: u32, _| {
                            HEARD.with(|h| h.borrow_mut().push(n));
                        });
                        // Reports from another thread, before the first frame.
                        let away = sender.clone();
                        std::thread::spawn(move || {
                            assert!(away.send(7));
                            assert!(away.send(8));
                        })
                        .join()
                        .unwrap();
                        SENDER.with(|s| *s.borrow_mut() = Some(sender));
                    },
                );
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn wheel(dy: f64) -> RawEvent {
    RawEvent::Scroll(RawScroll {
        window: WIN,
        x: 10.0,
        y: 10.0,
        delta_x: 0.0,
        delta_y: dy,
        modifiers: Modifiers::default(),
        phase: ScrollPhase::None,
    })
}

fn run(mut script: Vec<RawEvent>) -> (DrivenApp<HostApp>, Vec<NativePlace>) {
    PLACES.with(|p| p.borrow_mut().clear());
    HEARD.with(|h| h.borrow_mut().clear());
    script.insert(0, redraw());
    script.insert(0, redraw());
    script.push(redraw());
    let app = drive_scripted::<HostApp>(script, STEP);
    (app, PLACES.with(|p| p.borrow().clone()))
}

#[test]
fn a_native_view_hears_its_place_once_until_it_moves() {
    let (_app, places) = run(vec![redraw()]);
    assert_eq!(places.len(), 1, "{places:?}");
    let first = places[0];
    assert!(first.shown);
    assert_eq!(
        (first.frame.y, first.frame.w, first.frame.h),
        (200.0, 100.0, 100.0)
    );
    assert_eq!(first.clip, first.frame);
}

#[test]
fn a_scroll_moves_it_and_clips_it_and_hides_it_when_gone() {
    let (app, places) = run(vec![wheel(250.0), redraw()]);
    let scrolled = app.store().scroll(app.root().unwrap()).y;
    assert!(scrolled > 200.0, "{scrolled}");
    let last = *places.last().unwrap();
    assert_eq!(last.frame.y, 200.0 - scrolled);
    if scrolled < 300.0 {
        assert!(last.shown);
        assert_eq!(last.clip.y, 0.0);
        assert_eq!(last.clip.h, last.frame.h + last.frame.y);
    }

    let (_app, places) = run(vec![wheel(2000.0), redraw()]);
    let last = *places.last().unwrap();
    assert!(!last.shown);
    assert!(last.clip.is_empty());
}

#[test]
fn reports_reach_the_handler_and_the_view_goes_with_its_node() {
    DROPPED.with(|d| *d.borrow_mut() = 0);
    let (app, _) = run(vec![redraw()]);
    assert_eq!(HEARD.with(|h| h.borrow().clone()), [7, 8]);
    let sender = SENDER.with(|s| s.borrow_mut().take()).unwrap();
    assert!(sender.is_open());
    drop(app);
    assert_eq!(DROPPED.with(|d| *d.borrow()), 1);
    assert!(!sender.is_open());
    assert!(!sender.send(9));
}
