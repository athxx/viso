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
use viso_render::{Primitive, Rect, Renderer, Rgba};
use viso_runtime::{FramePhase, RuntimeCx, Scheduler};
use viso_ui::{
    Align, Axis, BindingTable, BoxStyle, BuildCx, Component, ComputedStore, DirtyClass,
    EffectStore, FlexStyle, FrameRecompute, Inset, LeafStyle, NodeId, NodeStore, Size, StateId,
    StateStore,
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
    /// Reusable buffer the flush drains this frame's pending state ids into, so
    /// the steady path allocates nothing while draining the transaction.
    changed: Vec<StateId>,
    /// The tree root declared by [`Scene`], if the build succeeded.
    root: Option<NodeId>,
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
            changed: Vec::new(),
            root: None,
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

/// The demo scene, built as a real retained tree: a padded Row of three boxes —
/// two fixed and one that fills the leftover width — centered on the cross axis.
/// This replaces the fixed `test_scene` primitive list; the same visual now
/// flows from Component → Node → Flex layout → paint.
struct Scene;

impl Component for Scene {
    fn build(&self, cx: &mut BuildCx<'_>) {
        cx.flex(
            FlexStyle {
                axis: Axis::Row,
                gap: 8.0,
                padding: Inset::all(12.0),
                align: Align::Center,
                size: Size::fill(),
                style: BoxStyle::solid(Rgba {
                    r: 0.15,
                    g: 0.16,
                    b: 0.20,
                    a: 1.0,
                }),
            },
            |cx| {
                cx.leaf(LeafStyle {
                    size: Size::fixed(48.0, 40.0),
                    style: BoxStyle::solid(Rgba {
                        r: 0.9,
                        g: 0.1,
                        b: 0.1,
                        a: 1.0,
                    })
                    .with_radius(8.0),
                });
                cx.leaf(LeafStyle {
                    size: Size {
                        width: viso_ui::Length::fill(),
                        height: viso_ui::Length::Fixed(56.0),
                    },
                    style: BoxStyle::solid(Rgba {
                        r: 0.1,
                        g: 0.7,
                        b: 0.3,
                        a: 1.0,
                    })
                    .with_radius(4.0),
                });
                cx.leaf(LeafStyle {
                    size: Size::fixed(64.0, 48.0),
                    style: BoxStyle::solid(Rgba {
                        r: 0.2,
                        g: 0.4,
                        b: 0.95,
                        a: 1.0,
                    })
                    .with_radius(6.0),
                });
            },
        );
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

        // Build the retained UI tree once, now that we have a surface size. The
        // structure is fixed this slice; targeted structural rebuilds land with
        // reactive state. Layout runs incrementally per frame in `run_phase`.
        //
        // When structural teardown arrives (a targeted rebuild that frees nodes),
        // each freed node must run `self.effects.cancel_for_node(id)` before its
        // slot is reused, so scoped effects release their resources (cleanup then
        // drop) at unmount. The current Scene frees nothing, so there is no live
        // call site yet; this is where it will hook.
        self.store.clear();
        let mut build = BuildCx::new(&mut self.store);
        Scene.build(&mut build);
        self.root = build.root();

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

    fn on_input(&mut self) {
        // Input is aggregated into a redraw reason by the scheduler; hit-testing
        // and dispatch land with the input subsystem.
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
                // Incrementally re-place invalidated subtrees and repaint if any
                // paint-affecting class is pending; a clean frame touches nothing.
                self.relayout_and_paint();
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

/// The curated default prelude. Kept to a small, stable, low-ambiguity set —
/// GPU/backend/internal-compiler types never appear here.
pub mod prelude {
    pub use crate::{Application, run};
    pub use viso_ui::context::AppCx;
    pub use viso_ui::dirty::DirtyClass;
    pub use viso_ui::node::NodeId;
    // Application, Component, View, Window, Button, Label, Text, Image, List,
    // Scroll, State, Computed, Event, Task, Route, Theme, Color, Vec2, Rect,
    // Size, Constraints and the component!/ui!/view!/routes! macros join this
    // as their subsystems land in later phases.
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
