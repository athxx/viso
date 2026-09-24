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

use std::cell::Cell;

use viso_gpu::{Backend, GpuBackend, SurfaceId};
use viso_platform::{Insets, LogicalRect, RawWindowHandle, WindowId};
use viso_render::{Primitive, Rect, Renderer};
use viso_runtime::{FramePhase, RuntimeCx, Scheduler};
use viso_ui::{
    AnimationRegistry, Axis, BindingTable, BuildCx, ChromeContext, ComputedStore, DirtyClass,
    EffectStore, FlexStyle, FrameRecompute, ImeEvent, KeyEvent, KeyRouter, Modifiers, NodeId,
    NodeStore, PointerButtons, PointerEvent, PointerPhase, PointerRouter, ScrollEvent,
    ScrollRouter, SemanticProjector, Size, StateId, StateStore, TextEdits, TextRequest,
    TimerRegistry, TimerRequest, TranslateAnim, VirtualLists, WindowOpenRequest, focus_next,
    text_edit, virtual_list,
};
use viso_widgets::caption_bar;

pub mod system_fonts;
mod text_content;
mod text_worker;
use text_content::{ParagraphSlot, TextShaper};

mod window;
pub use window::{WindowBuilder, WindowHandle, window};

pub use viso_ui::context::AppCx;

// The introspection snapshot model (architecture section 34/62), re-exported so
// a tool depending only on the `viso` facade — Studio transport (Slice B), the
// `viso inspect --json` CLI (Slice C) — names one `viso::InspectSnapshot` and
// its `snapshot_ui` builder rather than reaching into `viso-ui`. A cold-path
// tooling surface: deliberately kept out of the default prelude (section 6.2).
pub use viso_ui::{InspectSnapshot, snapshot_ui};

// The UI-tier window configuration is the shape a `window(...)` author fills in
// (title + logical size) — the facade translates it into the platform config at
// the open-drain point. Re-export it under the facade so app code names one
// `viso::WindowConfig`, never the internal `viso_platform::WindowConfig` (kept
// private to this module for the drain translation).
pub use viso_ui::{WindowChrome, WindowConfig};

// The `ui!` proc-macro lives in the compile-time-only `viso-ui-macros` crate; it
// emits `::viso_ui::…` builder tokens but does not itself depend on `viso-ui`. The
// facade re-exports it and already depends on `viso-ui`, so those emitted paths
// resolve at the call site — the same reverse-re-export shape `viso-gpu` uses for
// `viso_macros::GpuPod`.
pub use viso_ui_macros::ui;

// The SVG input lane (§13), re-exported as `viso::svg` so an app that has SVG
// bytes calls `viso::svg::parse_svg(..)` without naming the internal
// `viso-svg` crate. A cold-path input codec (parse once, cache the resulting
// primitives), deliberately kept out of the default prelude (§3.2): most apps
// never touch it, and `SvgScene`/`SvgError` are not part of the default mental
// model.
pub use viso_svg as svg;

/// The application entry-point contract implemented by every Viso app.
///
/// The single generic entry point is [`run`]. An `Application` owns top-level
/// state; it is not forced to contain a Router or a global Store — those are
/// opt-in.
pub trait Application: Sized + 'static {
    /// Construct the application. Windows and services are created via `cx`.
    fn new(cx: &mut AppCx) -> Self;

    /// Configure the launch window: its title, logical size, chrome, and whether
    /// the facade wraps the content in a self-drawn caption band. Read once on
    /// launch, before the window opens. The default — [`WindowConfig::default`] —
    /// is a self-drawn, captioned window titled "Viso", so an app that does not
    /// override this still gets a window-centered title with no authoring. Set
    /// `caption: false` to opt out of the wrap, or `chrome` to change who draws
    /// the window buttons.
    fn window_config(&self) -> WindowConfig {
        WindowConfig::default()
    }

    /// Author the app's retained scene: declare nodes, allocate reactive state,
    /// register handlers, and wire state→node bindings through `cx`. Runs once
    /// on launch and replaces the framework's default (empty) scene. `&mut self`
    /// so the app can stash the [`StateId`](viso_ui::StateId)s / node handles it
    /// reads from its handlers. The default builds nothing — an empty window.
    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let _ = cx;
    }

    /// The application menu bar, installed once on launch. Return a
    /// [`Menu::Main`](viso_platform::Menu::Main) tree; custom
    /// [`Menu::Item`](viso_platform::Menu::Item)s deliver their
    /// [`MenuCommandId`](viso_platform::MenuCommandId) to
    /// [`on_menu_command`](Self::on_menu_command) when picked, and
    /// [`Menu::System`](viso_platform::Menu::System) items (Quit/Close/…) are
    /// performed by the OS. The default returns `None`: the framework installs
    /// its standard app menu (with a working Quit) and the app adds nothing.
    fn menu(&self) -> Option<viso_platform::Menu> {
        None
    }

    /// Handle a custom menu command the user picked (the id from this app's
    /// [`menu`](Self::menu) tree). Mutate app/window state here; the frame that
    /// follows flushes it. The default ignores every command.
    fn on_menu_command(&mut self, command: viso_platform::MenuCommandId) {
        let _ = command;
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
    // The browser's event loop cannot block, so the scheduler outlives `run`
    // there; everywhere else the pump blocks until the last window closes. In
    // a page the WebGPU device opens asynchronously, before the first window
    // asks for it.
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    wasm_bindgen_futures::spawn_local(async move {
        viso_gpu::webgpu::prepare().await;
        Scheduler::new(platform_app, driver).run_detached();
    });
    #[cfg(all(target_arch = "wasm32", not(target_os = "unknown")))]
    Scheduler::new(platform_app, driver).run_detached();
    #[cfg(not(target_arch = "wasm32"))]
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
    use std::time::Duration;

    use viso_runtime::Instant;
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

        /// How many windows are open on the settled driver. One for a
        /// single-window app; two once a handler opened a second through the
        /// `window()` seam; back to one after a `WindowHandle::close` tore it
        /// down. The multi-window facade tests read this to prove open/close.
        pub fn window_count(&self) -> usize {
            self.driver.windows.len()
        }

        /// The retained store of the window at `index` (open order; `0` is the
        /// launch window), for asserting per-window state isolation.
        pub fn store_at(&self, index: usize) -> &super::NodeStore {
            &self.driver.windows[index].store
        }

        /// The declared root of the window at `index`, if any.
        pub fn root_at(&self, index: usize) -> Option<super::NodeId> {
            self.driver.windows[index].root
        }

        /// The [`WindowId`](viso_platform::WindowId) of the window at `index`.
        pub fn window_id_at(&self, index: usize) -> viso_platform::WindowId {
            self.driver.windows[index].window
        }

        /// The physical surface extent the window at `index` lays out against.
        /// A resize event names one window, so this is how the multi-window
        /// facade tests prove per-window geometry isolation: resizing one window
        /// changes only that window's `surface_size`, never a sibling's.
        pub fn surface_size_at(&self, index: usize) -> (u32, u32) {
            self.driver.windows[index].surface_size
        }

        /// The native chrome (traffic-light) bounding box last reported for the
        /// window at `index`, in logical points — `None` for a native-chrome
        /// window, which never fires the event. The self-drawn-chrome facade test
        /// reads it to prove a `WindowChromeGeom` event lands on the right window.
        pub fn chrome_buttons_at(&self, index: usize) -> Option<viso_platform::LogicalRect> {
            self.driver.windows[index].chrome_buttons
        }

        /// The text of the focused text control in the launch window, or
        /// `None` when focus is not in text entry.
        pub fn focused_text(&self) -> Option<&str> {
            let ws = &self.driver.windows[0];
            let node = ws.focused_text_node()?;
            ws.text_edits.get(node).map(|b| b.text.as_str())
        }

        /// The text-entry area last pushed to the platform for the launch
        /// window: `None` before any push, `Some(None)` once pushed clear.
        pub fn ime_area(&self) -> Option<Option<viso_platform::LogicalRect>> {
            self.driver.windows[0].ime_area
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

        /// A single read-only introspection snapshot of the first window's
        /// settled frame — the node tree, per-node paint spans, semantics tree,
        /// draw batches, and frame counters, aggregated by
        /// [`viso_ui::snapshot_ui`] (architecture section 34/62). This is the
        /// one model Studio transport and `viso inspect --json` will share.
        ///
        /// A cold-path readout off `&self` accessors; it mutates nothing and
        /// never runs on the steady frame path. The render-side batches/stats
        /// come from the window's live [`Renderer`](super::Renderer) when it has
        /// a GPU surface; a headless window has none, so those degrade to an
        /// empty batch list and zeroed counters — the JSON shape is stable
        /// either way (empty `batches`, zero `draw_calls`/`instances`), and the
        /// UI-side tree/paint/semantics are always present.
        pub fn inspect(&self) -> viso_ui::InspectSnapshot {
            let ws = &self.driver.windows[0];
            let (batches, stats) = match &ws.gpu {
                Some(gpu) => (gpu.renderer.inspect_batches(), gpu.renderer.frame_stats()),
                None => (
                    viso_render::InspectBatches::default(),
                    viso_render::FrameStats {
                        draw_calls: 0,
                        instances: 0,
                        ..Default::default()
                    },
                ),
            };
            match ws.root {
                Some(root) => viso_ui::snapshot_ui(&ws.store, root, batches, stats),
                // No declared root: an empty tree still yields a valid snapshot.
                None => viso_ui::InspectSnapshot {
                    tree: viso_ui::InspectTree::default(),
                    paint_ranges: viso_ui::PaintRanges::default(),
                    semantics: viso_ui::SemanticsTree::default(),
                    batches,
                    stats,
                },
            }
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
        drive_host::<A>(script, step, true)
    }

    /// [`drive_scripted`] on a host whose windows fill the screen (mobile, a
    /// browser tab): no caption, content inset by the safe area.
    pub fn drive_scripted_full_screen<A: Application>(
        script: Vec<viso_platform::RawEvent>,
        step: Duration,
    ) -> DrivenApp<A> {
        drive_host::<A>(script, step, false)
    }

    fn drive_host<A: Application>(
        script: Vec<viso_platform::RawEvent>,
        step: Duration,
        framed: bool,
    ) -> DrivenApp<A> {
        let mut app = Box::new(viso_platform::backend::headless::HeadlessApp::scripted(
            script,
        ));
        app.set_framed_windows(framed);
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
    /// Reusable scratch the flush phase drains each window's queued window-open
    /// requests into, so collecting a frame's `window()` requests off every
    /// window's store releases the `&mut windows` borrow before the driver opens
    /// the new windows (which needs `windows` mutably again to push). Empty on
    /// the steady path — a frame that opens no window allocates nothing.
    pending_opens: Vec<WindowOpenRequest>,
    /// Reusable scratch the flush phase drains each window's queued window-close
    /// requests into (raw ids from the UI tier), mirroring `pending_opens`.
    pending_closes: Vec<u32>,
    /// The raw sfnt faces the app registered at init through `AppCx::load_font`,
    /// drained from the cx once after `Application::new`. Session-scoped: loaded
    /// into every window's shaper as the window opens (ahead of the system
    /// fallbacks), so a face registered at init applies to the launch window and
    /// to any window opened later alike. Empty for an app that loads no font and
    /// relies on the system default.
    fonts: Vec<Box<[u8]>>,
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
    /// The window's device-pixel density (logical→physical scale), seeded from
    /// the platform at open and refreshed on every geometry change. Text shapes
    /// and rasterizes glyphs at this density, so a HiDPI surface gets crisp
    /// glyphs instead of 1x SDFs upscaled by the compositor. Independent of the
    /// GPU: a headless window still carries its reported scale.
    dpi: f32,
    /// The native chrome affordances' bounding box for a self-drawn-chrome window,
    /// in logical points (top-left origin, relative to the content area) — on
    /// macOS the traffic-light buttons. `None` until the platform reports one and
    /// for the whole life of a native-chrome window (which never fires the event).
    /// The app reads it to align its own caption around the native buttons; the
    /// facade also derives the caption's draggable strip from it and pushes that
    /// back to the platform so a press on the caption begins a native window drag.
    chrome_buttons: Option<LogicalRect>,
    /// The caption's draggable regions as last pushed to the platform, in logical
    /// points. The Layout phase collects the world boxes of the nodes a caption
    /// widget registered as draggable, converts them to logical rects, and pushes
    /// them through the `set_draggable_regions` back-channel only when they differ
    /// from this cache — so a steady frame whose caption did not move re-pushes
    /// nothing (the back-channel is a cold path, section 7.2). Empty until a
    /// caption first lays out; a window with no caption never fills it.
    draggable_cache: Vec<LogicalRect>,
    /// Who draws this window's chrome, known when the window opens (from its
    /// `WindowConfig`). Seeded into every (re)build's [`ChromeContext`] so a
    /// caption widget can decide whether self-drawn min/max/close buttons are
    /// allowed — the build-time half of the chrome data contract, paired with the
    /// later-frame [`chrome_buttons`](Self::chrome_buttons) width.
    chrome: WindowChrome,
    /// The root node of this window's self-drawn caption bar, captured when the
    /// tree was built (`None` for a native-chrome window that draws no caption).
    /// Held so a fullscreen transition can hide the caption in place — on macOS
    /// the OS draws its own auto-hiding title bar in fullscreen and removes the
    /// traffic lights — and restore it on exit, by toggling this node's
    /// visibility rather than rebuilding the tree.
    caption: Option<NodeId>,
    /// The root container a full-screen window pads to keep content clear of
    /// system UI (`None` on a framed desktop window), with the last reported
    /// safe area and on-screen keyboard height, in logical points.
    safe_area_root: Option<NodeId>,
    safe_area: Insets,
    keyboard_inset: f64,
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
    /// Semantic-state projections keyed by node. A control registers one at build
    /// time (`bind_semantic_state`); the flush wakes those whose dependencies
    /// changed and writes each affected node's `semantic_state` column, so the
    /// semantics derive pass reads live accessibility state (checked / value /
    /// range) from a node column without a cross-layer read (AGENTS section 3.5).
    projectors: SemanticProjector,
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
    /// The text-entry area last pushed to the platform, in logical points:
    /// `Some(Some(rect))` while a text control has focus, `Some(None)` once
    /// pushed clear, `None` before the first push. The Layout phase compares
    /// against it so a steady frame pushes nothing and the soft keyboard is
    /// shown or hidden only when focus moves into or out of text entry.
    ime_area: Option<Option<LogicalRect>>,
    /// Reusable ancestry buffer the pointer router fills each event, owned here
    /// so routing a pointer allocates nothing on the steady path.
    route_chain: Vec<NodeId>,
    /// Reusable scratch for [`WindowState::settle_text_edits`]: the edited
    /// nodes drained from the registry, and the text of the one being reported.
    edited: Vec<NodeId>,
    edited_text: String,
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
    /// until launch, when a GPU backend exists to allocate the atlas; the app's
    /// own faces (from `AppCx::load_font`) are loaded into it then, ahead of the
    /// system fallbacks the first shape resolves. No font is bundled.
    text: Option<TextShaper>,
    /// Reusable buffer the text seam drains pending requests into, so re-shaping
    /// text allocates only the shaped payloads, not the request list.
    text_scratch: Vec<(NodeId, Box<TextRequest>)>,
    /// Reusable buffer the text commit drains the slots whose layout the
    /// worker finished into, so committing allocates nothing on the steady
    /// path.
    text_updated: Vec<ParagraphSlot>,
    /// Retained source request of every shaped run, keyed by node — the one
    /// place the facade keeps a run's declaration after the store's request
    /// column is drained (`take_text_requests` consumes it, and the store treats
    /// the request as write-once: an edit re-declares it, see
    /// `text_edit::reconcile`). Two consumers need the source after the first
    /// shape: the two-phase reflow reshapes a `soft_wrap` leaf at the width
    /// layout assigned it, and a memory-warning trim reshapes every mounted run
    /// into the freshly emptied glyph atlas (a shaped payload carries atlas UVs,
    /// so it cannot outlive the pages it points into). Entries are pruned when a
    /// node is freed (checked live at drain time) and overwritten when a run is
    /// re-declared.
    text_sources: std::collections::HashMap<NodeId, Box<TextRequest>>,
    /// Reusable buffer the Layout phase drains the store's queued text reflows
    /// into (`take_text_reflows`), so servicing a reflow allocates only the
    /// reshaped payloads. Empty and allocation-free on the steady path with no
    /// wrap-eligible text whose assigned width changed.
    reflow_scratch: Vec<(NodeId, f32)>,
    /// Reusable buffer the flush drains this window's queued window-open requests
    /// into before the driver appends them to its session-level pending list, so
    /// servicing a `window()` call reuses this window's own scratch rather than
    /// allocating one per frame. Empty on the steady path.
    scratch_opens: Vec<WindowOpenRequest>,
    /// Reusable buffer for this window's queued window-close ids, mirroring
    /// [`scratch_opens`](Self::scratch_opens).
    scratch_closes: Vec<u32>,
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
    /// The window's surface; `None` while the platform has taken the native
    /// window away (a backgrounded mobile app).
    surface: Option<SurfaceId>,
    /// Current surface size in physical pixels `(width, height)`.
    size: (u32, u32),
}

impl<A: Application> AppDriver<A> {
    fn new() -> Self {
        Self {
            app: None,
            cx: AppCx::__new(),
            windows: Vec::new(),
            pending_opens: Vec::new(),
            pending_closes: Vec::new(),
            fonts: Vec::new(),
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
            dpi: 1.0,
            chrome_buttons: None,
            draggable_cache: Vec::new(),
            chrome: WindowChrome::Native,
            caption: None,
            safe_area_root: None,
            safe_area: Insets::default(),
            keyboard_inset: 0.0,
            store: NodeStore::new(),
            states: StateStore::new(),
            bindings: BindingTable::new(),
            computeds: ComputedStore::new(),
            effects: EffectStore::new(),
            projectors: SemanticProjector::new(),
            virtual_lists: VirtualLists::new(),
            text_edits: TextEdits::new(),
            animations: AnimationRegistry::new(),
            anim_requests: Vec::new(),
            timers: TimerRegistry::new(),
            timer_requests: Vec::new(),
            changed: Vec::new(),
            root: None,
            ime_area: None,
            route_chain: Vec::new(),
            edited: Vec::new(),
            edited_text: String::new(),
            primitives: Vec::new(),
            scratch: Vec::new(),
            redo_roots: Vec::new(),
            recompute: FrameRecompute::default(),
            awaiting_first_frame: true,
            text: None,
            text_scratch: Vec::new(),
            text_sources: std::collections::HashMap::new(),
            text_updated: Vec::new(),
            reflow_scratch: Vec::new(),
            scratch_opens: Vec::new(),
            scratch_closes: Vec::new(),
        }
    }

    /// Open a fresh window's state: resolve its launch size, bring the GPU up
    /// against its native handle (staying headless without one), run `build`
    /// against its brand-new store to author the retained tree, and seed the
    /// first frame's dirty. This is the single window-bring-up path — the launch
    /// window and every window opened mid-session through the `window()` seam
    /// both flow through it, differing only in *which* build closure authors the
    /// tree (the application's `build` for the launch window, the deferred
    /// `WindowOpenRequest::build` for a later one). Requires a live scheduling
    /// context (`RuntimeCx`), so it is only reachable from `on_launch`/`run_phase`
    /// — the two phases the facade holds one.
    fn open(
        cx: &mut RuntimeCx<'_>,
        window: WindowId,
        fonts: &[Box<[u8]>],
        chrome: WindowChrome,
        initial_chrome_geom: Option<LogicalRect>,
        build: impl FnOnce(&mut BuildCx) -> Option<NodeId>,
    ) -> Self {
        let mut ws = WindowState::new(window);
        // Record who draws this window's chrome. Known at open time from the
        // window config; the build-time half of the chrome data contract a
        // caption widget reads through `BuildCx::chrome` (section 24 — driven by
        // data, not `target_os`).
        ws.chrome = chrome;
        // Seed the native traffic-light box synchronously, before the single
        // build reads it: the facade queried `RuntimeCx::window_chrome_geom` the
        // instant the window existed, so a self-drawn caption decides at build
        // time whether to draw its own window buttons or yield to the OS overlay
        // — it never has to wait for the later `WindowChromeGeom` event, which
        // still fires to refine this box on resize/scale.
        ws.chrome_buttons = initial_chrome_geom;

        // Record the launch surface size up front, independent of the GPU: the
        // tree lays out against this every frame, so a headless window (no GPU)
        // still measures and places the whole tree, it just paints to nothing.
        let (w, h) = cx.inner_size(window).unwrap_or((1, 1));
        ws.surface_size = (w.max(1), h.max(1));

        // Record the window's device-pixel density up front so the first shape
        // rasterizes glyphs at the surface's real scale. A window with no scale
        // reported (or a value the platform cannot supply) falls back to 1x.
        ws.dpi = cx.scale_factor(window).unwrap_or(1.0) as f32;

        // Bring up the GPU for this window when it exposes a real windowing
        // handle: create the device, attach a surface to that handle, and build
        // the renderer for its format. A headless window reports a
        // `RawWindowHandle::Headless` (or no handle at all) — there is no
        // surface to attach, and on a real-GPU target such as Metal
        // `create_surface` would reject the non-native handle — so we stay
        // `gpu = None` for the session and draw nothing. The retained tree below
        // is built and driven regardless; this is section 66's headless backend
        // as a first-class path, not a degraded one.
        let native_handle = match cx.raw_handle(window) {
            Some(RawWindowHandle::Headless) | None => None,
            Some(handle) => Some(handle),
        };
        // A browser without WebGPU keeps the window without a GPU surface.
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        let native_handle = native_handle.filter(|_| viso_gpu::webgpu::is_prepared());
        if let Some(raw) = native_handle {
            let mut backend = viso_gpu::create_device();
            let surface = backend.create_surface(raw, w.max(1), h.max(1));
            // Both the format and the color space come from the surface, so the
            // renderer's intermediate targets are planned for the domain this
            // window actually composites in (§19).
            let renderer = Renderer::for_surface(&mut backend, surface);

            ws.gpu = Some(GpuState {
                backend,
                renderer,
                surface: Some(surface),
                size: (w.max(1), h.max(1)),
            });
            // Stand up the font stack now that a backend exists to allocate the
            // atlas, then register the app's own faces (from `AppCx::load_font`)
            // into it in call order — the first becomes the primary, ahead of any
            // system fallback resolved later. Each face's bytes must be a raw sfnt
            // (`.ttf`/`.otf`): the core knows nothing about compressed web fonts.
            // A developer with WOFF2 bytes decompresses them to sfnt first with the
            // standalone `viso-woff2` library, then passes the result here. A face
            // whose bytes are not a valid sfnt is skipped, leaving the system
            // default in place. An app that loaded no font gets an empty chain the
            // first shape seeds from the system.
            let mut shaper = TextShaper::new();
            for face in fonts {
                // Each window gets its own shaper, so hand it a fresh copy of the
                // sfnt bytes rather than moving the shared registration out. A face
                // whose bytes are not a valid sfnt is skipped, keeping the system
                // default in place.
                shaper.load_font(face.clone(), 0);
            }
            ws.text = Some(shaper);
        }

        // Build the retained UI tree once, now that we have a surface size. The
        // window owns `store`, `states`, and `bindings` as sibling fields, so
        // all three can be borrowed together into a reactive build context —
        // this is why scene authoring works here where new-time allocation could
        // not (the session-long `AppCx` marker cannot retain a live store
        // borrow). Layout runs incrementally per frame in `run_phase`.
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
        {
            let mut build_cx = BuildCx::with_reactive(
                &mut ws.store,
                &mut ws.states,
                &mut ws.bindings,
                &mut ws.virtual_lists,
                &mut ws.text_edits,
                &mut ws.projectors,
            )
            .with_chrome(ChromeContext {
                chrome: ws.chrome,
                buttons_width: ws.chrome_buttons.map(|r| r.width as f32),
                // Derive the caption height the OS traffic lights want from their
                // measured box: `ceil(top_inset * 2 + box_height)` centers the
                // fixed-size buttons vertically in the bar. The buttons don't zoom,
                // so the bar must wrap *them* — not a self-chosen constant that may
                // fall short of (or overshoot) the traffic-light region. `None`
                // when no native box was reported, letting the caption keep its own
                // fixed height for the self-drawn (Windows/Linux/headless) case.
                buttons_height: ws
                    .chrome_buttons
                    .map(|r| (r.y * 2.0 + r.height).ceil() as f32),
            });
            ws.root = build(&mut build_cx);
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

        ws
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
        // Declaring text is the frame that may have freed older runs, so drop
        // their sources here; a steady frame declares nothing and never scans.
        let arena = self.store.arena();
        self.text_sources.retain(|id, _| arena.is_live(*id));
        text.retain_paragraphs(|slot| {
            arena.live_id(slot.index()).map(ParagraphSlot::from) == Some(slot)
        });
        // Rasterize glyphs at the window's real device-pixel density (seeded at
        // open, refreshed on every geometry change), so a HiDPI surface gets
        // crisp SDFs instead of 1x coverage upscaled by the compositor.
        let dpi = self.dpi;
        for (id, request) in self.text_scratch.drain(..) {
            // A new run shapes unconstrained: its `natural` extent is the
            // unwrapped single-line width. A run already laid out keeps the
            // width it last wrapped to, so an edit reflows only the edited
            // lines and the paragraph never flashes unwrapped. If layout then
            // assigns a wrap-eligible leaf a different box, it enqueues a
            // reflow that the Layout phase drains and reshapes at that width.
            let slot = ParagraphSlot::from(id);
            let width = text.wrap_width(slot);
            let content = text.shape(&mut gpu.backend, slot, &request, dpi, width);
            // Every run keeps its source so a reflow or a memory trim can
            // reshape it (the store drops the request on drain). Re-declaring a
            // run (edit/rebuild) overwrites its entry; the reflow drain and the
            // trim prune freed nodes.
            self.text_sources.insert(id, request);
            self.store.set_content_payload(id, content);
        }
    }

    /// Commit the text worker's finished layouts within the frame's commit
    /// budget and redraw each run they replaced, at the width it last wrapped
    /// to. Until a run's layout is committed it keeps drawing its last good
    /// one. A no-op when the worker owes nothing (the steady case).
    fn commit_text_work(&mut self) {
        let (Some(gpu), Some(text)) = (self.gpu.as_mut(), self.text.as_mut()) else {
            return;
        };
        if !text.has_pending_work() {
            return;
        }
        text.pump(
            &mut gpu.backend,
            text_content::TEXT_COMMIT_BUDGET,
            &mut self.text_updated,
        );
        let dpi = self.dpi;
        for slot in self.text_updated.drain(..) {
            let Some(id) = self
                .store
                .arena()
                .live_id(slot.index())
                .filter(|&id| ParagraphSlot::from(id) == slot)
            else {
                continue;
            };
            let Some(request) = self.text_sources.get(&id) else {
                continue;
            };
            let width = text.wrap_width(slot);
            let content = text.shape(&mut gpu.backend, slot, request, dpi, width);
            // The request is unchanged, so its accessible name is too.
            self.store.set_reflowed_content(id, content);
        }
    }

    /// Hand the text worker every layout this frame asked for, in one batch.
    fn dispatch_text_work(&mut self) {
        if let Some(text) = self.text.as_mut() {
            text.dispatch();
        }
    }

    /// Whether the text worker still owes results a later frame commits.
    fn text_work_pending(&self) -> bool {
        self.text.as_ref().is_some_and(TextShaper::has_pending_work)
    }

    /// Fold every queued edit into its buffer and tell each control whose text
    /// changed what it now reads, so `on_change` observes typing, deletion,
    /// paste, and cut through one path. Runs right after an input sample is
    /// routed, so state a change handler writes flushes in the same frame.
    fn settle_text_edits(&mut self) {
        let geometry = self
            .text
            .as_ref()
            .map(|t| t as &dyn text_edit::EditGeometry);
        text_edit::reconcile(&mut self.store, &mut self.text_edits, geometry);
        self.text_edits.take_changed(&mut self.edited);
        for i in 0..self.edited.len() {
            let node = self.edited[i];
            let Some(buffer) = self.text_edits.get(node) else {
                continue;
            };
            self.edited_text.clear();
            self.edited_text.push_str(&buffer.text);
            KeyRouter::route_text_change(
                &mut self.store,
                &mut self.states,
                &self.bindings,
                &mut self.text_edits,
                node,
                &self.edited_text,
            );
        }
    }

    /// Give back every GPU cache the next frame can rebuild: idle pooled
    /// targets, and the whole glyph atlas. Mounted text is reshaped at once into
    /// fresh planes, so the atlas comes back holding only the live working set
    /// and no retained payload is left pointing at a retired page.
    fn trim_memory(&mut self) {
        let (Some(gpu), Some(text)) = (self.gpu.as_mut(), self.text.as_mut()) else {
            return;
        };
        gpu.renderer.trim_caches(&mut gpu.backend);
        text.trim(|texture| gpu.renderer.release_texture(&mut gpu.backend, texture));
        let arena = self.store.arena();
        self.text_sources.retain(|id, _| arena.is_live(*id));
        text.retain_paragraphs(|slot| {
            arena.live_id(slot.index()).map(ParagraphSlot::from) == Some(slot)
        });
        for (&id, request) in &self.text_sources {
            self.store.set_text_request(id, (**request).clone());
        }
        self.shape_pending_text();
    }

    /// The surface size in logical points: the physical surface divided by the
    /// device scale factor. This is the coordinate space layout and paint run in
    /// (fixed sizes and spacers are authored in logical points), and the space
    /// the projection viewport describes. `dpi.max(1.0)` guards a degenerate 0.
    /// On a 1x display (headless seeds dpi = 1) this equals the physical size.
    fn logical_surface(&self) -> (f32, f32) {
        let (w, h) = self.surface_size;
        let scale = self.dpi.max(1.0);
        (w as f32 / scale, h as f32 / scale)
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
        // Lay out in logical points, not physical pixels: the surface is stored
        // physical (the GPU needs it), but every layout dimension — a fixed
        // caption height, a spacer width — is authored in logical points, so the
        // root box must be the physical surface divided by the scale factor.
        // Feeding physical pixels here halves absolute sizes on a 2x display.
        // Glyph rasterization stays physical (text picks its atlas ppem from dpi
        // independently); only the layout coordinate space is logical.
        let (lw, lh) = self.logical_surface();
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: lw,
            h: lh,
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
        // Re-select interaction-state boxes when a STYLE class is pending. A
        // press/hover flip marks its node STYLE (and PAINT) through the flush;
        // this folds the winning variant (pressed > hover > resting) into the
        // warm `style` the paint walk reads, before the repaint below. Theme-free
        // (it reads only the reactive cells), so it runs here in the live loop
        // where the theme-token `resolve_styles` pass does not yet. Gated on
        // STYLE so a pure paint/transform frame skips it; a steady frame with no
        // interaction change carries no STYLE dirt and pays nothing.
        if self.store.any_dirty_class(DirtyClass::STYLE) {
            self.store.resolve_interaction_styles(&self.states);
        }
        let painted = self.store.repaint_dirty(root, &mut self.primitives);
        self.recompute = FrameRecompute {
            measured,
            laid_out,
            painted,
        };
    }

    /// Recompute the caption's draggable regions from the just-laid-out tree and,
    /// only when they differ from the last push, hand them to the platform through
    /// `set_draggable_regions` — the cold back-channel that turns a primary press on
    /// the caption into a native window drag (section 7.2). A caption widget
    /// registers its blank band via `BuildCx::register_draggable`; the store keeps
    /// those node ids in a small retained list, and here we resolve each to its
    /// world box — already in logical points (layout runs logical), the same space
    /// the platform's `mouseDown` hit-tests — and diff against the cache.
    ///
    /// Returns `true` when it pushed a changed set. Skips the platform call — and
    /// the borrow of `cx` at the call site — on every steady frame whose caption
    /// held still: the registered set is tiny (one band per caption) so building the
    /// candidate list and comparing it is a handful of `f32` ops, not a full-tree
    /// sweep, and a window with no caption registers nothing and returns instantly.
    /// The focused node's nearest text-edit control: the focused node itself or
    /// the closest ancestor that registered a buffer.
    fn focused_text_node(&self) -> Option<NodeId> {
        let mut node = self.store.focused();
        while let Some(id) = node {
            if self.text_edits.get(id).is_some() {
                return Some(id);
            }
            node = self.store.parent(id);
        }
        None
    }

    /// Recompute the text-entry area from the laid-out tree and report whether
    /// it differs from what was last pushed. The area is the caret's line box
    /// in the focused control, one logical pixel wide so a candidate window
    /// anchors beside the insertion point; a control whose run is not shaped
    /// yet reports its whole box.
    fn ime_area_changed(&mut self) -> bool {
        let area = self.focused_text_node().map(|id| {
            let r = self.store.world(id);
            let caret = match (
                self.text.as_ref(),
                self.text_sources.get(&id),
                self.text_edits.get(id),
            ) {
                (Some(text), Some(request), Some(buffer)) if request.text == buffer.text => {
                    // A composing control anchors the candidate window at the
                    // composition, not wherever the caret sits inside it.
                    let at = if buffer.has_composition() {
                        buffer.composition().anchor()
                    } else {
                        buffer.sel.focus
                    };
                    text.caret(ParagraphSlot::from(id), request, at)
                }
                _ => None,
            };
            caret_area(r, caret)
        });
        if self.ime_area == Some(area) {
            return false;
        }
        self.ime_area = Some(area);
        true
    }

    fn draggable_regions_changed(&mut self) -> bool {
        let regions = self.store.draggable_regions();
        // Fast exit shared by every window without a self-drawn caption: nothing
        // registered and nothing cached means no work and no push.
        if regions.is_empty() && self.draggable_cache.is_empty() {
            return false;
        }
        let mut changed = regions.len() != self.draggable_cache.len();
        // Reuse the cache's backing storage: build the new set in place, comparing
        // element-by-element against the old contents as we overwrite them, so a
        // held-still caption allocates nothing and reports no change.
        if !changed {
            for (i, &id) in regions.iter().enumerate() {
                let b = self.store.world(id);
                let next = LogicalRect::new(b.x as f64, b.y as f64, b.w as f64, b.h as f64);
                if self.draggable_cache[i] != next {
                    changed = true;
                    break;
                }
            }
        }
        if !changed {
            return false;
        }
        self.draggable_cache.clear();
        for &id in regions {
            let b = self.store.world(id);
            self.draggable_cache.push(LogicalRect::new(
                b.x as f64, b.y as f64, b.w as f64, b.h as f64,
            ));
        }
        true
    }

    /// Phase B of width-aware text reflow (DL1): drain the reflow requests the
    /// just-finished layout recorded, reshape each wrap-eligible run at the
    /// width layout assigned it, write the wrapped run back, and relayout — a
    /// run whose height grew now re-places its Flex siblings. Loops until the
    /// queue drains empty (layout assigned no new mismatched widths) or a small
    /// iteration cap, whichever comes first.
    ///
    /// Convergence: a reshape only ever changes a run's height and can only
    /// shrink or hold its natural width (wrapping never widens a run), and the
    /// recorder excludes `Fit`-width leaves — the one case that could feed width
    /// back into width. So each pass either settles the run at its assigned
    /// width (mismatch gone → not re-enqueued) or the run's width monotonically
    /// shrinks toward the box; there is no width→height→width oscillation. The
    /// cap is a safety net, not the mechanism — a normal Column/Row paragraph
    /// settles in one reshape pass (the second pass observes no mismatch and the
    /// loop exits). Returns the number of reshape passes run, for the caller's
    /// first-frame double-shape counter (§61); `0` on the steady frame that
    /// enqueued nothing.
    ///
    /// The first appearance of a wrapped `Fill` paragraph is shaped twice — once
    /// unconstrained in Phase A, once at its width here — inherent to the premise
    /// that text height depends on a width that is a layout *output*. This is a
    /// one-time cost on the frame the paragraph appears or its width changes, not
    /// a steady-state per-frame cost: once shaped at a width, an unchanged width
    /// enqueues no reflow (the recorder's quantize+epsilon guard), so a static
    /// paragraph pays nothing on subsequent frames.
    fn reflow_wrapped_text(&mut self) -> u32 {
        // Bounded so a pathological feedback (should be impossible given the
        // eligibility rule, but a cap keeps a bug from spinning the frame) can
        // never loop unboundedly. Three passes is generous — real content
        // settles in one.
        const MAX_REFLOW_PASSES: u32 = 3;
        let mut passes = 0;
        while passes < MAX_REFLOW_PASSES {
            self.store.take_text_reflows(&mut self.reflow_scratch);
            if self.reflow_scratch.is_empty() {
                break;
            }
            // Drop retained sources for wrap runs whose node is gone (removed
            // from the tree, or its index reused with a fresh generation). Only
            // on a frame that actually reflows — never a steady frame — and the
            // map holds only wrap paragraphs, so this stays small. Keeps a long
            // session that churns wrapped paragraphs from leaking their sources.
            let (Some(gpu), Some(text)) = (self.gpu.as_mut(), self.text.as_mut()) else {
                // No shaper/GPU (headless-without-surface, pre-launch): nothing
                // can reshape, so drop the drained requests and stop rather than
                // spin re-enqueuing them.
                self.reflow_scratch.clear();
                break;
            };
            if passes == 0 {
                let arena = self.store.arena();
                self.text_sources.retain(|id, _| arena.is_live(*id));
                text.retain_paragraphs(|slot| {
                    arena.live_id(slot.index()).map(ParagraphSlot::from) == Some(slot)
                });
            }
            let dpi = self.dpi;
            for (id, width) in self.reflow_scratch.drain(..) {
                // A missing source means the node was freed, so skip it.
                let Some(request) = self.text_sources.get(&id) else {
                    continue;
                };
                let content = text.shape(
                    &mut gpu.backend,
                    ParagraphSlot::from(id),
                    request,
                    dpi,
                    Some(width),
                );
                // Width-only reshape: mark MEASURE|LAYOUT|PAINT but not
                // SEMANTICS — the accessible name is unchanged by wrapping.
                self.store.set_reflowed_content(id, content);
            }
            // Re-place the tree so a run whose height grew pushes its siblings;
            // the next pass's layout may assign a still-different width (nested
            // wrap-in-wrap), which re-enqueues and reshapes again until settled.
            self.relayout_and_paint();
            passes += 1;
        }
        passes
    }
}

/// Translate a UI-tier [`WindowConfig`] into the platform config the facade
/// hands to `create_window`. The one place the `viso-ui -> viso-platform` name
/// gap is bridged (section 3.5): the app and every handler name only the UI
/// config; the facade resolves it at the open-drain point. Shared by the launch
/// path and the deferred `window(...)` path so the translation lives once.
fn to_platform_config(cfg: &WindowConfig) -> viso_platform::WindowConfig {
    viso_platform::WindowConfig {
        title: cfg.title.clone(),
        logical_size: cfg.size,
        chrome: match cfg.chrome {
            WindowChrome::Native => viso_platform::WindowChrome::Native,
            WindowChrome::SelfDrawn => viso_platform::WindowChrome::SelfDrawn,
        },
    }
}

/// Wrap a window's authored content in the default self-drawn caption band when
/// `caption` asks for it, which is the default:
/// every window gets a window-centered title with no authoring. Called inside a
/// window's build closure, after the app has declared its own tree.
///
/// - `caption == false`: opt out. `build_content` runs at the tree root, so the
///   app's returned root becomes the window root verbatim — no extra nodes.
/// - `caption == true`: stack a fixed-height [`caption_bar`] above a fill body,
///   under a single fill Column that becomes the window root. `build_content`
///   runs inside the body, so an app root of any size (`Fit`/`Fill`) fills the
///   remaining height below the caption.
///
/// Runs once per window build (the tree is never wholly rebuilt), so the wrap is
/// a cold, build-time cost. Returns the window root.
fn wrap_root_with_caption(
    cx: &mut BuildCx<'_>,
    title: &str,
    caption: bool,
    caption_out: &Cell<Option<NodeId>>,
    build_content: impl FnOnce(&mut BuildCx<'_>) -> Option<NodeId>,
) -> Option<NodeId> {
    if !caption {
        return build_content(cx);
    }

    // A fill Column: caption band on top (its own `Fit`/fixed height), app body
    // below filling the rest. The Column is the sole parentless node, so it is
    // recorded as the window root.
    let root = cx.flex(
        FlexStyle {
            axis: Axis::Column,
            // Stretch children across the cross (horizontal) axis so the
            // fill-width caption band and body span the whole window width — a
            // Start cross-align would let them collapse to content width.
            align: viso_ui::Align::Stretch,
            size: Size::fill(),
            ..FlexStyle::default()
        },
        |cx| {
            // Capture the caption root so a later fullscreen transition can hide
            // it in place; the plain `Component::build` would discard the handle.
            let cap = caption_bar(title.to_string()).build_root(cx);
            caption_out.set(Some(cap.id()));
            // Body: a fill container so the app's tree occupies the height left
            // under the caption regardless of the app root's own size request.
            cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    align: viso_ui::Align::Stretch,
                    size: Size::fill(),
                    ..FlexStyle::default()
                },
                |cx| {
                    build_content(cx);
                },
            );
        },
    );
    Some(root.id())
}

/// Wrap a full-screen window's content (mobile, a browser tab) in a fill
/// Column the facade pads by the safe area and keyboard, so content stays
/// clear of the status bar, notch, home indicator and on-screen keyboard. The
/// Column is the window root; such a window draws no caption.
fn wrap_root_in_safe_area(
    cx: &mut BuildCx<'_>,
    build_content: impl FnOnce(&mut BuildCx<'_>) -> Option<NodeId>,
) -> Option<NodeId> {
    let root = cx.flex(
        FlexStyle {
            axis: Axis::Column,
            align: viso_ui::Align::Stretch,
            size: Size::fill(),
            ..FlexStyle::default()
        },
        |cx| {
            build_content(cx);
        },
    );
    Some(root.id())
}

impl WindowState {
    /// Pad the safe-area root by the reported insets; the bottom edge clears
    /// whichever is taller, the home indicator or the on-screen keyboard.
    fn apply_safe_area(&mut self) {
        let Some(root) = self.safe_area_root else {
            return;
        };
        let a = self.safe_area;
        self.store.set_padding(
            root,
            viso_ui::Inset {
                left: a.left as f32,
                top: a.top as f32,
                right: a.right as f32,
                bottom: a.bottom.max(self.keyboard_inset) as f32,
            },
        );
    }
}

impl<A: Application> viso_runtime::FrameDriver for AppDriver<A> {
    fn on_launch(&mut self, cx: &mut RuntimeCx<'_>) {
        // Construct the user application now that the pump is live, then take the
        // faces it registered through `AppCx::load_font` so every window opened
        // this session loads them ahead of the system fallbacks.
        self.app = Some(A::new(&mut self.cx));
        self.fonts = self.cx.__take_fonts();
        // Install the app's menu bar, if it declares one, before opening any
        // window. A `None` leaves the framework's standard app menu (with its
        // working Quit) in place from platform bring-up.
        if let Some(menu) = self.app.as_ref().and_then(Application::menu) {
            cx.set_menu(&menu);
        }
        // Open the initial window with the app's declared configuration
        // (`window_config`, defaulting to a self-drawn, captioned window). Later
        // phases let the app request further windows via `AppCx`.
        let cfg = self
            .app
            .as_ref()
            .map(Application::window_config)
            .unwrap_or_default();
        let Ok(id) = cx.create_window(to_platform_config(&cfg)) else {
            return;
        };
        // Open the launch window through the shared bring-up path, authoring its
        // tree with the application's own `build`, wrapped by default in a
        // self-drawn caption band (window-centered title, no authoring). A window
        // opened later through the `window()` seam runs the identical path with
        // the request's deferred build closure — the launch window is not a
        // special case.
        let app = self.app.as_mut();
        let ui_chrome = cfg.chrome;
        let title = cfg.title.clone();
        let framed = cx.framed_windows();
        let caption = cfg.caption && framed;
        // Read the native traffic-light box the instant the window exists, so the
        // single build below sees it and a self-drawn caption yields to the OS
        // overlay instead of drawing its own buttons (section 24 data contract).
        let chrome_geom = cx.window_chrome_geom(id);
        // The wrap captures the caption root into this cell during the synchronous
        // build inside `open`; read it back afterward to hold for fullscreen hide.
        let caption_cell = Cell::new(None);
        let mut ws = WindowState::open(cx, id, &self.fonts, ui_chrome, chrome_geom, |build| {
            let content = |build: &mut BuildCx<'_>| {
                app.and_then(|app| {
                    app.build(build);
                    build.root()
                })
            };
            if framed {
                wrap_root_with_caption(build, &title, caption, &caption_cell, content)
            } else {
                wrap_root_in_safe_area(build, content)
            }
        });
        ws.caption = caption_cell.get();
        if !framed {
            ws.safe_area_root = ws.root;
        }
        self.windows.push(ws);
    }

    fn on_geometry(&mut self, window: WindowId, scale: f64, width: u32, height: u32) {
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
        // Track the window's current density so text (re)declared after a
        // scale change rasterizes at the new density. A non-positive scale
        // (unreported) keeps the prior value rather than collapsing to zero.
        if scale > 0.0 {
            ws.dpi = scale as f32;
        }
        if let Some(gpu) = &mut ws.gpu {
            if let Some(surface) = gpu.surface {
                gpu.backend.resize_surface(surface, w, h);
            }
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
        // The sample arrives in physical pixels (the scheduler resolved the
        // window scale), but layout and hit testing run in logical points, so
        // pointer/scroll positions are divided by the scale factor before they
        // reach the router — the same space as node bounds. Route it to the
        // target window's tree along the hit node's ancestry; any state a handler
        // writes lands in that window's pending set and is turned into targeted
        // dirtying by the next frame's flush (the scheduler already flagged the
        // frame input-dirty). A sample naming an unknown window (already closed)
        // is dropped.
        let target = sample.window();
        let Some(ws) = self.window_mut(target) else {
            return;
        };
        let Some(root) = ws.root else {
            return;
        };
        let scale = ws.dpi.max(1.0);
        // Samples that can edit the focused text control settle their edits
        // once routed; a pointer sample settles only when a handler placed a
        // caret, and scroll samples never record one.
        let edits_text = matches!(
            sample,
            viso_runtime::InputSample::Key(_)
                | viso_runtime::InputSample::Text(_)
                | viso_runtime::InputSample::Paste(_)
                | viso_runtime::InputSample::ImePreedit(_)
        );
        match sample {
            viso_runtime::InputSample::Pointer(p) => {
                let ev = PointerEvent {
                    x: p.x / scale,
                    y: p.y / scale,
                    phase: match p.phase {
                        viso_runtime::PointerPhase::Down => PointerPhase::Down,
                        viso_runtime::PointerPhase::Move => PointerPhase::Move,
                        viso_runtime::PointerPhase::Up => PointerPhase::Up,
                        // A cancelled contact must not activate anything: it
                        // reaches the tree as a leave, which clears press and
                        // hover without a click.
                        viso_runtime::PointerPhase::Leave | viso_runtime::PointerPhase::Cancel => {
                            PointerPhase::Leave
                        }
                    },
                    buttons: PointerButtons(p.buttons),
                    modifiers: Modifiers {
                        shift: p.modifiers.shift,
                        control: p.modifiers.control,
                        alt: p.modifiers.alt,
                        logo: p.modifiers.logo,
                    },
                };
                // Every pointer routes on its own; a finger or pen is a direct
                // contact (landing capture + pan arbitration). A cancelled
                // contact arrives as a leave, which ends it and its capture.
                let contact = viso_ui::PointerContact {
                    id: viso_ui::PointerId(p.pointer.0),
                    direct: p.kind != viso_runtime::PointerKind::Mouse,
                };
                PointerRouter::route_contact(
                    &mut ws.store,
                    &mut ws.states,
                    &ws.bindings,
                    root,
                    contact,
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
                        key: k.key,
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
            viso_runtime::InputSample::Paste(t) => {
                // Pasted text enters the focused control exactly like a committed
                // IME segment: it replaces the selection. A single-line buffer
                // folds any line breaks in it.
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
            viso_runtime::InputSample::Copy(c) => {
                // The platform asks for the selection while its copy/cut handler
                // is still on the stack. Apply any edits queued this event batch
                // first so the reply reflects what the user sees, then answer
                // from the focused text control's buffer. An empty selection
                // leaves the reply unset and the clipboard untouched.
                let geometry = ws.text.as_ref().map(|t| t as &dyn text_edit::EditGeometry);
                text_edit::reconcile(&mut ws.store, &mut ws.text_edits, geometry);
                let Some(node) = ws.focused_text_node() else {
                    return;
                };
                let Some(buffer) = ws.text_edits.get_mut(node) else {
                    return;
                };
                let (start, end) = buffer.sel.logical_range();
                if start == end {
                    return;
                }
                c.reply.set(buffer.text[start.0..end.0].to_owned());
                if c.cut {
                    buffer.queue(text_edit::EditIntent::Delete);
                    ws.settle_text_edits();
                }
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
                // grows, so it maps straight onto the offset delta. Both the hit
                // position and the delta are divided by the scale factor into
                // logical points — the offset lives in the same logical space as
                // layout. The router clamps per axis and marks only
                // TRANSFORM/HIT_TEST/PAINT — a scroll never relayouts — so no
                // state flush is involved.
                let ev = ScrollEvent {
                    x: s.x / scale,
                    y: s.y / scale,
                    delta_x: s.delta_x / scale,
                    delta_y: s.delta_y / scale,
                    modifiers: lower_modifiers(s.modifiers),
                };
                ScrollRouter::route(&mut ws.store, root, ev);
            }
        }
        if edits_text || ws.store.has_edit_requests() {
            ws.settle_text_edits();
        }
    }

    fn on_lifecycle(&mut self, cx: &mut RuntimeCx<'_>, event: viso_runtime::Lifecycle) {
        match event {
            // Frames were skipped while suspended and a surface may have been
            // recreated underneath, so every window paints once on return.
            viso_runtime::Lifecycle::Resumed => {
                for ws in &self.windows {
                    cx.request_redraw(ws.window);
                }
            }
            // Give back the GPU memory the next frame can rebuild on demand.
            viso_runtime::Lifecycle::LowMemory => {
                for ws in &mut self.windows {
                    ws.trim_memory();
                    cx.request_redraw(ws.window);
                }
            }
            viso_runtime::Lifecycle::Suspended => {}
        }
    }

    fn on_surface(&mut self, cx: &mut RuntimeCx<'_>, window: WindowId, available: bool) {
        let raw = cx.raw_handle(window);
        let Some(ws) = self.window_mut(window) else {
            return;
        };
        let Some(gpu) = &mut ws.gpu else {
            return;
        };
        if let Some(old) = gpu.surface.take() {
            gpu.backend.destroy_surface(old);
        }
        let Some(raw) = raw.filter(|r| available && !matches!(r, RawWindowHandle::Headless)) else {
            return;
        };
        let (w, h) = gpu.size;
        let surface = gpu.backend.create_surface(raw, w, h);
        // A new native window may composite differently from the old one;
        // the pipelines are rebuilt only then.
        if gpu.backend.surface_format(surface) != gpu.renderer.surface_format()
            || gpu.backend.surface_color_space(surface) != gpu.renderer.color_space()
        {
            gpu.renderer = Renderer::for_surface(&mut gpu.backend, surface);
        }
        gpu.surface = Some(surface);
        if let Some(root) = ws.root {
            ws.store.mark_dirty(root, DirtyClass::PAINT);
        }
        cx.request_redraw(window);
    }

    fn on_menu_command(&mut self, command: viso_platform::MenuCommandId) {
        // Hand the pick straight to the user application; the scheduler already
        // flagged the frame input-dirty, so any state it writes flushes next
        // frame. A menu command targets the app, not a window, so there is no
        // per-window routing here.
        if let Some(app) = self.app.as_mut() {
            app.on_menu_command(command);
        }
    }

    fn on_window_chrome_geom(
        &mut self,
        _cx: &mut RuntimeCx<'_>,
        window: WindowId,
        buttons_rect: LogicalRect,
    ) {
        // Record the native traffic-light box so a caption widget can align its
        // content around the buttons (it reads the width through `BuildCx::chrome`)
        // and so the app can too. The draggable regions are no longer derived here:
        // a caption widget declares which of its nodes are draggable, and the Layout
        // phase pushes their real world boxes to the platform once the tree has been
        // laid out — the geometry is only half the picture, the widget's own layout
        // is the other half. An event for an unknown window (already closed) is a
        // no-op.
        if let Some(ws) = self.window_mut(window) {
            ws.chrome_buttons = Some(buttons_rect);
        }
    }

    fn on_safe_area(&mut self, window: WindowId, insets: Insets) {
        if let Some(ws) = self.window_mut(window) {
            ws.safe_area = insets;
            ws.apply_safe_area();
        }
    }

    fn on_keyboard_inset(&mut self, window: WindowId, height: f64) {
        if let Some(ws) = self.window_mut(window) {
            ws.keyboard_inset = height;
            ws.apply_safe_area();
        }
    }

    fn on_fullscreen_changed(&mut self, window: WindowId, fullscreen: bool) {
        // Hide the self-drawn caption while fullscreen and restore it on exit by
        // toggling its root's visibility in place — on macOS the OS draws its own
        // auto-hiding title bar in fullscreen and removes the traffic lights, so
        // the caption must yield. `set_hidden` marks LAYOUT|PAINT, so the next
        // frame reflows the body to fill the reclaimed strip. A window with no
        // caption, or an event for an already-closed window, is a no-op.
        if let Some(ws) = self.window_mut(window)
            && let Some(cap) = ws.caption
        {
            ws.store.set_hidden(cap, fullscreen);
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
                        // 3. Semantic-state projections: re-run those whose cells
                        //    changed, each writing its node's `semantic_state`
                        //    column (and marking SEMANTICS) — both stores are live
                        //    here, so a control's checked/value/range reaches the
                        //    node column without the derive pass reading state.
                        ws.projectors.wake(&ws.changed, &ws.states, &mut ws.store);
                        // 4. Effects: re-run those whose dependencies changed. An
                        //    effect that writes state records it as pending for
                        //    the next frame; the scheduler carries state-dirty
                        //    forward, so a follow-up frame runs — no in-frame
                        //    cascade.
                        ws.effects.wake(&ws.changed, &ws.states);
                        ws.changed.clear();
                    }
                }

                // Session-level window open/close drain, once per frame after
                // every window has flushed its own transaction. A `window(...)`
                // handler or a `WindowHandle::close` recorded its request through
                // `EventCx`, the router handed it to the *originating* window's
                // store queue, and here — the one phase holding a live
                // `RuntimeCx` — the driver services it. It runs after the
                // per-window loop, not inside it, because opening a window pushes
                // to `self.windows` (which the loop borrows) and needs the live
                // `cx` to create the OS window; draining every window's queue
                // into driver-owned scratch first releases that borrow.
                //
                // Collect this frame's requests off every open window's store
                // into the reusable driver scratch. `take_*` clears the scratch
                // and appends, so a window that queued nothing contributes
                // nothing; a frame with no `window()` call leaves both empty and
                // the two loops below run zero iterations (steady-path no-op).
                self.pending_opens.clear();
                self.pending_closes.clear();
                for ws in &mut self.windows {
                    ws.store.take_window_opens(&mut ws.scratch_opens);
                    self.pending_opens.append(&mut ws.scratch_opens);
                    ws.store.take_window_closes(&mut ws.scratch_closes);
                    self.pending_closes.append(&mut ws.scratch_closes);
                }
                // Open each requested window through the shared bring-up path:
                // create the OS window (bumping the scheduler's open count so the
                // loop stays alive), then author its tree with the deferred build
                // closure the handler passed to `window(...).content(...)`. The
                // new window's root is marked dirty, so it renders next frame with
                // no self-scheduled spin. A `create_window` failure drops the
                // request — the app simply gets no window, no panic.
                for req in self.pending_opens.drain(..) {
                    // Carry the UI-tier chrome mode through to the build below as
                    // the build-time half of the caption data contract. It is the
                    // same value translated into the platform config, kept as the
                    // UI enum so `WindowState::open` seeds `BuildCx::chrome`.
                    let ui_chrome = req.config.chrome;
                    let config = to_platform_config(&req.config);
                    // Values the build closure moves in to wrap the deferred tree
                    // in the default caption band, exactly as the launch path does.
                    let title = req.config.title.clone();
                    let framed = cx.framed_windows();
                    let caption = req.config.caption && framed;
                    let build = req.build;
                    let id_slot = req.id_slot;
                    if let Ok(id) = cx.create_window(config) {
                        // Back-fill the tracking handle's slot with the raw id
                        // so a `WindowHandle` can observe and later close this
                        // window. `create_window` is the one place the id is
                        // known; an untracked open (`id_slot == None`) skips it.
                        if let Some(slot) = &id_slot {
                            slot.set(Some(id.0));
                        }
                        // Read the native traffic-light box synchronously so the
                        // deferred build sees it, exactly as the launch path does.
                        let chrome_geom = cx.window_chrome_geom(id);
                        // Capture the caption root during the synchronous build, as
                        // the launch path does, to hold for fullscreen hide.
                        let caption_cell = Cell::new(None);
                        let mut ws =
                            WindowState::open(cx, id, &self.fonts, ui_chrome, chrome_geom, |cx| {
                                if framed {
                                    wrap_root_with_caption(
                                        cx,
                                        &title,
                                        caption,
                                        &caption_cell,
                                        build,
                                    )
                                } else {
                                    wrap_root_in_safe_area(cx, build)
                                }
                            });
                        ws.caption = caption_cell.get();
                        if !framed {
                            ws.safe_area_root = ws.root;
                        }
                        self.windows.push(ws);
                    }
                }
                // Close each requested window through the platform seam only:
                // `close_window` destroys the OS window, which delivers a
                // `WindowClosed` event, which routes to `on_window_closed` — the
                // single teardown path shared with a user-driven OS close. We do
                // *not* retain-drop the `WindowState` here: doing both would tear
                // the window down twice and desync the scheduler's open count.
                for raw in self.pending_closes.drain(..) {
                    cx.close_window(WindowId(raw));
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
                    // Commit the layouts the text worker finished since the
                    // last frame, within the commit budget, so this frame's
                    // measure sees them; what the budget leaves waits for the
                    // next frame, drawing its last good layout meanwhile.
                    ws.commit_text_work();
                    ws.settle_text_edits();
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
                    // Width-aware text reflow (DL1): the layout just above records
                    // a reflow for each wrap-eligible leaf whose assigned box width
                    // differs from the width it was shaped at. Drain and reshape
                    // those at their assigned width, then relayout so a run whose
                    // wrapped height grew re-places its siblings — bounded, and a
                    // no-op on the steady frame that enqueued none. Runs before
                    // `absorb_measurements` so a virtualized list's height model
                    // sees the final wrapped row heights, not the unconstrained
                    // single-line ones.
                    let reflow_passes = ws.reflow_wrapped_text();
                    // Every layout this frame asked for goes to the worker at
                    // once, so it lays them out while the frame renders.
                    ws.dispatch_text_work();
                    // First-frame double-shape visibility (§61): a wrapped `Fill`
                    // paragraph is shaped once unconstrained then once at its width
                    // on the frame it appears/resizes; this surfaces that one-time
                    // cost, gated so a steady frame (zero passes) pays only the
                    // env-var check and nothing prints.
                    if reflow_passes > 0 && std::env::var_os("VISO_FRAME_TRACE").is_some() {
                        eprintln!("[viso] text reflow: {reflow_passes} pass(es)");
                    }
                    // Feed measured row heights back into each list's height model
                    // so a variable-height list corrects its extent and anchor
                    // next frame. Bounded to this frame's newly-mounted rows — no
                    // full-list sweep.
                    virtual_list::absorb_measurements(&ws.store, &mut ws.virtual_lists);
                    // Push the caption's draggable regions to the platform, but only
                    // when this frame's layout moved them (or first established them).
                    // The world boxes are final now; a caption that held still — the
                    // steady case, and every window without one — recomputes a tiny
                    // candidate list, finds it unchanged, and skips the cold
                    // back-channel entirely.
                    if ws.draggable_regions_changed() {
                        cx.set_draggable_regions(ws.window, &ws.draggable_cache);
                    }
                    // Tell the platform where text entry is, so an IME candidate
                    // window and the soft keyboard track the focused control. The
                    // keyboard is toggled only when focus crosses into or out of
                    // text entry, not when the control merely moves.
                    let was_editing = matches!(ws.ime_area, Some(Some(_)));
                    if ws.ime_area_changed() {
                        let area = ws.ime_area.flatten();
                        cx.set_ime_area(ws.window, area);
                        if was_editing != area.is_some() {
                            cx.show_soft_keyboard(ws.window, area.is_some());
                        }
                    }
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
                    // The viewport is the logical size: the projection maps
                    // primitive positions (logical) into the physical surface,
                    // which the shader's `pos / viewport * 2 - 1` does correctly
                    // whatever the scale — the physical surface is still fully
                    // covered and fragments still rasterize at physical density.
                    let scale = ws.dpi.max(1.0);
                    if let Some(gpu) = &mut ws.gpu {
                        let (w, h) = gpu.size;
                        let viewport = [w as f32 / scale, h as f32 / scale];
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
                                if let Some(text) = &ws.text {
                                    let c = text.counters();
                                    eprintln!(
                                        "viso: text reshapes={} relinebreaks={} shaped_runs={} main_shaped_runs={} commits={} deferred={} main_commit_us={} rasters={} atlas_upload_bytes={}",
                                        c.reshapes(),
                                        c.relinebreaks(),
                                        c.shaped_runs(),
                                        c.main_shaped_runs(),
                                        c.commits(),
                                        c.deferred(),
                                        c.main_commit_micros(),
                                        c.rasters(),
                                        c.atlas_upload_bytes(),
                                    );
                                    // Glyph residency, per representation pool:
                                    // what is resident, how many pages hold it,
                                    // and how the pools turned over this frame.
                                    let r = text.residency();
                                    use viso_text::GlyphImageKind as Pool;
                                    let pools = [
                                        ("a8", Pool::MaskA8),
                                        ("mtsdf", Pool::ScalableMtsdf),
                                        ("rgba", Pool::ColorRgba8),
                                        ("vector", Pool::OutlineVector),
                                    ];
                                    for (name, kind) in pools {
                                        eprintln!(
                                            "viso: glyph pool {name} glyphs={} pages={} bytes={} upload_bytes={}",
                                            r.pool_resident_glyphs(kind),
                                            r.pool_page_count(kind),
                                            r.pool_resident_bytes(kind),
                                            r.pool_upload_bytes(kind),
                                        );
                                    }
                                    eprintln!(
                                        "viso: glyph residency evictions={} admission_failures={}",
                                        c.evictions(),
                                        c.admission_failures(),
                                    );
                                    let f = text.face_cache();
                                    eprintln!(
                                        "viso: face cache bytes={}/{} faces={} hits={} misses={} evictions={} recency_updates={}",
                                        f.total_bytes(),
                                        f.budget_bytes(),
                                        f.len(),
                                        f.hits(),
                                        f.misses(),
                                        f.evictions(),
                                        f.recency_updates(),
                                    );
                                    let s = text.shaping_cache();
                                    eprintln!(
                                        "viso: shaping cache bytes={}/{} spans={} hits={} misses={} evictions={}",
                                        s.bytes(),
                                        s.budget_bytes(),
                                        s.len(),
                                        s.hits(),
                                        s.misses(),
                                        s.evictions(),
                                    );
                                }
                            }
                        }
                        if let Some(surface) = gpu.surface {
                            gpu.renderer.submit(
                                &mut gpu.backend,
                                surface,
                                [0.1, 0.1, 0.1, 1.0],
                                viewport,
                            );
                        }
                    }
                }
            }
            FramePhase::PostFrameCleanup => {
                for ws in &mut self.windows {
                    // The incremental passes have consumed this frame's
                    // invalidation; clear every node's dirty set so the next frame
                    // starts clean and an idle frame recomputes nothing.
                    ws.store.clear_dirty();
                    // Close the text frame: fold the glyph pages drawn this frame
                    // into page recency once, and zero the counters so each
                    // frame's trace reflects only that frame's shaping / raster /
                    // upload work (a steady-state repaint reads back all zeros).
                    if let Some(text) = &mut ws.text {
                        text.end_frame();
                    }
                    // Keep the loop beating while this window has live animations.
                    // `wants_animation` already tells the scheduler to stay in
                    // `Poll`, but on a headless backend `Poll` produces no beat on
                    // its own — a beat comes only from a `request_redraw`. So
                    // self-reschedule the next frame here whenever the registry is
                    // non-empty; the moment it empties (the last slide settled)
                    // this stops firing and, once every window is idle, the loop
                    // falls idle, holding the zero-CPU-when-idle contract.
                    // Text the worker still owes is committed by a later frame,
                    // so keep beating until it lands.
                    if !ws.animations.is_empty() || ws.text_work_pending() {
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
        self.windows
            .iter()
            .any(|w| !w.animations.is_empty() || w.text_work_pending())
    }

    fn next_timer_deadline(&self) -> Option<viso_runtime::Instant> {
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

/// Lower runtime-tier modifier state onto the UI-tier mirror (same fields, a
/// crate-boundary copy).
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
        AdaptiveColumns, BuildCx, FlexStyle, GridPlacement, GridStyle, LeafStyle, Role, Semantics,
        StateId, StateValue, TextRequest, TrackMax, TrackSizing, VirtualListStyle,
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
    // The top-level Window control (§68 public application lifecycle): a window
    // is not a node control — it does not enter the UI tree — so it appears as an
    // application-level handle, not a `Component`. `window(cfg).content(..).open(ev)`
    // opens another OS window mid-session and hands back a `WindowHandle` to close
    // it. `WindowConfig` is the (title, size) shape the author fills in. These are
    // commonly used, stable, and unambiguous, so they belong in the default set.
    pub use crate::{WindowBuilder, WindowChrome, WindowConfig, WindowHandle, window};
    // The application-menu model (§68 lifecycle): an app returns a `Menu` tree
    // from `Application::menu`, wiring custom items to `MenuCommandId`s it picks
    // and shortcuts through `Accel`; standard entries use `SystemAction`. Menus
    // are an application-level concern like `window`, so they belong here.
    pub use viso_platform::{Accel, Menu, MenuCommandId, SystemAction};
    // The declarative view-fragment entry point (§21.5): a small local `ui! { … }`
    // fragment lowers, at Rust compile time, to a static `BuildCx` builder closure.
    pub use crate::ui;
    // Text, List, Computed, Event, Task, Route, Theme, Color, Vec2, Rect,
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

/// The text-entry area for a control at `world` whose caret, relative to the
/// run's origin, is `caret`: a one-pixel column on the caret's line, kept inside
/// the control, or the whole control when no caret geometry exists.
fn caret_area(world: Rect, caret: Option<Rect>) -> LogicalRect {
    match caret {
        Some(c) => LogicalRect {
            x: (world.x + c.x.clamp(0.0, world.w)) as f64,
            y: (world.y + c.y) as f64,
            width: 1.0,
            height: c.h as f64,
        },
        None => LogicalRect {
            x: world.x as f64,
            y: world.y as f64,
            width: world.w as f64,
            height: world.h as f64,
        },
    }
}

#[cfg(test)]
mod caret_area_tests {
    use super::*;

    const FIELD: Rect = Rect {
        x: 10.0,
        y: 20.0,
        w: 160.0,
        h: 30.0,
    };

    #[test]
    fn a_caret_becomes_a_column_on_its_line_inside_the_field() {
        let caret = Rect {
            x: 42.0,
            y: 3.0,
            w: 0.0,
            h: 18.0,
        };
        assert_eq!(
            caret_area(FIELD, Some(caret)),
            LogicalRect::new(52.0, 23.0, 1.0, 18.0)
        );
        let past = Rect { x: 400.0, ..caret };
        assert_eq!(
            caret_area(FIELD, Some(past)).x,
            170.0,
            "clamped to the field"
        );
    }

    #[test]
    fn an_unshaped_field_reports_its_whole_box() {
        assert_eq!(
            caret_area(FIELD, None),
            LogicalRect::new(10.0, 20.0, 160.0, 30.0)
        );
    }
}

#[cfg(test)]
mod draggable_tests {
    use super::*;
    use viso_render::Rect;
    use viso_ui::{BoxStyle, LeafStyle, Size};

    /// Build a `WindowState` whose store holds a single fixed-size leaf registered
    /// as the caption's draggable region, laid out at `logical` points under `dpi`.
    /// The pipeline now lays out against the logical surface (physical ÷ dpi), so
    /// the store's world box is already in logical points; this helper mirrors that
    /// by laying out against the logical rect directly. Returns the state ready for
    /// `draggable_regions_changed`.
    fn state_with_draggable_leaf(logical: (f32, f32), dpi: f32) -> WindowState {
        let mut ws = WindowState::new(WindowId(0));
        ws.dpi = dpi;
        let root = {
            let mut cx = BuildCx::new(&mut ws.store);
            let leaf = cx.leaf(LeafStyle {
                size: Size::fixed(logical.0, logical.1),
                style: BoxStyle::NONE,
            });
            cx.register_draggable(leaf);
            leaf.id()
        };
        ws.root = Some(root);
        let mut scratch = Vec::new();
        ws.store.layout(
            root,
            Rect {
                x: 0.0,
                y: 0.0,
                w: logical.0,
                h: logical.1,
            },
            &mut scratch,
        );
        ws
    }

    /// The first call after a layout reports a change and fills the cache with the
    /// registered node's world box, already in logical points (the whole pipeline
    /// lays out logically), pushed verbatim; an immediately following call with the
    /// same layout reports no change and so would skip the platform push.
    #[test]
    fn first_layout_pushes_logical_regions_then_holds_still() {
        let mut ws = state_with_draggable_leaf((100.0, 40.0), 2.0);

        assert!(
            ws.draggable_regions_changed(),
            "first layout establishes the regions"
        );
        assert_eq!(
            ws.draggable_cache,
            vec![LogicalRect::new(0.0, 0.0, 100.0, 40.0)],
            "logical world box pushed verbatim"
        );

        assert!(
            !ws.draggable_regions_changed(),
            "an unchanged layout re-pushes nothing"
        );
    }

    /// A window with no caption (nothing registered) never touches the cache and
    /// always reports no change — the fast exit every plain window takes.
    #[test]
    fn no_registered_regions_never_changes() {
        let mut ws = WindowState::new(WindowId(0));
        assert!(!ws.draggable_regions_changed());
        assert!(ws.draggable_cache.is_empty());
    }

    /// Relaying the same tree at a new size moves the region, which the diff catches
    /// and the cache follows.
    #[test]
    fn relayout_at_new_size_reports_change() {
        let mut ws = state_with_draggable_leaf((200.0, 80.0), 1.0);
        assert!(ws.draggable_regions_changed());

        let root = ws.root.unwrap();
        ws.store.set_fixed_size(root, Size::fixed(300.0, 80.0));
        let mut scratch = Vec::new();
        ws.store.layout(
            root,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 300.0,
                h: 80.0,
            },
            &mut scratch,
        );

        assert!(
            ws.draggable_regions_changed(),
            "a wider layout moves the region"
        );
        assert_eq!(
            ws.draggable_cache,
            vec![LogicalRect::new(0.0, 0.0, 300.0, 80.0)]
        );
    }
}

#[cfg(test)]
mod fullscreen_tests {
    use super::*;
    use viso_ui::{
        BindingTable, ChromeContext, LeafStyle, SemanticProjector, StateStore, TextEdits,
        VirtualLists,
    };

    /// Build a caption-wrapped tree against a fresh reactive cx (as `open` does),
    /// returning the store and the caption id the wrap captured through the cell.
    fn wrap_capturing(caption: bool) -> (NodeStore, Option<NodeId>) {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        )
        .with_chrome(ChromeContext::default());
        let caption_cell = Cell::new(None);
        // An app body: a single leaf so the wrap has content to place under the bar.
        let _root = wrap_root_with_caption(&mut cx, "Title", caption, &caption_cell, |cx| {
            Some(cx.leaf(LeafStyle::default()).id())
        });
        (store, caption_cell.get())
    }

    #[test]
    fn wrap_captures_the_caption_id_when_a_caption_is_drawn() {
        let (_store, caption) = wrap_capturing(true);
        assert!(
            caption.is_some(),
            "a caption-drawing window exposes its bar root for fullscreen hide"
        );
    }

    #[test]
    fn wrap_leaves_no_caption_id_for_a_captionless_window() {
        let (_store, caption) = wrap_capturing(false);
        assert!(
            caption.is_none(),
            "a window that draws no caption has nothing to hide in fullscreen"
        );
    }

    #[test]
    fn hiding_and_restoring_the_captured_caption_toggles_its_visibility() {
        // The exact store mutation `on_fullscreen_changed` performs: flip the
        // captured node hidden on enter, visible on exit. `set_hidden`'s LAYOUT|PAINT
        // dirtying and body reflow are covered by ui/paint tests; here we prove the
        // captured id is the right target and the toggle round-trips.
        let (mut store, caption) = wrap_capturing(true);
        let cap = caption.expect("a caption window captures its bar root");

        assert!(!store.hidden(cap), "the caption starts visible");
        store.set_hidden(cap, true);
        assert!(store.hidden(cap), "entering fullscreen hides the caption");
        store.set_hidden(cap, false);
        assert!(
            !store.hidden(cap),
            "leaving fullscreen restores the caption"
        );
    }
}
