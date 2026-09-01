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
//! This crate is the single public facade (AGENTS §3.1). Ordinary apps depend
//! only on `viso` and never on the internal crates (`viso-ui`,
//! `viso-render`, `viso-runtime`, …). Internal complexity is allowed; public
//! accidental complexity is not.
//!
//! Design summary:
//! > External declarative, internal retained.
//! > External object-oriented, internal data-oriented.
//! > Dynamic in development, AOT in release.
//! > Abstraction on cold paths, flat data on hot paths.
//!
//! ## Phase 1 status
//!
//! [`run`] now owns a real platform event pump and frame scheduler. It opens a
//! native window (headless when no native backend is available), handles
//! resize, receives input, and drives blank 12-phase frames — with no makepad
//! `AppMain` adapter. Pixels (the GPU swapchain) arrive in Phase 2.

#![forbid(unsafe_op_in_unsafe_fn)]

use viso_gpu::{Backend, GpuBackend, SurfaceId, TextureDesc, TextureFormat, TextureId};
use viso_platform::{WindowConfig, WindowId};
use viso_render::Renderer;
use viso_runtime::{FramePhase, RuntimeCx, Scheduler};

pub use viso_ui::context::AppCx;

/// The application entry-point contract implemented by every Viso app.
///
/// The single generic entry point is [`run`]. An `Application` owns top-level
/// state; it is not forced to contain a Router or a global Store (§6.1) —
/// those are opt-in.
pub trait Application: Sized + 'static {
    /// Construct the application. Windows and services are created via `cx`.
    fn new(cx: &mut AppCx) -> Self;
}

/// Run a Viso application to completion.
///
/// Owns the platform event pump and the frame scheduler (§11.1). It creates the
/// native platform app (falling back to a headless app where no native backend
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
}

/// The facade-owned GPU state: the concrete backend, the renderer, and the
/// per-window surface.
///
/// ADR-007: the backend is the compile-time-selected concrete [`Backend`]
/// (Metal on macOS, software raster elsewhere), held by value so the frame hot
/// path is monomorphized — no `dyn GpuBackend`.
struct GpuState {
    backend: Backend,
    renderer: Renderer,
    surface: SurfaceId,
    /// The Image test-scene texture (a small checkerboard), created once at
    /// launch and sampled by the scene's [`viso_render::Primitive::Image`].
    test_texture: TextureId,
    /// The R8 SDF glyph atlas for the test scene's text run, created and
    /// uploaded once at launch.
    glyph_atlas: TextureId,
    /// The positioned glyphs and color for the test scene's text run, prepared
    /// once at launch and re-wrapped into a [`GlyphRunDraw`] each frame.
    glyphs: viso_render::TestGlyphs,
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
        }
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

        // Create the Image test-scene texture (a 4×4 checkerboard) and upload it.
        let (tw, th, texels) = viso_render::test_texture();
        let test_texture = backend.create_texture(&TextureDesc {
            width: tw,
            height: th,
            format: TextureFormat::Bgra8Unorm,
            render_target: false,
            label: "test-checkerboard",
        });
        backend.write_texture(test_texture, 0, 0, tw, th, &texels);

        // Prepare the test text run and upload its R8 SDF atlas.
        let glyphs = viso_render::test_glyphs([16.0, 16.0], 28.0);
        let glyph_atlas = backend.create_texture(&TextureDesc {
            width: glyphs.atlas_size,
            height: glyphs.atlas_size,
            format: TextureFormat::R8Unorm,
            render_target: false,
            label: "test-glyph-atlas",
        });
        backend.write_texture(
            glyph_atlas,
            0,
            0,
            glyphs.atlas_size,
            glyphs.atlas_size,
            &glyphs.atlas_pixels,
        );

        self.gpu = Some(GpuState {
            backend,
            renderer,
            surface,
            test_texture,
            glyph_atlas,
            glyphs,
            size: (w.max(1), h.max(1)),
        });
    }

    fn on_geometry(&mut self, _window: WindowId, _scale: f64, width: u32, height: u32) {
        // Resize the swapchain so the next frame maps pixel-space to the new
        // extent. Layout consumes the geometry once the layout subsystem lands.
        if let Some(gpu) = &mut self.gpu {
            let (w, h) = (width.max(1), height.max(1));
            gpu.backend.resize_surface(gpu.surface, w, h);
            gpu.size = (w, h);
        }
    }

    fn on_input(&mut self) {
        // Phase 2: input is aggregated into a redraw reason by the scheduler;
        // hit-testing and dispatch land with the input subsystem (Phase 3+).
    }

    fn run_phase(&mut self, phase: FramePhase, _cx: &mut RuntimeCx<'_>) {
        // Phase 2 vertical slice: the render phases drive the GPU. The app's
        // real widget tree (Phase 3) will feed primitives into BuildPaintChanges;
        // for now the phases render a fixed test scene so pixels reach the
        // window. Phases other than the render trio remain no-ops here.
        let Some(gpu) = &mut self.gpu else {
            return;
        };
        match phase {
            FramePhase::UploadGpuChanges => {
                // Build this frame's primitives (test scene) and stage their
                // instances into the persistent buffer.
                let glyphs = viso_render::GlyphRunDraw {
                    glyphs: gpu.glyphs.glyphs.clone(),
                    atlas: gpu.glyph_atlas,
                    color: gpu.glyphs.color,
                };
                let scene = viso_render::test_scene(gpu.test_texture, glyphs);
                gpu.renderer.upload(&mut gpu.backend, &scene);
            }
            FramePhase::Submit => {
                let (w, h) = gpu.size;
                gpu.renderer.submit(
                    &mut gpu.backend,
                    gpu.surface,
                    [0.1, 0.1, 0.1, 1.0],
                    [w as f32, h as f32],
                );
            }
            _ => {}
        }
    }
}

/// The curated default prelude (§5.1). Kept to a small, stable, low-ambiguity
/// set — GPU/backend/internal-compiler types never appear here.
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

// -- Advanced escape hatches. Opt-in, clearly namespaced (§5.1, §6.1). --
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
