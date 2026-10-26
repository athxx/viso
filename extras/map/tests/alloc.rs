//! The allocation profile of a moving map: once its tiles, image tiles,
//! shapes, markers and puck are loaded and placed, a frame of a drag that
//! pans or turns it allocates nothing in the view's own work (its pointer
//! handler and its structure hook). A counting allocator arms around just
//! that work.

mod support;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use viso::__test_support::{drive_scripted, probe_frames};
use viso::platform::{Modifiers, PointerButtons, PointerPhase, RawEvent, RawPointer, WindowId};
use viso::prelude::*;
use viso::ui::{BuildCx, Component, WindowConfig};
use viso_map::style::{Color, Layer, builtin};
use viso_map::{Camera, LngLat, Marker, Polyline, Puck, RasterSource, TileId, View, map_view};

thread_local! {
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static ARMED: Cell<bool> = const { Cell::new(false) };
}

/// Counts the allocations of the arming thread while it is armed; the
/// rest pass through untouched.
struct Counting;

// SAFETY: forwards every call to the system allocator unchanged; the only
// added behavior is a thread-local counter bumped while armed.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: the caller's contract for `alloc` is passed on as is.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `System` through this allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        // SAFETY: `ptr` came from `System` through this allocator.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

fn count() {
    // `try_with`: the thread may be tearing its locals down.
    let _ = ARMED.try_with(|a| {
        if a.get() {
            let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        }
    });
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);
const W: f64 = 640.0;
const H: f64 = 480.0;

type Setup = Box<dyn Fn(&mut BuildCx<'_>)>;

thread_local! {
    static SETUP: RefCell<Option<Setup>> = const { RefCell::new(None) };
}

struct MapApp;

impl Application for MapApp {
    fn new(_: &mut AppCx) -> Self {
        MapApp
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            caption: false,
            size: (W, H),
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        SETUP.with(|s| (s.borrow().as_ref().expect("a setup"))(cx));
    }
}

fn mouse(x: f64, y: f64, phase: PointerPhase, alt: bool) -> RawEvent {
    let buttons = if phase == PointerPhase::Up {
        PointerButtons::NONE
    } else {
        PointerButtons::PRIMARY
    };
    let m = Modifiers {
        alt,
        ..Modifiers::default()
    };
    RawEvent::Pointer(RawPointer::mouse(WIN, x, y, buttons, m, phase))
}

fn image(tile: TileId) -> Vec<u8> {
    let c = if (tile.x + tile.y).is_multiple_of(2) {
        [200, 80, 40, 255]
    } else {
        [40, 200, 80, 255]
    };
    support::png(256, 256, &c.repeat(256 * 256))
}

/// The view's allocations in each frame of a drag by `step` points a frame
/// (`alt` turning the map), once everything has loaded.
fn drag_allocations(alt: bool, step: [f64; 2]) -> Vec<usize> {
    let start = Camera::new(LngLat::new(13.40, 52.52), 13.0).with_bearing(15.0);
    let view = View::new(start, [W, H]);
    let route: Vec<LngLat> = [[100.0, 400.0], [320.0, 240.0], [560.0, 90.0]]
        .iter()
        .map(|&p| view.unproject(p))
        .collect();
    let pinned = view.unproject([400.0, 200.0]);
    let here = view.unproject([300.0, 260.0]);
    SETUP.with(|setup| {
        let route = route.clone();
        *setup.borrow_mut() = Some(Box::new(move |cx| {
            let mut style = builtin::openmaptiles_light();
            let at = style
                .layers
                .iter()
                .position(|l| l.id.starts_with("road"))
                .unwrap_or(style.layers.len());
            style
                .layers
                .insert(at, Layer::raster("photo", "photo").opacity(0.4));
            map_view(|tile: TileId| viso_map::Reply::ready(Ok(Some(support::synthetic(tile)))))
                .camera(start)
                .style(style)
                .raster_source(
                    "photo",
                    RasterSource::new(
                        |tile: TileId| viso_map::Reply::ready(Ok(Some(image(tile)))),
                        0,
                        18,
                    ),
                )
                .shape(Polyline::new(route.clone(), 6.0, Color::rgb(30, 90, 230)).traveled(0.4))
                .marker(Marker::new(1, pinned, |cx: &mut BuildCx<'_>| {
                    cx.leaf(viso::ui::LeafStyle {
                        size: viso::ui::Size::fixed(16.0, 16.0),
                        style: viso::ui::BoxStyle::solid(viso::render::Rgba::new(
                            1.0, 0.0, 0.0, 1.0,
                        )),
                    });
                }))
                .puck(Puck::new(here, 80.0).heading(40.0))
                .build(cx);
        }))
    });
    let per_frame: Rc<RefCell<Vec<usize>>> = Rc::default();
    let log = Rc::clone(&per_frame);
    probe_frames(Some(Box::new(move |_, _| {
        log.borrow_mut().push(ALLOCS.with(|n| n.replace(0)));
    })));
    viso_map::view::probe::set(Some(|on| ARMED.with(|a| a.set(on))));
    let mut script: Vec<RawEvent> = (0..12)
        .map(|_| RawEvent::RedrawRequested { window: WIN })
        .collect();
    script.push(mouse(320.0, 240.0, PointerPhase::Down, alt));
    for i in 1..=16 {
        let i = f64::from(i);
        script.push(mouse(
            320.0 + step[0] * i,
            240.0 + step[1] * i,
            PointerPhase::Moved,
            alt,
        ));
        script.push(RawEvent::RedrawRequested { window: WIN });
    }
    drive_scripted::<MapApp>(script, STEP);
    viso_map::view::probe::set(None);
    probe_frames(None);
    ARMED.with(|a| a.set(false));
    per_frame.take()
}

#[test]
fn a_pan_or_turn_frame_allocates_nothing_in_the_view() {
    // Small steps, so few new tiles come into view: a frame that brings one
    // in allocates its node, the others nothing.
    for (alt, step) in [(false, [1.5, 1.0]), (true, [2.0, 0.0])] {
        let frames = drag_allocations(alt, step);
        assert!(frames.len() > 10, "{frames:?}");
        assert!(frames.iter().any(|&n| n > 0), "loading allocates");
        let tail = &frames[frames.len() - 8..];
        assert!(
            tail.iter().all(|&n| n == 0),
            "alt {alt}: allocations a frame {frames:?}"
        );
    }
}
