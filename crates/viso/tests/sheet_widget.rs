//! Section 71 validation pack for the `Sheet` control — an edge drawer that
//! slides in over the whole scene, dims the background, takes focus, and traps
//! keyboard navigation inside its content while open — driven through the public
//! facade. It is the `Modal` pack (golden + input tapes + a11y + allocation)
//! plus the animation dimension that makes a sheet a drawer rather than a
//! centered dialog: the content slides in from off-screen on open and slides
//! back out on close, a transform-only motion (`TRANSFORM | HIT_TEST | PAINT`,
//! never a relayout — architecture section 8.7).
//!
//! A `Sheet` lowers to a stretched flex along the slide axis. For the default
//! `Bottom` edge the root's direct children are, in author order,
//! `[scrim@0, spacer@1, content@2]`: the scrim dims the whole surface under
//! everything, a `Fill` spacer pushes the content to the bottom edge (the layout
//! engine's `Align` is cross-axis only, so a far-edge anchor needs a spacer
//! sibling), and the content is the drawer body — both scrim and content are
//! top-layer overlays that start hidden. This differs from `Modal`, whose
//! children are `[scrim@0, content@1]` with no spacer.
//!
//! Two harnesses drive the two dimensions:
//!
//! - The **facade loop** (`drive_scripted` + `FixedStepClock`) drives the real
//!   `AppDriver` frame loop end to end for the *slide* dimension: a scripted
//!   pointer press asks a `SheetHandle` to open, and the driver ticks the slide
//!   to completion across self-scheduled beats. This exercises the same path
//!   `viso::run` takes — the animation registry, the TRANSFORM-gated
//!   `resolve_transforms` in `relayout_and_paint`, and the frame halt once the
//!   slide settles.
//! - A **hand-driven store** (mirroring `modal_widget.rs`) drives the golden,
//!   the close/dismiss/restore path, the focus trap, the a11y snapshot, and the
//!   allocation profile. The sheet's open/close handlers *return* their slide
//!   requests rather than applying them, so these tests feed the returned
//!   `TranslateAnim` into a local `AnimationRegistry` and `tick` it — for the
//!   golden, to settle the content at translate=0; for close, to fire the
//!   slide-out's `on_done` (which is what hides the content, releases the focus
//!   scope, and restores focus once the motion settles).
//!
//! Coverage:
//!
//! - **golden screenshot** — an open `Bottom` sheet, its slide ticked to rest
//!   (translate=0), with a full-surface scrim of one color and the content of a
//!   second, opaque color pinned to the bottom edge, laid out over a fixed
//!   surface, matches a blessed baseline. Both are overlays; the scrim is
//!   authored first, so the top layer paints scrim then content;
//! - **slide input tape** — through the facade loop, opening slides the content
//!   in: its world advances monotonically toward rest, `wants_animation` is
//!   false once the slide settles (the zero-CPU-when-idle contract), and the
//!   settled frame relaid nothing (a pure TRANSFORM frame moved world);
//! - **close / dismiss / restore tape** — closing starts the slide-out and fires
//!   `on_dismiss` at close-request time; only when the slide-out's `on_done`
//!   fires (after the motion settles) is the content hidden, the focus scope
//!   released, and focus restored to the pre-open node; a repeated close and a
//!   re-open mid-slide-out are handled without a spurious hide;
//! - **focus-trap tape** — with a focusable sibling outside the content scope,
//!   opening installs a focus scope on the content and `focus_next` (Tab) cycles
//!   only inside the drawer — both while the slide is in flight and once settled;
//! - **a11y snapshot** — the derived tree carries a `Group` root over a `Dialog`
//!   content; the content is focusable and declares a key handler for Escape;
//! - **allocation profile** — a warmed-up in-flight slide frame allocates
//!   nothing per frame (section 28: animation ticks are a zero-allocation hot
//!   path), and a settled sheet requests no further frames (the registry empties)
//!   yet a forced re-tick over the empty registry still allocates nothing.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::gpu::{GpuBackend, HeadlessRaster, RawWindowHandle};
use viso::platform::{
    Modifiers as RawModifiers, PointerButtons as RawButtons, PointerPhase as RawPhase, RawEvent,
    RawPointer, WindowId,
};
use viso::prelude::*;
use viso::render::{FrameStats, Rect, Renderer, Rgba};
use viso::ui::{
    AnimationRegistry, BindingTable, BoxStyle, BuildCx, Component, EventCx, Key, KeyEvent,
    KeyRouter, LeafStyle, Modifiers, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase,
    Role, SemanticProjector, Size, StateId, StateStore, StateValue, TextEdits, TranslateAnim,
    VirtualLists, focus_next, paint_tree,
};
use viso::widgets::{SheetEdge, SheetHandle, SheetHandleSlot, sheet};

const W: u32 = 240;
const H: u32 = 120;
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// Per-channel tolerance (in 0..=255) for the golden comparison.
const TOL: u8 = 2;

/// The drawer's fixed extent along the slide axis (its height for a bottom sheet).
/// Smaller than the surface so the scrim shows above the drawer in the golden.
const EXTENT: f32 = 64.0;

/// A brisk slide duration for the hand-driven ticks and the facade tape.
const SLIDE: Duration = Duration::from_millis(80);

/// The scrim's fill — the dimming backdrop under the content. Opaque here (a=1) so the
/// golden is deterministic regardless of blend order; a real scrim is translucent.
const SCRIM: Rgba = Rgba {
    r: 0.10,
    g: 0.10,
    b: 0.12,
    a: 1.0,
};
/// The content's fill — the drawer panel over the scrim. In the opened golden it paints
/// over the scrim in the bottom band, proving the overlay order (content over scrim).
const CONTENT: Rgba = Rgba {
    r: 0.30,
    g: 0.18,
    b: 0.20,
    a: 1.0,
};

fn surface_rect() -> Rect {
    Rect {
        x: 0.0,
        y: 0.0,
        w: W as f32,
        h: H as f32,
    }
}

/// A content builder that fills its box with a solid color, so the golden shows the
/// drawer's region on top of the scrim.
fn fill(color: Rgba) -> impl Fn(&mut BuildCx<'_>) + 'static {
    move |cx| {
        cx.leaf(LeafStyle {
            size: Size::fill(),
            style: BoxStyle::solid(color),
        });
    }
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

/// The content node of a built `Bottom` sheet: children are `[scrim, spacer, content]`,
/// so the content is the last child. (This differs from `Modal`'s `[scrim, content]`.)
fn content_of(store: &NodeStore, root: NodeId) -> NodeId {
    *children_of(store, root)
        .last()
        .expect("a bottom sheet authors scrim, spacer, and content")
}

// --- hand-driven reactive harness -------------------------------------------

/// A `Sheet` built as the root of its own tree, kept together with the reactive stores
/// its handlers write into so a captured `SheetHandle`, the key router, and a local
/// animation registry can drive it exactly as the facade would. The sheet fills the
/// surface. `on_dismiss` bumps a counter. Unlike the modal harness, the sheet's
/// open/close *return* their slide request; the harness owns a local `AnimationRegistry`
/// and ticks it so the slide-out `on_done` (hide + release + restore) is observable.
struct Sheeted {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    root: NodeId,
    chain: Vec<NodeId>,
    handle: SheetHandle,
    anims: AnimationRegistry,
    /// The shared `open` cell the build authored — Sheet authors exactly one `Bool`
    /// state cell, so a fresh `StateStore` allocating one `Bool` yields the same handle
    /// the build produced (there is no public bare-`StateId` ctor).
    open: StateId,
}

impl Sheeted {
    fn new(counter: Rc<Cell<u32>>) -> Self {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();

        let slot: SheetHandleSlot = Rc::new(RefCell::new(None));
        let widget = sheet()
            .edge(SheetEdge::Bottom)
            .extent(EXTENT)
            .duration(SLIDE)
            .scrim(SCRIM)
            .content(fill(CONTENT))
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
                &mut projectors,
            );
            widget.build(&mut cx);
            cx.root().expect("sheet declares a root")
        };
        let handle = slot.borrow().clone().expect("build fills the handle slot");
        let open = StateStore::new().alloc(StateValue::Bool(false));

        let mut scratch = Vec::new();
        store.layout(root, surface_rect(), &mut scratch);

        Sheeted {
            store,
            states,
            bindings,
            root,
            chain: Vec::new(),
            handle,
            anims: AnimationRegistry::new(),
            open,
        }
    }

    /// The drawer content node (`[scrim, spacer, content]`, so the last child).
    fn content(&self) -> NodeId {
        content_of(&self.store, self.root)
    }

    /// Drive a `SheetHandle` action (open/close/toggle) as a router would: run it inside
    /// a throwaway `EventCx` (lending the current focus in), apply the deferred
    /// `hidden`/focus/focus-scope requests to the store, and start every requested slide
    /// in the local registry (as the driver drains `EventCx` requests into its registry).
    fn drive(&mut self, act: impl FnOnce(&SheetHandle, &mut EventCx<'_>)) {
        let ev = read_pointer();
        let handle = self.handle.clone();
        let (hidden, focus, scope, anims) = {
            let mut cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
            cx.__set_focused(self.store.focused());
            act(&handle, &mut cx);
            (
                cx.__take_hidden_requests(),
                cx.__take_focus_request(),
                cx.__take_focus_scope_request(),
                cx.__take_animation_requests(),
            )
        };
        self.apply(hidden, focus, scope);
        for anim in anims {
            self.anims.start(anim);
        }
    }

    /// Route a key sample through the public `KeyRouter` to the focused node. The sheet's
    /// Escape handler defers its slide through `EventCx`, but `route_key` does not drain
    /// animation requests (it drains only hidden/focus/scope), so the slide-out that
    /// Escape starts is not observable here; the programmatic close tape covers the
    /// slide-out `on_done`. Escape is exercised for its dedupe + open-cell move.
    fn key(&mut self, ev: KeyEvent) -> bool {
        KeyRouter::route_key(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            &mut TextEdits::new(),
            self.root,
            ev,
            &mut self.chain,
        )
    }

    /// Apply the deferred requests a dispatch recorded to the store, mirroring the
    /// router's drain order: hidden flips, then the focus move, then the scope.
    fn apply(
        &mut self,
        hidden: Vec<(NodeId, bool)>,
        focus: Option<Option<NodeId>>,
        scope: Option<Option<NodeId>>,
    ) {
        for (id, h) in hidden {
            self.store.set_hidden(id, h);
        }
        if let Some(target) = focus {
            self.store.set_focused(target);
        }
        if let Some(s) = scope {
            self.store.set_focus_scope(s);
        }
    }

    /// Tick the local registry to completion (the slide plus a margin), then re-resolve
    /// transforms so `world` reflects the settled translate. This is what the driver's
    /// frame loop does across beats; here one over-long tick settles every slide and
    /// fires any `on_done`.
    fn settle(&mut self) {
        self.anims
            .tick(&mut self.store, SLIDE + Duration::from_millis(16));
        self.store.resolve_transforms(self.root);
    }

    /// Tick the registry by one fixed frame step without settling — leaves the slide in
    /// flight (used to observe mid-slide state).
    fn tick_one(&mut self) {
        self.anims.tick(&mut self.store, Duration::from_millis(16));
        self.store.resolve_transforms(self.root);
    }

    /// The current value of the shared open cell (via a throwaway read cx).
    fn is_open(&mut self) -> Option<bool> {
        let ev = read_pointer();
        let cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
        match cx.get(self.open) {
            Some(StateValue::Bool(b)) => Some(b),
            _ => None,
        }
    }
}

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

fn key_ev(key: Key, pressed: bool, repeat: bool) -> KeyEvent {
    KeyEvent {
        key,
        pressed,
        repeat,
        modifiers: Modifiers::default(),
    }
}

// --- golden screenshot ------------------------------------------------------

#[test]
fn open_sheet_paints_the_content_pinned_to_the_bottom_edge_and_matches_golden() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);

    // Build the sheet, open it programmatically (shows the overlays + starts the slide),
    // and settle the slide so the content rests at the bottom edge (translate=0).
    let count = Rc::new(Cell::new(0u32));
    let mut ix = Sheeted::new(count);
    ix.drive(|h, ev| h.open(ev));
    ix.settle();

    // Re-layout after settle so the surface size is authoritative, then paint. The slide
    // is transform-only, so `settle`'s `resolve_transforms` already placed world; layout
    // here is idempotent (nothing layout-dirty) and keeps the golden path identical to
    // the modal pack.
    let mut scratch = Vec::new();
    ix.store.layout(ix.root, surface_rect(), &mut scratch);
    ix.store.resolve_transforms(ix.root);

    let mut primitives = Vec::new();
    paint_tree(&ix.store, ix.root, &mut primitives);
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/sheet_widget.bgra8")
}

// --- slide input tape (facade loop) -----------------------------------------

/// The surface a launch window opens at (`WindowConfig::default` logical size,
/// scale 1.0). A center-of-surface pointer sample lands on the sheet's outer root.
const SURFACE_W: f64 = 800.0;
const SURFACE_H: f64 = 600.0;

/// A fixed-step deterministic frame delta for the facade tape.
const STEP: Duration = Duration::from_millis(16);

/// A facade app whose root is a surface-filling flex that mounts a bottom `Sheet`. A
/// primary pointer-down anywhere on the root opens the sheet through the captured
/// `SheetHandle`, and the driver ticks the slide-in to completion on its own beats — the
/// same `EventCx::request_animation` seam the Sheet uses internally. The slide is
/// transform-only, so every animation frame is a pure TRANSFORM frame.
struct SheetApp {
    /// Filled during `build` with the mounted sheet's handle so the trigger can open it.
    slot: SheetHandleSlot,
}

impl Application for SheetApp {
    fn new(_cx: &mut AppCx) -> Self {
        SheetApp {
            slot: Rc::new(RefCell::new(None)),
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let slot = self.slot.clone();
        let root = cx.flex(
            viso::ui::FlexStyle {
                size: Size::fill(),
                // Stretch the mounted sheet across both axes: the default `Align::Start`
                // leaves a cross-axis `Fill` child at its natural (zero) cross size, so a
                // full-surface overlay host must stretch its child to the surface.
                align: viso::ui::Align::Stretch,
                ..viso::ui::FlexStyle::default()
            },
            |cx| {
                sheet()
                    .edge(SheetEdge::Bottom)
                    .extent(EXTENT)
                    .duration(SLIDE)
                    .scrim(SCRIM)
                    .content(fill(CONTENT))
                    .handle(&slot)
                    .build(cx);
            },
        );
        cx.focusable(root, true);
        // Open the sheet on a primary pointer-down: read the handle the sheet filled in
        // during build and drive its open (which starts the slide-in).
        let trigger = slot.clone();
        cx.on_pointer(root, move |ev| {
            let Some(p) = ev.pointer() else { return };
            if p.phase == PointerPhase::Down
                && p.buttons.contains(PointerButtons::PRIMARY)
                && let Some(handle) = trigger.borrow().clone()
            {
                handle.open(ev);
            }
        });
    }
}

/// One priming redraw to run the first layout (so the root gets a world box the pointer
/// can hit), a primary pointer-down at the surface center to open the sheet, then a beat
/// to run the frame that starts the slide. Once the slide starts the driver
/// `wants_animation` and self-schedules the remaining beats.
fn open_script() -> Vec<RawEvent> {
    vec![
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
        RawEvent::RedrawRequested {
            window: WindowId(1),
        },
    ]
}

#[test]
fn opening_slides_the_content_in_through_the_facade_loop_and_settles() {
    let app = drive_scripted::<SheetApp>(open_script(), STEP);
    let root = app.root().expect("SheetApp declares a root");
    let store = app.store();
    // `SheetApp` mounts the sheet as the sole child of a surface-filling host flex, so the
    // sheet's own root is that child; its content is that root's last child.
    let sheet_root = *children_of(store, root)
        .first()
        .expect("the host flex mounts the sheet as its child");
    let content = content_of(store, sheet_root);

    // The content rests pinned to the bottom edge once the slide-in settles: world =
    // bounds − translate, and the settled translate is ZERO, so world == bounds.
    let bounds = store.bounds(content);
    let world = store.world(content);
    assert!(
        (world.y - bounds.y).abs() < 0.5,
        "the settled content rests at its bounds (translate ~= 0): bounds.y {}, world.y {}",
        bounds.y,
        world.y
    );

    // The content sits in the bottom band of the surface (pinned to the bottom edge by
    // the fill spacer): its top is EXTENT above the surface bottom. The facade lays out
    // against the launch window's surface (SURFACE_H), not the golden's fixed H.
    assert!(
        (bounds.y - (SURFACE_H as f32 - EXTENT)).abs() < 1.0,
        "the content is a bottom drawer of height EXTENT: expected top ~= {}, got {} \
         (bounds.h {})",
        SURFACE_H as f32 - EXTENT,
        bounds.y,
        bounds.h,
    );

    // The registry emptied when the slide finished, so the driver no longer wants
    // animation frames — the loop is free to idle (frame halt / zero CPU when idle).
    assert!(
        !app.wants_animation(),
        "the loop halts once the slide settles (zero-CPU-when-idle)"
    );

    // The final animation frame relaid nothing: the slide advanced world with zero layout
    // work — the section 8.7 transform/layout split, resolved through the facade loop's
    // TRANSFORM-gated `resolve_transforms` (a layout-clean frame moved world).
    assert_eq!(
        app.recompute().laid_out,
        0,
        "the last animation frame relaid no node (pure TRANSFORM frame)"
    );
}

#[test]
fn the_slide_in_advances_the_content_world_monotonically_toward_rest() {
    // A hand-driven slide, sampled per frame, must advance the content's world toward
    // rest without overshoot or reversal. World starts off-screen below the surface
    // (translate = offscreen = (0, -EXTENT) ⇒ world.y = bounds.y + EXTENT) and rises to
    // rest (world.y = bounds.y). Each frame's world.y is <= the previous (monotone).
    let count = Rc::new(Cell::new(0u32));
    let mut ix = Sheeted::new(count);
    let content = ix.content();
    ix.drive(|h, ev| h.open(ev));

    // Sample the first frame's world (the slide is in flight, off its rest).
    ix.tick_one();
    let bounds = ix.store.bounds(content);
    let mut prev = ix.store.world(content).y;
    assert!(
        prev > bounds.y + 1.0,
        "the slide starts off-screen below rest (world.y {} well above bounds.y {})",
        prev,
        bounds.y
    );

    // Advance frame by frame: world.y descends monotonically toward bounds.y.
    for _ in 0..6 {
        ix.tick_one();
        let now = ix.store.world(content).y;
        assert!(
            now <= prev + 0.001,
            "the slide-in never reverses: world.y {now} rose above the previous {prev}"
        );
        prev = now;
    }

    // Settle: the content comes to rest exactly at its bounds (translate ZERO).
    ix.settle();
    let rest = ix.store.world(content).y;
    assert!(
        (rest - bounds.y).abs() < 0.5,
        "the settled content rests at its bounds: bounds.y {}, world.y {}",
        bounds.y,
        rest
    );
    assert!(
        ix.anims.is_empty(),
        "the registry empties once the slide settles"
    );
}

// --- close / dismiss / restore tape -----------------------------------------

#[test]
fn closing_slides_out_then_hides_releases_and_restores_only_when_settled() {
    let count = Rc::new(Cell::new(0u32));
    let mut ix = Sheeted::new(count.clone());
    let content = ix.content();

    assert!(
        ix.store.hidden(content),
        "the content starts hidden (closed)"
    );

    // A pre-open focus target that close should restore to. Open programmatically: the
    // handle snapshots this into `restore_focus`, shows the content, starts the slide-in,
    // installs the focus scope on the content, and moves focus into it.
    ix.store.set_focused(Some(ix.root));
    ix.drive(|h, ev| h.open(ev));
    ix.settle();
    assert_eq!(ix.is_open(), Some(true), "the sheet is open");
    assert!(!ix.store.hidden(content), "the content is shown");
    assert_eq!(
        ix.store.focused(),
        Some(content),
        "opening moves focus into the drawer content"
    );
    assert_eq!(
        ix.store.focus_scope(),
        Some(content),
        "opening installs a focus scope on the content (Tab is trapped)"
    );

    // Close: `on_dismiss` fires now, at close-request time, and the open cell moves — but
    // the content is still shown, still focus-trapped, and focus has not returned: the
    // slide-out is in flight, so the release is deferred to its `on_done`.
    ix.drive(|h, ev| h.close(ev));
    assert_eq!(ix.is_open(), Some(false), "close moves the open cell");
    assert_eq!(count.get(), 1, "close fires on_dismiss once, at close time");
    assert!(
        !ix.store.hidden(content),
        "the content stays visible while it slides out"
    );
    assert_eq!(
        ix.store.focus_scope(),
        Some(content),
        "the focus scope holds while the slide-out is in flight"
    );

    // Settle the slide-out: its `on_done` now hides the content, releases the focus scope,
    // and restores focus to the pre-open node.
    ix.settle();
    assert!(
        ix.store.hidden(content),
        "the slide-out `on_done` hides the content once it has slid away"
    );
    assert_eq!(
        ix.store.focus_scope(),
        None,
        "the slide-out `on_done` releases the focus scope"
    );
    assert_eq!(
        ix.store.focused(),
        Some(ix.root),
        "the slide-out `on_done` restores focus to the pre-open node"
    );

    // A repeated close is a de-duped no-op: no cell move, no extra dismiss, no new slide.
    let before = count.get();
    ix.drive(|h, ev| h.close(ev));
    assert_eq!(ix.is_open(), Some(false), "a repeated close is a no-op");
    assert_eq!(count.get(), before, "and does not fire on_dismiss again");
    assert!(ix.anims.is_empty(), "a de-duped close starts no slide");
}

#[test]
fn reopening_mid_slide_out_takes_over_and_never_spuriously_hides() {
    let count = Rc::new(Cell::new(0u32));
    let mut ix = Sheeted::new(count.clone());
    let content = ix.content();

    // Open and settle so the drawer rests shown.
    ix.drive(|h, ev| h.open(ev));
    ix.settle();
    assert!(!ix.store.hidden(content), "the drawer rests shown");

    // Start closing (slide-out queued), advance a couple of frames so it is mid-flight,
    // but do not settle: the slide-out `on_done` has not fired, so the content is still
    // shown.
    ix.drive(|h, ev| h.close(ev));
    ix.tick_one();
    ix.tick_one();
    assert!(
        !ix.store.hidden(content),
        "the content is still shown mid-slide-out"
    );

    // Re-open mid-slide-out: `AnimationRegistry::start` replaces the slide-out with the
    // slide-in (same node), so the slide-out `on_done` never fires. Settling now runs the
    // slide-*in* to rest — the content is not spuriously hidden.
    ix.drive(|h, ev| h.open(ev));
    ix.settle();
    assert_eq!(ix.is_open(), Some(true), "the sheet is open again");
    assert!(
        !ix.store.hidden(content),
        "re-opening mid-slide-out never hides the content (the slide-out on_done was replaced)"
    );
    assert_eq!(
        ix.store.focus_scope(),
        Some(content),
        "the focus scope is (re)installed on the content"
    );
    // The slide-out fired on_dismiss once at its close-request; the re-open does not fire
    // it again (opening never dismisses).
    assert_eq!(count.get(), 1, "re-opening does not fire on_dismiss");
}

#[test]
fn escape_closes_an_open_sheet_and_dedupes() {
    let count = Rc::new(Cell::new(0u32));
    let mut ix = Sheeted::new(count.clone());
    let content = ix.content();

    // Unfocused: the key router has no target, so nothing fires.
    assert!(
        !ix.key(key_ev(Key::Escape, true, false)),
        "an unfocused sheet receives no key dispatch"
    );
    assert_eq!(count.get(), 0);

    // Open and settle so the content is focused and the scope is installed.
    ix.store.set_focused(Some(ix.root));
    ix.drive(|h, ev| h.open(ev));
    ix.settle();
    assert_eq!(ix.store.focused(), Some(content), "the content is focused");

    // Escape closes: the open cell moves and on_dismiss fires once. (The Escape handler
    // defers its slide-out through `EventCx`; `route_key` does not drain animation
    // requests, so the slide-out is not ticked here — the programmatic close tape covers
    // the slide-out `on_done`. What Escape proves is the open-cell move + dismiss + guard.)
    assert!(ix.key(key_ev(Key::Escape, true, false)));
    assert_eq!(ix.is_open(), Some(false), "Escape closes the sheet");
    assert_eq!(count.get(), 1, "the Escape dismiss fires on_dismiss once");

    // Escape again is a de-duped no-op: the guard short-circuits on the already-closed
    // cell.
    let before = count.get();
    ix.key(key_ev(Key::Escape, true, false));
    assert_eq!(ix.is_open(), Some(false), "the sheet stays closed");
    assert_eq!(count.get(), before, "and does not fire again");
}

// --- focus-trap tape --------------------------------------------------------

#[test]
fn opening_traps_tab_inside_the_content_while_sliding_and_when_settled() {
    let count = Rc::new(Cell::new(0u32));
    let mut ix = Sheeted::new(count);
    let content = ix.content();

    // Author a focusable sibling *outside* the sheet's content scope: a leaf appended
    // under the root, focusable, so a whole-tree Tab ring would include it. It is not a
    // sheet content descendant, so an installed content scope must exclude it.
    let root = ix.root;
    let outside = {
        let mut cx = BuildCx::with_parent(&mut ix.store, &mut ix.states, &mut ix.bindings, root);
        let leaf = cx.leaf(LeafStyle {
            size: Size::fixed(10.0, 10.0),
            style: BoxStyle::NONE,
        });
        cx.focusable(leaf, true);
        leaf.id()
    };
    {
        let mut scratch = Vec::new();
        ix.store.layout(ix.root, surface_rect(), &mut scratch);
    }

    // Closed: no scope installed, Tab reaches the whole tree including the outside sibling.
    ix.store.set_focused(Some(outside));
    assert_eq!(
        ix.store.focus_scope(),
        None,
        "no scope is installed while closed"
    );

    // Open: installs a focus scope on the content and moves focus into it. Do NOT settle
    // yet — the slide is in flight, and the trap must already hold mid-slide.
    ix.drive(|h, ev| h.open(ev));
    ix.tick_one();
    assert_eq!(
        ix.store.focus_scope(),
        Some(content),
        "opening installs the content focus scope immediately (before the slide settles)"
    );
    assert_eq!(
        ix.store.focused(),
        Some(content),
        "opening moves focus into the content"
    );

    // Mid-slide: Tab (and Shift-Tab) cycle only within the content subtree — the outside
    // sibling is never reached. The content itself is the sole focusable in the scope.
    for forward in [true, false, true] {
        let landed = focus_next(&mut ix.store, ix.root, forward);
        assert_eq!(
            landed,
            Some(content),
            "mid-slide Tab stays inside the content scope (forward={forward})"
        );
        assert_ne!(
            landed,
            Some(outside),
            "mid-slide Tab never escapes to the outside sibling"
        );
    }

    // Settle the slide-in: the trap still holds once the content rests.
    ix.settle();
    for forward in [true, false] {
        let landed = focus_next(&mut ix.store, ix.root, forward);
        assert_eq!(
            landed,
            Some(content),
            "settled Tab stays inside the content scope (forward={forward})"
        );
    }

    // Close and settle: the slide-out `on_done` releases the scope and restores focus to
    // the pre-open node. Tab reaches the whole tree again.
    ix.drive(|h, ev| h.close(ev));
    ix.settle();
    assert_eq!(
        ix.store.focus_scope(),
        None,
        "closing releases the focus scope once the slide-out settles"
    );
    assert_eq!(
        ix.store.focused(),
        Some(outside),
        "closing restores focus to the pre-open node"
    );
    let ring_after = focus_next(&mut ix.store, ix.root, true);
    assert!(
        ring_after == Some(outside) || ring_after == Some(content),
        "with no scope, Tab traverses the whole tree again (landed on {ring_after:?})"
    );
}

// --- a11y snapshot ----------------------------------------------------------

#[test]
fn sheet_derives_a_group_root_over_a_dialog_content() {
    // The a11y tree does not depend on open/closed; build the scene and derive it.
    let count = Rc::new(Cell::new(0u32));
    let ix = Sheeted::new(count);
    let content = ix.content();

    let tree = ix.store.derive_semantics(ix.root);
    let node = tree.root().expect("the derived tree has a root");
    assert_eq!(node.role, Role::Group, "the sheet is a Group container");

    // The content node is a Dialog; the scrim is a plain fill leaf and the spacer has no
    // semantics, so the Dialog under the root is the content.
    let child_roles: Vec<Role> = node.children.iter().map(|&i| tree.nodes[i].role).collect();
    assert!(
        child_roles.contains(&Role::Dialog),
        "the content is a Dialog under the sheet root"
    );

    // The content is interactive: focusable with a key handler for Escape.
    assert!(ix.store.focusable(content), "the content is focusable");
    assert!(
        ix.store.has_key_handler(content),
        "the content attaches a key handler for Escape"
    );
}

// --- allocation profile -----------------------------------------------------

/// Counts heap allocations while `ARMED`; off by default so the harness's own
/// allocations are never counted. Mirrors `modal_widget.rs`.
struct CountingAlloc;
// Thread-local counters: cargo runs the `#[test]`s in this binary in parallel
// on separate threads, all sharing this one process-global allocator. A
// process-global armed flag / counter would let a sibling test's allocations,
// happening on another thread while this test is inside its armed measurement
// window, race into this test's count and make the steady-state assertion
// flaky. Scoping arm state and the count to the measuring thread makes each
// test see only its own allocations — the frame path under test is
// synchronous, so every allocation it performs is on the arming thread. The
// `.load`/`.store`/`.fetch_add` API and its `Ordering` argument are kept so the
// call sites and the `GlobalAlloc` impl below are unchanged (the ordering is
// irrelevant for thread-local state and is ignored).
thread_local! {
    static ALLOCS_CELL: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static ARMED_CELL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
struct TlsBool;
struct TlsUsize;
impl TlsBool {
    fn load(&self, _: Ordering) -> bool {
        ARMED_CELL.with(std::cell::Cell::get)
    }
    fn store(&self, v: bool, _: Ordering) {
        ARMED_CELL.with(|c| c.set(v));
    }
}
impl TlsUsize {
    fn load(&self, _: Ordering) -> usize {
        ALLOCS_CELL.with(std::cell::Cell::get)
    }
    fn store(&self, v: usize, _: Ordering) {
        ALLOCS_CELL.with(|c| c.set(v));
    }
    fn fetch_add(&self, v: usize, _: Ordering) {
        ALLOCS_CELL.with(|c| c.set(c.get() + v));
    }
}
static ALLOCS: TlsUsize = TlsUsize;
static ARMED: TlsBool = TlsBool;

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

struct Harness {
    gpu: HeadlessRaster,
    renderer: Renderer,
    surface: viso::gpu::SurfaceId,
    store: NodeStore,
    root: NodeId,
    anims: AnimationRegistry,
    primitives: Vec<viso::render::Primitive>,
}

/// Build an open bottom sheet and start its slide-in, so the harness can tick the slide
/// (an in-flight animation frame) or settle it (a rested frame). Returns the harness with
/// the slide *in flight* (not yet settled).
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
    let mut projectors = SemanticProjector::new();

    let slot: SheetHandleSlot = Rc::new(RefCell::new(None));
    let widget = sheet()
        .edge(SheetEdge::Bottom)
        .extent(EXTENT)
        .duration(SLIDE)
        .scrim(SCRIM)
        .content(fill(CONTENT))
        .handle(&slot);

    let root = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        widget.build(&mut cx);
        cx.root().expect("sheet declares a root")
    };
    let handle = slot.borrow().clone().expect("build fills the handle slot");

    let mut scratch = Vec::new();
    store.layout(root, surface_rect(), &mut scratch);

    // Open (show overlays) and start the slide-in via a throwaway EventCx, applying the
    // deferred hidden/focus/scope and starting the slide in a local registry.
    let mut anims = AnimationRegistry::new();
    {
        let ev = read_pointer();
        let (hidden, _focus, _scope, requested) = {
            let mut cx = EventCx::__new_pointer(&mut states, &bindings, &ev);
            cx.__set_focused(store.focused());
            handle.open(&mut cx);
            (
                cx.__take_hidden_requests(),
                cx.__take_focus_request(),
                cx.__take_focus_scope_request(),
                cx.__take_animation_requests(),
            )
        };
        for (id, h) in hidden {
            store.set_hidden(id, h);
        }
        for anim in requested {
            anims.start(anim);
        }
    }

    Harness {
        gpu,
        renderer,
        surface,
        store,
        root,
        anims,
        primitives: Vec::new(),
    }
}

/// The section 28 animation-tick hot path in isolation: advance the registry one fixed
/// step and re-derive world. This is the part that runs every animating frame regardless
/// of the renderer, and the part section 28 governs ("animation ticks" must not allocate).
fn tick_step(h: &mut Harness) {
    h.anims.tick(&mut h.store, STEP);
    h.store.resolve_transforms(h.root);
}

/// One full in-flight slide frame: the tick/transform step, then paint/upload/submit — the
/// whole per-frame path an animating sheet takes through the renderer.
fn slide_frame(h: &mut Harness) {
    tick_step(h);
    h.primitives.clear();
    paint_tree(&h.store, h.root, &mut h.primitives);
    h.renderer.upload(&mut h.gpu, &h.primitives);
    h.renderer
        .submit(&mut h.gpu, h.surface, CLEAR, [W as f32, H as f32]);
}

/// Re-arm a fresh long slide on the content so it stays in flight across the whole
/// warmup+measure budget (the harness's default `SLIDE` is short enough to finish
/// mid-measurement). Linear easing keeps every step's advance identical.
fn arm_long_slide(h: &mut Harness) {
    let content = content_of(&h.store, h.root);
    h.anims = AnimationRegistry::new();
    h.anims.start(TranslateAnim::new(
        content,
        viso::ui::Vec2 { x: 0.0, y: -EXTENT },
        viso::ui::Vec2::ZERO,
        Duration::from_secs(10),
        viso::ui::Easing::Linear,
    ));
}

#[test]
fn the_animation_tick_step_is_allocation_free() {
    // Section 28: the animation tick is a zero-allocation hot path. Measure the tick +
    // transform-resolve step alone (no renderer): advancing the slide and re-deriving
    // world must not touch the heap — `AnimationRegistry::tick` writes translate in place
    // and `resolve_transforms` is a recursive fold over preallocated hot stores.
    let mut h = setup_alloc();
    arm_long_slide(&mut h);

    // Warm up so any first-touch capacity growth is behind us.
    for _ in 0..4 {
        tick_step(&mut h);
    }

    for i in 0..2 {
        ALLOCS.store(0, Ordering::Relaxed);
        ARMED.store(true, Ordering::Relaxed);
        tick_step(&mut h);
        ARMED.store(false, Ordering::Relaxed);
        assert_eq!(
            ALLOCS.load(Ordering::Relaxed),
            0,
            "tick {i}: the animation tick + transform resolve must be allocation-free \
             (section 28 animation ticks)"
        );
    }

    // The slide is still in flight (armed for 10s, ticked ~6 * 16ms), so this measured a
    // genuinely animating step, not a settled no-op.
    assert!(
        !h.anims.is_empty(),
        "the long slide is still in flight during measurement"
    );
}

#[test]
fn an_in_flight_slide_frame_is_deterministic_and_reuses_gpu_resources() {
    // The full animating frame (tick + paint + upload + submit) legitimately re-uploads
    // the changed geometry each frame — that is the slide. What must hold is that two
    // identical in-flight frames allocate the same amount (no unbounded scratch churn) and
    // that no GPU resource is created per frame (buffers/textures/bind groups are reused —
    // section 17.4). This is the modal steady-frame invariant extended to a moving frame.
    let mut h = setup_alloc();
    arm_long_slide(&mut h);

    for _ in 0..4 {
        slide_frame(&mut h);
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
        slide_frame(&mut h);
        ARMED.store(false, Ordering::Relaxed);
        *slot = ALLOCS.load(Ordering::Relaxed);

        assert_eq!(
            h.gpu.buffer_count(),
            buffers,
            "frame {i}: a GPU buffer was allocated for an in-flight slide frame"
        );
        assert_eq!(
            h.gpu.texture_count(),
            textures,
            "frame {i}: a GPU texture was allocated for an in-flight slide frame"
        );
        assert_eq!(
            h.gpu.bind_group_count(),
            bind_groups,
            "frame {i}: a bind group was allocated for an in-flight slide frame"
        );
    }

    assert_eq!(
        frame_allocs[0], frame_allocs[1],
        "an in-flight slide frame allocated a different amount on two identical frames \
         ({} vs {}): the paint / encode scratch is not deterministic",
        frame_allocs[0], frame_allocs[1]
    );

    let FrameStats {
        draw_calls,
        instances,
    } = stats;
    assert!(draw_calls > 0, "the sliding sheet must emit draw calls");
    assert!(instances > 0, "the sliding sheet must emit instances");
}

#[test]
fn a_settled_sheet_registry_is_empty_and_a_forced_re_tick_allocates_nothing() {
    let mut h = setup_alloc();

    // Settle the slide-in fully: the registry empties (the driver would stop scheduling
    // frames — the zero-CPU-when-idle contract).
    h.anims
        .tick(&mut h.store, SLIDE + Duration::from_millis(16));
    h.store.resolve_transforms(h.root);
    assert!(
        h.anims.is_empty(),
        "a settled sheet has an empty animation registry (the loop would halt)"
    );

    // A forced re-tick over the empty registry — the guard a still-scheduled frame would
    // hit — allocates nothing (no work to do, no scratch churn).
    ALLOCS.store(0, Ordering::Relaxed);
    ARMED.store(true, Ordering::Relaxed);
    h.anims.tick(&mut h.store, STEP);
    ARMED.store(false, Ordering::Relaxed);
    assert_eq!(
        ALLOCS.load(Ordering::Relaxed),
        0,
        "a tick over an empty registry must allocate nothing"
    );
    assert!(
        h.anims.is_empty(),
        "the registry stays empty after a no-op tick"
    );
}
