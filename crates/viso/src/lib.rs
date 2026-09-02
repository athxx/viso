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
use viso_platform::{WindowConfig, WindowId};
use viso_render::{Primitive, Rect, Renderer};
use viso_runtime::{FramePhase, RuntimeCx, Scheduler};
use viso_ui::{
    BindingTable, BuildCx, ComputedStore, DirtyClass, EffectStore, FrameRecompute, ImeEvent, Key,
    KeyEvent, KeyRouter, Modifiers, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase,
    PointerRouter, ScrollEvent, ScrollRouter, StateId, StateStore, VirtualLists, focus_next,
    virtual_list,
};

pub use viso_ui::context::AppCx;

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
    window: Option<WindowId>,
    /// The GPU state, created on launch once the window (and its native handle)
    /// exists. `None` until then, or when surface creation is unavailable
    /// (headless platform with no window handle).
    gpu: Option<GpuState>,
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
            window: None,
            gpu: None,
            store: NodeStore::new(),
            states: StateStore::new(),
            bindings: BindingTable::new(),
            computeds: ComputedStore::new(),
            effects: EffectStore::new(),
            virtual_lists: VirtualLists::new(),
            changed: Vec::new(),
            root: None,
            route_chain: Vec::new(),
            primitives: Vec::new(),
            scratch: Vec::new(),
            redo_roots: Vec::new(),
            recompute: FrameRecompute::default(),
            awaiting_first_frame: true,
        }
    }

    /// Incrementally relayout and repaint against the current surface size,
    /// recording how much each layer touched. Only the subtrees carrying
    /// measure/layout invalidation are re-placed; paint is rebuilt only when a
    /// paint-affecting class is pending, otherwise the primitive buffer is left
    /// intact for reuse. A no-op if the tree or GPU is absent.
    fn relayout_and_paint(&mut self) {
        let (Some(root), Some(gpu)) = (self.root, &self.gpu) else {
            return;
        };
        let (w, h) = gpu.size;
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: w as f32,
            h: h as f32,
        };
        let (measured, laid_out) =
            self.store
                .relayout_dirty(root, surface, &mut self.scratch, &mut self.redo_roots);
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
        self.window = Some(id);

        // Bring up the GPU for this window: create the device, attach a surface
        // to the window's native handle, and build the renderer for that
        // surface's format. If the platform exposes no native handle (headless
        // app with no window backend), we stay `gpu = None` and draw nothing.
        let Some(raw) = cx.raw_handle(id) else {
            return;
        };
        let (w, h) = cx.inner_size(id).unwrap_or((1, 1));
        let mut backend = viso_gpu::create_device();
        let surface = backend.create_surface(raw, w.max(1), h.max(1));
        let format = backend.surface_format(surface);
        let renderer = Renderer::new(&mut backend, format);

        self.gpu = Some(GpuState {
            backend,
            renderer,
            surface,
            size: (w.max(1), h.max(1)),
        });

        // Build the user application's retained UI tree once, now that we have a
        // surface size. The driver owns `store`, `states`, and `bindings` as
        // sibling fields, so all three can be borrowed together into a reactive
        // build context — this is why scene authoring works here where new-time
        // allocation could not (the session-long `AppCx` marker cannot retain a
        // live store borrow). Layout runs incrementally per frame in `run_phase`.
        //
        // When structural teardown arrives (a targeted rebuild that frees nodes),
        // each freed node must run `self.effects.cancel_for_node(id)` before its
        // slot is reused, so scoped effects release their resources (cleanup then
        // drop) at unmount. `build` runs once and frees nothing, so there is no
        // live call site yet; this is where it will hook.
        self.store.clear();
        self.virtual_lists.clear();
        if let Some(app) = &mut self.app {
            let mut build = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.virtual_lists,
            );
            app.build(&mut build);
            self.root = build.root();
        }

        // Seed the first frame: mark the root fully dirty so the incremental
        // passes do the initial measure/layout/paint for the whole tree.
        if let Some(root) = self.root {
            self.store.mark_dirty(
                root,
                DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
            );
        }
    }

    fn on_geometry(&mut self, _window: WindowId, _scale: f64, width: u32, height: u32) {
        // Resize the swapchain so the next frame maps pixel-space to the new
        // extent, and mark the root for relayout so the next incremental frame
        // re-places the tree against the new surface and repaints.
        if let Some(gpu) = &mut self.gpu {
            let (w, h) = (width.max(1), height.max(1));
            gpu.backend.resize_surface(gpu.surface, w, h);
            gpu.size = (w, h);
        }
        if let Some(root) = self.root {
            self.store.mark_dirty(
                root,
                DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
            );
        }
    }

    fn on_input(&mut self, sample: viso_runtime::InputSample) {
        // The sample is already in physical pixels (the scheduler resolved the
        // window scale), so it maps straight onto the UI-tier `PointerEvent` —
        // the same space as node bounds and hit testing. Route it along the hit
        // node's ancestry; any state a handler writes lands in this frame's
        // pending set and is turned into targeted dirtying by the next frame's
        // flush (the scheduler already flagged the frame input-dirty).
        let Some(root) = self.root else {
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
                    &mut self.store,
                    &mut self.states,
                    &self.bindings,
                    root,
                    ev,
                    &mut self.route_chain,
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
                    focus_next(&mut self.store, root, !modifiers.shift);
                } else {
                    let ev = KeyEvent {
                        key: lower_key(k.key),
                        pressed: k.pressed,
                        repeat: k.repeat,
                        modifiers,
                    };
                    KeyRouter::route_key(
                        &mut self.store,
                        &mut self.states,
                        &self.bindings,
                        root,
                        ev,
                        &mut self.route_chain,
                    );
                }
            }
            viso_runtime::InputSample::Text(t) => {
                // A committed (post-IME) segment routes to the focused node as an
                // IME commit — the text a control appends to its buffer.
                KeyRouter::route_ime(
                    &mut self.store,
                    &mut self.states,
                    &self.bindings,
                    root,
                    ImeEvent::Commit { text: t.text },
                    &mut self.route_chain,
                );
            }
            viso_runtime::InputSample::ImePreedit(p) => {
                // An in-progress composition routes as a preedit; a control shows
                // it inline and replaces it on each update until the commit.
                KeyRouter::route_ime(
                    &mut self.store,
                    &mut self.states,
                    &self.bindings,
                    root,
                    ImeEvent::Preedit {
                        text: p.text,
                        caret: p.caret,
                    },
                    &mut self.route_chain,
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
                ScrollRouter::route(&mut self.store, root, ev);
            }
        }
    }

    fn run_phase(&mut self, phase: FramePhase, _cx: &mut RuntimeCx<'_>) {
        // The render phases drive the GPU from the real retained tree: Measure +
        // Layout resolve node boxes, then paint lowers them to primitives which
        // the renderer batches and submits. Non-render phases are no-ops here.
        match phase {
            FramePhase::FlushStateTransactions => {
                // Drain this frame's pending state writes once and fan the same
                // changed set through the three downstream reactors, in order.
                // Many writes in one transaction collapse here; a frame with no
                // writes touches nothing.
                if self.states.has_pending() {
                    self.states.take_pending(&mut self.changed);
                    // 1. Derivations first: a memo-gated re-eval dirties a node
                    //    only when its derived value actually changed, so any
                    //    dirtying it produces is in place before Measure/Layout.
                    self.computeds
                        .wake_computed(&self.changed, &self.states, &mut self.store);
                    // 2. Direct bindings: turn each changed cell into targeted
                    //    node dirtying through the compiled static + dynamic-script
                    //    edges. (Computed no longer registers dynamic edges, so a
                    //    derivation's node is dirtied once, by the pass above.)
                    self.store
                        .flush_state_transactions(&self.changed, &self.bindings);
                    // 3. Effects: re-run those whose dependencies changed. An
                    //    effect that writes state records it as pending for the
                    //    next frame; the scheduler carries state-dirty forward, so
                    //    a follow-up frame runs — no in-frame cascade.
                    self.effects.wake(&self.changed, &self.states);
                    self.changed.clear();
                }
            }
            FramePhase::Layout => {
                // Reconcile virtualized lists first: each list reads its viewport's
                // current scroll, and only when the visible range crossed a row
                // boundary does it recycle/rebind a handful of hosts and mark the
                // canvas LAYOUT|MEASURE. Steady scroll within a row is a no-op here
                // (the scroll's TRANSFORM already moved the mounted rows), so the
                // relayout below stays confined to the changed canvas subtree.
                virtual_list::reconcile(
                    &mut self.store,
                    &mut self.virtual_lists,
                    &mut self.states,
                    &mut self.bindings,
                    &mut self.effects,
                );
                // Incrementally re-place invalidated subtrees and repaint if any
                // paint-affecting class is pending; a clean frame touches nothing.
                self.relayout_and_paint();
                // Feed measured row heights back into each list's height model so a
                // variable-height list corrects its extent and anchor next frame.
                // Bounded to this frame's newly-mounted rows — no full-list sweep.
                virtual_list::absorb_measurements(&self.store, &mut self.virtual_lists);
            }
            FramePhase::UploadGpuChanges => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.renderer.upload(&mut gpu.backend, &self.primitives);
                }
            }
            FramePhase::Submit => {
                if let Some(gpu) = &mut self.gpu {
                    let (w, h) = gpu.size;
                    // One-shot proof the first frame reached the GPU, gated on an
                    // env var so it costs a single bool check per steady-state
                    // frame and nothing else. Read once, then the branch dies.
                    // Stats must be read before `submit` consumes the segments.
                    if self.awaiting_first_frame {
                        self.awaiting_first_frame = false;
                        if std::env::var_os("VISO_FRAME_TRACE").is_some() {
                            let stats = gpu.renderer.frame_stats();
                            let recompute = self.recompute;
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
            FramePhase::PostFrameCleanup => {
                // The incremental passes have consumed this frame's invalidation;
                // clear every node's dirty set so the next frame starts clean and
                // an idle frame recomputes nothing.
                self.store.clear_dirty();
            }
            _ => {}
        }
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
        BuildCx, FlexStyle, LeafStyle, Role, Semantics, StateId, StateValue, VirtualListStyle,
    };
    // View, Window, Button, Label, Text, Image, List, Scroll, Computed, Event,
    // Task, Route, Theme, Color, Vec2, Rect, Constraints and the
    // component!/ui!/view!/routes! macros join this as their subsystems land in
    // later phases.
}

// -- Advanced escape hatches. Opt-in, clearly namespaced. --
pub mod ui {
    pub use viso_ui::*;
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
