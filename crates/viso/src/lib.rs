//! # Viso
//!
//! A Rust-native, GPU-first, cross-platform application framework.
//!
//! ```no_run
//! use viso::prelude::*;
//!
//! struct App;
//!
//! impl Application for App {
//!     fn new(_cx: &mut AppCx) -> Self {
//!         App
//!     }
//! }
//!
//! fn main() {
//!     viso::run::<App>();
//! }
//! ```
//!
//! This crate is the single public facade. Ordinary apps depend only on `viso`
//! and never on the internal crates (`viso-ui`, `viso-render`, `viso-runtime`,
//! …). Internal complexity is allowed; public accidental complexity is not.
//!
//! Design summary:
//! > External declarative, internal retained.
//! > External object-oriented, internal data-oriented.
//! > Dynamic in development, AOT in release.
//! > Abstraction on cold paths, flat data on hot paths.
//!
//! [`run`] owns the platform event pump and frame scheduler. It opens a native
//! window (headless when no native backend is available), handles resize,
//! receives input, and drives the 12-phase frame. A real retained UI tree flows
//! Component → Node → Flex layout → paint → renderer to the GPU, and each frame
//! recomputes only the invalidated subtree rather than the whole tree.

#![forbid(unsafe_op_in_unsafe_fn)]

use viso_gpu::{Backend, GpuBackend, SurfaceId};
use viso_platform::{RawWindowHandle, WindowConfig, WindowId};
use viso_render::{Primitive, Rect, Renderer};
use viso_runtime::{FramePhase, RuntimeCx, Scheduler};
use viso_ui::{
    AnimationRegistry, BindingTable, BuildCx, ComputedStore, DirtyClass, EffectStore,
    FrameRecompute, ImeEvent, Key, KeyEvent, KeyRouter, Modifiers, NodeId, NodeStore,
    PointerButtons, PointerEvent, PointerPhase, PointerRouter, ScrollEvent, ScrollRouter, StateId,
    StateStore, TextEdits, TextRequest, TimerRegistry, TimerRequest, TranslateAnim, VirtualLists,
    focus_next, text_edit, virtual_list,
};

mod text_content;
use text_content::TextShaper;

pub use viso_ui::context::AppCx;

// The `ui!` proc-macro lives in the compile-time-only `viso-ui-macros` crate; it
// emits `::viso_ui::…` builder tokens but does not itself depend on `viso-ui`. The
// facade re-exports it and already depends on `viso-ui`, so those emitted paths
// resolve at the call site — the same reverse-re-export shape `viso-gpu` uses for
// `viso_macros::GpuInstance`.
pub use viso_ui_macros::ui;

/// The application entry-point contract implemented by every Viso app.
///
/// The single generic entry point is [`run`]. An `Application` owns top-level
/// state; it is not forced to contain a Router or a global Store — those are
/// opt-in.
pub trait Application: Sized + 'static {
    /// Construct the application. Windows and services are created via `cx`.
    fn new(cx: &mut AppCx) -> Self;

    /// Author the app's retained scene: declare nodes, allocate reactive state,
    /// register handlers, and wire state→node bindings through `cx`. Runs once
    /// on launch and replaces the framework's default (empty) scene. `&mut self`
    /// so the app can stash the [`StateId`](viso_ui::StateId)s / node handles it
    /// reads from its handlers. The default builds nothing — an empty window.
    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let _ = cx;
    }
}

/// Run a Viso application to completion.
///
/// Owns the platform event pump and the frame scheduler. It creates the native
/// platform app (falling back to a headless app where no native backend
/// exists — CI, tests), builds an [`AppDriver`] that bridges the runtime's
/// UI-agnostic [`viso_runtime::FrameDriver`] to the user's [`Application`] and
/// its [`AppCx`], and runs the scheduler until the last window closes.
pub fn run<A: Application>() {
    let platform_app =
        viso_platform::create_app().unwrap_or_else(|_| viso_platform::create_headless_app());
    let driver = AppDriver::<A>::new();
    Scheduler::new(platform_app, driver).run();
}

/// Deterministic headless-loop harness for facade integration tests.
///
/// Not part of the public API — this is the section 66 first-class headless
/// seam, hidden from docs and only meant to be reached by this crate's own
/// integration tests. It drives the *real* [`AppDriver`] frame loop (the same
/// one [`run`] uses) through a [`Scheduler`] over a scripted [`HeadlessApp`],
/// injecting a self-stepping [`FixedStepClock`] so every animation frame
/// observes a fixed, reproducible delta with no test intervention between
/// beats (the pump loops internally while `wants_animation` holds). It returns
/// the driver after the pump exits so a test can inspect the settled retained
/// tree — world rects, per-frame recompute counts, and whether the loop halted.
#[doc(hidden)]
pub mod __test_support {
    use super::AppDriver;
    use crate::Application;
    use std::time::{Duration, Instant};
    use viso_runtime::{FixedStepClock, Scheduler};

    /// Inspection view over a settled [`AppDriver`], returned from
    /// [`drive_scripted`]. Exposes exactly what the Commit-4 facade tests read:
    /// the retained store (for `world`/`dirty`/`translate`), the tree root, the
    /// last frame's recompute counts, and the loop's animation state.
    pub struct DrivenApp<A: Application> {
        driver: AppDriver<A>,
    }

    impl<A: Application> DrivenApp<A> {
        /// The retained tree of the first (launch) window. Read
        /// `world`/`bounds`/`translate` off it.
        pub fn store(&self) -> &super::NodeStore {
            &self.driver.windows[0].store
        }

        /// The first window's declared root node, if any.
        pub fn root(&self) -> Option<super::NodeId> {
            self.driver.windows[0].root
        }

        /// How much each layer recomputed on the first window's most recent
        /// frame. A pure TRANSFORM animation frame has `laid_out == 0` yet
        /// `painted > 0` — the observable proof that world moved without a
        /// relayout.
        pub fn recompute(&self) -> super::FrameRecompute {
            self.driver.windows[0].recompute
        }

        /// Whether the driver still wants animation frames — false once every
        /// slide has settled (the frame-halt / zero-CPU-when-idle contract).
        pub fn wants_animation(&self) -> bool {
            use viso_runtime::FrameDriver;
            self.driver.wants_animation()
        }

        /// The driver's earliest live timer deadline, or `None` when no timer is
        /// armed. `Some` while a toast waits (the scheduler blocks on it via
        /// `WaitUntil`); `None` once every timer has fired — the timer-side
        /// frame-halt contract, the counterpart to `wants_animation`.
        pub fn next_timer_deadline(&self) -> Option<Instant> {
            use viso_runtime::FrameDriver;
            self.driver.next_timer_deadline()
        }
    }

    /// Build application `A`, replay `script` through a headless pump driven by
    /// a [`FixedStepClock`] stepping `step` per frame, and return the settled
    /// driver once the loop exits. `script` is a sequence of raw platform events
    /// (an input that triggers a handler, followed by redraw beats to advance
    /// the animation); a driver that `wants_animation` self-reschedules further
    /// beats until its registry empties, so the tail need only prime the pump.
    pub fn drive_scripted<A: Application>(
        script: Vec<viso_platform::RawEvent>,
        step: Duration,
    ) -> DrivenApp<A> {
        let app = Box::new(viso_platform::backend::headless::HeadlessApp::scripted(
            script,
        ));
        let driver = AppDriver::<A>::new();
        let clock = FixedStepClock::new(Instant::now(), step);
        let driver = Scheduler::with_clock(app, driver, clock).run_returning();
        DrivenApp { driver }
    }
}

/// Bridges the UI-agnostic runtime [`viso_runtime::FrameDriver`] to the user's
/// [`Application`].
///
/// It owns the user app and its [`AppCx`], and lives above the runtime in the
/// DAG (the facade legally depends on both `viso-runtime` and `viso-ui`), so it
/// is the only place where the two meet. The user `Application` is constructed
/// lazily on launch — after the pump is live — matching AppKit's
/// finish-launching-then-loop ordering.
struct AppDriver<A: Application> {
    app: Option<A>,
    /// The application-scope context. `AppCx` is a lifetime-marked capability
    /// handle; the facade owns it for the whole session, so it is `'static`
    /// here. (In Phase 1 it is a marker type; real capabilities land later.)
    cx: AppCx<'static>,
    /// The live windows. One `Application` drives every window, but each window
    /// owns its own retained tree, reactive state, GPU surface, animations, and
    /// timers — an independent [`WindowState`]. A desktop session has a
    /// single-digit window count, and every frame *iterates all windows* (it
    /// never keys a lookup by id on the hot path), so a linear `Vec` is the
    /// most compact, cache-friendly layout — a `HashMap` would only add a hash
    /// and a heap bucket per access with nothing to lookup against (section 45).
    /// Ordered by open time; `windows[0]` is the launch window.
    windows: Vec<WindowState>,
}

/// Everything one window owns: its retained tree, reactive state, GPU surface,
/// live animations/timers, and the reusable scratch the frame hot path drains
/// into. The session-scope `Application` and `AppCx` stay on [`AppDriver`]; each
/// window holds an independent instance of everything below, so closing a window
/// drops exactly its tree and GPU resources (its `EffectStore` cancelled first,
/// so scoped effects release their resources — cleanup then drop).
struct WindowState {
    /// The window this state drives. Input, geometry, and close events name a
    /// [`WindowId`]; the driver routes each to the matching state by this field.
    window: WindowId,
    /// The GPU state, created on launch once the window exposes a native
    /// windowing handle. `None` until then, and `None` for the whole session
    /// under a headless window (a `RawWindowHandle::Headless`): the tree still
    /// builds and lays out against `surface_size`, it just paints to no surface.
    gpu: Option<GpuState>,
    /// The current surface size in physical pixels, held independently of
    /// [`gpu`](Self::gpu). Layout resolves against this every frame, so the
    /// retained tree is measured/placed and its world rects re-derived even
    /// when there is no GPU (a headless window on any platform, including the
    /// Metal target where a headless surface cannot be created). `on_geometry`
    /// keeps it and the swapchain in step; seeded from the window's launch size.
    surface_size: (u32, u32),
    /// The retained UI tree: real nodes built once on launch, then relaid only
    /// where invalidated each frame and painted to primitives.
    store: NodeStore,
    /// Reactive state cells. Writes record a pending change; the frame's flush
    /// phase turns each changed cell into targeted node dirtying via `bindings`.
    states: StateStore,
    /// Compiled state→node edges. Built alongside the tree; read every flush.
    bindings: BindingTable,
    /// Pure cached derivations. The flush wakes those whose dependencies changed
    /// and dirties their downstream nodes only when the derived value changed.
    computeds: ComputedStore,
    /// Side effects scoped to nodes. The flush re-runs those whose dependencies
    /// changed; freeing a node cancels its effects (cleanup then drop).
    effects: EffectStore,
    /// Virtualized lists keyed by viewport node. Reconciled each frame before
    /// layout: reads each list's scroll, remounts only the visible range's hosts,
    /// keeps the content extent (so `scroll_range` is right) with ~40 mounted
    /// nodes instead of one per logical item. Driver-owned so a with-reactive
    /// build cx can register lists and the frame can drive reconcile.
    virtual_lists: VirtualLists,
    /// Retained text-edit buffers keyed by text-control node. A control registers
    /// its buffer at build time; a key/IME handler records `EditIntent`s that the
    /// router queues onto the node's buffer, and `text_edit::reconcile` (before
    /// text shaping each layout phase) applies them and re-declares the node's
    /// `TextRequest` when the text changed. Driver-owned, mirroring `virtual_lists`.
    text_edits: TextEdits,
    /// Live transform-only animations (a sheet sliding in, a scroll settling).
    /// Ticked once per frame at the head of the flush phase with the frame delta,
    /// writing each animating node's interpolated world-space translate before
    /// layout/transform/paint consume it that same frame. Empty in steady state,
    /// which is what `wants_animation` reads to let the frame loop halt.
    /// Driver-owned so a build cx can register no animations directly — they are
    /// requested through `EventCx` and handed off via the store's queue.
    animations: AnimationRegistry,
    /// Reusable buffer the flush drains the store's queued animation requests
    /// into before starting each on `animations`, so handing a frame's slide-in
    /// requests to the registry allocates nothing on the steady path.
    anim_requests: Vec<TranslateAnim>,
    /// Live one-shot UI timers (a toast's auto-dismiss). Unlike `animations`, a
    /// live timer costs *no* frame while it waits: the driver surfaces its
    /// earliest deadline through `next_timer_deadline`, which the scheduler turns
    /// into a `ControlFlow::WaitUntil` so the loop blocks until the deadline
    /// instead of spinning. Fired once at the head of the flush phase with the
    /// frame instant; empties itself as timers fire so the loop can halt again.
    timers: TimerRegistry,
    /// Reusable buffer the flush drains the store's queued timer requests into
    /// before arming each on `timers` against the frame instant, so handing a
    /// frame's toast timer to the registry allocates nothing on the steady path.
    timer_requests: Vec<TimerRequest>,
    /// Reusable buffer the flush drains this frame's pending state ids into, so
    /// the steady path allocates nothing while draining the transaction.
    changed: Vec<StateId>,
    /// The tree root declared by the application's `build`, if it authored one.
    root: Option<NodeId>,
    /// Reusable ancestry buffer the pointer router fills each event, owned here
    /// so routing a pointer allocates nothing on the steady path.
    route_chain: Vec<NodeId>,
    /// Reusable primitive buffer. Rebuilt only on a paint-affecting frame; reused
    /// verbatim (and re-uploaded) on frames with no paint invalidation.
    primitives: Vec<Primitive>,
    /// Reusable child-id scratch for the layout passes.
    scratch: Vec<u32>,
    /// Reusable buffer of redo roots for incremental relayout, owned here so a
    /// relayout allocates nothing on the hot path.
    redo_roots: Vec<NodeId>,
    /// How much each layer recomputed on the most recent frame — surfaced for
    /// diagnostics and asserted by tests to confirm only the dirty subtree moved.
    recompute: FrameRecompute,
    /// True until the first frame has been submitted. Lets the Submit phase emit
    /// a one-shot diagnostic (gated on `VISO_FRAME_TRACE`) proving the first
    /// frame reached the GPU, then fall dark for every steady-state frame.
    awaiting_first_frame: bool,
    /// The facade's font stack + glyph atlas. Shapes each node's `TextRequest`
    /// into a `Content::Text` payload (`viso-ui` holds no font stack). `None`
    /// until launch, when the embedded UI font is loaded.
    text: Option<TextShaper>,
    /// Reusable buffer the text seam drains pending requests into, so re-shaping
    /// text allocates only the shaped payloads, not the request list.
    text_scratch: Vec<(NodeId, Box<TextRequest>)>,
}

/// The facade-owned GPU state: the concrete backend, the renderer, and the
/// per-window surface.
///
/// The backend is the compile-time-selected concrete [`Backend`] (Metal on
/// macOS, software raster elsewhere), held by value so the frame hot path is
/// monomorphized — no `dyn GpuBackend`.
struct GpuState {
    backend: Backend,
    renderer: Renderer,
    surface: SurfaceId,
    /// Current surface size in physical pixels `(width, height)`.
    size: (u32, u32),
}

impl<A: Application> AppDriver<A> {
    fn new() -> Self {
        Self {
            app: None,
            cx: AppCx::__new(),
            windows: Vec::new(),
        }
    }

    /// The live window state for `id`, by linear scan. Windows number in the
    /// single digits, so this is cheaper than a keyed map (section 45).
    fn window_mut(&mut self, id: WindowId) -> Option<&mut WindowState> {
        self.windows.iter_mut().find(|w| w.window == id)
    }
}

impl WindowState {
    /// A freshly-opened window's state: empty stores/registries and no GPU yet
    /// (the driver brings the surface up once it has the window's native handle).
    /// Mirrors the session-scope initialisation the single-window driver did on
    /// launch, now scoped to one window.
    fn new(window: WindowId) -> Self {
        Self {
            window,
            gpu: None,
            surface_size: (1, 1),
            store: NodeStore::new(),
            states: StateStore::new(),
            bindings: BindingTable::new(),
            computeds: ComputedStore::new(),
            effects: EffectStore::new(),
            virtual_lists: VirtualLists::new(),
            text_edits: TextEdits::new(),
            animations: AnimationRegistry::new(),
            anim_requests: Vec::new(),
            timers: TimerRegistry::new(),
            timer_requests: Vec::new(),
            changed: Vec::new(),
            root: None,
            route_chain: Vec::new(),
            primitives: Vec::new(),
            scratch: Vec::new(),
            redo_roots: Vec::new(),
            recompute: FrameRecompute::default(),
            awaiting_first_frame: true,
            text: None,
            text_scratch: Vec::new(),
        }
    }

    /// Shape every node that carries a pending [`TextRequest`] into a
    /// `Content::Text` payload, uploading newly-packed glyphs to the atlas.
    /// Runs after a build/rebuild and before layout, so a `Fit` text node
    /// measures against the shaped run's natural size. A no-op with no GPU
    /// (headless-without-surface) or no shaper (pre-launch).
    fn shape_pending_text(&mut self) {
        let (Some(gpu), Some(text)) = (self.gpu.as_mut(), self.text.as_mut()) else {
            return;
        };
        self.store.take_text_requests(&mut self.text_scratch);
        if self.text_scratch.is_empty() {
            return;
        }
        // DPI factor 1.0 for now: the surface density plumbs through with the
        // scale-aware input path later; the embedded font rasterizes at 1x.
        let dpi = 1.0;
        for (id, request) in self.text_scratch.drain(..) {
            let content = text.shape(&mut gpu.backend, &request, dpi);
            self.store.set_content_payload(id, content);
        }
    }

    /// Incrementally relayout and repaint against the current surface size,
    /// recording how much each layer touched. Only the subtrees carrying
    /// measure/layout invalidation are re-placed; paint is rebuilt only when a
    /// paint-affecting class is pending, otherwise the primitive buffer is left
    /// intact for reuse. A no-op if the tree is absent. Runs with no GPU too (a
    /// headless window): layout/transform resolve against `surface_size` and
    /// paint fills `primitives`; only the GPU upload/submit phases skip.
    fn relayout_and_paint(&mut self) {
        let Some(root) = self.root else {
            return;
        };
        let (w, h) = self.surface_size;
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: w as f32,
            h: h as f32,
        };
        let (measured, laid_out) =
            self.store
                .relayout_dirty(root, surface, &mut self.scratch, &mut self.redo_roots);
        // Re-derive world rects when a transform-only class is pending. A frame
        // that relaid anything already resolved transforms inside `relayout_dirty`
        // (which folds fresh `bounds` into `world` from the root, as the whole-tree
        // `NodeStore::layout` does), but a pure TRANSFORM frame (a scroll step, an
        // animation tick) marks no MEASURE/LAYOUT and so triggers no redo — its
        // moved translate would otherwise not reach `world`, and paint/hit-test
        // read only world. Gated on the TRANSFORM class so a paint-only frame (a
        // colour change) skips it; harmlessly idempotent on a layout frame that
        // also carried TRANSFORM. This is the wiring that makes section 8.7
        // transform-only updates land in a real frame loop, for scroll and
        // animation alike.
        if self.store.any_dirty_class(DirtyClass::TRANSFORM) {
            self.store.resolve_transforms(root);
        }
        let painted = self.store.repaint_dirty(root, &mut self.primitives);
        self.recompute = FrameRecompute {
            measured,
            laid_out,
            painted,
        };
    }
}

impl<A: Application> viso_runtime::FrameDriver for AppDriver<A> {
    fn on_launch(&mut self, cx: &mut RuntimeCx<'_>) {
        // Construct the user application now that the pump is live.
        self.app = Some(A::new(&mut self.cx));
        // Open the initial window. Later phases let the app request its own
        // windows via `AppCx`; Phase 2 opens one canonical window.
        let Ok(id) = cx.create_window(WindowConfig::default()) else {
            return;
        };
        let mut ws = WindowState::new(id);

        // Record the launch surface size up front, independent of the GPU: the
        // tree lays out against this every frame, so a headless window (no GPU)
        // still measures and places the whole tree, it just paints to nothing.
        let (w, h) = cx.inner_size(id).unwrap_or((1, 1));
        ws.surface_size = (w.max(1), h.max(1));

        // Bring up the GPU for this window when it exposes a real windowing
        // handle: create the device, attach a surface to that handle, and build
        // the renderer for its format. A headless window reports a
        // `RawWindowHandle::Headless` (or no handle at all) — there is no
        // surface to attach, and on a real-GPU target such as Metal
        // `create_surface` would reject the non-native handle — so we stay
        // `gpu = None` for the session and draw nothing. The retained tree below
        // is built and driven regardless; this is section 66's headless backend
        // as a first-class path, not a degraded one.
        let native_handle = match cx.raw_handle(id) {
            Some(RawWindowHandle::Headless) | None => None,
            Some(handle) => Some(handle),
        };
        if let Some(raw) = native_handle {
            let mut backend = viso_gpu::create_device();
            let surface = backend.create_surface(raw, w.max(1), h.max(1));
            let format = backend.surface_format(surface);
            let renderer = Renderer::new(&mut backend, format);

            ws.gpu = Some(GpuState {
                backend,
                renderer,
                surface,
                size: (w.max(1), h.max(1)),
            });
            // Load the font stack now that a backend exists to allocate the atlas.
            ws.text = Some(TextShaper::new());
        }

        // Build the user application's retained UI tree once, now that we have a
        // surface size. The window owns `store`, `states`, and `bindings` as
        // sibling fields, so all three can be borrowed together into a reactive
        // build context — this is why scene authoring works here where new-time
        // allocation could not (the session-long `AppCx` marker cannot retain a
        // live store borrow). Layout runs incrementally per frame in `run_phase`.
        //
        // When structural teardown arrives (a targeted rebuild that frees nodes),
        // each freed node must run `ws.effects.cancel_for_node(id)` before its
        // slot is reused, so scoped effects release their resources (cleanup then
        // drop) at unmount. Whole-window teardown at close runs the same cleanups
        // through `EffectStore::cancel_all` in `on_window_closed`; a per-node
        // rebuild has no live call site yet, so `build` runs once and frees
        // nothing here.
        ws.store.clear();
        ws.virtual_lists.clear();
        ws.text_edits.clear();
        if let Some(app) = &mut self.app {
            let mut build = BuildCx::with_reactive(
                &mut ws.store,
                &mut ws.states,
                &mut ws.bindings,
                &mut ws.virtual_lists,
                &mut ws.text_edits,
            );
            app.build(&mut build);
            ws.root = build.root();
        }

        // Shape any static text declared during build into content payloads,
        // before the first measure so a `Fit` text node sizes to its run.
        ws.shape_pending_text();

        // Seed the first frame: mark the root fully dirty so the incremental
        // passes do the initial measure/layout/paint for the whole tree.
        if let Some(root) = ws.root {
            ws.store.mark_dirty(
                root,
                DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
            );
        }

        self.windows.push(ws);
    }

    fn on_geometry(&mut self, window: WindowId, _scale: f64, width: u32, height: u32) {
        // Resize the named window's swapchain so its next frame maps pixel-space
        // to the new extent, and mark its root for relayout so the next
        // incremental frame re-places the tree against the new surface and
        // repaints. Record the new extent on `surface_size` regardless of GPU:
        // layout resolves against it, so a headless window still re-places the
        // tree on resize. An event for an unknown window (already closed) is a
        // no-op.
        let Some(ws) = self.window_mut(window) else {
            return;
        };
        let (w, h) = (width.max(1), height.max(1));
        ws.surface_size = (w, h);
        if let Some(gpu) = &mut ws.gpu {
            gpu.backend.resize_surface(gpu.surface, w, h);
            gpu.size = (w, h);
        }
        if let Some(root) = ws.root {
            ws.store.mark_dirty(
                root,
                DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
            );
        }
    }

    fn on_input(&mut self, sample: viso_runtime::InputSample) {
        // The sample is already in physical pixels (the scheduler resolved the
        // window scale), so it maps straight onto the UI-tier `PointerEvent` —
        // the same space as node bounds and hit testing. Route it to the target
        // window's tree along the hit node's ancestry; any state a handler writes
        // lands in that window's pending set and is turned into targeted dirtying
        // by the next frame's flush (the scheduler already flagged the frame
        // input-dirty). A sample naming an unknown window (already closed) is
        // dropped.
        let target = sample.window();
        let Some(ws) = self.window_mut(target) else {
            return;
        };
        let Some(root) = ws.root else {
            return;
        };
        match sample {
            viso_runtime::InputSample::Pointer(p) => {
                let ev = PointerEvent {
                    x: p.x,
                    y: p.y,
                    phase: match p.phase {
                        viso_runtime::PointerPhase::Down => PointerPhase::Down,
                        viso_runtime::PointerPhase::Move => PointerPhase::Move,
                        viso_runtime::PointerPhase::Up => PointerPhase::Up,
                        viso_runtime::PointerPhase::Leave => PointerPhase::Leave,
                    },
                    buttons: PointerButtons(p.buttons),
                    modifiers: Modifiers {
                        shift: p.modifiers.shift,
                        control: p.modifiers.control,
                        alt: p.modifiers.alt,
                        logo: p.modifiers.logo,
                    },
                };
                PointerRouter::route(
                    &mut ws.store,
                    &mut ws.states,
                    &ws.bindings,
                    root,
                    ev,
                    &mut ws.route_chain,
                );
            }
            viso_runtime::InputSample::Key(k) => {
                // Lower the runtime-tier key sample onto the UI-tier event, then
                // route by focus (not hit test). Tab is a framework-level
                // focus-traversal command: on a Tab press we advance the focus
                // ring instead of routing the key to a handler (Shift-Tab goes
                // backward). Every other key routes to the focused node's
                // ancestry so a control can react to it.
                let modifiers = lower_modifiers(k.modifiers);
                if matches!(k.key, viso_runtime::Key::Tab) && k.pressed {
                    focus_next(&mut ws.store, root, !modifiers.shift);
                } else {
                    let ev = KeyEvent {
                        key: lower_key(k.key),
                        pressed: k.pressed,
                        repeat: k.repeat,
                        modifiers,
                    };
                    KeyRouter::route_key(
                        &mut ws.store,
                        &mut ws.states,
                        &ws.bindings,
                        &mut ws.text_edits,
                        root,
                        ev,
                        &mut ws.route_chain,
                    );
                }
            }
            viso_runtime::InputSample::Text(t) => {
                // A committed (post-IME) segment routes to the focused node as an
                // IME commit — the text a control appends to its buffer.
                KeyRouter::route_ime(
                    &mut ws.store,
                    &mut ws.states,
                    &ws.bindings,
                    &mut ws.text_edits,
                    root,
                    ImeEvent::Commit { text: t.text },
                    &mut ws.route_chain,
                );
            }
            viso_runtime::InputSample::ImePreedit(p) => {
                // An in-progress composition routes as a preedit; a control shows
                // it inline and replaces it on each update until the commit.
                KeyRouter::route_ime(
                    &mut ws.store,
                    &mut ws.states,
                    &ws.bindings,
                    &mut ws.text_edits,
                    root,
                    ImeEvent::Preedit {
                        text: p.text,
                        caret: p.caret,
                    },
                    &mut ws.route_chain,
                );
            }
            viso_runtime::InputSample::Scroll(s) => {
                // A wheel/trackpad sample routes to the innermost scroll viewport
                // under the pointer. The runtime reports the delta as content
                // motion (positive = content moves down/right, revealing later
                // content), which is exactly the direction the viewport's offset
                // grows, so it maps straight onto the offset delta. The router
                // clamps per axis and marks only TRANSFORM/HIT_TEST/PAINT — a
                // scroll never relayouts — so no state flush is involved.
                let ev = ScrollEvent {
                    x: s.x,
                    y: s.y,
                    delta_x: s.delta_x,
                    delta_y: s.delta_y,
                    modifiers: lower_modifiers(s.modifiers),
                };
                ScrollRouter::route(&mut ws.store, root, ev);
            }
        }
    }

    fn run_phase(&mut self, phase: FramePhase, cx: &mut RuntimeCx<'_>) {
        // The render phases drive the GPU from the real retained tree: Measure +
        // Layout resolve node boxes, then paint lowers them to primitives which
        // the renderer batches and submits. Non-render phases are no-ops here.
        //
        // Each phase runs once per open window. Windows own disjoint state
        // (store/gpu/animations/timers), so the per-window bodies are
        // independent — a linear walk of `self.windows` is the whole fan-out.
        // A single-window app (the common case) walks a one-element vec; the
        // cost scales with the window count, not with any per-window map lookup
        // (section 45: we iterate all windows, never key one).
        match phase {
            FramePhase::FlushStateTransactions => {
                for ws in &mut self.windows {
                    // Start any animations a handler requested this frame — a
                    // sheet sliding in queued a `TranslateAnim` through
                    // `EventCx`, which the router handed to the store's queue;
                    // drain it into the registry now. Then advance every live
                    // animation by this frame's delta, writing each node's
                    // interpolated translate through `set_translate` (a
                    // TRANSFORM|HIT_TEST|PAINT write). This runs before the state
                    // flush and the Layout phase below, so the moved translate is
                    // resolved into `world` and painted this same frame. Both
                    // steps are no-ops when nothing is animating (the steady
                    // case) — the queue is empty and the registry has nothing to
                    // tick.
                    ws.store.take_animation_requests(&mut ws.anim_requests);
                    for anim in ws.anim_requests.drain(..) {
                        ws.animations.start(anim);
                    }
                    if !ws.animations.is_empty() {
                        ws.animations.tick(&mut ws.store, cx.frame_delta());
                    }
                    // Arm any one-shot timers a handler requested this frame (a
                    // toast's auto-dismiss), then fire every timer whose deadline
                    // this frame's instant has crossed. Arming resolves each
                    // request's `delay` against `frame_now` (not a stray
                    // `Instant::now`), so a headless `ManualClock` stays
                    // deterministic. `fire_due` runs each due callback with the
                    // live store and removes it, so the registry empties itself
                    // and the loop can halt again — a timer costs no frame while
                    // it waits, only the one frame it fires on. Both steps are
                    // no-ops when no timer is armed or pending (the steady case).
                    ws.store.take_timer_requests(&mut ws.timer_requests);
                    for req in ws.timer_requests.drain(..) {
                        ws.timers.arm_request(req, cx.frame_now());
                    }
                    if !ws.timers.is_empty() {
                        ws.timers.fire_due(&mut ws.store, cx.frame_now());
                    }
                    // Drain this frame's pending state writes once and fan the
                    // same changed set through the three downstream reactors, in
                    // order. Many writes in one transaction collapse here; a
                    // frame with no writes touches nothing.
                    if ws.states.has_pending() {
                        ws.states.take_pending(&mut ws.changed);
                        // 1. Derivations first: a memo-gated re-eval dirties a
                        //    node only when its derived value actually changed,
                        //    so any dirtying it produces is in place before
                        //    Measure/Layout.
                        ws.computeds
                            .wake_computed(&ws.changed, &ws.states, &mut ws.store);
                        // 2. Direct bindings: turn each changed cell into targeted
                        //    node dirtying through the compiled static +
                        //    dynamic-script edges. (Computed no longer registers
                        //    dynamic edges, so a derivation's node is dirtied
                        //    once, by the pass above.)
                        ws.store.flush_state_transactions(&ws.changed, &ws.bindings);
                        // 3. Effects: re-run those whose dependencies changed. An
                        //    effect that writes state records it as pending for
                        //    the next frame; the scheduler carries state-dirty
                        //    forward, so a follow-up frame runs — no in-frame
                        //    cascade.
                        ws.effects.wake(&ws.changed, &ws.states);
                        ws.changed.clear();
                    }
                }
            }
            FramePhase::Layout => {
                for ws in &mut self.windows {
                    // Reconcile virtualized lists first: each list reads its
                    // viewport's current scroll, and only when the visible range
                    // crossed a row boundary does it recycle/rebind a handful of
                    // hosts and mark the canvas LAYOUT|MEASURE. Steady scroll
                    // within a row is a no-op here (the scroll's TRANSFORM already
                    // moved the mounted rows), so the relayout below stays
                    // confined to the changed canvas subtree.
                    virtual_list::reconcile(
                        &mut ws.store,
                        &mut ws.virtual_lists,
                        &mut ws.states,
                        &mut ws.bindings,
                        &mut ws.effects,
                    );
                    // Apply any text edits queued this frame by the key/IME router
                    // into each control's retained buffer: a buffer whose text
                    // actually changed re-declares its `TextRequest` so the
                    // shaping step below re-shapes the new string. A no-op when no
                    // control took an edit this frame (the steady case), so a
                    // non-text frame pays nothing.
                    text_edit::reconcile(&mut ws.store, &mut ws.text_edits);
                    // Shape any text (re)declared this frame — a rebuilt list row,
                    // an applied text edit, or a future reactive text update
                    // leaves a pending `TextRequest` — into a content payload
                    // before measure, so a `Fit` text node sizes to its run. A
                    // no-op when nothing declared text (the steady case).
                    ws.shape_pending_text();
                    // Incrementally re-place invalidated subtrees and repaint if
                    // any paint-affecting class is pending; a clean frame touches
                    // nothing.
                    ws.relayout_and_paint();
                    // Feed measured row heights back into each list's height model
                    // so a variable-height list corrects its extent and anchor
                    // next frame. Bounded to this frame's newly-mounted rows — no
                    // full-list sweep.
                    virtual_list::absorb_measurements(&ws.store, &mut ws.virtual_lists);
                }
            }
            FramePhase::UploadGpuChanges => {
                for ws in &mut self.windows {
                    if let Some(gpu) = &mut ws.gpu {
                        gpu.renderer.upload(&mut gpu.backend, &ws.primitives);
                    }
                }
            }
            FramePhase::Submit => {
                for ws in &mut self.windows {
                    if let Some(gpu) = &mut ws.gpu {
                        let (w, h) = gpu.size;
                        // One-shot proof the first frame reached the GPU, gated on
                        // an env var so it costs a single bool check per
                        // steady-state frame and nothing else. Read once, then the
                        // branch dies. Stats must be read before `submit` consumes
                        // the segments.
                        if ws.awaiting_first_frame {
                            ws.awaiting_first_frame = false;
                            if std::env::var_os("VISO_FRAME_TRACE").is_some() {
                                let stats = gpu.renderer.frame_stats();
                                let recompute = ws.recompute;
                                eprintln!(
                                    "viso: first frame submitting {w}x{h} {stats:?} {recompute:?}"
                                );
                            }
                        }
                        gpu.renderer.submit(
                            &mut gpu.backend,
                            gpu.surface,
                            [0.1, 0.1, 0.1, 1.0],
                            [w as f32, h as f32],
                        );
                    }
                }
            }
            FramePhase::PostFrameCleanup => {
                for ws in &mut self.windows {
                    // The incremental passes have consumed this frame's
                    // invalidation; clear every node's dirty set so the next frame
                    // starts clean and an idle frame recomputes nothing.
                    ws.store.clear_dirty();
                    // Keep the loop beating while this window has live animations.
                    // `wants_animation` already tells the scheduler to stay in
                    // `Poll`, but on a headless backend `Poll` produces no beat on
                    // its own — a beat comes only from a `request_redraw`. So
                    // self-reschedule the next frame here whenever the registry is
                    // non-empty; the moment it empties (the last slide settled)
                    // this stops firing and, once every window is idle, the loop
                    // falls idle, holding the zero-CPU-when-idle contract.
                    if !ws.animations.is_empty() {
                        cx.request_redraw(ws.window);
                    }
                }
            }
            _ => {}
        }
    }

    fn on_window_closed(&mut self, window: WindowId) {
        // A window closed — the OS close button or a programmatic
        // `RuntimeCx::close_window`, both arriving as the same `WindowClosed`
        // event, so this single hook is the one teardown path (no double
        // teardown). Cancel every scoped effect this window's tree owns first,
        // so each effect runs its cleanup (release resource) then drops, before
        // the store that indexed them is discarded — otherwise a scoped effect
        // would leak the resource it was holding. Then drop the whole
        // `WindowState`: its store/gpu/timers/animations destruct in turn, and
        // `GpuState`'s drop releases the surface/device. A hook for an unknown
        // window (already gone) finds nothing to retain and is a no-op.
        if let Some(ws) = self.window_mut(window) {
            ws.effects.cancel_all();
        }
        self.windows.retain(|w| w.window != window);
    }

    fn wants_animation(&self) -> bool {
        // Any window with a non-empty registry has at least one node mid-slide,
        // so the scheduler should keep requesting frames (each window's flush
        // phase ticks its own registry). Each window's registry empties itself
        // as its animations finish; once every window is idle this returns
        // false and the loop is free to idle — the counterpart to the per-window
        // `request_redraw` self-reschedule in `PostFrameCleanup`.
        self.windows.iter().any(|w| !w.animations.is_empty())
    }

    fn next_timer_deadline(&self) -> Option<std::time::Instant> {
        // The earliest live timer deadline across all windows (a toast's
        // auto-dismiss instant), or `None` when no window has a timer armed. The
        // scheduler turns `Some(deadline)` into a `ControlFlow::WaitUntil` so an
        // idle loop blocks until the soonest timer is due instead of spinning —
        // unlike `wants_animation`, this does *not* keep beating frames. Each
        // window's registry empties itself as its timers fire, so once the last
        // timer in the last window dismisses this returns `None` and the loop
        // falls fully idle (the zero-CPU-when-idle contract).
        self.windows
            .iter()
            .filter_map(|w| w.timers.earliest())
            .min()
    }
}

/// Lower a runtime-tier key identity onto the UI-tier one. The two enums are
/// deliberate value mirrors (the UI layer must not depend on the runtime), so
/// the facade — which sees both — is the one place that maps between them.
fn lower_key(key: viso_runtime::Key) -> Key {
    match key {
        viso_runtime::Key::Escape => Key::Escape,
        viso_runtime::Key::Enter => Key::Enter,
        viso_runtime::Key::Space => Key::Space,
        viso_runtime::Key::Tab => Key::Tab,
        viso_runtime::Key::Backspace => Key::Backspace,
        viso_runtime::Key::Left => Key::Left,
        viso_runtime::Key::Right => Key::Right,
        viso_runtime::Key::Up => Key::Up,
        viso_runtime::Key::Down => Key::Down,
        viso_runtime::Key::Delete => Key::Delete,
        viso_runtime::Key::Home => Key::Home,
        viso_runtime::Key::End => Key::End,
        viso_runtime::Key::Other(code) => Key::Other(code),
    }
}

/// Lower runtime-tier modifier state onto the UI-tier mirror (same fields, a
/// crate-boundary copy — see [`lower_key`]).
fn lower_modifiers(m: viso_runtime::Modifiers) -> Modifiers {
    Modifiers {
        shift: m.shift,
        control: m.control,
        alt: m.alt,
        logo: m.logo,
    }
}

/// The curated default prelude. Kept to a small, stable, low-ambiguity set —
/// GPU/backend/internal-compiler types never appear here.
pub mod prelude {
    pub use crate::{Application, run};
    pub use viso_ui::context::AppCx;
    pub use viso_ui::dirty::DirtyClass;
    pub use viso_ui::node::NodeId;
    // Scene authoring: the mental model for a real app is `Application::build`
    // declaring nodes and wiring reactive state, so the authoring context, the
    // layout styles, the semantics facts, and the state cell handles belong in
    // the default set (commonly used, stable, unambiguous).
    pub use viso_ui::{
        BuildCx, FlexStyle, GridPlacement, GridStyle, LeafStyle, Role, Semantics, StateId,
        StateValue, TextRequest, TrackSizing, VirtualListStyle,
    };
    // Tier 1 widgets: the base layout container, the static text control, the
    // texture-backed image control, and the vector icon control — plus the Tier 2
    // interactive controls, Button, CheckBox, Toggle, RadioGroup, Slider, and
    // TextInput, the Tier 3 layout structures — the Scroll viewport, the
    // virtualized VirtualList, the two-dimensional Grid, and the draggable-pane
    // Splitter — and the Tier 4 navigation/overlay controls, the panel-switching
    // Tabs, the page-stack NavigationStack (with its app-captured NavHandle for
    // programmatic push/pop), the Popup (a persistent anchor with a floating
    // top-layer content, with its app-captured PopupHandle for programmatic
    // open/close/toggle), the Modal (a full-surface dialog that dims the scene
    // and traps focus while open, with its app-captured ModalHandle), and the
    // Sheet (an edge drawer that slides in over a dimming scrim and traps focus,
    // with its app-captured SheetHandle for programmatic open/close/toggle). A
    // widget is a `Component` an app authors and builds into its `BuildCx`, so
    // these belong in the default set as they land.
    pub use viso_widgets::{
        Button, ButtonStyle, CheckBox, CheckBoxStyle, Grid, GridViewStyle, Icon, IconStyle, Image,
        ImageStyle, Label, LabelStyle, Modal, ModalHandle, ModalHandleSlot, ModalStyle, NavHandle,
        NavHandleSlot, NavigationStack, NavigationStackStyle, Popup, PopupHandle, PopupHandleSlot,
        PopupStyle, RadioGroup, RadioStyle, Scroll, ScrollViewStyle, Sheet, SheetEdge, SheetHandle,
        SheetHandleSlot, SheetStyle, Slider, SliderStyle, Splitter, SplitterStyle, Tabs, TabsStyle,
        TextInput, TextInputStyle, Toast, ToastEdge, ToastHandle, ToastHandleSlot, ToastStyle,
        Toggle, ToggleStyle, View, ViewStyle, VirtualList, VirtualListViewStyle, button, checkbox,
        grid, icon, image, label, modal, navigation_stack, popup, radio_group, scroll, sheet,
        slider, splitter, tabs, text_input, toast, toggle, view, virtual_list,
    };
    // The declarative view-fragment entry point (§21.5): a small local `ui! { … }`
    // fragment lowers, at Rust compile time, to a static `BuildCx` builder closure.
    pub use crate::ui;
    // Window, Text, List, Computed, Event, Task, Route, Theme, Color, Vec2, Rect,
    // Constraints and the component!/view!/routes! macros join this as their
    // subsystems land in later phases.
}

// -- Advanced escape hatches. Opt-in, clearly namespaced. --
pub mod ui {
    pub use viso_ui::*;
}
pub mod widgets {
    pub use viso_widgets::*;
}
pub mod render {
    pub use viso_render::*;
}
pub mod gpu {
    pub use viso_gpu::*;
}
pub mod platform {
    pub use viso_platform::*;
}
pub mod runtime {
    pub use viso_runtime::*;
}
