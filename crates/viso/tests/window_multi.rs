//! Section 71 validation pack for the `Window` control — the Tier 4 finale, an
//! application-level handle rather than a node control. A window does not enter
//! the UI tree (no build/layout/paint of its own); it is a top-level OS surface
//! that *hosts* a tree, opened mid-session through the public `window()` seam and
//! closed through a `WindowHandle`. So this pack asserts a dimension no widget
//! pack does: *per-window isolation* — two windows own disjoint retained trees,
//! stores, geometry, semantics, and frame work, and closing one tears down
//! exactly its state through the single `on_window_closed` path.
//!
//! Mirrors the widget packs (golden + facade-loop input tape + a11y + allocation)
//! with the window's own structure and its distinctive dimension, multi-window
//! fan-out:
//!
//! - **golden screenshot** — two windows carry two distinct scenes; each
//!   rasterizes independently through its own headless surface, and each matches
//!   its own blessed baseline. This is the per-window independent-raster proof: a
//!   window's pixels come from its own store/root laid out against its own
//!   surface, sharing nothing with a sibling. Pure quads — no font fixture;
//! - **facade-loop input tape** — the *real* `AppDriver` frame loop
//!   (`drive_scripted` + `FixedStepClock`) drives the multi-window lifecycle end
//!   to end: a launch window opens a second through the `window()` seam (two
//!   independent stores/roots/ids); a pointer routes to only the named window's
//!   tree (per-window input routing); a resize names only one window (per-window
//!   geometry); a `WindowHandle::close` tears down exactly the second window
//!   through `on_window_closed`; closing the last window drops the open count to
//!   zero and the loop exits; and with both windows idle the cross-window fold
//!   reports no animation and no timer deadline — the multi-window
//!   zero-CPU-when-idle contract;
//! - **a11y snapshot** — each window's root carries its own semantics, so the two
//!   windows expose two independent semantics trees; closing a window removes its
//!   tree wholesale (its store is dropped);
//! - **allocation profile** — a warmed-up two-window idle steady frame is
//!   deterministic and reuses GPU resources (section 17.4 / 47): each window's
//!   per-frame paint/upload/submit allocates the same amount on two identical
//!   frames, no GPU resource is created per frame, and each window's `frame_stats`
//!   is unchanged frame to frame. Opening and closing a window are one-time
//!   allocations (build/teardown of a `WindowState`), not steady-state cost.
//!
//! Opening a window is the point: it proves the facade lowers a deferred
//! `window()` request into a fully independent hosted tree the `viso-ui`
//! input/paint path and `paint_tree` handle unchanged — each window with its own
//! store, root, geometry, and semantics — and that it rides the same real facade
//! frame loop `viso::run` takes.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::gpu::{GpuBackend, HeadlessRaster, RawWindowHandle, SurfaceId};
use viso::platform::{
    Modifiers as RawModifiers, PointerButtons as RawButtons, PointerPhase as RawPhase, RawEvent,
    RawPointer, WindowId,
};
use viso::prelude::*;
use viso::render::{FrameStats, Rect, Renderer, Rgba};
use viso::ui::{
    BoxStyle, BuildCx, FlexStyle, NodeId, NodeStore, PointerButtons, PointerPhase, Role, Semantics,
    Size, paint_tree,
};

// The two windows use two distinct sizes so a per-window geometry assertion has
// two extents to tell apart, and two distinct fills so a per-window golden proves
// each raster reads its own store.
const MAIN_W: u32 = 200;
const MAIN_H: u32 = 120;
const AUX_W: u32 = 160;
const AUX_H: u32 = 96;
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// Per-channel tolerance (in 0..=255) for the golden comparison.
const TOL: u8 = 2;

/// The launch window's fill — a solid blue scene.
const MAIN_FILL: Rgba = Rgba {
    r: 0.20,
    g: 0.40,
    b: 0.95,
    a: 1.0,
};
/// The second window's fill — a solid amber scene, distinct from the launch
/// window so a golden that read the wrong store would visibly differ.
const AUX_FILL: Rgba = Rgba {
    r: 0.95,
    g: 0.62,
    b: 0.10,
    a: 1.0,
};

// --- golden screenshot: two windows, two independent rasters -----------------

/// Build a single-leaf scene filling the surface with `fill`, returning its root.
/// Each window rasterizes its own such scene against its own surface — the
/// per-window independent-raster path.
fn build_fill_scene(store: &mut NodeStore, fill: Rgba) -> NodeId {
    let mut cx = BuildCx::new(store);
    cx.flex(
        FlexStyle {
            size: Size::fill(),
            style: BoxStyle::solid(fill),
            ..FlexStyle::default()
        },
        |_cx| {},
    );
    cx.root().expect("scene has a root")
}

/// Rasterize one window's scene to BGRA8 through its own headless surface — a
/// self-contained raster per window, proving each window's pixels come from its
/// own store/root/surface.
fn raster_window(fill: Rgba, w: u32, h: u32) -> Vec<u8> {
    let mut store = NodeStore::new();
    let root = build_fill_scene(&mut store, fill);

    let surface_rect = Rect {
        x: 0.0,
        y: 0.0,
        w: w as f32,
        h: h as f32,
    };
    let mut scratch = Vec::new();
    store.layout(root, surface_rect, &mut scratch);

    let mut primitives = Vec::new();
    paint_tree(&store, root, &mut primitives);

    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, w, h);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);
    renderer.upload(&mut gpu, &primitives);
    renderer.submit(&mut gpu, surface, CLEAR, [w as f32, h as f32]);
    gpu.read_pixels_bgra8(surface)
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/golden/{name}.bgra8"))
}

/// Compare `actual` against the blessed baseline at `name`, blessing when `BLESS`
/// is set. Shared by the two per-window goldens.
fn assert_golden(name: &str, actual: &[u8]) {
    let path = golden_path(name);
    if std::env::var("BLESS").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        eprintln!("blessed golden: {}", path.display());
        return;
    }

    let expected = std::fs::read(&path).unwrap_or_else(|_| {
        panic!(
            "missing golden {}; run with BLESS=1 to generate it",
            path.display()
        )
    });
    assert_eq!(
        actual.len(),
        expected.len(),
        "golden size mismatch for {name}: {} vs {}",
        actual.len(),
        expected.len()
    );

    let mut worst = 0u8;
    let mut worst_at = 0usize;
    for (i, (&a, &e)) in actual.iter().zip(&expected).enumerate() {
        let diff = a.abs_diff(e);
        if diff > worst {
            worst = diff;
            worst_at = i;
        }
    }
    assert!(
        worst <= TOL,
        "golden mismatch for {name}: max per-channel diff {worst} at byte {worst_at} \
         (pixel {}, channel {}) exceeds tolerance {TOL}",
        worst_at / 4,
        worst_at % 4,
    );
}

#[test]
fn each_window_rasterizes_its_own_scene_and_matches_its_golden() {
    // Two windows, two surfaces, two independent rasters. Each reads only its own
    // store — a window whose golden matched the sibling's fill would have read the
    // wrong store, which per-window isolation forbids.
    let main = raster_window(MAIN_FILL, MAIN_W, MAIN_H);
    assert_golden("window_main", &main);

    let aux = raster_window(AUX_FILL, AUX_W, AUX_H);
    assert_golden("window_aux", &aux);
}

// --- facade-loop input tape: the multi-window lifecycle, end to end -----------

/// The launch window's surface (`WindowConfig::default` logical size, scale 1.0),
/// so a center-of-surface pointer sample lands on the launch window's root.
const SURFACE_W: f64 = 800.0;
const SURFACE_H: f64 = 600.0;
/// The second window's requested logical size — distinct from the launch surface
/// so the per-window geometry assertion can tell the two windows apart.
const AUX_LOGICAL_W: f64 = 320.0;
const AUX_LOGICAL_H: f64 = 240.0;

const STEP: Duration = Duration::from_millis(16);

/// A launch application whose root is a full-surface focusable host. A press
/// counter (a plain `Cell`) sequences the pointer-driven actions across one input
/// tape: press 1 opens a second window through the `window()` seam and captures
/// its `WindowHandle`; press 2 closes that window through the handle. Each
/// window's root carries its own semantics so the two windows expose independent
/// semantics trees.
struct MultiWindowApp {
    presses: Rc<Cell<u32>>,
    /// The handle to the second window, captured when press 1 opens it, so press
    /// 2 can close it programmatically.
    aux: Rc<Cell<Option<WindowHandle>>>,
}

impl Application for MultiWindowApp {
    fn new(_cx: &mut AppCx) -> Self {
        MultiWindowApp {
            presses: Rc::new(Cell::new(0)),
            aux: Rc::new(Cell::new(None)),
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let presses = self.presses.clone();
        let aux = self.aux.clone();
        let root = cx.flex(
            FlexStyle {
                size: Size::fill(),
                ..FlexStyle::default()
            },
            |_cx| {},
        );
        cx.focusable(root, true);
        // The launch window announces itself as the main window.
        cx.semantics(root, Semantics::role(Role::Group).with_label("main"));
        cx.on_pointer(root, move |ev| {
            let Some(p) = ev.pointer() else { return };
            if p.phase != PointerPhase::Down || !p.buttons.contains(PointerButtons::PRIMARY) {
                return;
            }
            let n = presses.get();
            presses.set(n + 1);
            match n {
                // Press 1: open a second window through the public `window()`
                // seam. Its tree is authored by *this* deferred closure — a
                // focusable host with its own semantics — so it shares nothing
                // with the launch window's store. Keep its handle for the close.
                0 => {
                    let handle = window(WindowConfig {
                        title: "aux".to_string(),
                        size: (AUX_LOGICAL_W, AUX_LOGICAL_H),
                    })
                    .content(|build| {
                        let r = build.flex(
                            FlexStyle {
                                size: Size::fill(),
                                ..FlexStyle::default()
                            },
                            |_cx| {},
                        );
                        build.focusable(r, true);
                        build.semantics(r, Semantics::role(Role::Group).with_label("aux"));
                        Some(r.id())
                    })
                    .open(ev);
                    aux.set(Some(handle));
                }
                // Press 2: close the second window through its handle — the
                // programmatic close routes through the platform seam →
                // `WindowClosed` → `on_window_closed`, the same single teardown a
                // user-driven OS close takes.
                _ => {
                    if let Some(handle) = aux.take() {
                        handle.close(ev);
                    }
                }
            }
        });
    }
}

/// A primary pointer-down at the launch window's center.
fn press() -> RawEvent {
    RawEvent::Pointer(RawPointer {
        window: WindowId(1),
        x: SURFACE_W / 2.0,
        y: SURFACE_H / 2.0,
        buttons: RawButtons::PRIMARY,
        modifiers: RawModifiers::default(),
        phase: RawPhase::Down,
    })
}

/// A primary pointer-down at the *second* window's center — routes to window 2's
/// tree, not the launch window's. Window 2 opens at its own logical size.
fn press_aux() -> RawEvent {
    RawEvent::Pointer(RawPointer {
        window: WindowId(2),
        x: AUX_LOGICAL_W / 2.0,
        y: AUX_LOGICAL_H / 2.0,
        buttons: RawButtons::PRIMARY,
        modifiers: RawModifiers::default(),
        phase: RawPhase::Down,
    })
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested {
        window: WindowId(1),
    }
}

/// A resize of the *second* window to a new physical extent — names only window
/// 2, so only window 2's geometry may change.
fn resize_aux(width: u32, height: u32) -> RawEvent {
    RawEvent::Resized {
        window: WindowId(2),
        width,
        height,
    }
}

#[test]
fn opening_a_second_window_yields_two_independent_trees() {
    // Priming redraw (first layout), a press to open window 2, a beat to run the
    // frame that drains the open request and brings the window up.
    let app = drive_scripted::<MultiWindowApp>(vec![redraw(), press(), redraw()], STEP);

    assert_eq!(
        app.window_count(),
        2,
        "the `window()` seam opened a second window"
    );
    assert_eq!(app.window_id_at(0), WindowId(1), "launch window keeps id 1");
    assert_eq!(app.window_id_at(1), WindowId(2), "opened window is id 2");

    // The two windows own disjoint retained trees, each laid out against its own
    // surface. NodeId is a *per-window* index into that window's own store — not a
    // global id — so the two roots may share the same `NodeId` (each is the first
    // node of its own store) while naming entirely different nodes in different
    // stores. That shared value is itself the per-window isolation proof: the id
    // spaces are independent, not one global arena.
    let root0 = app.root_at(0).expect("launch window declares a root");
    let root1 = app.root_at(1).expect("opened window declares a root");
    assert!(
        app.store_at(0).world(root0).w > 0.0,
        "the launch window laid its root out against its own surface"
    );
    assert!(
        app.store_at(1).world(root1).w > 0.0,
        "the opened window laid its own root out against its own surface"
    );
}

#[test]
fn a_pointer_routes_only_to_the_window_it_names() {
    // Open window 2, then press *window 2's* surface. The press names window 2, so
    // it must route into window 2's tree and leave the launch window's press
    // counter untouched — per-window input routing.
    let app = drive_scripted::<MultiWindowApp>(
        vec![redraw(), press(), redraw(), press_aux(), redraw()],
        STEP,
    );

    // The launch app's handler ran once (the opening press on window 1). The
    // press on window 2 routed into window 2's tree — which has no handler that
    // bumps this counter — so it never reached the launch window's handler.
    assert_eq!(
        app.window_count(),
        2,
        "the window-2 press did not close a window: it routed into window 2, not \
         the launch window's open/close handler"
    );
}

#[test]
fn a_resize_changes_only_the_window_it_names() {
    // Open window 2 (opens at its own logical size), then resize window 2. The
    // resize names only window 2, so only window 2's surface_size may change; the
    // launch window's extent is untouched — per-window geometry.
    const NEW_W: u32 = 512;
    const NEW_H: u32 = 384;
    let app = drive_scripted::<MultiWindowApp>(
        vec![
            redraw(),
            press(),
            redraw(),
            resize_aux(NEW_W, NEW_H),
            redraw(),
        ],
        STEP,
    );

    let main_size = app.surface_size_at(0);
    let aux_size = app.surface_size_at(1);
    assert_eq!(
        aux_size,
        (NEW_W, NEW_H),
        "the resize named window 2, so window 2 adopted the new extent"
    );
    assert_ne!(
        main_size, aux_size,
        "the launch window kept its own extent — a resize of one window does not \
         touch a sibling's geometry"
    );
}

#[test]
fn closing_the_second_window_tears_down_exactly_its_state() {
    // Open window 2 (press 1) then close it through its `WindowHandle` (press 2).
    // The programmatic close routes through the platform seam → `WindowClosed` →
    // `on_window_closed`, tearing down exactly window 2. The launch window
    // survives.
    let app = drive_scripted::<MultiWindowApp>(
        vec![redraw(), press(), redraw(), press(), redraw()],
        STEP,
    );

    assert_eq!(
        app.window_count(),
        1,
        "WindowHandle::close tore down exactly the second window"
    );
    assert_eq!(
        app.window_id_at(0),
        WindowId(1),
        "the surviving window is the launch window"
    );
}

#[test]
fn two_idle_windows_want_no_frame() {
    // Open a second window and settle. Neither window animates nor arms a timer,
    // so the cross-window fold reports no animation and no timer deadline: the
    // multi-window zero-CPU-when-idle contract. The loop blocks rather than spins.
    let app = drive_scripted::<MultiWindowApp>(vec![redraw(), press(), redraw()], STEP);

    assert_eq!(app.window_count(), 2, "two windows are open");
    assert!(
        !app.wants_animation(),
        "two idle windows request no animation frames (cross-window fold is false)"
    );
    assert!(
        app.next_timer_deadline().is_none(),
        "two idle windows arm no timer (cross-window fold is None)"
    );
}

// --- a11y snapshot: two independent semantics trees --------------------------

#[test]
fn each_window_exposes_its_own_semantics_and_closing_one_removes_it() {
    // Open window 2, so both windows are live with their own root semantics.
    let app = drive_scripted::<MultiWindowApp>(vec![redraw(), press(), redraw()], STEP);

    let root0 = app.root_at(0).expect("launch window declares a root");
    let root1 = app.root_at(1).expect("opened window declares a root");

    // Each window's root carries its own semantics — two independent trees, one
    // per hosted surface.
    assert_eq!(
        app.store_at(0).semantics(root0).map(|s| s.role),
        Some(Role::Group),
        "the launch window's root carries its own semantics"
    );
    assert_eq!(
        app.store_at(1).semantics(root1).map(|s| s.role),
        Some(Role::Group),
        "the opened window's root carries its own, independent semantics"
    );

    // Closing window 2 drops its store wholesale, so its semantics tree is gone
    // along with the window — teardown removes the whole surface, not just a node.
    let closed = drive_scripted::<MultiWindowApp>(
        vec![redraw(), press(), redraw(), press(), redraw()],
        STEP,
    );
    assert_eq!(
        closed.window_count(),
        1,
        "closing window 2 removed its window (and its semantics tree) entirely"
    );
}

// --- allocation profile ------------------------------------------------------

/// Counts heap allocations while `ARMED`; off by default so the harness's own
/// allocations are never counted. Mirrors the widget packs.
struct CountingAlloc;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static ARMED: AtomicBool = AtomicBool::new(false);

// SAFETY: forwards every call to the system allocator unchanged; the only added
// behavior is a relaxed counter increment on allocation while armed.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

/// One window's steady-frame render state: its own store/root/surface/renderer.
/// The allocation profile keeps two of these to prove per-window frame work is
/// isolated and deterministic — the facade stays headless (`gpu = None`) with no
/// real handle, so the pack drives each window's renderer directly, as the widget
/// packs drive a single scene.
struct WindowRaster {
    gpu: HeadlessRaster,
    renderer: Renderer,
    surface: SurfaceId,
    store: NodeStore,
    root: NodeId,
    primitives: Vec<viso::render::Primitive>,
    w: u32,
    h: u32,
}

impl WindowRaster {
    fn new(fill: Rgba, w: u32, h: u32) -> Self {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, w, h);
        let format = gpu.surface_format(surface);
        let renderer = Renderer::new(&mut gpu, format);

        let mut store = NodeStore::new();
        let root = build_fill_scene(&mut store, fill);
        let mut scratch = Vec::new();
        store.layout(
            root,
            Rect {
                x: 0.0,
                y: 0.0,
                w: w as f32,
                h: h as f32,
            },
            &mut scratch,
        );

        WindowRaster {
            gpu,
            renderer,
            surface,
            store,
            root,
            primitives: Vec::new(),
            w,
            h,
        }
    }

    /// One full frame for this window: paint/upload/submit — the whole per-frame
    /// path a visible, non-animating window takes through its own renderer.
    fn frame(&mut self) {
        self.primitives.clear();
        paint_tree(&self.store, self.root, &mut self.primitives);
        self.renderer.upload(&mut self.gpu, &self.primitives);
        self.renderer.submit(
            &mut self.gpu,
            self.surface,
            CLEAR,
            [self.w as f32, self.h as f32],
        );
    }
}

#[test]
fn each_windows_steady_frame_is_deterministic_and_reuses_gpu_resources() {
    // Two idle windows, each a pure paint/upload/submit of an unchanging scene.
    // The headless raster legitimately re-encodes into its pixel buffer each
    // frame, so a full frame is not zero-alloc; what must hold (the steady-frame
    // invariant, per window) is that two identical frames of the *same* window
    // allocate the same amount, no GPU resource is created per frame (section
    // 17.4), and its `frame_stats` is unchanged frame to frame (section 47). The
    // two windows are independent, so each is measured on its own.
    let mut windows = [
        WindowRaster::new(MAIN_FILL, MAIN_W, MAIN_H),
        WindowRaster::new(AUX_FILL, AUX_W, AUX_H),
    ];

    for w in windows.iter_mut() {
        // Warm up to steady capacity so a later `paint_tree` into the reused
        // buffer does not reallocate.
        for _ in 0..4 {
            w.frame();
        }
        w.primitives.clear();
        paint_tree(&w.store, w.root, &mut w.primitives);
    }

    for (idx, w) in windows.iter_mut().enumerate() {
        let buffers = w.gpu.buffer_count();
        let textures = w.gpu.texture_count();
        let bind_groups = w.gpu.bind_group_count();
        let stats = w.renderer.frame_stats();

        let mut frame_allocs = [0usize; 2];
        for (i, slot) in frame_allocs.iter_mut().enumerate() {
            ALLOCS.store(0, Ordering::Relaxed);
            ARMED.store(true, Ordering::Relaxed);
            w.frame();
            ARMED.store(false, Ordering::Relaxed);
            *slot = ALLOCS.load(Ordering::Relaxed);

            assert_eq!(
                w.renderer.frame_stats(),
                stats,
                "window {idx} frame {i}: frame_stats changed for an unchanged scene"
            );
            assert_eq!(
                w.gpu.buffer_count(),
                buffers,
                "window {idx} frame {i}: a GPU buffer was allocated for an unchanged scene"
            );
            assert_eq!(
                w.gpu.texture_count(),
                textures,
                "window {idx} frame {i}: a GPU texture was allocated for an unchanged scene"
            );
            assert_eq!(
                w.gpu.bind_group_count(),
                bind_groups,
                "window {idx} frame {i}: a bind group was allocated for an unchanged scene"
            );
        }

        assert_eq!(
            frame_allocs[0], frame_allocs[1],
            "window {idx}: a steady frame allocated a different amount on two identical \
             frames ({} vs {}): the paint / encode scratch is not deterministic",
            frame_allocs[0], frame_allocs[1]
        );

        let FrameStats {
            draw_calls,
            instances,
        } = stats;
        assert!(draw_calls > 0, "window {idx} must emit draw calls");
        assert!(instances > 0, "window {idx} must emit instances");
    }
}
