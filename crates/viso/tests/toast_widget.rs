//! Section 71 validation pack for the `Toast` control — a non-modal, auto-dismissing
//! status overlay layered over the whole scene that neither dims the background nor
//! takes focus, and hides itself once its `duration` elapses — driven through the
//! public facade. Mirrors `sheet_widget.rs` (golden + facade-loop input tape + a11y +
//! allocation) with the toast's own structure and its distinctive dimension, the
//! one-shot timer: the content builds once into an edge-pinned, cross-axis-centered
//! `Fit x Fit` panel, flagged an overlay (top layer) starting hidden. Showing flips the
//! content's `hidden` flag and arms a one-shot `request_timer` whose store-only `on_fire`
//! hides it again when the deadline is crossed; a manual dismiss hides it early and fires
//! `on_dismiss`. Unlike a `Modal` it installs no scrim, takes no focus, and traps no
//! keyboard navigation. All store effects (`hidden`) and the timer arm ride the deferred
//! request seams the router/driver apply after a handler returns.
//!
//! - **golden screenshot** — build a `Toast` pinned to the top edge with a fixed-size,
//!   opaque colored content panel, show it, lay it out over a fixed surface, and confirm
//!   the pixels match a blessed baseline: the panel paints centered on the top edge over
//!   the clear background, proving the overlay shows and the edge-pin/center layout. Pure
//!   quads — no font fixture;
//! - **facade-loop input tape** — the *real* `AppDriver` frame loop (`drive_scripted` +
//!   `FixedStepClock`) drives the timer dimension end to end: a scripted pointer press
//!   asks the app to `show` the toast through its captured `ToastHandle`, revealing the
//!   content and arming the auto-dismiss timer; before the deadline the content is still
//!   shown and `next_timer_deadline` is `Some` (an armed timer costs no frame — it blocks,
//!   it does not spin, so `wants_animation` stays false); after enough `STEP` beats carry
//!   the clock past the deadline, `fire_due` hides the content and the registry empties, so
//!   `next_timer_deadline` is `None` — the timer-side zero-CPU-when-idle contract;
//! - **programmatic input tape** — a captured `ToastHandle`, driven inside an `EventCx` as
//!   the router would run it (draining the deferred `hidden` and timer requests), shows,
//!   auto-dismisses (via a local registry `fire_due` past the deadline), and manually
//!   dismisses: a manual dismiss hides early and fires `on_dismiss` once; a stale
//!   auto-dismiss timer that fires after a manual dismiss (or a re-show) is an
//!   epoch-guarded no-op; dismissing an already-hidden toast is a de-duped no-op;
//! - **a11y snapshot** — the content carries `Role::Status` (a WAI-ARIA `role=status`
//!   polite live region) and is *not* focusable: a toast announces itself without moving
//!   focus, the key non-modal difference from a `Dialog`;
//! - **allocation profile** — a warmed-up shown `Toast` paint frame allocates nothing per
//!   frame (architecture section 47 hot-path contract): the toast has no animation, so a
//!   steady shown frame is a pure paint/upload/submit with reused GPU resources; and the
//!   `fire_due` timer-fire step (the section 28 auto-dismiss tick) allocates nothing —
//!   `fire_due` swap-removes the fired timer and calls a boxed `FnOnce` that flips one
//!   `hidden` flag, touching no heap.
//!
//! Building through `Toast::build` is the point: it proves the widget lowers to a
//! non-modal, edge-pinned overlay subtree the `viso-ui` input/paint path and `paint_tree`
//! handle unchanged — including the top-layer overlay, the deferred `hidden` seam, and the
//! one-shot `request_timer` an auto-dismiss needs — and that it rides the same real facade
//! frame loop `viso::run` takes.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use viso::__test_support::drive_scripted;
use viso::gpu::{GpuBackend, HeadlessRaster, RawWindowHandle};
use viso::platform::{
    Modifiers as RawModifiers, PointerButtons as RawButtons, PointerPhase as RawPhase, RawEvent,
    RawPointer, WindowId,
};
use viso::prelude::*;
use viso::render::{FrameStats, Rect, Renderer, Rgba};
use viso::ui::{
    BindingTable, BoxStyle, BuildCx, Component, EventCx, LeafStyle, Modifiers, NodeId, NodeStore,
    PointerButtons, PointerEvent, PointerPhase, Role, Size, StateStore, TextEdits, TimerRegistry,
    VirtualLists, paint_tree,
};
use viso::widgets::{ToastEdge, ToastHandle, ToastHandleSlot, toast};

const W: u32 = 240;
const H: u32 = 120;
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// Per-channel tolerance (in 0..=255) for the golden comparison.
const TOL: u8 = 2;

/// The toast panel's fill — an opaque notification body so the golden is deterministic
/// regardless of blend order (a real toast panel may be translucent). It paints centered
/// on the top edge over the clear background.
const PANEL: Rgba = Rgba {
    r: 0.16,
    g: 0.42,
    b: 0.30,
    a: 1.0,
};
/// The fixed panel size — a small centered notification, not a full-bleed drawer.
const PANEL_W: f32 = 160.0;
const PANEL_H: f32 = 32.0;

/// The auto-dismiss duration used across the pack.
const DURATION: Duration = Duration::from_millis(64);

fn surface_rect() -> Rect {
    Rect {
        x: 0.0,
        y: 0.0,
        w: W as f32,
        h: H as f32,
    }
}

/// A content builder that fills a fixed-size box with a solid color: the toast wraps its
/// content in a `Fit x Fit` centered panel, so a fixed-size leaf gives the panel a
/// deterministic size and position for the golden.
fn panel(color: Rgba) -> impl Fn(&mut BuildCx<'_>) + 'static {
    move |cx| {
        cx.leaf(LeafStyle {
            size: Size::fixed(PANEL_W, PANEL_H),
            style: BoxStyle::solid(color),
        });
    }
}

/// Build a top-edge `Toast` with a fixed colored content panel, then show it (so the
/// content overlay is revealed for the golden), and return the root. Toast authors a
/// reactive cell, so it builds through a reactive cx; showing for the golden flips the
/// content's `hidden` flag directly (the same effect the router applies for a
/// `ToastHandle::show`) — build leaves the content hidden.
fn build_shown_scene(store: &mut NodeStore) -> NodeId {
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();

    let widget = toast()
        .edge(ToastEdge::Top)
        .duration(DURATION)
        .content(panel(PANEL));

    let root = {
        let mut cx = BuildCx::with_reactive(
            store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
        );
        widget.build(&mut cx);
        cx.root().expect("toast declares a root")
    };
    // Show the toast: reveal the content (build leaves it hidden). A top-edge toast needs
    // no spacer, so the content is the root's first (and only) child.
    let content = content_of(store, root);
    store.set_hidden(content, false);
    root
}

/// Walk a node's direct children (arena sibling chain).
fn children_of(store: &NodeStore, parent: NodeId) -> Vec<NodeId> {
    let arena = store.arena();
    let mut out = Vec::new();
    let mut child = arena.links(parent).and_then(|l| l.first_child);
    while let Some(c) = child {
        out.push(c);
        child = arena.links(c).and_then(|l| l.next_sibling);
    }
    out
}

/// The content node — the toast root's *last* child. A top/leading toast pins the content
/// at main-start (it is the only child); a bottom/trailing toast pins it at main-end
/// behind a fill spacer (it is the last child). Taking the last child covers both.
fn content_of(store: &NodeStore, root: NodeId) -> NodeId {
    *children_of(store, root)
        .last()
        .expect("toast mounts a content child")
}

// --- golden screenshot ------------------------------------------------------

#[test]
fn shown_toast_paints_the_panel_pinned_to_the_top_edge_and_matches_golden() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);

    let mut store = NodeStore::new();
    let root = build_shown_scene(&mut store);

    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    let mut primitives = Vec::new();
    paint_tree(&store, root, &mut primitives);
    renderer.upload(&mut gpu, &primitives);
    renderer.submit(&mut gpu, surface, CLEAR, [W as f32, H as f32]);
    let actual = gpu.read_pixels_bgra8(surface);

    let path = golden_path();
    if std::env::var("BLESS").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
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
        "golden size mismatch: {} vs {}",
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
        "golden mismatch: max per-channel diff {worst} at byte {worst_at} \
         (pixel {}, channel {}) exceeds tolerance {TOL}",
        worst_at / 4,
        worst_at % 4,
    );
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/toast_widget.bgra8")
}

// --- facade-loop input tape (the timer dimension, end to end) ----------------

/// The surface a launch window opens at (`WindowConfig::default` logical size, scale 1.0).
/// A center-of-surface pointer sample lands on the toast's surface-filling root.
const SURFACE_W: f64 = 800.0;
const SURFACE_H: f64 = 600.0;

/// A fixed-step deterministic frame delta for the facade tape. The duration is a small
/// multiple of it, so a handful of beats crosses the deadline.
const STEP: Duration = Duration::from_millis(16);

/// A facade app whose root is a surface-filling flex that mounts a top `Toast`. A primary
/// pointer-down anywhere on the root shows the toast through the captured `ToastHandle`,
/// which arms the one-shot auto-dismiss timer — the same `EventCx::request_timer` seam the
/// Toast uses internally. Unlike the sheet's slide, the toast never `wants_animation`: the
/// armed timer blocks until its deadline rather than spinning frames.
struct ToastApp {
    /// Filled during `build` with the mounted toast's handle so the trigger can show it.
    slot: ToastHandleSlot,
}

impl Application for ToastApp {
    fn new(_cx: &mut AppCx) -> Self {
        ToastApp {
            slot: Rc::new(RefCell::new(None)),
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let slot = self.slot.clone();
        let root = cx.flex(
            viso::ui::FlexStyle {
                size: Size::fill(),
                ..viso::ui::FlexStyle::default()
            },
            |cx| {
                toast()
                    .edge(ToastEdge::Top)
                    .duration(DURATION)
                    .content(panel(PANEL))
                    .handle(&slot)
                    .build(cx);
            },
        );
        cx.focusable(root, true);
        // Show the toast on a primary pointer-down: read the handle the toast filled in
        // during build and drive its show (which arms the auto-dismiss timer).
        let trigger = slot.clone();
        cx.on_pointer(root, move |ev| {
            let Some(p) = ev.pointer() else { return };
            if p.phase == PointerPhase::Down
                && p.buttons.contains(PointerButtons::PRIMARY)
                && let Some(handle) = trigger.borrow().clone()
            {
                handle.show(ev);
            }
        });
    }
}

/// One priming redraw to run the first layout (so the root gets a world box the pointer
/// can hit), a primary pointer-down at the surface center to show the toast, the beat that
/// arms the timer, then `beats` wake beats. An armed timer never sets a redraw reason (it
/// blocks, it does not spin), so each beat must be supplied by the script: a `Wakeup`
/// (arm an `AsyncCompletion` reason) then a `RedrawRequested` (run the frame, stepping the
/// `FixedStepClock` by one `STEP`). In production the backend honors the timer's
/// `WaitUntil(deadline)` and supplies that beat at the deadline.
fn show_script(beats: usize) -> Vec<RawEvent> {
    let mut script = vec![
        RawEvent::RedrawRequested {
            window: WindowId(1),
        },
        RawEvent::Pointer(RawPointer {
            window: WindowId(1),
            x: SURFACE_W / 2.0,
            y: SURFACE_H / 2.0,
            buttons: RawButtons::PRIMARY,
            modifiers: RawModifiers::default(),
            phase: RawPhase::Down,
        }),
        // The input beat that arms the timer: `InputDirty` from the pointer drives this
        // frame, which drains the request and calls `arm_request`.
        RawEvent::RedrawRequested {
            window: WindowId(1),
        },
    ];
    for _ in 0..beats {
        script.push(RawEvent::Wakeup);
        script.push(RawEvent::RedrawRequested {
            window: WindowId(1),
        });
    }
    script
}

#[test]
fn showing_arms_the_timer_and_the_auto_dismiss_hides_the_content_through_the_facade_loop() {
    // The timer arms at t = 2 * STEP = 32ms (launch, priming, then the arm frame), so its
    // deadline is 32 + 64 = 96ms. Each wake beat steps the clock one STEP past the arm
    // frame: beats land at 48, 64, 80, 96(=deadline, fires), 112, 128ms. Six beats carry
    // the clock to 128ms, well past the deadline.
    let app = drive_scripted::<ToastApp>(show_script(6), STEP);
    let root = app.root().expect("ToastApp declares a root");
    let store = app.store();
    // ToastApp mounts the toast as the sole child of a surface-filling host flex, so the
    // toast's own root is that child; its content is that root's last child.
    let toast_root = *children_of(store, root)
        .first()
        .expect("the host flex mounts the toast as its child");
    let content = content_of(store, toast_root);

    // The auto-dismiss timer's `on_fire` ran: the content is hidden again. It runs at most
    // once (fire_due removes a fired timer), so a hidden content is one auto-dismiss.
    assert!(
        store.hidden(content),
        "the auto-dismiss timer fired and hid the toast content through the facade loop"
    );

    // The registry emptied when the timer fired, so no deadline is pending — the loop is
    // free to idle (the timer-side frame-halt / zero-CPU-when-idle contract).
    assert!(
        app.next_timer_deadline().is_none(),
        "no timer deadline remains once the one-shot auto-dismiss has fired"
    );
}

#[test]
fn a_shown_toast_costs_no_frame_and_stays_visible_before_its_deadline() {
    // The timer arms at t = 32ms with a 96ms deadline. Two wake beats land at 48 and 64ms —
    // both short of 96ms — so the deadline is not yet crossed.
    let app = drive_scripted::<ToastApp>(show_script(2), STEP);
    let root = app.root().expect("ToastApp declares a root");
    let store = app.store();
    let toast_root = *children_of(store, root)
        .first()
        .expect("the host flex mounts the toast as its child");
    let content = content_of(store, toast_root);

    // Before the deadline the toast has not auto-dismissed: the content is still shown.
    assert!(
        !store.hidden(content),
        "the toast has not auto-dismissed before its deadline (content still shown)"
    );

    // An armed timer never set `wants_animation`, so the pump did not spin frames to reach
    // here; and the deadline is still pending — the driver surfaces `Some`, which the
    // scheduler turns into a `WaitUntil` (a real backend blocks on it rather than spinning).
    assert!(
        !app.wants_animation(),
        "a shown toast does not request animation frames (its timer blocks, not spins)"
    );
    assert!(
        app.next_timer_deadline().is_some(),
        "the toast's auto-dismiss deadline is still pending before it is crossed"
    );
}

// --- programmatic input tape ------------------------------------------------

/// A neutral pointer sample for read-only / handle-driven cx construction.
fn read_pointer() -> PointerEvent {
    PointerEvent {
        x: 0.0,
        y: 0.0,
        phase: PointerPhase::Move,
        buttons: PointerButtons::NONE,
        modifiers: Modifiers::default(),
    }
}

/// A `Toast` laid out as the sole child of a surface-filling host, kept together with the
/// reactive stores and a local timer registry so a captured `ToastHandle` can be driven the
/// way the router/driver would: showing defers a `hidden` flip and a timer arm; the local
/// registry arms/fires them against a controlled clock. `on_dismiss` bumps a counter.
struct Interactive {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    content: NodeId,
    handle: ToastHandle,
    timers: TimerRegistry,
    /// A monotone virtual clock: `arm` records `now`, `fire_due` is called against a later
    /// `now`, so the pack crosses the deadline deterministically without wall-clock waits.
    now: Instant,
}

impl Interactive {
    fn new(counter: Rc<Cell<u32>>) -> Self {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();

        let slot: ToastHandleSlot = Rc::new(RefCell::new(None));
        let widget = toast()
            .edge(ToastEdge::Top)
            .duration(DURATION)
            .content(panel(PANEL))
            .handle(&slot)
            .on_dismiss(move |_ev| {
                counter.set(counter.get() + 1);
            });

        let root = {
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut states,
                &mut bindings,
                &mut lists,
                &mut text_edits,
            );
            widget.build(&mut cx);
            cx.root().expect("toast declares a root")
        };
        let handle = slot.borrow().clone().expect("build fills the handle slot");
        let content = content_of(&store, root);

        let mut scratch = Vec::new();
        store.layout(root, surface_rect(), &mut scratch);

        Interactive {
            store,
            states,
            bindings,
            content,
            handle,
            timers: TimerRegistry::new(),
            now: Instant::now(),
        }
    }

    /// Drive a `ToastHandle` action (show/dismiss) as the router/driver would: run it inside
    /// a throwaway `EventCx` (lending the current focus in), take the deferred `hidden` and
    /// timer requests, apply the `hidden` flips to the store, and arm the timer requests in
    /// the local registry against the current `now`.
    fn drive(&mut self, act: impl FnOnce(&ToastHandle, &mut EventCx<'_>)) {
        let ev = read_pointer();
        let handle = self.handle.clone();
        let (hidden, timers) = {
            let mut cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
            cx.__set_focused(self.store.focused());
            act(&handle, &mut cx);
            (cx.__take_hidden_requests(), cx.__take_timer_requests())
        };
        for (id, h) in hidden {
            self.store.set_hidden(id, h);
        }
        for req in timers {
            self.timers.arm_request(req, self.now);
        }
    }

    /// Advance the virtual clock by `d` and fire every timer whose deadline is now reached
    /// (the driver's frame-head `fire_due`, store-only — the auto-dismiss path).
    fn advance_and_fire(&mut self, d: Duration) {
        self.now += d;
        self.timers.fire_due(&mut self.store, self.now);
    }
}

#[test]
fn a_manual_dismiss_hides_early_and_fires_on_dismiss_once() {
    let count = Rc::new(Cell::new(0u32));
    let mut ix = Interactive::new(count.clone());
    let content = ix.content;

    assert!(ix.store.hidden(content), "the content starts hidden");

    // Show: reveal the content and arm the auto-dismiss timer; showing does not fire
    // on_dismiss.
    ix.drive(|t, ev| t.show(ev));
    assert!(!ix.store.hidden(content), "show reveals the content");
    assert!(
        ix.timers.earliest().is_some(),
        "show arms the auto-dismiss timer"
    );
    assert_eq!(count.get(), 0, "showing does not fire on_dismiss");

    // Manually dismiss before the deadline: hide early and fire on_dismiss once.
    ix.drive(|t, ev| t.dismiss(ev));
    assert!(ix.store.hidden(content), "manual dismiss hides the content");
    assert_eq!(count.get(), 1, "manual dismiss fires on_dismiss once");

    // The stale auto-dismiss timer still reaches its deadline, but the epoch guard makes its
    // fire a no-op: the content stays hidden and on_dismiss stays at 1 (the store-only fire
    // never runs on_dismiss anyway — only a manual dismiss does).
    ix.advance_and_fire(DURATION + STEP);
    assert!(
        ix.store.hidden(content),
        "the epoch-guarded stale fire leaves the content hidden"
    );
    assert_eq!(
        count.get(),
        1,
        "the stale auto-dismiss does not fire on_dismiss"
    );
}

#[test]
fn the_auto_dismiss_hides_the_content_when_the_deadline_is_crossed() {
    let count = Rc::new(Cell::new(0u32));
    let mut ix = Interactive::new(count.clone());
    let content = ix.content;

    ix.drive(|t, ev| t.show(ev));
    assert!(!ix.store.hidden(content), "show reveals the content");

    // Before the deadline the content stays shown.
    ix.advance_and_fire(DURATION / 2);
    assert!(
        !ix.store.hidden(content),
        "the content stays shown before the deadline"
    );

    // Crossing the deadline fires the store-only auto-dismiss: the content hides, the
    // registry empties, and on_dismiss does NOT fire (the store-only fire cannot re-enter an
    // EventCx).
    ix.advance_and_fire(DURATION);
    assert!(
        ix.store.hidden(content),
        "crossing the deadline auto-dismisses the content"
    );
    assert!(
        ix.timers.earliest().is_none(),
        "the one-shot timer emptied the registry after firing"
    );
    assert_eq!(
        count.get(),
        0,
        "the store-only auto-dismiss does not fire on_dismiss"
    );
}

#[test]
fn dismissing_an_already_hidden_toast_is_a_noop() {
    let count = Rc::new(Cell::new(0u32));
    let mut ix = Interactive::new(count.clone());
    let content = ix.content;

    // Never shown: a dismiss is a de-duped no-op — the content stays hidden and on_dismiss
    // never fires.
    ix.drive(|t, ev| t.dismiss(ev));
    assert!(ix.store.hidden(content), "the content stays hidden");
    assert_eq!(
        count.get(),
        0,
        "dismissing a hidden toast does not fire on_dismiss"
    );
}

// --- a11y snapshot ----------------------------------------------------------

#[test]
fn toast_content_is_a_status_live_region_and_is_not_focusable() {
    let mut store = NodeStore::new();
    let root = build_shown_scene(&mut store);
    let content = content_of(&store, root);

    // The content is a polite live region: it announces itself without moving focus.
    assert_eq!(
        store.semantics(content).map(|s| s.role),
        Some(Role::Status),
        "the toast content carries Role::Status (a WAI-ARIA role=status live region)"
    );

    // And it is NOT focusable — the key non-modal difference from a Dialog: a toast never
    // takes focus.
    assert!(
        !store.focusable(content),
        "the toast content is not focusable (a toast never takes focus)"
    );
}

// --- allocation profile -----------------------------------------------------

/// Counts heap allocations while `ARMED`; off by default so the harness's own allocations
/// are never counted. Mirrors `sheet_widget.rs`.
struct CountingAlloc;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static ARMED: AtomicBool = AtomicBool::new(false);

// SAFETY: forwards every call to the system allocator unchanged; the only added behavior is
// a relaxed counter increment on allocation while armed.
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

struct Harness {
    gpu: HeadlessRaster,
    renderer: Renderer,
    surface: viso::gpu::SurfaceId,
    store: NodeStore,
    root: NodeId,
    content: NodeId,
    timers: TimerRegistry,
    now: Instant,
    primitives: Vec<viso::render::Primitive>,
}

/// Build a shown top toast (content revealed) with the auto-dismiss timer armed in a local
/// registry, so the harness can paint a steady shown frame or fire the timer.
fn setup_alloc() -> Harness {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let renderer = Renderer::new(&mut gpu, format);

    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();

    let slot: ToastHandleSlot = Rc::new(RefCell::new(None));
    let widget = toast()
        .edge(ToastEdge::Top)
        .duration(DURATION)
        .content(panel(PANEL))
        .handle(&slot);

    let root = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
        );
        widget.build(&mut cx);
        cx.root().expect("toast declares a root")
    };
    let handle = slot.borrow().clone().expect("build fills the handle slot");
    let content = content_of(&store, root);

    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    // Show (reveal the content) and arm the auto-dismiss timer via a throwaway EventCx,
    // applying the deferred hidden flip and arming the timer in a local registry.
    let mut timers = TimerRegistry::new();
    let now = Instant::now();
    {
        let ev = read_pointer();
        let (hidden, reqs) = {
            let mut cx = EventCx::__new_pointer(&mut states, &bindings, &ev);
            cx.__set_focused(store.focused());
            handle.show(&mut cx);
            (cx.__take_hidden_requests(), cx.__take_timer_requests())
        };
        for (id, h) in hidden {
            store.set_hidden(id, h);
        }
        for req in reqs {
            timers.arm_request(req, now);
        }
    }

    // Re-lay out so the revealed content gets a world box before we paint the steady frame.
    store.layout(root, surface_rect(), &mut scratch);

    Harness {
        gpu,
        renderer,
        surface,
        store,
        root,
        content,
        timers,
        now,
        primitives: Vec::new(),
    }
}

/// One full shown-toast frame: paint/upload/submit — the whole per-frame path a visible,
/// non-animating toast takes through the renderer.
fn shown_frame(h: &mut Harness) {
    h.primitives.clear();
    paint_tree(&h.store, h.root, &mut h.primitives);
    h.renderer.upload(&mut h.gpu, &h.primitives);
    h.renderer
        .submit(&mut h.gpu, h.surface, CLEAR, [W as f32, H as f32]);
}

#[test]
fn a_steady_shown_toast_frame_is_deterministic_and_reuses_gpu_resources() {
    // A shown toast has no animation, so a steady frame is a pure paint/upload/submit of an
    // unchanging scene. The headless raster legitimately re-encodes into its pixel buffer
    // each frame, so a full render frame is not zero-alloc; what must hold (the modal
    // steady-frame invariant) is that two identical frames allocate the *same* amount (no
    // unbounded scratch churn), no GPU resource is created per frame (buffers/textures/bind
    // groups reused — section 17.4), and `frame_stats` is unchanged frame to frame (section
    // 47: an unchanging scene neither leaks resources nor drifts its draw work). The
    // per-frame zero-alloc contract is asserted on the isolated non-render hot path —
    // `fire_due` — below.
    let mut h = setup_alloc();

    for _ in 0..4 {
        shown_frame(&mut h);
    }
    // Grow the reused paint buffer to steady capacity so a later `paint_tree` into it does
    // not reallocate.
    h.primitives.clear();
    paint_tree(&h.store, h.root, &mut h.primitives);

    let buffers = h.gpu.buffer_count();
    let textures = h.gpu.texture_count();
    let bind_groups = h.gpu.bind_group_count();
    let stats = h.renderer.frame_stats();

    let mut frame_allocs = [0usize; 2];
    for (i, slot) in frame_allocs.iter_mut().enumerate() {
        ALLOCS.store(0, Ordering::Relaxed);
        ARMED.store(true, Ordering::Relaxed);
        shown_frame(&mut h);
        ARMED.store(false, Ordering::Relaxed);
        *slot = ALLOCS.load(Ordering::Relaxed);

        assert_eq!(
            h.renderer.frame_stats(),
            stats,
            "frame {i}: frame_stats changed for an unchanged shown-toast scene"
        );
        assert_eq!(
            h.gpu.buffer_count(),
            buffers,
            "frame {i}: a GPU buffer was allocated for an unchanged shown-toast scene"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "frame {i}: a GPU texture was allocated for an unchanged shown-toast scene"
        );
        assert_eq!(
            h.gpu.bind_group_count(),
            bind_groups,
            "frame {i}: a bind group was allocated for an unchanged shown-toast scene"
        );
    }

    assert_eq!(
        frame_allocs[0], frame_allocs[1],
        "a steady shown-toast frame allocated a different amount on two identical frames \
         ({} vs {}): the paint / encode scratch is not deterministic",
        frame_allocs[0], frame_allocs[1]
    );

    let FrameStats {
        draw_calls,
        instances,
    } = stats;
    assert!(draw_calls > 0, "the shown toast must emit draw calls");
    assert!(instances > 0, "the shown toast must emit instances");
}

#[test]
fn the_auto_dismiss_fire_step_is_allocation_free() {
    // Section 28: the auto-dismiss timer fire is a hot-path tick. `fire_due` swap-removes the
    // reached timer and calls its boxed `FnOnce`, which flips one `hidden` flag — no heap
    // touch. Measure the fire step in isolation (no renderer): crossing the deadline and
    // running `fire_due` must not allocate.
    let mut h = setup_alloc();

    // Warm up the registry's internal capacity with a few no-op fire_due calls before the
    // deadline (nothing is due yet, so the content stays shown).
    for _ in 0..4 {
        h.timers.fire_due(&mut h.store, h.now);
    }
    assert!(
        !h.store.hidden(h.content),
        "nothing fired before the deadline (content still shown)"
    );

    // Cross the deadline and measure the fire step: it flips the content hidden with no heap
    // allocation.
    h.now += DURATION + STEP;
    ALLOCS.store(0, Ordering::Relaxed);
    ARMED.store(true, Ordering::Relaxed);
    h.timers.fire_due(&mut h.store, h.now);
    ARMED.store(false, Ordering::Relaxed);
    assert_eq!(
        ALLOCS.load(Ordering::Relaxed),
        0,
        "the auto-dismiss timer fire must be allocation-free (section 28 ticks)"
    );

    // The fire ran: the content is hidden and the registry emptied.
    assert!(h.store.hidden(h.content), "the fire hid the toast content");
    assert!(
        h.timers.earliest().is_none(),
        "the one-shot timer emptied the registry after firing"
    );
}
