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
//! receives input, and drives the 17-phase frame. A real retained UI tree flows
//! Component → Node → Flex layout → paint → renderer to the GPU, and each frame
//! recomputes only the invalidated subtree rather than the whole tree.

#![forbid(unsafe_op_in_unsafe_fn)]

use std::any::Any;
use std::cell::Cell;
use std::rc::Rc;

use viso_gpu::{Backend, GpuBackend, SurfaceId};
use viso_platform::{LogicalInsets, LogicalRect, RawWindowHandle, TextEntry, WindowId};
use viso_render::{Primitive, Rect, Renderer};
use viso_runtime::{FramePhase, Instant, RuntimeCx, Scheduler};
use viso_ui::context::UpdateCx;
use viso_ui::{
    AnimationRegistry, Axis, BindingTable, BuildCx, ChromeContext, ComputedStore, Continuation,
    DirtyCause, DirtyClass, EffectStore, FlexStyle, FrameRecompute, ImeEvent, KeyEvent, KeyRouter,
    LinkPointer, Modifiers, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase,
    PointerRouter, ScrollEvent, ScrollRouter, SemanticProjector, Size, StateId, StateStore,
    TextEdits, TextRequest, TimerRegistry, TimerRequest, TranslateAnim, VirtualLists,
    WindowOpenRequest, focus_next, text_edit, virtual_list,
};
#[cfg(any(feature = "hot-reload", feature = "headless"))]
use viso_view::profile::Count;
use viso_view::profile::Span;
use viso_widgets::caption_bar;

mod accessibility;
pub mod alloc;
mod app_menu;
mod assets;
pub use assets::ImageLoader;
pub mod audio;
pub mod cursor;
mod edit_menu;
mod environment;
mod footprint;
mod game_sheet;
#[cfg(feature = "headless")]
mod headless;
#[cfg(any(feature = "hot-reload", feature = "headless"))]
mod inspect;
pub mod native;
#[cfg(feature = "pdf")]
pub mod pdf;
mod persist;
mod places;
mod playback;
mod profile;
#[cfg(any(feature = "headless", feature = "hot-reload"))]
mod scenario;
pub mod services;
mod shaders;
pub mod shell;
pub mod system_fonts;
mod text_content;
mod text_harness;
mod text_worker;
mod videos;
use text_content::{ParagraphSlot, TextShaper};
pub use videos::cameras;

#[cfg(feature = "hot-reload")]
mod hot_reload;
#[cfg(any(feature = "hot-reload", feature = "headless"))]
mod tape;
mod window;
pub use footprint::{BudgetExceeded, BudgetKind, Footprint, MemoryBudget};
pub use window::{WindowBuilder, WindowHandle, window};

pub use viso_ui::context::AppCx;

// The introspection snapshot model, re-exported so a tool depending only on the
// `viso` facade — Studio transport, the `viso inspect --json` CLI — names one
// `viso::InspectSnapshot` and its `snapshot_ui` builder rather than reaching
// into `viso-ui`. A cold-path tooling surface: deliberately kept out of the
// default prelude.
pub use viso_ui::{InspectSnapshot, snapshot_ui};

// The UI-tier window configuration is the shape a `window(...)` author fills in
// (title + logical size) — the facade translates it into the platform config at
// the open-drain point. Re-export it under the facade so app code names one
// `viso::WindowConfig`, never the internal `viso_platform::WindowConfig` (kept
// private to this module for the drain translation).
pub use viso_ui::{WindowChrome, WindowConfig};

// The DSL entry points `ui!`, `component!` and `view!` live in the compile-time-only
// `viso-ui-macros` crate, which depends on neither crate its expansion builds
// with; the expansion names them through `viso::__private`, so an app that
// depends on `viso` alone expands them.
pub use viso_ui_macros::{asset, component, ui, view};

/// The crates the expansion of `ui!`, `component!`, `view!` and `asset!`
/// builds with. Not an API: its paths change with the macros.
#[doc(hidden)]
pub mod __private {
    pub use viso_behavior;
    pub use viso_ende;
    pub use viso_platform;
    pub use viso_ui;
    pub use viso_view;
}

// `print!`, `println!`, `eprint!` and `eprintln!` for every target: natively
// what `std`'s print, and on the web, where `std` drops what is printed, each
// line goes to the page's console (output to `console.log`, errors to
// `console.error`), where devtools and the dev session read it. A glob
// import cannot shadow `std`'s, so they are named by path (`viso::println!`)
// or imported by name (`use viso::println;`).

#[doc(hidden)]
#[macro_export]
macro_rules! __print {
    ($($arg:tt)*) => {
        $crate::__private::viso_platform::stdio::write(
            $crate::__private::viso_platform::stdio::Stream::Out,
            ::core::format_args!($($arg)*),
        )
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __println {
    () => {
        $crate::__print!("\n")
    };
    ($($arg:tt)*) => {
        $crate::__private::viso_platform::stdio::write(
            $crate::__private::viso_platform::stdio::Stream::Out,
            ::core::format_args!("{}\n", ::core::format_args!($($arg)*)),
        )
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __eprint {
    ($($arg:tt)*) => {
        $crate::__private::viso_platform::stdio::write(
            $crate::__private::viso_platform::stdio::Stream::Err,
            ::core::format_args!($($arg)*),
        )
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __eprintln {
    () => {
        $crate::__eprint!("\n")
    };
    ($($arg:tt)*) => {
        $crate::__private::viso_platform::stdio::write(
            $crate::__private::viso_platform::stdio::Stream::Err,
            ::core::format_args!("{}\n", ::core::format_args!($($arg)*)),
        )
    };
}

/// Prints to the standard error, as [`std::eprint!`] does; on the web, to the
/// page's console once the line ends.
pub use crate::__eprint as eprint;
/// Prints a line to the standard error, as [`std::eprintln!`] does; on the
/// web, to the page's console (`console.error`).
pub use crate::__eprintln as eprintln;
/// Prints to the standard output, as [`std::print!`] does; on the web, to the
/// page's console once the line ends.
pub use crate::__print as print;
/// Prints a line to the standard output, as [`std::println!`] does; on the
/// web, to the page's console (`console.log`).
pub use crate::__println as println;

/// The form of a view's behavior the expansion of `view!` keeps, as its
/// module's bytes and the module compiled to Rust: in a release build, and
/// in any build without the `vm` feature, the compiled code, run in place of
/// the interpreter, and the module without bytecode beside it; a debug build
/// with the interpreter keeps the bytecode it can hot-reload, and compiles
/// none of the Rust. The form not kept is compiled out, its bytes with it.
#[doc(hidden)]
#[macro_export]
#[cfg(any(not(debug_assertions), not(feature = "vm")))]
macro_rules! __behavior {
    (bytecode: $bytecode:block, compiled: $compiled:block, native: { $($code:tt)* } $(,)?) => {
        ($compiled, ::core::option::Option::Some({ $($code)* }))
    };
}

#[doc(hidden)]
#[macro_export]
#[cfg(all(debug_assertions, feature = "vm"))]
macro_rules! __behavior {
    (bytecode: $bytecode:block, compiled: $compiled:block, native: { $($code:tt)* } $(,)?) => {
        ($bytecode, ::core::option::Option::None)
    };
}

// The facade's own code and tests expand the macros too.
extern crate self as viso;

// The SVG input lane, re-exported as `viso::svg` so an app that has SVG bytes
// calls `viso::svg::parse_svg(..)` without naming the internal crates. A
// cold-path input codec (parse once, cache the resulting primitives),
// deliberately kept out of the default prelude: most apps never touch it, and
// `SvgScene`/`SvgError` are not part of the default mental model.
pub use viso_render::svg;

// The clipboard as an app reaches it: the content a handler copies
// (`cx.copy`), the kinds a paste takes, and the image a paste brings, shown
// as an asset or decoded. Out of the prelude: an app names it when it copies
// more than text.
pub use viso_ui::clipboard;

/// Fonts packaged into the binary from the crate's `assets/fonts/`:
/// `cx.use_packaged_fonts(viso::packaged_fonts!())` in `Application::new`.
pub mod fonts {
    pub use viso_text::{FontSlant, PackagedFace, PackagedFonts};

    /// Whether this build rasterizes glyphs at run time (the `rasterizer`
    /// feature); `packaged_fonts!` checks its faces against it.
    #[doc(hidden)]
    pub const __RASTERIZER: bool = cfg!(feature = "rasterizer");
}
pub use viso_macros::packaged_fonts;

/// The application entry-point contract implemented by every Viso app.
///
/// The single generic entry point is [`run`]. An `Application` owns top-level
/// state; it is not forced to contain a Router or a global Store — those are
/// opt-in.
pub trait Application: Sized + 'static {
    /// Construct the application. `cx` registers fonts (`load_font`,
    /// `use_packaged_fonts`) and the dev state a warm restart keeps; the launch
    /// window opens from [`window_config`](Self::window_config) and its scene is
    /// authored in [`build`](Self::build). Further windows are opened from
    /// handlers, and services are reached through a handler's context.
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

    /// Open `url`, a link the app was opened with: a URL of a scheme
    /// `[package.links]` declares in `Viso.toml`, or an `https` link to one
    /// of its hosts (an iOS universal link, an Android app link). Called for
    /// the link that started the app and for each one that arrives while it
    /// runs, in order, once the app has built its window: a link that comes
    /// earlier waits for it. On Windows and Linux a second start of the app
    /// hands its link to the one running and ends; on the web, a page URL
    /// with a fragment is such a link. `cx` is the launch window's, as a
    /// handler at its root gets: write state, spawn a task, open a window.
    /// The default ignores every link.
    fn on_open_url(&mut self, cx: &mut viso_ui::EventCx<'_>, url: &str) {
        let _ = (cx, url);
    }

    /// Handle a click on the app's tray icon, shown through
    /// [`shell::set_tray`]. Picks of its menu reach
    /// [`on_menu_command`](Self::on_menu_command). The default ignores every
    /// click.
    fn on_tray(&mut self, event: shell::TrayEvent) {
        let _ = event;
    }

    /// The memory budget the app's caches share, read once before the first
    /// window opens. The default is the build's: its footprint profile's
    /// figures with the target's `budget` overrides
    /// ([`MemoryBudget::of_build`]).
    fn memory_budget() -> MemoryBudget {
        MemoryBudget::of_build()
    }

    /// What fetches the images the app shows from URLs, read once before
    /// the first window opens. The default is none: such an image fails and
    /// shows its error view.
    fn image_loader() -> Option<ImageLoader> {
        None
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
    // `viso run` executes a new build once before restarting on it, for the
    // OS to check the executable while the old app runs on.
    #[cfg(feature = "hot-reload")]
    if viso_platform::launch_var(viso_view::dev::wire::DEV_PREWARM_ENV).is_some() {
        return;
    }
    #[cfg(feature = "headless")]
    {
        headless::run::<A>();
        return;
    }
    // A headless fallback has no OS behind its services.
    #[cfg_attr(feature = "headless", allow(unreachable_code))]
    let (platform_app, services) = match viso_platform::create_app() {
        Ok(app) => (app, None),
        Err(_) => (
            viso_platform::create_headless_app(),
            Some(services::Services::unsupported()),
        ),
    };
    let driver = AppDriver::<A>::new(services);
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
/// Not part of the public API — this is the first-class headless
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
    pub use crate::game_sheet::render as render_game_sheet;
    #[cfg(any(feature = "hot-reload", feature = "headless"))]
    pub use viso_view::tape::UiTape;

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

        /// The first window's reactive state cells.
        pub fn states(&self) -> &super::StateStore {
            &self.driver.windows[0].states
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

        /// The text text field `node` in the launch window holds, focused or
        /// not.
        pub fn field_text(&self, node: super::NodeId) -> Option<&str> {
            let ws = &self.driver.windows[0];
            ws.text_edits.get(node).map(|b| b.text.as_str())
        }

        /// The selection of the focused text control in the launch window, as
        /// a byte range of its text.
        pub fn focused_selection(&self) -> Option<(usize, usize)> {
            let ws = &self.driver.windows[0];
            let node = ws.focused_text_node()?;
            let (start, end) = ws.text_edits.get(node)?.sel.logical_range();
            Some((start.0, end.0))
        }

        /// The edit menu last pushed to the platform for the launch window,
        /// while it shows.
        pub fn edit_menu(&self) -> Option<viso_platform::EditMenu> {
            self.driver.windows[0].edit_menu
        }

        /// The magnifier last pushed to the platform for the launch window,
        /// while it shows.
        pub fn loupe(&self) -> Option<viso_platform::Loupe> {
            self.driver.windows[0].loupe
        }

        /// The context menu the launch window last popped up, until it
        /// closes or the field takes its request back.
        pub fn context_menu(&self) -> Option<viso_platform::EditMenu> {
            self.driver.windows[0].context_menu
        }

        /// The menu of edits the launch window draws itself, while it
        /// shows: whether it is the context menu (else the touch bar), its
        /// panel's box, and each item's box and whether it applies — Cut,
        /// Copy, Paste, Select All — in window space.
        #[allow(clippy::type_complexity)]
        pub fn drawn_menu(&self) -> Option<(bool, viso_ui::Rect, Vec<(viso_ui::Rect, bool)>)> {
            let ws = &self.driver.windows[0];
            ws.drawn_menu.seen(&ws.store)
        }

        /// The tray icon the session last handed to the platform.
        pub fn tray(&self) -> Option<&crate::shell::Tray> {
            self.driver.shell.tray.as_ref()
        }

        /// The badge the session last handed to the platform.
        pub fn badge(&self) -> Option<&crate::shell::AppBadge> {
            self.driver.shell.badge.as_ref()
        }

        /// The pointer cursor last pushed to the platform for the launch
        /// window.
        pub fn cursor(&self) -> viso_platform::CursorIcon {
            self.driver.windows[0].cursor
        }

        /// The text entry last pushed to the platform for the launch window:
        /// `None` before any push, `Some(None)` once pushed clear.
        pub fn text_entry(&self) -> Option<Option<viso_platform::TextEntry>> {
            self.driver.windows[0].text_entry
        }

        /// The launch window's last frame as the software raster drew it:
        /// its size in device pixels and its BGRA bytes; `None` without a
        /// surface.
        #[cfg(feature = "headless")]
        pub fn pixels(&self) -> Option<(u32, u32, Vec<u8>)> {
            let gpu = self.driver.windows[0].gpu.as_ref()?;
            let surface = gpu.surface?;
            let (w, h) = gpu.size;
            Some((w, h, gpu.backend.read_pixels_bgra8(surface)))
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
        /// [`viso_ui::snapshot_ui`]. This is the
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
        drive_host::<A>(script, step, true, None, None)
    }

    thread_local! {
        pub(crate) static ALT_ACCESS: std::cell::Cell<Option<bool>> =
            const { std::cell::Cell::new(None) };
    }

    /// Make Alt reach the access keys and the menu bar on this thread's
    /// drives as it does off Apple systems (`true`), as on them (`false`),
    /// or as this system does (`None`).
    pub fn set_alt_access(on: Option<bool>) {
        ALT_ACCESS.with(|a| a.set(on));
    }

    /// The in-memory store [`keep_window_places`] keeps windows' places in.
    pub type WindowPlaces = viso_view::persist::MemoryStore;

    /// Keeps the places of the windows this thread's drives open in a fresh
    /// in-memory store, returned so a test reads what was stored, and drives
    /// again on it as a later launch would.
    pub fn keep_window_places() -> WindowPlaces {
        let memory = viso_view::persist::MemoryStore::default();
        crate::places::install(Some(viso_view::persist::SharedStore::new(memory.clone())));
        memory
    }

    /// [`drive_scripted`] on a host offering `caps`.
    pub fn drive_scripted_with_caps<A: Application>(
        script: Vec<viso_platform::RawEvent>,
        step: Duration,
        caps: viso_platform::PlatformCaps,
    ) -> DrivenApp<A> {
        let mut app = Box::new(viso_platform::backend::headless::HeadlessApp::scripted(
            script,
        ));
        app.set_capabilities(caps);
        let driver = AppDriver::<A>::new(Some(crate::services::Services::unsupported()));
        let clock = FixedStepClock::new(Instant::now(), step);
        let driver = Scheduler::with_clock(app, driver, clock).run_returning();
        DrivenApp { driver }
    }

    /// [`drive_scripted`] bounded to `frames` frames, for an app that never
    /// stops asking for them (a running game); each scripted event lands
    /// before the frames queued behind it.
    pub fn drive_scripted_frames<A: Application>(
        script: Vec<viso_platform::RawEvent>,
        step: Duration,
        frames: u32,
    ) -> DrivenApp<A> {
        drive_host::<A>(script, step, true, None, Some(frames))
    }

    /// [`drive_scripted_frames`] on a device whose gamepads are `pads`: each
    /// step queued on them lands at one frame's poll.
    pub fn drive_gamepad_frames<A: Application>(
        script: Vec<viso_platform::RawEvent>,
        pads: viso_platform::backend::headless::HeadlessGamepads,
        step: Duration,
        frames: u32,
    ) -> DrivenApp<A> {
        let mut app = Box::new(viso_platform::backend::headless::HeadlessApp::scripted(
            script,
        ));
        app.set_framed_windows(true);
        app.frame_budget(frames);
        app.attach_gamepads(pads);
        let driver = AppDriver::<A>::new(Some(crate::services::Services::unsupported()));
        let clock = FixedStepClock::new(Instant::now(), step);
        let driver = Scheduler::with_clock(app, driver, clock).run_returning();
        DrivenApp { driver }
    }

    /// [`drive_scripted`] with `services` lent to every handler in place of
    /// the headless fallback's unsupported registry.
    pub fn drive_scripted_with_services<A: Application>(
        script: Vec<viso_platform::RawEvent>,
        step: Duration,
        services: crate::services::Services,
    ) -> DrivenApp<A> {
        drive_host::<A>(script, step, true, Some(services), None)
    }

    /// [`drive_scripted`] on a host whose windows fill the screen (mobile, a
    /// browser tab): no caption, content inset by the safe area.
    pub fn drive_scripted_full_screen<A: Application>(
        script: Vec<viso_platform::RawEvent>,
        step: Duration,
    ) -> DrivenApp<A> {
        drive_host::<A>(script, step, false, None, None)
    }

    /// [`drive_scripted`] recording the first window's input: the settled
    /// app and its tape.
    #[cfg(any(feature = "hot-reload", feature = "headless"))]
    pub fn drive_recording<A: Application>(
        script: Vec<viso_platform::RawEvent>,
        step: Duration,
    ) -> (DrivenApp<A>, viso_view::tape::UiTape) {
        let mut app = Box::new(viso_platform::backend::headless::HeadlessApp::scripted(
            script,
        ));
        app.set_framed_windows(true);
        let mut driver = AppDriver::<A>::new(Some(crate::services::Services::unsupported()));
        driver.recorder = Some(crate::tape::TapeRecorder::new(None));
        let clock = FixedStepClock::new(Instant::now(), step);
        let driver = Scheduler::with_clock(app, driver, clock).run_returning();
        let tape = driver
            .recorder
            .as_ref()
            .map(|r| r.tape().clone())
            .unwrap_or_default();
        (DrivenApp { driver }, tape)
    }

    /// A fresh `A` replaying `tape` as a headless run does: on its surface,
    /// each frame's input ahead of that frame, the frame clock held to the
    /// recorded times.
    #[cfg(any(feature = "hot-reload", feature = "headless"))]
    pub fn drive_tape<A: Application>(tape: viso_view::tape::UiTape) -> DrivenApp<A> {
        let mut app = Box::new(viso_platform::backend::headless::HeadlessApp::new());
        app.set_framed_windows(true);
        let surface = tape.surface;
        app.fixed_surface(
            surface.width.round() as u32,
            surface.height.round() as u32,
            f64::from(surface.scale),
        );
        let mut driver = AppDriver::<A>::new(Some(crate::services::Services::unsupported()));
        let clock = crate::tape::TapeClock::new(Instant::now());
        driver.player = Some(crate::tape::TapePlayer::new(
            tape,
            clock.clone(),
            app.feed(),
            None,
        ));
        let driver = Scheduler::with_clock(app, driver, clock).run_returning();
        DrivenApp { driver }
    }

    fn drive_host<A: Application>(
        script: Vec<viso_platform::RawEvent>,
        step: Duration,
        framed: bool,
        services: Option<crate::services::Services>,
        frames: Option<u32>,
    ) -> DrivenApp<A> {
        let mut app = Box::new(viso_platform::backend::headless::HeadlessApp::scripted(
            script,
        ));
        app.set_framed_windows(framed);
        if let Some(frames) = frames {
            app.frame_budget(frames);
        }
        let services = services.unwrap_or_else(crate::services::Services::unsupported);
        let driver = AppDriver::<A>::new(Some(services));
        let clock = FixedStepClock::new(Instant::now(), step);
        let driver = Scheduler::with_clock(app, driver, clock).run_returning();
        DrivenApp { driver }
    }

    pub use crate::text_harness::{
        LineShape, TextDraw, TextHarness, TextStats, text_commit_budget,
    };

    /// What a frame probe is shown: a window's renderer counters as of its
    /// last upload, and whether the frame about to be submitted uploaded —
    /// a frame that did not leaves the counters as they were.
    pub type FrameProbe = Box<dyn FnMut(&viso_render::FrameStats, bool)>;

    thread_local! {
        pub(crate) static FRAME_PROBE: std::cell::RefCell<Option<FrameProbe>> =
            const { std::cell::RefCell::new(None) };
    }

    /// Shows `probe` each frame this thread's drives submit, before the
    /// submit, or stops with `None`.
    pub fn probe_frames(probe: Option<FrameProbe>) {
        FRAME_PROBE.with(|p| *p.borrow_mut() = probe);
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
    /// here.
    cx: AppCx<'static>,
    /// The live windows. One `Application` drives every window, but each window
    /// owns its own retained tree, reactive state, GPU surface, animations, and
    /// timers — an independent [`WindowState`]. A desktop session has a
    /// single-digit window count, and every frame *iterates all windows* (it
    /// never keys a lookup by id on the hot path), so a linear `Vec` is the
    /// most compact, cache-friendly layout — a `HashMap` would only add a hash
    /// and a heap bucket per access with nothing to lookup against.
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
    fonts: SessionFonts,
    /// The session's service registry, lent type-erased to every window's
    /// store. Created on launch unless injected (a headless session, a test).
    services: Option<Rc<dyn Any>>,
    /// The assets the windows show, decoded once for all of them.
    assets: assets::Assets,
    /// The session's hard memory limits, checked after every frame.
    budget_watch: footprint::BudgetWatch,
    /// Whether the backend announces semantics to assistive technology
    /// ([`PlatformCaps::accessibility`](viso_platform::PlatformCaps::accessibility)),
    /// read on launch. Without a bridge no window derives deltas; the
    /// semantics tree still serves tests and the Inspector.
    accessibility: bool,
    /// The application's menu, which a captioned window draws as its menu
    /// bar where the platform shows none.
    app_menu: Option<viso_platform::Menu>,
    /// The picks of the windows' drawn menu bars, until a frame takes them.
    menu_picks: app_menu::Picks,
    /// The links the app was asked to open that it has no window for yet.
    pending_urls: Vec<String>,
    /// What the session shows in the desktop shell.
    shell: shell::Shown,
    /// The PDF documents being written, each in the window it is built in.
    #[cfg(feature = "pdf")]
    pdf_jobs: Vec<(WindowId, pdf::Running)>,
    /// The windows whose size and place are kept across launches.
    places: places::Places,
    /// The development session: the mounted views, their watched files and
    /// the reloads staged for the next frame.
    #[cfg(feature = "hot-reload")]
    hot_reload: hot_reload::HotReloadSession,
    /// The UI scenario a headless run plays.
    #[cfg(feature = "headless")]
    script: Option<scenario::ScriptRunner>,
    /// The view a headless run mounts alone in place of the app's.
    #[cfg(feature = "headless")]
    harness: headless::Harness,
    /// The capture of the app's frames a tool asked for, if any.
    #[cfg(any(feature = "hot-reload", feature = "headless"))]
    profiler: profile::Profiler,
    /// The first window's input, recorded while the launch names a tape.
    #[cfg(any(feature = "hot-reload", feature = "headless"))]
    recorder: Option<tape::TapeRecorder>,
    /// The tape a headless run replays.
    #[cfg(any(feature = "hot-reload", feature = "headless"))]
    player: Option<tape::TapePlayer>,
    /// The gamepads connected, in connection order.
    gamepads: Vec<viso_runtime::GamepadId>,
}

/// The fonts an app registered at init, installed into every window's shaper.
#[derive(Default)]
struct SessionFonts {
    /// Raw sfnt faces from `AppCx::load_font`, in call order.
    loaded: Vec<Box<[u8]>>,
    /// Faces packaged from `assets/fonts/` via `AppCx::use_packaged_fonts`.
    packaged: fonts::PackagedFonts,
}

/// Everything one window owns: its retained tree, reactive state, GPU surface,
/// live animations/timers, and the reusable scratch the frame hot path drains
/// into. The session-scope `Application` and `AppCx` stay on [`AppDriver`]; each
/// window holds an independent instance of everything below, so closing a window
/// drops exactly its tree and GPU resources (its `EffectStore` cancelled first,
/// so scoped effects release their resources — cleanup then drop).
/// How long a text field's caret shows, and then hides, while it blinks.
const CARET_BLINK: std::time::Duration = std::time::Duration::from_millis(530);

/// How long after it last moved a caret stops blinking and shows steadily, so
/// an idle field wakes no frames.
const CARET_IDLE: std::time::Duration = std::time::Duration::from_secs(10);

/// A focused text field as its caret last stood: the field, its text's
/// revision and its selection.
#[derive(Debug, Clone, Copy, PartialEq)]
struct FieldKey {
    node: NodeId,
    revision: viso_text::ime::Revision,
    sel: viso_text::selection::Selection,
}

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
    /// nothing (the back-channel is a cold path). Empty until a
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
    safe_area: LogicalInsets,
    keyboard_inset: f64,
    /// The accessibility tree published to the OS, present only while an
    /// assistive technology listens to this window.
    access: Option<accessibility::AccessBridge>,
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
    /// range) from a node column without a cross-layer read.
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
    text_entry: Option<Option<TextEntry>>,
    /// The edit menu last pushed to the platform, while it shows.
    edit_menu: Option<viso_platform::EditMenu>,
    /// The context menu last asked for, while the field asks for it.
    context_menu: Option<viso_platform::EditMenu>,
    /// The menus of edits the window draws where the platform draws none.
    drawn_menu: edit_menu::DrawnMenu,
    /// The magnifier last pushed to the platform, while it shows.
    loupe: Option<viso_platform::Loupe>,
    /// The cursor last pushed to the platform for the mouse.
    cursor: viso_platform::CursorIcon,
    /// The text field drawn with the focus last frame.
    marked: Option<NodeId>,
    /// The focused field's text and selection when its caret last moved, and
    /// when: its caret blinks from then.
    blink: Option<(FieldKey, Instant)>,
    /// The focused field's text and selection it last scrolled to show.
    revealed: Option<FieldKey>,
    /// When the caret next blinks, while it blinks.
    caret_due: Option<Instant>,
    /// When the next frame of an animated image playing in the window is due.
    animation_due: Option<Instant>,
    /// The animated images playing in the window.
    players: playback::Players,
    /// The players of the window's video nodes.
    videos: videos::Videos,
    /// What the videos' last step left: whether one plays on screen, and
    /// when one is next polled.
    video_step: videos::Step,
    /// Whether the window is the active one, taking the keys.
    active: bool,
    /// The marks a field drew before the ones it was just given, kept so
    /// marking a field each frame allocates nothing.
    marks: viso_ui::FieldMarks,
    /// Reusable ancestry buffer the pointer router fills each event, owned here
    /// so routing a pointer allocates nothing on the steady path.
    route_chain: Vec<NodeId>,
    /// Whether Alt is down with nothing pressed since: let go, it asks for
    /// the menu bar.
    alt_alone: bool,
    /// What happens in the window while an inspector follows it; `None`
    /// while none does.
    #[cfg(any(feature = "hot-reload", feature = "headless"))]
    journal: Option<Box<inspect::Journal>>,
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
    /// The failed-reload panel drawn over the tree, while an edit of a view
    /// this window mounts does not compile.
    #[cfg(feature = "hot-reload")]
    dev_overlay: Option<NodeId>,
    /// The frame time drawn over the tree, while a capture asks for it.
    #[cfg(feature = "hot-reload")]
    dev_hud: Option<NodeId>,
    /// How much each layer recomputed on the most recent frame — surfaced for
    /// diagnostics and asserted by tests to confirm only the dirty subtree moved.
    recompute: FrameRecompute,
    /// What this frame's paint did to `primitives`, for its upload.
    painted: viso_ui::Painted,
    /// Whether this frame's upload phase wrote primitives to the renderer.
    uploaded: bool,
    /// The measure/layout counts of the frame in progress, folded into
    /// `recompute` when its paint phase runs.
    laid: (u32, u32),
    /// How far into this frame's STYLE worklist the style phase resolved.
    style_seen: usize,
    /// True until the first frame has been submitted. Lets the Submit phase emit
    /// a one-shot diagnostic (gated on `VISO_DEBUG`) proving the first
    /// frame reached the GPU, then fall dark for every steady-state frame.
    awaiting_first_frame: bool,
    /// The revision of the store's asset table the app last served: a frame
    /// that moved nothing in it serves nothing.
    assets_served: u64,
    /// What the window's renderer holds of the programs its nodes paint
    /// with.
    shaders: shaders::InstalledShaders,
    /// The instant of the window's first submitted frame, which the clock
    /// its brushes read counts from.
    clock_origin: Option<viso_runtime::Instant>,
    /// Whether the frame last uploaded draws a brush reading the clock, which
    /// keeps the window drawing frames, as an animation does.
    draws_clock: bool,
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
    /// Reusable buffer the flush polls this window's woken tasks into: one
    /// entry per task that finished, its continuation if it writes back.
    continuations: Vec<Option<Continuation>>,
    /// The pastes asked of the platform and not yet answered, by token: the
    /// node each goes to, or the focused text field (`None`).
    pastes: Vec<(u32, Option<NodeId>)>,
    /// The token the next paste request carries.
    next_paste: u32,
    /// Reusable buffer the flush drains this window's paste asks into.
    scratch_pastes: Vec<(NodeId, viso_ui::ClipboardKinds)>,
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

/// The loop ended — the last window closed, or a quit broke the pump with
/// windows still open: what those views persisted is made durable before
/// their stores go.
impl<A: Application> Drop for AppDriver<A> {
    fn drop(&mut self) {
        self.places.flush();
        for ws in &mut self.windows {
            persist::suspend(&mut ws.store, &ws.states);
        }
    }
}

impl<A: Application> AppDriver<A> {
    /// Hand the app each link it was asked to open, in order, once it has a
    /// window: with the launch window's context placed at its root, applying
    /// what the app asked of the tree as a handler's requests are, and
    /// framing the window.
    fn open_pending_urls(&mut self, cx: &mut RuntimeCx<'_>) {
        let (Some(app), Some(ws)) = (self.app.as_mut(), self.windows.first_mut()) else {
            return;
        };
        for url in std::mem::take(&mut self.pending_urls) {
            let mut open = |ev: &mut viso_ui::EventCx<'_>| app.on_open_url(ev, &url);
            match ws.root {
                Some(root) => {
                    KeyRouter::run_at(
                        &mut ws.store,
                        &mut ws.states,
                        &ws.bindings,
                        &mut ws.text_edits,
                        root,
                        open,
                    );
                }
                // A window that builds nothing has no tree to ask anything
                // of: only its state is there.
                None => open(&mut viso_ui::EventCx::__new(&mut ws.states, &ws.bindings)),
            }
        }
        cx.request_redraw(ws.window);
    }

    /// Do what the drawn menu bars' picks ask: a command reaches the app,
    /// a standard action is done as the system's menu would.
    fn take_menu_picks(&mut self, cx: &mut RuntimeCx<'_>) {
        let picks = std::mem::take(&mut *self.menu_picks.borrow_mut());
        for (window, pick) in picks {
            match pick {
                app_menu::Pick::Command(command) => {
                    if let Some(app) = self.app.as_mut() {
                        app.on_menu_command(command);
                    }
                }
                app_menu::Pick::System(viso_platform::SystemAction::Quit) => {
                    for ws in &self.windows {
                        cx.close_window(ws.window);
                    }
                }
                app_menu::Pick::System(viso_platform::SystemAction::CloseWindow) => {
                    cx.close_window(window);
                }
                app_menu::Pick::System(action) => {
                    let edit = match action {
                        viso_platform::SystemAction::Copy => edit_menu::Edit::Copy,
                        viso_platform::SystemAction::Cut => edit_menu::Edit::Cut,
                        viso_platform::SystemAction::Paste => edit_menu::Edit::Paste,
                        _ => continue,
                    };
                    if let Some(ws) = self.window_mut(window) {
                        ws.apply_menu_edit(cx, edit, edit_menu::MenuLook::List);
                    }
                }
            }
        }
    }

    fn new(services: Option<services::Services>) -> Self {
        footprint::install(A::memory_budget());
        // A headless build runs off-loop jobs inline, to completion each
        // frame, so what a run shows never depends on the machine's speed.
        viso_ui::work::configure(if cfg!(feature = "headless") {
            0
        } else {
            footprint::workers()
        });
        shell::begin();
        Self {
            app: None,
            cx: AppCx::__new(),
            windows: Vec::new(),
            pending_opens: Vec::new(),
            pending_closes: Vec::new(),
            fonts: SessionFonts::default(),
            services: services.map(|s| Rc::new(s) as Rc<dyn Any>),
            assets: assets::Assets::with_loader(A::image_loader()),
            budget_watch: footprint::BudgetWatch::default(),
            accessibility: true,
            app_menu: None,
            menu_picks: Default::default(),
            pending_urls: Vec::new(),
            shell: shell::Shown::default(),
            #[cfg(feature = "pdf")]
            pdf_jobs: Vec::new(),
            places: places::Places::default(),
            #[cfg(feature = "hot-reload")]
            hot_reload: hot_reload::HotReloadSession::default(),
            #[cfg(feature = "headless")]
            script: None,
            #[cfg(feature = "headless")]
            harness: headless::Harness::default(),
            #[cfg(any(feature = "hot-reload", feature = "headless"))]
            profiler: profile::Profiler::default(),
            #[cfg(any(feature = "hot-reload", feature = "headless"))]
            recorder: viso_platform::launch_var(viso_view::tape::RECORD_ENV)
                .map(|path| tape::TapeRecorder::new(Some(path.into()))),
            #[cfg(any(feature = "hot-reload", feature = "headless"))]
            player: None,
            gamepads: Vec::new(),
        }
    }

    /// Hands the first window to the tape being recorded or replayed.
    #[allow(unused_variables)]
    fn adopt_tape(&mut self, ws: &WindowState) {
        #[cfg(any(feature = "hot-reload", feature = "headless"))]
        if let Some(recorder) = &mut self.recorder {
            #[cfg(feature = "hot-reload")]
            let code = self.hot_reload.code();
            #[cfg(not(feature = "hot-reload"))]
            let code = None;
            recorder.adopt(ws.window, ws.dpi, ws.surface_size, code);
        }
        #[cfg(any(feature = "hot-reload", feature = "headless"))]
        if let Some(player) = &mut self.player {
            player.adopt(ws.window);
        }
    }

    /// Moves each PDF document being written on by a frame, taking those
    /// asked for since the last; each is built in the first window.
    #[cfg(feature = "pdf")]
    fn drive_pdfs(&mut self, cx: &mut RuntimeCx<'_>) {
        for (pdf, done) in pdf::take_asked() {
            let job = pdf::Running::new(pdf, done);
            match self.windows.first() {
                Some(ws) => self.pdf_jobs.push((ws.window, job)),
                None => job.fail(services::ServiceError::Unsupported),
            }
        }
        let windows = &mut self.windows;
        self.pdf_jobs.retain_mut(|(window, job)| {
            match windows.iter_mut().find(|ws| ws.window == *window) {
                Some(ws) => {
                    let finished = job.step(ws);
                    if !finished {
                        cx.request_redraw(ws.window);
                    }
                    !finished
                }
                None => false,
            }
        });
    }

    /// Checks the frame against the session's hard limits: over one, every
    /// window drops what the next frame can rebuild; still over after that,
    /// the excess is reported once. The device is only measured under a GPU
    /// limit.
    fn watch_budget(&mut self, cx: &mut RuntimeCx<'_>) {
        let budget = footprint::budget();
        let mut used = footprint::Usage {
            heap: alloc::heap().map(|h| h.live),
            ..footprint::Usage::default()
        };
        for ws in &self.windows {
            used.images += ws.store.assets().texture_bytes();
            if budget.gpu.is_some() {
                used.gpu += ws.device_bytes();
            }
        }
        match self.budget_watch.check(budget, used) {
            footprint::Verdict::Within => {}
            footprint::Verdict::Trim => {
                for ws in &mut self.windows {
                    ws.store.request_memory_trim(&mut ws.states);
                    ws.trim_memory();
                    cx.request_redraw(ws.window);
                }
            }
            footprint::Verdict::Exceeded(exceeded) => eprintln!("[viso] {exceeded}"),
        }
    }

    /// Keeps what each window holds of the images no node shows within the
    /// images budget, less what the other windows' textures take: the least
    /// recently shown go first.
    fn sweep_assets(&mut self) {
        let budget = footprint::budget().images;
        let total: u64 = self
            .windows
            .iter()
            .map(|ws| ws.store.assets().texture_bytes())
            .sum();
        for ws in &mut self.windows {
            let own = ws.store.assets().texture_bytes();
            let allowance = budget.saturating_sub(total - own);
            let evicted = match ws.gpu.as_mut() {
                Some(gpu) => ws.store.assets_mut().sweep(allowance, |texture| {
                    gpu.renderer.release_texture(&mut gpu.backend, texture);
                }),
                // Without a device no texture is resident to release.
                None if own == 0 => ws.store.assets_mut().sweep(allowance, |_| {}),
                None => 0,
            };
            self.assets.evicted(evicted);
        }
        let windows = &self.windows;
        self.assets
            .playback
            .forget(|id| windows.iter().any(|ws| ws.store.assets().knows(id)));
    }

    /// Writes each window's animated images' frames that came due at `now`
    /// and records when the next is.
    fn play_animations(&mut self, now: Instant, waker: &dyn Fn() -> viso_platform::LoopWaker) {
        let playback = &mut self.assets.playback;
        playback.receive();
        for ws in &mut self.windows {
            ws.animation_due = None;
            let (w, h) = ws.logical_surface();
            let Some(gpu) = ws.gpu.as_mut() else {
                continue;
            };
            let viewport = Rect {
                x: 0.0,
                y: 0.0,
                w,
                h,
            };
            let still = ws.states.env().environment().reduced_motion;
            ws.animation_due = ws.players.step(
                &mut ws.store,
                &mut gpu.backend,
                playback,
                now,
                viewport,
                still,
                waker,
            );
        }
    }

    /// Steps each window's videos at `now`: their players opened, closed and
    /// steered, their pictures written.
    fn play_videos(&mut self, now: Instant) {
        for ws in &mut self.windows {
            if ws.videos.is_empty() && ws.store.videos().is_empty() {
                ws.video_step = videos::Step::default();
                continue;
            }
            let (w, h) = ws.logical_surface();
            let viewport = Rect {
                x: 0.0,
                y: 0.0,
                w,
                h,
            };
            let gpu = ws.gpu.as_mut().map(|g| (&mut g.backend, &mut g.renderer));
            ws.video_step = ws.videos.step(&mut ws.store, gpu, now, viewport);
        }
    }

    /// The live window state for `id`, by linear scan. Windows number in the
    /// single digits, so this is cheaper than a keyed map.
    fn window_mut(&mut self, id: WindowId) -> Option<&mut WindowState> {
        self.windows.iter_mut().find(|w| w.window == id)
    }
}

impl WindowState {
    /// Shows the cursor the node under the mouse names; the platform hears of
    /// it only when it changes.
    fn push_cursor(&mut self, cx: &mut RuntimeCx<'_>) {
        let cursor = self.store.mouse_cursor();
        if cursor != self.cursor {
            self.cursor = cursor;
            cx.set_cursor(self.window, cursor);
        }
    }

    /// Hands the platform the pointer lock a node asked for, or its release;
    /// a platform without pointer lock answers the ask with the lock ended.
    fn push_pointer_lock(&mut self, cx: &mut RuntimeCx<'_>) {
        let Some(lock) = self.store.take_pointer_lock_ask() else {
            return;
        };
        if cx.capabilities().pointer_lock {
            cx.set_pointer_lock(self.window, lock);
        } else if lock {
            KeyRouter::route_pointer_lock(
                &mut self.store,
                &mut self.states,
                &self.bindings,
                &mut self.text_edits,
                false,
            );
        }
    }

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
            access: None,
            safe_area_root: None,
            safe_area: LogicalInsets::default(),
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
            text_entry: None,
            edit_menu: None,
            context_menu: None,
            drawn_menu: edit_menu::DrawnMenu::new(),
            loupe: None,
            cursor: viso_platform::CursorIcon::Default,
            marked: None,
            blink: None,
            revealed: None,
            caret_due: None,
            animation_due: None,
            players: playback::Players::default(),
            videos: videos::Videos::default(),
            video_step: videos::Step::default(),
            active: true,
            marks: viso_ui::FieldMarks::default(),
            route_chain: Vec::new(),
            alt_alone: false,
            #[cfg(any(feature = "hot-reload", feature = "headless"))]
            journal: None,
            edited: Vec::new(),
            edited_text: String::new(),
            primitives: Vec::new(),
            scratch: Vec::new(),
            redo_roots: Vec::new(),
            #[cfg(feature = "hot-reload")]
            dev_overlay: None,
            #[cfg(feature = "hot-reload")]
            dev_hud: None,
            recompute: FrameRecompute::default(),
            painted: viso_ui::Painted::Nothing,
            uploaded: false,
            laid: (0, 0),
            style_seen: 0,
            awaiting_first_frame: true,
            assets_served: 0,
            shaders: shaders::InstalledShaders::default(),
            clock_origin: None,
            draws_clock: false,
            text: None,
            text_scratch: Vec::new(),
            text_sources: std::collections::HashMap::new(),
            text_updated: Vec::new(),
            reflow_scratch: Vec::new(),
            scratch_opens: Vec::new(),
            scratch_closes: Vec::new(),
            continuations: Vec::new(),
            pastes: Vec::new(),
            next_paste: 0,
            scratch_pastes: Vec::new(),
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
    #[allow(clippy::too_many_arguments)]
    fn open(
        cx: &mut RuntimeCx<'_>,
        window: WindowId,
        fonts: &SessionFonts,
        gamepads: bool,
        services: Option<Rc<dyn Any>>,
        chrome: WindowChrome,
        initial_chrome_geom: Option<LogicalRect>,
        build: impl FnOnce(&mut BuildCx) -> Option<NodeId>,
    ) -> Self {
        let mut ws = WindowState::new(window);
        // Tasks a handler spawns wake the loop through its waker, and handlers
        // reach the session's services through the store.
        ws.store.__install_task_host(cx.loop_waker(), services);
        // Record who draws this window's chrome. Known at open time from the
        // window config; the build-time half of the chrome data contract a
        // caption widget reads through `BuildCx::chrome` (driven by data,
        // not `target_os`).
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
        ws.follow_scale_factor();
        ws.seed_environment(cx.appearance(), &cx.locale(), &cx.display_features(window));
        environment::note_gamepads(&mut ws.states, gamepads);

        // Bring up the GPU for this window when it exposes a real windowing
        // handle: create the device, attach a surface to that handle, and build
        // the renderer for its format. A headless window reports a
        // `RawWindowHandle::Headless` (or no handle at all) — there is no
        // surface to attach, and on a real-GPU target such as Metal
        // `create_surface` would reject the non-native handle — so we stay
        // `gpu = None` for the session and draw nothing. The retained tree below
        // is built and driven regardless; this is the headless backend as a
        // first-class path, not a degraded one.
        let native_handle = match cx.raw_handle(window) {
            // A headless build draws every window through the software raster,
            // which takes the headless handle.
            Some(RawWindowHandle::Headless) if cfg!(feature = "headless") => {
                Some(RawWindowHandle::Headless)
            }
            Some(RawWindowHandle::Headless) | None => None,
            Some(handle) => Some(handle),
        };
        // A browser without WebGPU keeps the window without a GPU surface.
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        let native_handle = native_handle.filter(|_| viso_gpu::webgpu::is_prepared());
        if let Some(raw) = native_handle {
            let mut backend = viso_gpu::create_device();
            #[cfg(not(feature = "headless"))]
            backend.set_cpu_workers(footprint::workers());
            let surface = backend.create_surface(raw, w.max(1), h.max(1));
            // Both the format and the color space come from the surface, so the
            // renderer's intermediate targets are planned for the domain this
            // window actually composites in.
            let renderer = footprint::renderer(&mut backend, surface);

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
            shaper.use_packaged_fonts(fonts.packaged);
            for face in &fonts.loaded {
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
    /// `Content::Text` payload, uploading newly-packed glyphs to the atlas,
    /// and place again each run whose size moved with its font sources or the
    /// environment. Runs after a build/rebuild and before layout, so a `Fit`
    /// text node measures against the shaped run's natural size. A no-op with
    /// no GPU (headless-without-surface) or no shaper (pre-launch).
    fn shape_pending_text(&mut self) {
        let (Some(gpu), Some(text)) = (self.gpu.as_mut(), self.text.as_mut()) else {
            return;
        };
        self.store.take_text_requests(&mut self.text_scratch);
        let refamilied = text.set_family(self.store.text_family());
        let resized = self.store.take_text_resized() || refamilied;
        if self.text_scratch.is_empty() && !resized {
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
            let sizing = self.store.text_sizing(id, &request);
            let width = text.box_width(slot);
            let content = text.shape(&mut gpu.backend, slot, &request, sizing, dpi, width);
            // Every run keeps its source so a reflow or a memory trim can
            // reshape it (the store drops the request on drain). Re-declaring a
            // run (edit/rebuild) overwrites its entry; the reflow drain and the
            // trim prune freed nodes.
            self.text_sources.insert(id, request);
            self.store.set_content_payload(id, content);
        }
        if !resized {
            return;
        }
        // Shaping is in em, so a run whose size moved is only placed again; a
        // run of the default family shapes anew when that family moved.
        for (&id, request) in &self.text_sources {
            let slot = ParagraphSlot::from(id);
            let sizing = self.store.text_sizing(id, request);
            let refaced = refamilied && request.style.family.is_none();
            if text.sizing(slot) == Some(sizing) && !refaced {
                continue;
            }
            let width = text.box_width(slot);
            let content = text.shape(&mut gpu.backend, slot, request, sizing, dpi, width);
            self.store.set_reflowed_content(id, content);
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
        // A headless build commits everything the worker finished, so the
        // frames a run takes never depend on how fast the machine is.
        let budget = if cfg!(feature = "headless") {
            std::time::Duration::MAX
        } else {
            text_content::text_commit_budget(text_content::DEFAULT_FRAME_INTERVAL)
        };
        text.pump(&mut gpu.backend, budget, &mut self.text_updated);
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
            let sizing = self.store.text_sizing(id, request);
            let width = text.box_width(slot);
            let content = text.shape(&mut gpu.backend, slot, request, sizing, dpi, width);
            // The request is unchanged, so its accessible name is too.
            self.store.set_reflowed_content(id, content);
        }
    }

    /// Serve `bytes` for the packaged font file `file` from now on and shape
    /// again each paragraph that laid out with a face of it, pushing its node
    /// to `reshaped`. Until the new face's layout is committed a paragraph
    /// keeps drawing its last good one.
    #[cfg(feature = "hot-reload")]
    fn replace_packaged_font(
        &mut self,
        file: u32,
        bytes: std::sync::Arc<[u8]>,
        reshaped: &mut Vec<NodeId>,
    ) {
        let (Some(gpu), Some(text)) = (self.gpu.as_mut(), self.text.as_mut()) else {
            return;
        };
        let dpi = self.dpi;
        for slot in text.replace_packaged(file, bytes) {
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
            let sizing = self.store.text_sizing(id, request);
            let width = text.box_width(slot);
            let content = text.shape(&mut gpu.backend, slot, request, sizing, dpi, width);
            self.store.set_reflowed_content(id, content);
            reshaped.push(id);
        }
    }

    /// Hand the text worker every layout this frame asked for, in one batch.
    fn dispatch_text_work(&mut self) {
        if let Some(text) = self.text.as_mut() {
            text.dispatch();
        }
    }

    /// Whether a node of the window is mid-slide or a look property mid-move.
    fn is_animating(&self) -> bool {
        !self.animations.is_empty()
            || self.store.is_transitioning()
            || self.store.is_scroll_moving()
            || self.store.is_spinning()
            || self.store.is_shimmering()
            || self.store.is_rippling()
            || self.store.is_progressing()
            || self.store.is_sliding()
            || self.store.refreshes_due()
            || self.store.snaps_due()
            || self.store.row_gestures_due()
            || self.store.sheets_due()
            || self.store.drops_due()
            || self.store.frame_clients_want_frame()
            || self.video_step.beating
            || self.draws_clock
    }

    /// Whether the text worker still owes results a later frame commits.
    fn text_work_pending(&self) -> bool {
        self.text.as_ref().is_some_and(TextShaper::has_pending_work)
    }

    /// Tell the nodes the focus moved between, and settle the edits what
    /// they did about it asked for.
    fn tell_focus(&mut self) {
        if KeyRouter::route_focus(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            &mut self.text_edits,
        ) && self.store.has_edit_requests()
        {
            self.settle_text_edits();
        }
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

    /// The bytes this window holds on the device: the renderer's buffers,
    /// ramps, masks and targets, the glyph atlas planes and the image
    /// textures. Cold: it reads the pools' sizes.
    fn device_bytes(&self) -> u64 {
        let Some(gpu) = &self.gpu else {
            return 0;
        };
        let r = gpu.renderer.resources();
        r.buffer_bytes
            + r.gradient_lut_bytes
            + r.mask_page_bytes
            + r.transient_bytes
            + self.text.as_ref().map_or(0, TextShaper::atlas_bytes)
            + self.store.assets().texture_bytes()
    }

    /// Give back every GPU cache the next frame can rebuild: idle pooled
    /// targets, and every glyph page no mounted text draws from. Unmounted
    /// text is pruned first so it holds no page. Mounted text is then
    /// reshaped, which re-admits anything a shed page held — none of its own
    /// pages is shed, so that costs no raster and nothing on screen goes
    /// missing.
    fn trim_memory(&mut self) {
        let (Some(gpu), Some(text)) = (self.gpu.as_mut(), self.text.as_mut()) else {
            return;
        };
        let arena = self.store.arena();
        self.text_sources.retain(|id, _| arena.is_live(*id));
        text.retain_paragraphs(|slot| {
            arena.live_id(slot.index()).map(ParagraphSlot::from) == Some(slot)
        });
        gpu.renderer.trim_caches(&mut gpu.backend);
        text.trim(|texture| gpu.renderer.release_texture(&mut gpu.backend, texture));
        self.store
            .assets_mut()
            .evict_unshown(|texture| gpu.renderer.release_texture(&mut gpu.backend, texture));
        for (&id, request) in &self.text_sources {
            self.store.set_text_request(id, (**request).clone());
        }
        self.shape_pending_text();
    }

    /// Resolves `px` lengths against the window's current scale factor.
    fn follow_scale_factor(&mut self) {
        let env = self.store.length_env();
        self.store.set_length_env(viso_ui::LengthEnv {
            scale_factor: self.dpi.max(1.0),
            ..env
        });
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

    /// Every render phase at once, outside the frame loop: style, layout,
    /// transforms and paint — what a test of a commit landing between frames
    /// shows its result with.
    #[cfg(all(test, feature = "hot-reload"))]
    fn relayout_and_paint(&mut self) {
        self.resolve_style();
        self.relayout();
        self.update_transforms();
        self.repaint();
    }

    /// The `ResolveStyle` phase: re-select interaction-state boxes when a
    /// STYLE class is pending. A press/hover flip marks its node STYLE (and
    /// PAINT) through the flush; this folds the winning variant (pressed >
    /// hover > resting) into the warm `style` the measure and paint passes
    /// read. Theme-free (it reads only the reactive cells), so it runs in the
    /// live loop where the theme-token `resolve_styles` pass does not yet. The
    /// STYLE worklist is empty on a frame with no interaction change, which
    /// then pays one length check.
    ///
    /// It resolves only what gained STYLE since it last ran this frame, so the
    /// paint phase calls it again to pick up a node a virtual list mounted
    /// during layout.
    fn resolve_style(&mut self) {
        let listed = self.store.dirty_lists().of(DirtyClass::STYLE).len();
        // A clear outside the frame loop emptied the lists under the cursor.
        if listed < self.style_seen {
            self.style_seen = 0;
        }
        if listed > self.style_seen {
            let _style = profile::time(Span::Style);
            let (_, seen) = self
                .store
                .resolve_interaction_styles_since(&self.states, self.style_seen);
            self.style_seen = seen;
        }
    }

    /// The `Measure` and `Layout` phases: re-measure and re-place the dirty
    /// subtrees, then settle the adaptive environment against the boxes just
    /// placed. Counts accumulate into the frame's recompute until its paint.
    fn relayout(&mut self) {
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
        let _layout = profile::time(Span::Layout);
        // Anchored layers stay out of what the system covers.
        let env = self.states.env().environment();
        let mut keep_out = env.safe_area;
        keep_out.bottom = keep_out.bottom.max(env.keyboard_inset);
        let rtl = env.layout_direction == viso_ui::adaptive::LayoutDirection::Rtl;
        self.store.set_keep_out(keep_out, rtl);
        let (mut measured, mut laid_out) =
            self.store
                .relayout_dirty(root, surface, &mut self.scratch, &mut self.redo_roots);
        // Settle the adaptive environment against the boxes just placed: an
        // anchor whose constraints or size class moved wakes its readers, their
        // writes flush, and the tree lays out again until nothing moves. A frame
        // that placed nothing and changed no environment input resolves nothing.
        let (scratch, redo_roots) = (&mut self.scratch, &mut self.redo_roots);
        let settled = viso_ui::settle_adaptive(
            &mut self.store,
            &mut self.states,
            &mut self.bindings,
            &mut self.computeds,
            &mut self.projectors,
            &mut self.effects,
            &mut self.virtual_lists,
            &mut self.changed,
            measured + laid_out > 0,
            |store| {
                let (m, l) = store.relayout_dirty(root, surface, scratch, redo_roots);
                measured += m;
                laid_out += l;
                m + l > 0
            },
        );
        match settled {
            // A settle that flushed may have flipped an interaction cell of a
            // node resolved earlier this frame, which stays listed: resolve
            // the whole STYLE worklist again before paint reads the box.
            Ok(rounds) if rounds > 0 => self.style_seen = 0,
            Ok(_) => {}
            Err(cycle) => eprintln!("[viso] {cycle}"),
        }
        for warning in self.store.take_length_warnings() {
            eprintln!(
                "[viso] {}: node {:?}: {:?}",
                warning.issue.code(),
                warning.node,
                warning.issue
            );
        }
        self.laid.0 += measured;
        self.laid.1 += laid_out;
    }

    /// The `UpdateTransforms` phase: recompute the transform roots' places
    /// when a scroll, translate or turn is pending, or a turned node's box
    /// moved — one compose per root, no node visited. A frame that moved
    /// nothing skips it.
    fn update_transforms(&mut self) {
        // A scroll or a translate moved anchors without a layout: the layers
        // anchored to them follow.
        if self.root.is_some() && self.store.transforms_pending() {
            self.store.follow_anchors();
        }
        if self.root.is_some() && self.store.transforms_pending() {
            let _layout = profile::time(Span::Layout);
            self.store.update_transforms();
        }
    }

    /// The `BuildPaintChanges` phase: re-emit the primitives when a
    /// paint-affecting class is pending, and close the frame's recompute
    /// counts.
    fn repaint(&mut self) {
        self.paint(false);
    }

    /// Re-emit the primitives — when a paint-affecting class is pending, or
    /// whatever is dirty when `force`d — and close the frame's recompute counts.
    fn paint(&mut self, force: bool) {
        let Some(root) = self.root else {
            return;
        };
        self.resolve_style();
        let _paint = profile::time(Span::Paint);
        let painted = if force {
            viso_ui::Painted::Whole(self.store.repaint(root, &mut self.primitives))
        } else {
            self.store.repaint_dirty(root, &mut self.primitives)
        };
        // The dev panels paint after the tree, so a patch leaves theirs in
        // place.
        #[cfg(feature = "hot-reload")]
        let painted = match painted {
            viso_ui::Painted::Whole(n) => {
                let (lw, lh) = self.logical_surface();
                let surface = Rect {
                    x: 0.0,
                    y: 0.0,
                    w: lw,
                    h: lh,
                };
                viso_ui::Painted::Whole(n + hot_reload::overlay::paint(self, surface, n))
            }
            other => other,
        };
        self.painted = painted;
        if force {
            // A repaint the upload asked for: the frame's layout counts stand.
            self.recompute.painted = painted.count();
            return;
        }
        let (measured, laid_out) = core::mem::take(&mut self.laid);
        self.recompute = FrameRecompute {
            measured,
            laid_out,
            painted: painted.count(),
        };
    }

    /// The `UploadGpuChanges` phase. Only a frame that painted uploads, the
    /// primitives at the displacements they were painted at; one that only
    /// moved transform roots, advanced the clock or moved a root by less than
    /// a device pixel keeps the retained draws. Then the renderer takes the
    /// displacements as they are now, and when a moved root holds content a
    /// shift cannot move, the tree is painted again there.
    fn upload_gpu_changes(&mut self) {
        let Some(gpu) = &mut self.gpu else {
            return;
        };
        let (w, h) = gpu.size;
        gpu.renderer
            .set_surface_size([w as f32, h as f32], self.dpi.max(1.0));
        // The scenes committed since the last frame, ahead of the draws that
        // show them.
        self.store.scenes_mut().sync(|sync| match sync {
            viso_ui::SceneSync::Frame(slot, frame) => gpu.renderer.swap_scene(slot, frame),
            viso_ui::SceneSync::Released(slot) => {
                gpu.renderer.release_scene(&mut gpu.backend, slot)
            }
        });
        for model in self.store.assets_mut().take_retired_models() {
            gpu.renderer.release_scene_model(&mut gpu.backend, &model);
        }
        let painted = std::mem::replace(&mut self.painted, viso_ui::Painted::Nothing);
        self.uploaded = !matches!(
            painted,
            viso_ui::Painted::Nothing | viso_ui::Painted::Patched(0)
        );
        let whole = match painted {
            viso_ui::Painted::Nothing | viso_ui::Painted::Patched(0) => false,
            viso_ui::Painted::Whole(_) => true,
            viso_ui::Painted::Patched(_) => {
                !gpu.renderer
                    .patch(&mut gpu.backend, &self.primitives, self.store.patched())
            }
            viso_ui::Painted::Spliced(_) => {
                !gpu.renderer
                    .splice(&mut gpu.backend, &self.primitives, self.store.splices())
            }
        };
        if whole {
            gpu.renderer.upload_with_transforms(
                &mut gpu.backend,
                &self.primitives,
                self.store.transforms().places(),
            );
        }
        let update = gpu
            .renderer
            .set_transforms(self.store.transforms().places());
        if update == viso_render::TransformUpdate::NeedsUpload {
            self.uploaded = true;
            self.paint(true);
            self.painted = viso_ui::Painted::Nothing;
            if let Some(gpu) = &mut self.gpu {
                gpu.renderer.upload_with_transforms(
                    &mut gpu.backend,
                    &self.primitives,
                    self.store.transforms().places(),
                );
            }
        }
        if let Some(gpu) = &self.gpu {
            // A brush reading the clock on screen draws something new every
            // frame; one culled or gone stops the beat.
            self.draws_clock = gpu.renderer.reads_time();
        }
    }

    /// Recompute the caption's draggable regions from the just-laid-out tree and,
    /// only when they differ from the last push, hand them to the platform through
    /// `set_draggable_regions` — the cold back-channel that turns a primary press on
    /// the caption into a native window drag. A caption widget
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
    /// A fresh token for a paste that goes to `to`, or to the focused text
    /// field with `None`, kept until its answer arrives.
    fn paste_token(&mut self, to: Option<NodeId>) -> u32 {
        let token = self.next_paste;
        self.next_paste = self.next_paste.wrapping_add(1);
        // A request the platform never answered is dropped, not kept.
        if self.pastes.len() >= 64 {
            self.pastes.remove(0);
        }
        self.pastes.push((token, to));
        token
    }

    /// Where a paste gesture goes and what it takes: the focused text field
    /// takes plain text; otherwise the nearest paste target from the focus
    /// (from the root with nothing focused) takes its kinds; with none, the
    /// focused node hears plain text.
    fn paste_route(&self, root: NodeId) -> (Option<NodeId>, viso_ui::ClipboardKinds) {
        if self.focused_text_node().is_some() {
            return (None, viso_ui::ClipboardKinds::TEXT);
        }
        let from = self.store.focused().unwrap_or(root);
        match self.store.paste_target_from(from) {
            Some((node, kinds)) => (Some(node), kinds),
            None => (None, viso_ui::ClipboardKinds::TEXT),
        }
    }

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

    /// Recompute the edit menu the focused field shows and report whether it
    /// differs from what was last pushed: what it points at, in logical
    /// points and kept within the field's box, and the edits its selection
    /// takes. None shows while the window is inactive.
    fn edit_menu_changed(&mut self, with_context: bool) -> bool {
        let menu = self
            .focused_text_node()
            .filter(|_| self.active)
            .and_then(|id| {
                let marks = self.store.field_marks(id)?;
                let at = marks.menu.or(marks.context.filter(|_| with_context))?;
                self.edit_menu_at(id, at)
            });
        if self.edit_menu == menu {
            return false;
        }
        self.edit_menu = menu;
        true
    }

    /// Recompute the magnifier over the caret a finger carries in the
    /// focused field and report whether it differs from what was pushed.
    fn loupe_changed(&mut self) -> bool {
        let loupe = self
            .focused_text_node()
            .filter(|_| self.active)
            .and_then(|id| {
                let marks = self.store.field_marks(id)?;
                let (finger, caret) = marks.loupe?;
                let view = self.store.world(id);
                let (x, y) = (view.x - marks.scroll.x, view.y - marks.scroll.y);
                Some(viso_platform::Loupe {
                    x: f64::from(x + finger.x),
                    y: f64::from(y + finger.y),
                    caret: LogicalRect::new(
                        f64::from(x + caret.x),
                        f64::from(y + caret.y),
                        f64::from(caret.w),
                        f64::from(caret.h),
                    ),
                })
            });
        if self.loupe == loupe {
            return false;
        }
        self.loupe = loupe;
        true
    }

    /// Recompute the context menu the focused field asks for and report
    /// whether it is a new request.
    fn context_menu_changed(&mut self) -> bool {
        let menu = self.focused_text_node().and_then(|id| {
            let at = self.store.field_marks(id)?.context?;
            self.edit_menu_at(id, at)
        });
        if self.context_menu == menu {
            return false;
        }
        self.context_menu = menu;
        menu.is_some()
    }

    /// Show the menu of edits the focused field asks for where the platform
    /// draws none — a touch selection's bar, a context menu — and apply what
    /// the user picked from it.
    fn update_drawn_menu(&mut self, cx: &mut RuntimeCx<'_>) {
        if let Some((edit, look)) = self.drawn_menu.take_pick() {
            self.apply_menu_edit(cx, edit, look);
        }
        let caps = cx.capabilities();
        let want = self
            .focused_text_node()
            .filter(|_| self.active)
            .and_then(|id| {
                let marks = self.store.field_marks(id)?;
                let list = marks
                    .context
                    .filter(|_| !caps.context_menu && !caps.edit_menu)
                    .map(|at| (edit_menu::MenuLook::List, at));
                let bar = marks
                    .menu
                    .filter(|_| !caps.edit_menu)
                    .map(|at| (edit_menu::MenuLook::Bar, at));
                let (look, at) = list.or(bar)?;
                Some((look, self.edit_menu_at(id, at)?))
            });
        let Some(root) = self.root else {
            return;
        };
        if want.is_none() && self.drawn_menu.shown().is_none() {
            return;
        }
        let labels = viso_platform::EditLabels::for_locale(&self.states.env().environment().locale);
        let waits = self.drawn_menu.update(
            &mut self.store,
            &mut self.states,
            &mut self.bindings,
            root,
            want,
            &labels,
        );
        // The panel placed this frame paints where it now sits.
        self.update_transforms();
        if waits {
            cx.request_state_flush(self.window);
        }
    }

    /// Apply `edit` picked from the drawn menu of `look` to the focused
    /// field, through the same clipboard as the shortcuts; the menu closes,
    /// but for the bar's Select All.
    fn apply_menu_edit(
        &mut self,
        cx: &mut RuntimeCx<'_>,
        edit: edit_menu::Edit,
        look: edit_menu::MenuLook,
    ) {
        let Some(node) = self.focused_text_node() else {
            return;
        };
        let Some(buffer) = self.text_edits.get(node) else {
            return;
        };
        let (start, end) = buffer.sel.logical_range();
        let selected =
            (start != end && !buffer.look.secure).then(|| buffer.text[start.0..end.0].to_owned());
        let bar = look == edit_menu::MenuLook::Bar;
        let intent = match edit {
            edit_menu::Edit::Cut | edit_menu::Edit::Copy => {
                let Some(text) = selected else {
                    return;
                };
                cx.set_clipboard(viso_ui::ClipboardData::text(text));
                (edit == edit_menu::Edit::Cut).then_some(text_edit::EditIntent::Cut)
            }
            edit_menu::Edit::Paste => {
                let token = self.paste_token(None);
                cx.request_paste(self.window, viso_ui::ClipboardKinds::TEXT, token);
                None
            }
            edit_menu::Edit::SelectAll if bar => Some(text_edit::EditIntent::Touch(
                text_edit::TouchEdit::SelectAll,
            )),
            edit_menu::Edit::SelectAll => Some(text_edit::EditIntent::SelectAll),
        };
        if let Some(intent) = intent {
            self.store.queue_edit(node, intent);
        }
        if !(bar && edit == edit_menu::Edit::SelectAll) {
            self.close_drawn_menu();
        }
        self.settle_text_edits();
        cx.request_state_flush(self.window);
    }

    /// Alt and a letter or digit, or F10, pressed: the focused node's
    /// handlers may take it, else it reaches the access key or the menu bar.
    /// Returns whether one of them took it.
    fn route_access_key(
        &mut self,
        root: NodeId,
        key: AccessKey,
        code: viso_runtime::Key,
        modifiers: Modifiers,
    ) -> bool {
        let ev = KeyEvent {
            key: code,
            pressed: true,
            repeat: false,
            modifiers,
        };
        if KeyRouter::route_key_taken(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            &mut self.text_edits,
            root,
            ev,
            &mut self.route_chain,
        ) {
            return true;
        }
        match key {
            AccessKey::Char(c) => KeyRouter::route_access(
                &mut self.store,
                &mut self.states,
                &self.bindings,
                &mut self.text_edits,
                root,
                c,
            ),
            AccessKey::MenuBar => KeyRouter::route_menu_bar(
                &mut self.store,
                &mut self.states,
                &self.bindings,
                &mut self.text_edits,
            ),
        }
    }

    /// Close the drawn menu the focused field asks for.
    fn close_drawn_menu(&mut self) {
        if let Some(node) = self.focused_text_node() {
            self.store.queue_edit(
                node,
                text_edit::EditIntent::Touch(text_edit::TouchEdit::MenuClosed),
            );
        }
    }

    /// The menu of edits field `id` offers, pointing at `at` in its text
    /// space, kept inside the field's box.
    fn edit_menu_at(&self, id: NodeId, at: viso_ui::Rect) -> Option<viso_platform::EditMenu> {
        let buffer = self.text_edits.get(id)?;
        let scroll = self.store.field_marks(id)?.scroll;
        let view = self.store.world(id);
        let x = (view.x - scroll.x + at.x).max(view.x);
        let y = (view.y - scroll.y + at.y).max(view.y);
        let right = (view.x - scroll.x + at.x + at.w).min(view.x + view.w);
        let bottom = (view.y - scroll.y + at.y + at.h).min(view.y + view.h);
        let (start, end) = buffer.sel.logical_range();
        let selected = start != end && !buffer.look.secure;
        let whole = start.0 == 0 && end.0 == buffer.text.len();
        let editable = !buffer.look.read_only;
        Some(viso_platform::EditMenu {
            at: LogicalRect::new(
                x as f64,
                y as f64,
                (right - x).max(0.0) as f64,
                (bottom - y).max(0.0) as f64,
            ),
            cut: selected && editable,
            copy: selected,
            paste: editable,
            select_all: !buffer.text.is_empty() && !whole,
        })
    }

    /// Recompute the text-entry area from the laid-out tree and report whether
    /// it differs from what was last pushed. The area is the caret's line box
    /// in the focused control, one logical pixel wide so a candidate window
    /// anchors beside the insertion point; a control whose run is not shaped
    /// yet reports its whole box.
    fn text_entry_changed(&mut self) -> bool {
        // A read-only field takes no input: no keyboard, no input method.
        let entry = self
            .focused_text_node()
            .filter(|&id| !self.text_edits.get(id).is_some_and(|b| b.look.read_only))
            .map(|id| {
                let r = self.store.world(id);
                let caret = match (
                    self.text.as_ref(),
                    self.text_sources.get(&id),
                    self.text_edits.get(id),
                ) {
                    (Some(text), Some(request), Some(buffer)) if buffer.draws(&request.text) => {
                        // A composing control anchors the candidate window at the
                        // composition, not wherever the caret sits inside it.
                        let at = if buffer.has_composition() {
                            buffer.composition().anchor()
                        } else {
                            buffer.sel.focus
                        };
                        text.caret(ParagraphSlot::from(id), request, buffer.drawn_position(at))
                    }
                    _ => None,
                };
                let look = self
                    .text_edits
                    .get(id)
                    .map(|b| (b.look.secure, b.look.kind, b.holds_lines()));
                let (secure, kind, multiline) = look.unwrap_or_default();
                TextEntry {
                    caret: caret_area(r, caret),
                    secure,
                    kind,
                    multiline,
                }
            });
        // A control that edits its own document says where its caret is.
        let entry = entry.or_else(|| {
            let (id, own) = self.store.text_entry_from(self.store.focused()?)?;
            let r = self.store.world(id);
            Some(TextEntry {
                caret: caret_area(r, Some(own.caret)),
                secure: false,
                kind: own.kind,
                multiline: own.multiline,
            })
        });
        if self.text_entry == Some(entry) {
            return false;
        }
        self.text_entry = Some(entry);
        true
    }

    /// Give the focused text field its caret, selection and composition
    /// marks and scroll it to show its caret, and take them from the field
    /// that lost the focus. The caret shows steadily for [`CARET_BLINK`]
    /// after it moves, then blinks until [`CARET_IDLE`] passes, then shows
    /// steadily again; it never blinks under reduced motion, and shows only
    /// while the window is active.
    fn update_field_marks(&mut self, now: Instant) {
        let focused = self.focused_text_node();
        if self.marked != focused
            && let Some(old) = self.marked
        {
            self.mark_field(old, false, now);
            if self
                .text_edits
                .get(old)
                .is_some_and(|b| b.touch_state() != text_edit::TouchState::default())
            {
                self.store
                    .queue_edit(old, text_edit::EditIntent::Touch(text_edit::TouchEdit::End));
            }
            // A number field the focus leaves settles its text.
            if self.store.is_number_field(old) {
                self.store.queue_edit(old, text_edit::EditIntent::Settle);
            }
            // A read-only field's selection goes with the focus.
            if let Some(buffer) = self.text_edits.get_mut(old)
                && buffer.look.read_only
                && !buffer.sel.is_caret()
            {
                buffer.sel = viso_text::selection::Selection::caret(buffer.sel.focus);
                self.mark_field(old, false, now);
            }
        }
        self.marked = focused;
        self.caret_due = None;
        if let Some(node) = focused {
            self.mark_field(node, true, now);
        }
    }

    /// Mark text field `node`, `focused` or not, against its drawn lines. A
    /// field whose lines are older than its text keeps the marks it drew
    /// with them.
    fn mark_field(&mut self, node: NodeId, focused: bool, now: Instant) {
        let (Some(text), Some(buffer)) = (self.text.as_ref(), self.text_edits.get(node)) else {
            return;
        };
        let Some(layout) = text_edit::EditGeometry::layout(text, node) else {
            return;
        };
        if !buffer.draws(layout.text) {
            return;
        }
        let key = FieldKey {
            node,
            revision: buffer.revision(),
            sel: buffer.sel,
        };
        let keyboard = focused
            && self
                .store
                .in_state(node, viso_ui::Interaction::FocusVisible);
        let mut caret_on = true;
        // A read-only field's caret shows only under the keyboard's focus.
        if focused && (keyboard || !buffer.look.read_only) {
            let start = match self.blink {
                Some((at, start)) if at == key => start,
                _ => {
                    self.blink = Some((key, now));
                    now
                }
            };
            let since = now.saturating_duration_since(start);
            let steady = self.states.env().environment().reduced_motion || !self.active;
            if !steady && since < CARET_IDLE {
                let phase = since.as_nanos() / CARET_BLINK.as_nanos();
                caret_on = phase.is_multiple_of(2);
                let next = start + CARET_BLINK * (phase as u32 + 1);
                self.caret_due = Some(next.min(start + CARET_IDLE));
            }
        }
        let state = viso_ui::MarkState {
            focused,
            active: self.active,
            caret_on,
            keyboard,
            reveal: focused && self.revealed != Some(key),
        };
        let world = self.store.world(node);
        let baseline = match self.store.content_payload(node) {
            Some(viso_ui::Content::Text { baseline, .. }) => *baseline,
            _ => 0.0,
        };
        self.marks.scroll = self
            .store
            .field_marks(node)
            .map_or(viso_ui::Vec2::ZERO, |m| m.scroll);
        let view = viso_ui::Vec2 {
            x: world.w,
            y: world.h,
        };
        buffer.mark(layout, view, baseline, state, &mut self.marks);
        self.store.swap_field_marks(node, &mut self.marks);
        if focused {
            self.revealed = Some(key);
        }
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
    /// first-frame double-shape counter; `0` on the steady frame that
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
                    self.store.text_sizing(id, request),
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
            self.relayout();
            passes += 1;
        }
        passes
    }
}

/// Translate a UI-tier [`WindowConfig`] into the platform config the facade
/// hands to `create_window`. The one place the `viso-ui -> viso-platform` name
/// gap is bridged: the app and every handler name only the UI
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
        position: None,
        maximized: false,
        cpu_frames: !cfg!(feature = "headless")
            && <viso_gpu::Backend as viso_gpu::GpuBackend>::SHADER_LANG
                == viso_gpu::ShaderLang::None,
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
    bar: Option<viso_widgets::MenuBar>,
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
            // The app's menu bar under it, where the platform shows none.
            if let Some(bar) = &bar {
                viso_ui::Component::build(bar, cx);
            }
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

/// The menu bar a captioned window `window` draws for the app's `menu`
/// where the platform shows no menu bar, its picks queued on `picks`.
fn drawn_menu_bar(
    menu: Option<&viso_platform::Menu>,
    picks: &app_menu::Picks,
    cx: &RuntimeCx<'_>,
    window: WindowId,
    caption: bool,
) -> Option<viso_widgets::MenuBar> {
    if !caption || cx.capabilities().menus {
        return None;
    }
    app_menu::bar(menu?, window, picks)
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

/// What a drag from outside offers, as data a target may take: its files,
/// its link and its text, each empty until the OS gives its contents.
/// What a drag carries out of the app: its text, link and files. A drop
/// elsewhere may copy or move its text, and copy or link to its files, which
/// stay where they are.
/// The haptic service's pattern for a control's `haptic`.
fn play(haptic: viso_ui::Haptic) -> services::Haptic {
    use viso_ui::Haptic as H;
    match haptic {
        H::Selection => services::Haptic::Selection,
        H::Light => services::Haptic::Light,
        H::Medium => services::Haptic::Medium,
        H::Heavy => services::Haptic::Heavy,
        H::Success => services::Haptic::Success,
        H::Warning => services::Haptic::Warning,
        H::Error => services::Haptic::Error,
    }
}

fn drag_out(data: &viso_ui::DragData) -> viso_platform::DragOut {
    let files = data.files().map(<[_]>::to_vec).unwrap_or_default();
    viso_platform::DragOut {
        text: data.as_text().map(str::to_owned),
        uri: data.uri().map(str::to_owned),
        actions: viso_platform::DropActions {
            moves: files.is_empty(),
            copies: true,
            links: !files.is_empty(),
        },
        files,
    }
}

fn outside_data(offer: &viso_runtime::DropOffer) -> viso_ui::DragData {
    use viso_ui::{DragData, DragItem};
    let mut data = DragData::new();
    if offer.kinds.files {
        data = data.with(DragItem::Files(offer.files.clone().into()));
    }
    if offer.kinds.uri {
        data = data.with(DragItem::Uri(
            offer.uri.as_deref().unwrap_or_default().into(),
        ));
    }
    if offer.kinds.text {
        data = data.with(DragItem::Text(
            offer.text.as_deref().unwrap_or_default().into(),
        ));
    }
    data
}

impl<A: Application> viso_runtime::FrameDriver for AppDriver<A> {
    fn on_launch(&mut self, cx: &mut RuntimeCx<'_>) {
        shell::launch(cx);
        #[cfg(feature = "pdf")]
        pdf::launch(cx.loop_waker());
        // Construct the user application now that the pump is live, then take the
        // faces it registered through `AppCx::load_font` so every window opened
        // this session loads them ahead of the system fallbacks.
        self.app = Some(A::new(&mut self.cx));
        self.accessibility = cx.capabilities().accessibility;
        self.fonts = SessionFonts {
            loaded: self.cx.__take_fonts(),
            packaged: self.cx.__packaged_fonts(),
        };
        #[cfg(feature = "hot-reload")]
        {
            self.hot_reload.set_dev_states(self.cx.__take_dev_states());
            self.hot_reload.set_packaged_fonts(self.fonts.packaged);
        }
        // Install the app's menu bar, if it declares one, before opening any
        // window. A `None` leaves the framework's standard app menu (with its
        // working Quit) in place from platform bring-up.
        if let Some(menu) = self.app.as_ref().and_then(Application::menu) {
            cx.set_menu(&menu);
            self.app_menu = Some(menu);
        }
        shell::apply(cx, &mut self.shell);
        // The OS services, created once on the loop thread now that the
        // platform is up; every window's handlers share them.
        if self.services.is_none() {
            let app = services::app_name();
            let system = services::Services::system(&app);
            self.services = Some(Rc::new(system));
            // The views' `@persist` states persist where the OS keeps the
            // app's data; a session handed its services (a headless test)
            // persists nothing unless it installs a store itself.
            persist::install(&app);
            // The games the views run play their audio on the default
            // output device.
            audio::install();
        }
        // Open the initial window with the app's declared configuration
        // (`window_config`, defaulting to a self-drawn, captioned window). Later
        // phases let the app request further windows via `AppCx`.
        let cfg = self
            .app
            .as_ref()
            .map(Application::window_config)
            .unwrap_or_default();
        let mut config = to_platform_config(&cfg);
        let opening = self
            .places
            .open(cfg.persist.as_deref(), cx.framed_windows(), &mut config);
        #[cfg(feature = "hot-reload")]
        let config = hot_reload::launch_window(config);
        let Ok(id) = cx.create_window(config) else {
            return;
        };
        if let Some(opening) = opening {
            self.places.opened(cx, id, opening);
        }
        // Open the launch window through the shared bring-up path, authoring its
        // tree with the application's own `build`, wrapped by default in a
        // self-drawn caption band (window-centered title, no authoring). A window
        // opened later through the `window()` seam runs the identical path with
        // the request's deferred build closure — the launch window is not a
        // special case.
        let ui_chrome = cfg.chrome;
        let title = cfg.title.clone();
        let framed = cx.framed_windows();
        let caption = cfg.caption && framed;
        let bar = drawn_menu_bar(self.app_menu.as_ref(), &self.menu_picks, cx, id, caption);
        let app = self.app.as_mut();
        // Read the native traffic-light box the instant the window exists, so the
        // single build below sees it and a self-drawn caption yields to the OS
        // overlay instead of drawing its own buttons (chrome data contract).
        let chrome_geom = cx.window_chrome_geom(id);
        // The wrap captures the caption root into this cell during the synchronous
        // build inside `open`; read it back afterward to hold for fullscreen hide.
        let caption_cell = Cell::new(None);
        let services = self.services.clone();
        #[cfg(feature = "headless")]
        if self.harness.is_pending() {
            // The view mounts alone: no caption, no safe area, nothing of the
            // app's own view.
            let mut ws = WindowState::open(
                cx,
                id,
                &self.fonts,
                !self.gamepads.is_empty(),
                services,
                ui_chrome,
                chrome_geom,
                |_| None,
            );
            self.harness.mount(&mut ws);
            self.adopt_tape(&ws);
            self.windows.push(ws);
            return;
        }
        let mut ws = WindowState::open(
            cx,
            id,
            &self.fonts,
            !self.gamepads.is_empty(),
            services,
            ui_chrome,
            chrome_geom,
            |build| {
                let content = |build: &mut BuildCx<'_>| {
                    app.and_then(|app| {
                        app.build(build);
                        build.root()
                    })
                };
                if framed {
                    wrap_root_with_caption(build, &title, caption, &caption_cell, bar, content)
                } else {
                    wrap_root_in_safe_area(build, content)
                }
            },
        );
        ws.caption = caption_cell.get();
        if !framed {
            ws.safe_area_root = ws.root;
        }
        #[cfg(feature = "hot-reload")]
        self.hot_reload.adopt(|| cx.loop_waker(), &mut ws);
        #[cfg(feature = "headless")]
        self.harness.adopt(&mut ws);
        self.adopt_tape(&ws);
        self.windows.push(ws);
        // A link that came before the window opens now.
        self.open_pending_urls(cx);
    }

    fn on_geometry(&mut self, window: WindowId, scale: f64, width: u32, height: u32) {
        // Resize the named window's swapchain so its next frame maps pixel-space
        // to the new extent, and mark its root for relayout so the next
        // incremental frame re-places the tree against the new surface and
        // repaints. Record the new extent on `surface_size` regardless of GPU:
        // layout resolves against it, so a headless window still re-places the
        // tree on resize. An event for an unknown window (already closed) is a
        // no-op.
        #[cfg(any(feature = "hot-reload", feature = "headless"))]
        if let Some(recorder) = &mut self.recorder {
            recorder.resize(window, scale as f32, (width, height));
        }
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
            ws.follow_scale_factor();
        }
        if let Some(gpu) = &mut ws.gpu {
            if let Some(surface) = gpu.surface {
                gpu.backend.resize_surface(surface, w, h);
            }
            // A resized swapchain holds no earlier frame to keep.
            gpu.renderer.damage_all();
            gpu.size = (w, h);
        }
        if let Some(root) = ws.root {
            let cause = ws.store.set_dirty_cause(DirtyCause::Window);
            ws.store.mark_dirty(
                root,
                DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
            );
            ws.store.set_dirty_cause(cause);
        }
        ws.report_window();
    }

    fn on_input(&mut self, sample: viso_runtime::InputSample, at: viso_runtime::Instant) {
        // The sample arrives in physical pixels (the scheduler resolved the
        // window scale), but layout and hit testing run in logical points, so
        // pointer/scroll positions are divided by the scale factor before they
        // reach the router — the same space as node bounds. Route it to the
        // target window's tree along the hit node's ancestry; any state a handler
        // writes lands in that window's pending set and is turned into targeted
        // dirtying by the next frame's flush (the scheduler already flagged the
        // frame input-dirty). A sample naming an unknown window (already closed)
        // is dropped.
        let _input = profile::time(Span::Input);
        #[cfg(any(feature = "hot-reload", feature = "headless"))]
        if let Some(recorder) = &mut self.recorder {
            recorder.input(&sample);
        }
        let target = sample.window();
        let Some(ws) = self.window_mut(target) else {
            return;
        };
        let Some(root) = ws.root else {
            return;
        };
        ws.store.set_input_time(at);
        let scale = ws.dpi.max(1.0);
        #[cfg(any(feature = "hot-reload", feature = "headless"))]
        let journaled = match ws.journal {
            Some(_) => {
                ws.route_chain.clear();
                inspect::input_name(&sample, scale)
            }
            None => None,
        };
        let cause = ws.store.set_dirty_cause(DirtyCause::Input);
        // Samples that can edit the focused text control settle their edits
        // once routed; a pointer sample settles only when a handler placed a
        // caret, and scroll samples never record one.
        let edits_text = matches!(
            sample,
            viso_runtime::InputSample::Key(_)
                | viso_runtime::InputSample::Text(_)
                | viso_runtime::InputSample::Paste(_)
                | viso_runtime::InputSample::ImePreedit(_)
                | viso_runtime::InputSample::EditMenu(_)
        );
        match sample {
            viso_runtime::InputSample::Pointer(p) => {
                if p.phase == viso_runtime::PointerPhase::Down {
                    // A press while Alt is held makes it no menu-bar Alt.
                    ws.alt_alone = false;
                }
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
                environment::note_pointer(&mut ws.states, p.kind);
                // A press away from the drawn menu and its field closes it.
                if ev.phase == PointerPhase::Down
                    && ws.drawn_menu.shown().is_some()
                    && !ws.drawn_menu.holds(&ws.store, ev.x, ev.y)
                    && !ws.focused_text_node().is_some_and(|id| {
                        let r = ws.store.world(id);
                        r.x <= ev.x && ev.x < r.x + r.w && r.y <= ev.y && ev.y < r.y + r.h
                    })
                {
                    ws.close_drawn_menu();
                }
                // A press outside a menu, a list or a popover dismisses it
                // before it goes on to what it landed on.
                if ev.phase == PointerPhase::Down {
                    for layer in ws.store.dismissed_by_press(ev.x, ev.y) {
                        KeyRouter::route_dismiss(
                            &mut ws.store,
                            &mut ws.states,
                            &ws.bindings,
                            &mut ws.text_edits,
                            layer,
                        );
                    }
                }
                PointerRouter::route_contact(
                    &mut ws.store,
                    &mut ws.states,
                    &ws.bindings,
                    root,
                    contact,
                    ev,
                    &mut ws.route_chain,
                );
                let what = match ev.phase {
                    PointerPhase::Down if ev.buttons.contains(PointerButtons::PRIMARY) => {
                        Some(LinkPointer::Down)
                    }
                    PointerPhase::Down => None,
                    PointerPhase::Up => Some(LinkPointer::Up),
                    PointerPhase::Leave => Some(LinkPointer::Left),
                    _ => Some(LinkPointer::Moved),
                };
                if let Some(what) = what
                    && let Some((host, target)) =
                        ws.store.link_pointer(root, contact.id, what, ev.x, ev.y)
                {
                    KeyRouter::route_link(
                        &mut ws.store,
                        &mut ws.states,
                        &ws.bindings,
                        &mut ws.text_edits,
                        host,
                        &target,
                    );
                }
            }
            viso_runtime::InputSample::Key(k) => {
                // Lower the runtime-tier key sample onto the UI-tier event, then
                // route by focus (not hit test). Tab is a framework-level
                // focus-traversal command: on a Tab press we advance the focus
                // ring instead of routing the key to a handler (Shift-Tab goes
                // backward). Every other key routes to the focused node's
                // ancestry so a control can react to it.
                let modifiers = lower_modifiers(k.modifiers);
                environment::note_key(&mut ws.states);
                // Escape ends a pointer lock, as a browser's does, then goes
                // on to the focused node like any key.
                if k.pressed
                    && k.key == viso_runtime::Key::Escape
                    && ws.store.pointer_lock().is_some()
                {
                    KeyRouter::route_pointer_lock_end(
                        &mut ws.store,
                        &mut ws.states,
                        &ws.bindings,
                        &mut ws.text_edits,
                    );
                }
                // Off Apple systems Alt shows the access keys' cues while it is
                // held, and let go alone asks for the menu bar.
                let access = alt_access() && ws.store.has_access_keys();
                let alt = matches!(
                    k.key,
                    viso_runtime::Key::AltLeft | viso_runtime::Key::AltRight
                );
                if access && alt {
                    let cues = k.pressed;
                    if k.pressed && !k.repeat {
                        ws.alt_alone = !(modifiers.control || modifiers.logo || modifiers.shift);
                    }
                    KeyRouter::route_cues(
                        &mut ws.store,
                        &mut ws.states,
                        &ws.bindings,
                        &mut ws.text_edits,
                        cues,
                    );
                    if !k.pressed && std::mem::take(&mut ws.alt_alone) {
                        KeyRouter::route_menu_bar(
                            &mut ws.store,
                            &mut ws.states,
                            &ws.bindings,
                            &mut ws.text_edits,
                        );
                    }
                } else if k.pressed {
                    ws.alt_alone = false;
                }
                // Alt and a letter or digit, and F10, reach the access keys
                // and the menu bar when the focused node's handlers leave them.
                let access_key = access
                    .then(|| access_key_of(k.key, modifiers))
                    .flatten()
                    .filter(|_| k.pressed && !k.repeat);
                let menu_key = ws.drawn_menu.key(&mut ws.states, k.key, k.pressed);
                if menu_key == edit_menu::MenuKey::Close {
                    ws.close_drawn_menu();
                }
                if menu_key != edit_menu::MenuKey::Ignored {
                    // The drawn context menu takes the keys it answers.
                } else if matches!(
                    k.key,
                    viso_runtime::Key::Tab | viso_runtime::Key::Escape | viso_runtime::Key::Back
                ) && k.pressed
                {
                    // A handler may take Tab, Escape or the system back
                    // action first; else Tab moves the focus, Escape
                    // dismisses the innermost transient layer (a tooltip, a
                    // list the focus is not in), and back does so too or,
                    // with none, turns the deepest navigation stack back.
                    let ev = KeyEvent {
                        key: k.key,
                        pressed: k.pressed,
                        repeat: k.repeat,
                        modifiers,
                    };
                    let taken = KeyRouter::route_key_taken(
                        &mut ws.store,
                        &mut ws.states,
                        &ws.bindings,
                        &mut ws.text_edits,
                        root,
                        ev,
                        &mut ws.route_chain,
                    );
                    if taken || ws.store.carry_key(&ev) {
                        // The handler, or a gesture the keys carry on, did
                        // what the key does.
                    } else if matches!(k.key, viso_runtime::Key::Tab) {
                        focus_next(&mut ws.store, root, !modifiers.shift);
                    } else if let Some(layer) = ws.store.dismissed_by_escape() {
                        KeyRouter::route_dismiss(
                            &mut ws.store,
                            &mut ws.states,
                            &ws.bindings,
                            &mut ws.text_edits,
                            layer,
                        );
                    } else if matches!(k.key, viso_runtime::Key::Back)
                        && !k.repeat
                        && let Some(stack) = ws.store.back_target()
                    {
                        KeyRouter::route_back(
                            &mut ws.store,
                            &mut ws.states,
                            &ws.bindings,
                            &mut ws.text_edits,
                            root,
                            stack,
                            ev,
                            &mut ws.route_chain,
                        );
                    }
                } else if let Some((host, target)) = ws.store.link_key(
                    matches!(k.key, viso_runtime::Key::Enter) && !k.repeat,
                    k.pressed,
                ) {
                    KeyRouter::route_link(
                        &mut ws.store,
                        &mut ws.states,
                        &ws.bindings,
                        &mut ws.text_edits,
                        host,
                        &target,
                    );
                } else if let Some(key) = access_key
                    && ws.route_access_key(root, key, k.key, modifiers)
                {
                    // The focused node's handlers, an access key or the menu
                    // bar took it.
                } else if ws.store.keys_reach_clients()
                    && (ws.store.focused().is_none()
                        || k.repeat
                        || ws.store.frame_clients_key(k.key, k.pressed))
                {
                    // No UI node takes keys, or a focused game viewport
                    // does: a running game takes them. An auto-repeat is not
                    // a new press for it; a key the game has no use for
                    // reaches the viewport's ancestry. With nothing focused,
                    // a key no game takes scrolls the window's viewport.
                    if ws.store.focused().is_none() {
                        let took = if k.repeat {
                            ws.store.has_frame_clients()
                        } else {
                            ws.store.frame_clients_key(k.key, k.pressed)
                        };
                        if !took {
                            let ev = KeyEvent {
                                key: k.key,
                                pressed: k.pressed,
                                repeat: k.repeat,
                                modifiers,
                            };
                            ws.store
                                .scroll_by_key(root, &ev, viso_ui::primary_is_logo());
                        }
                    }
                } else {
                    let ev = KeyEvent {
                        key: k.key,
                        pressed: k.pressed,
                        repeat: k.repeat,
                        modifiers,
                    };
                    let taken = KeyRouter::route_key_taken(
                        &mut ws.store,
                        &mut ws.states,
                        &ws.bindings,
                        &mut ws.text_edits,
                        root,
                        ev,
                        &mut ws.route_chain,
                    );
                    // A key nothing took scrolls the viewport the keyboard
                    // is in, unless the focus edits text.
                    let edits = ws.store.focused().is_some_and(|id| {
                        ws.text_edits
                            .get(id)
                            .is_some_and(|buffer| !buffer.look.read_only)
                    });
                    if !taken && !edits {
                        ws.store
                            .scroll_by_key(root, &ev, viso_ui::primary_is_logo());
                    }
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
            viso_runtime::InputSample::PasteRequest(r) => {
                // A paste gesture: the focus says what it takes, read by the
                // backend once this returns.
                r.reply.set(ws.paste_route(root).1);
            }
            viso_runtime::InputSample::Paste(p) => {
                // An answer goes where its request said; a gesture's to the
                // paste target the focus is in, if any.
                let to = match p.token {
                    Some(token) => ws
                        .pastes
                        .iter()
                        .position(|(t, _)| *t == token)
                        .map(|at| ws.pastes.remove(at).1),
                    None => Some(ws.paste_route(root).0),
                };
                match to {
                    // An answer to no request still pending.
                    None => {}
                    Some(Some(node)) => {
                        KeyRouter::route_paste(
                            &mut ws.store,
                            &mut ws.states,
                            &ws.bindings,
                            &mut ws.text_edits,
                            node,
                            &p.data,
                        );
                    }
                    // Pasted text routes to the focused control, which
                    // replaces its selection with it as one edit. A
                    // single-line buffer folds any line breaks in it.
                    Some(None) => {
                        if let Some(text) = p.data.text {
                            KeyRouter::route_ime(
                                &mut ws.store,
                                &mut ws.states,
                                &ws.bindings,
                                &mut ws.text_edits,
                                root,
                                ImeEvent::Paste { text },
                                &mut ws.route_chain,
                            );
                        }
                    }
                }
            }
            viso_runtime::InputSample::Copy(c) => {
                // The platform asks for the selection while its copy/cut handler
                // is still on the stack. Apply any edits queued this event batch
                // first so the reply reflects what the user sees, then answer
                // from the focused text control's buffer. An empty selection,
                // or a secure field's, leaves the reply unset and the
                // clipboard untouched.
                let geometry = ws.text.as_ref().map(|t| t as &dyn text_edit::EditGeometry);
                text_edit::reconcile(&mut ws.store, &mut ws.text_edits, geometry);
                let Some(node) = ws.focused_text_node() else {
                    // Another focused control copies as its shortcut asks:
                    // what it hands the clipboard answers the platform.
                    let apple = viso_ui::primary_is_logo();
                    let ev = KeyEvent {
                        key: if c.cut {
                            viso_runtime::Key::X
                        } else {
                            viso_runtime::Key::C
                        },
                        pressed: true,
                        repeat: false,
                        modifiers: Modifiers {
                            logo: apple,
                            control: !apple,
                            ..Modifiers::default()
                        },
                    };
                    if ws.store.focused().is_some() {
                        KeyRouter::route_key_taken(
                            &mut ws.store,
                            &mut ws.states,
                            &ws.bindings,
                            &mut ws.text_edits,
                            root,
                            ev,
                            &mut ws.route_chain,
                        );
                    }
                    if let Some(data) = ws.store.take_copied() {
                        c.reply.set(data);
                    }
                    return;
                };
                let Some(buffer) = ws.text_edits.get_mut(node) else {
                    return;
                };
                let (start, end) = buffer.sel.logical_range();
                if start == end || buffer.look.secure || (c.cut && buffer.look.read_only) {
                    return;
                }
                c.reply
                    .set(viso_ui::ClipboardData::text(&buffer.text[start.0..end.0]));
                if c.cut {
                    buffer.queue(text_edit::EditIntent::Cut);
                    ws.settle_text_edits();
                }
            }
            viso_runtime::InputSample::Drop(d) => {
                // A drag from outside comes over the target under it; the
                // reply tells the OS what a drop there would do.
                let phase = match d.phase {
                    viso_runtime::DropPhase::Over => viso_ui::ArrivalPhase::Over,
                    viso_runtime::DropPhase::Leave => viso_ui::ArrivalPhase::Leave,
                    viso_runtime::DropPhase::Drop => viso_ui::ArrivalPhase::Drop,
                };
                let at = viso_ui::Vec2::new(d.x / scale, d.y / scale);
                let operation = ws
                    .store
                    .drag_from_outside(root, phase, at, outside_data(&d.offer));
                d.reply.set(operation.map(|op| match op {
                    viso_ui::DropOperation::Move => viso_runtime::DropAction::Move,
                    viso_ui::DropOperation::Copy => viso_runtime::DropAction::Copy,
                    viso_ui::DropOperation::Link => viso_runtime::DropAction::Link,
                }));
            }
            viso_runtime::InputSample::Motion { dx, dy, .. } => {
                KeyRouter::route_motion(
                    &mut ws.store,
                    &mut ws.states,
                    &ws.bindings,
                    &mut ws.text_edits,
                    viso_ui::Vec2::new(dx / scale, dy / scale),
                );
            }
            viso_runtime::InputSample::DragEnded { action, .. } => {
                // The drag the OS took ended: its source hears how.
                ws.store.drag_out_ended(action.map(|a| match a {
                    viso_runtime::DropAction::Move => viso_ui::DropOperation::Move,
                    viso_runtime::DropAction::Copy => viso_ui::DropOperation::Copy,
                    viso_runtime::DropAction::Link => viso_ui::DropOperation::Link,
                }));
            }
            viso_runtime::InputSample::EditMenu(e) => {
                // The menu's Select All selects the focused field's text and
                // keeps the menu; a menu that closed on its own is no longer
                // shown, so a later one is pushed afresh.
                // Only the touch menu's Select All keeps the handles; a
                // context menu's is a plain one.
                let edit = match e.action {
                    viso_platform::EditMenuAction::SelectAll if ws.edit_menu.is_some() => {
                        text_edit::EditIntent::Touch(text_edit::TouchEdit::SelectAll)
                    }
                    viso_platform::EditMenuAction::SelectAll => text_edit::EditIntent::SelectAll,
                    viso_platform::EditMenuAction::Closed => {
                        ws.edit_menu = None;
                        ws.context_menu = None;
                        text_edit::EditIntent::Touch(text_edit::TouchEdit::MenuClosed)
                    }
                };
                if let Some(node) = ws.focused_text_node() {
                    ws.store.queue_edit(node, edit);
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
                if ws.drawn_menu.shown() == Some(edit_menu::MenuLook::List) {
                    ws.close_drawn_menu();
                }
                // A wheel/trackpad sample routes to the innermost scroll viewport
                // under the pointer. The runtime reports the delta as content
                // motion (positive = content moves down/right, revealing later
                // content), which is exactly the direction the viewport's offset
                // grows, so it maps straight onto the offset delta. Both the hit
                // position and the delta are divided by the scale factor into
                // logical points — the offset lives in the same logical space as
                // layout. The router clamps per axis and marks only
                // TRANSFORM — a scroll never relayouts or repaints — so no
                // state flush is involved.
                let ev = ScrollEvent {
                    x: s.x / scale,
                    y: s.y / scale,
                    delta_x: s.delta_x / scale,
                    delta_y: s.delta_y / scale,
                    modifiers: lower_modifiers(s.modifiers),
                    phase: s.phase,
                };
                // A wheel turned with Control held zooms what takes pinches
                // under it, as a browser reports a trackpad's pinch.
                let zoomed = ev.modifiers.control
                    && viso_ui::route_wheel_zoom(
                        &mut ws.store,
                        &mut ws.states,
                        &ws.bindings,
                        root,
                        (ev.x, ev.y, ev.delta_y),
                        &mut ws.route_chain,
                    )
                    .is_some();
                // A node that takes the wheel hears it before the viewports.
                let taken = !zoomed
                    && viso_ui::route_wheel(
                        &mut ws.store,
                        &mut ws.states,
                        &ws.bindings,
                        root,
                        ev,
                        &mut ws.route_chain,
                    );
                if !zoomed && !taken {
                    ScrollRouter::route(&mut ws.store, root, ev);
                    // A focused slider under the wheel steps.
                    if ws.store.wakes_due() {
                        KeyRouter::route_wakes(
                            &mut ws.store,
                            &mut ws.states,
                            &ws.bindings,
                            &mut ws.text_edits,
                        );
                    }
                }
            }
            viso_runtime::InputSample::Gesture(g) => {
                let ev = viso_ui::TrackpadPinch {
                    x: g.x / scale,
                    y: g.y / scale,
                    scale: g.scale,
                    rotation: g.rotation,
                    phase: match g.phase {
                        viso_runtime::GesturePhase::Began => viso_ui::PinchPhase::Start,
                        viso_runtime::GesturePhase::Changed => viso_ui::PinchPhase::Move,
                        viso_runtime::GesturePhase::Ended => viso_ui::PinchPhase::End,
                    },
                };
                viso_ui::route_trackpad_pinch(
                    &mut ws.store,
                    &mut ws.states,
                    &ws.bindings,
                    root,
                    ev,
                    &mut ws.route_chain,
                );
            }
        }
        if edits_text || ws.store.has_edit_requests() {
            ws.settle_text_edits();
        }
        ws.tell_focus();
        ws.store.set_dirty_cause(cause);
        #[cfg(any(feature = "hot-reload", feature = "headless"))]
        if let Some(journal) = &mut ws.journal {
            if let Some((input, at)) = journaled {
                journal.input(input, at, &ws.route_chain);
            }
            journal.settle(&mut ws.store, &ws.states);
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
            // Give back the GPU memory the next frame can rebuild on demand,
            // and the preserved branches switched away: the next settle frees
            // them, and each mounts afresh when shown again.
            viso_runtime::Lifecycle::LowMemory => {
                for ws in &mut self.windows {
                    ws.store.request_memory_trim(&mut ws.states);
                    ws.trim_memory();
                    cx.request_redraw(ws.window);
                }
            }
            // The app may not come back: make the persisted writes durable.
            viso_runtime::Lifecycle::Suspended => {
                self.places.flush();
                for ws in &mut self.windows {
                    persist::suspend(&mut ws.store, &ws.states);
                }
            }
        }
    }

    fn on_window_placed(&mut self, cx: &mut RuntimeCx<'_>, window: WindowId) {
        self.places.placed(cx, window);
        self.places.store_due(cx.frame_now());
    }

    fn on_wakeup(&mut self, cx: &mut RuntimeCx<'_>) {
        shell::apply(cx, &mut self.shell);
        self.places.store_due(cx.frame_now());
        // A loop kick: a task woke (or was spawned), or a timer deadline
        // elapsed. Frame the windows with work, so the frame polls the tasks
        // and fires the timers.
        #[cfg(feature = "hot-reload")]
        {
            self.hot_reload.wakeup(cx, &self.windows);
            for ask in self.hot_reload.take_profiles() {
                match ask {
                    hot_reload::ProfileAsk::Start { id, hud } => {
                        self.start_profile(id, hud, true);
                        if hud {
                            for ws in &self.windows {
                                cx.request_redraw(ws.window);
                            }
                        }
                    }
                    hot_reload::ProfileAsk::End { id } => self.end_profile(id),
                }
                if !self.profiler.hud() {
                    self.show_hud(None, cx);
                }
            }
        }
        let now = cx.frame_now();
        // A decoded asset frames the windows that show assets.
        let decoded = self.assets.receive();
        let played = self.assets.playback.receive();
        for ws in &self.windows {
            let timer_due = ws
                .timers
                .earliest()
                .into_iter()
                .chain(ws.caret_due)
                .chain(ws.animation_due)
                .chain(ws.video_step.due)
                .any(|at| at <= now);
            let asset_due =
                (decoded && !ws.store.assets().is_empty()) || (played && !ws.players.is_empty());
            if ws.store.tasks_woken() || timer_due || asset_due {
                cx.request_redraw(ws.window);
            }
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
            gpu.renderer = footprint::renderer(&mut gpu.backend, surface);
        }
        gpu.surface = Some(surface);
        gpu.renderer.damage_all();
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

    fn on_open_url(&mut self, cx: &mut RuntimeCx<'_>, url: &str) {
        self.pending_urls.push(url.to_owned());
        self.open_pending_urls(cx);
    }

    fn after_input(&mut self, cx: &mut RuntimeCx<'_>, window: WindowId) {
        let Some(ws) = self.window_mut(window) else {
            return;
        };
        // The cursor follows the mouse as it moves, not at the next frame,
        // which a move that changes nothing else never asks for; a pointer
        // lock is asked while the press that asked for it is still the
        // event being handled, as a browser requires.
        ws.push_pointer_lock(cx);
        ws.push_cursor(cx);
    }

    fn on_pointer_lock(&mut self, window: WindowId, locked: bool) {
        if let Some(ws) = self.window_mut(window) {
            KeyRouter::route_pointer_lock(
                &mut ws.store,
                &mut ws.states,
                &ws.bindings,
                &mut ws.text_edits,
                locked,
            );
        }
    }

    fn on_tray(&mut self, event: viso_platform::TrayEvent) {
        if let Some(app) = self.app.as_mut() {
            app.on_tray(event);
        }
    }

    fn on_window_focus(&mut self, window: WindowId, focused: bool) {
        #[cfg(any(feature = "hot-reload", feature = "headless"))]
        if let Some(recorder) = &mut self.recorder {
            recorder.focus(window, focused);
        }
        // A window losing focus stops delivering key-ups: release every key
        // its games hold so none stays down.
        if let Some(ws) = self.windows.iter_mut().find(|w| w.window == window) {
            if !focused {
                ws.store.release_client_input();
                // A pointer lock ends with the focus.
                KeyRouter::route_pointer_lock_end(
                    &mut ws.store,
                    &mut ws.states,
                    &ws.bindings,
                    &mut ws.text_edits,
                );
                // Nor Alt's: the access keys' cues go.
                ws.alt_alone = false;
                KeyRouter::route_cues(
                    &mut ws.store,
                    &mut ws.states,
                    &ws.bindings,
                    &mut ws.text_edits,
                    false,
                );
            }
            // An inactive window shows no caret and greys its selection.
            ws.active = focused;
        }
    }

    fn wants_gamepads(&self) -> bool {
        true
    }

    fn on_gamepad(&mut self, cx: &mut RuntimeCx<'_>, event: viso_runtime::GamepadEvent) {
        use viso_runtime::GamepadEvent;
        let connected = !self.gamepads.is_empty();
        match &event {
            GamepadEvent::Connected(info) => self.gamepads.push(info.id),
            GamepadEvent::Disconnected(id) => self.gamepads.retain(|pad| pad != id),
            GamepadEvent::Changed(..) => {}
        }
        let available = !self.gamepads.is_empty();
        for ws in &mut self.windows {
            if available != connected {
                // `env.input.gamepad_available` follows the connections.
                environment::note_gamepads(&mut ws.states, available);
                cx.request_redraw(ws.window);
            }
            // A pad's input goes to the games of the window holding input
            // focus, while no UI node takes the keys; every game forgets a
            // pad that went away.
            let reaches = match event {
                GamepadEvent::Connected(_) => false,
                GamepadEvent::Changed(..) => ws.active && ws.store.keys_reach_clients(),
                GamepadEvent::Disconnected(_) => true,
            };
            if reaches && ws.store.has_frame_clients() {
                ws.store.frame_clients_gamepad(&event);
            }
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

    fn on_safe_area(&mut self, window: WindowId, insets: LogicalInsets) {
        if let Some(ws) = self.window_mut(window) {
            ws.safe_area = insets;
            ws.apply_safe_area();
            ws.report_window();
        }
    }

    fn on_appearance(&mut self, appearance: viso_platform::Appearance) {
        for ws in &mut self.windows {
            ws.report_appearance(appearance);
        }
    }

    fn on_locale(&mut self, locale: &str) {
        for ws in &mut self.windows {
            ws.report_locale(locale);
        }
    }

    fn on_display_features(
        &mut self,
        window: WindowId,
        features: &[viso_platform::DisplayFeature],
    ) {
        if let Some(ws) = self.window_mut(window) {
            environment::report_display_features(&mut ws.states, features);
        }
    }

    fn on_keyboard_inset(&mut self, window: WindowId, height: f64) {
        if let Some(ws) = self.window_mut(window) {
            ws.keyboard_inset = height;
            ws.apply_safe_area();
            ws.report_window();
        }
    }

    fn on_accessibility(&mut self, window: WindowId, request: viso_platform::AccessRequest) {
        if !self.accessibility {
            return;
        }
        let Some(ws) = self.window_mut(window) else {
            return;
        };
        let (target, action) = match request {
            viso_platform::AccessRequest::Activated => {
                ws.access = Some(accessibility::AccessBridge::new());
                return;
            }
            viso_platform::AccessRequest::Deactivated => {
                ws.access = None;
                return;
            }
            viso_platform::AccessRequest::Action { target, action } => (target, action),
        };
        let Some((node, link)) = ws.access.as_ref().and_then(|access| access.target(target)) else {
            return;
        };
        if !ws.store.arena().is_live(node) {
            return;
        }
        let scale = ws.dpi.max(1.0);
        // The action's samples arrive with the last input.
        let at = ws
            .store
            .input_time()
            .unwrap_or_else(viso_runtime::Instant::now);
        if let Some(samples) =
            accessibility::perform(&mut ws.store, window, node, link, action, scale)
        {
            for sample in samples {
                self.on_input(sample, at);
            }
        }
    }

    fn on_fullscreen_changed(&mut self, window: WindowId, fullscreen: bool) {
        self.places.fullscreen_changed(window, fullscreen);
        // Hide the self-drawn caption while fullscreen and restore it on exit by
        // toggling its root's visibility in place — on macOS the OS draws its own
        // auto-hiding title bar in fullscreen and removes the traffic lights, so
        // the caption must yield. `set_hidden` marks LAYOUT|PAINT, so the next
        // frame reflows the body to fill the reclaimed strip. A window with no
        // caption, or an event for an already-closed window, is a no-op.
        // Leaving it brings back a node shown full screen.
        if let Some(ws) = self.window_mut(window) {
            if let Some(cap) = ws.caption {
                ws.store.set_hidden(cap, fullscreen);
            }
            ws.store.window_fullscreen_changed(fullscreen);
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
        // (we iterate all windows, never key one).
        #[cfg(any(feature = "hot-reload", feature = "headless"))]
        if phase == FramePhase::CollectInput {
            self.begin_profiled_frame();
            if let Some(recorder) = &mut self.recorder {
                recorder.frame(cx.frame_delta());
            }
            for ws in &mut self.windows {
                if let Some(journal) = &mut ws.journal
                    && journal.begin_frame()
                {
                    ws.journal = None;
                    inspect::Journal::end(&mut ws.store);
                }
            }
        }
        let _span = match phase {
            FramePhase::FlushStateTransactions => Some(profile::time(Span::Flush)),
            FramePhase::UpdateSemantics => Some(profile::time(Span::Semantics)),
            FramePhase::UploadGpuChanges => Some(profile::time(Span::Upload)),
            FramePhase::Submit => Some(profile::time(Span::Submit)),
            _ => None,
        };
        match phase {
            FramePhase::RunContinuations => {
                // The faults the games raised last frame: to `viso run` when
                // it is attached, to stderr otherwise.
                let faults = viso_view::take_game_faults();
                #[cfg(feature = "hot-reload")]
                let faults = self.hot_reload.forward_game_faults(faults);
                for line in faults {
                    eprintln!("[viso] {line}");
                }
                // The window places that settled.
                self.places.store_due(cx.frame_now());
                for ws in &mut self.windows {
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
                    ws.store.set_dirty_cause(DirtyCause::Task);
                    ws.store.take_timer_requests(&mut ws.timer_requests);
                    for req in ws.timer_requests.drain(..) {
                        ws.timers.arm_request(req, cx.frame_now());
                    }
                    if !ws.timers.is_empty() {
                        ws.timers.fire_due(&mut ws.store, cx.frame_now());
                        // A press held still that long is a long press now.
                        if ws.store.long_press_due() {
                            viso_ui::deliver_long_presses(
                                &mut ws.store,
                                &mut ws.states,
                                &ws.bindings,
                            );
                        }
                        // A wheel's pinch that rested ends.
                        if ws.store.pinch_end_due() {
                            viso_ui::deliver_pinch_ends(
                                &mut ws.store,
                                &mut ws.states,
                                &ws.bindings,
                            );
                        }
                        // The wakes whose time came reach their handlers.
                        if ws.store.wakes_due() {
                            KeyRouter::route_wakes(
                                &mut ws.store,
                                &mut ws.states,
                                &ws.bindings,
                                &mut ws.text_edits,
                            );
                            ws.tell_focus();
                        }
                    }
                    // The viewports a pull asked to refresh hear it.
                    if ws.store.refreshes_due() {
                        KeyRouter::route_refreshes(
                            &mut ws.store,
                            &mut ws.states,
                            &ws.bindings,
                            &mut ws.text_edits,
                        );
                        ws.tell_focus();
                    }
                    // The snapping viewports whose item changed hear it.
                    if ws.store.snaps_due() {
                        KeyRouter::route_snaps(
                            &mut ws.store,
                            &mut ws.states,
                            &ws.bindings,
                            &mut ws.text_edits,
                        );
                        ws.tell_focus();
                    }
                    // The rows whose gestures settled hear what they did.
                    if ws.store.row_gestures_due() {
                        KeyRouter::route_row_gestures(
                            &mut ws.store,
                            &mut ws.states,
                            &ws.bindings,
                            &mut ws.text_edits,
                        );
                        ws.tell_focus();
                    }
                    // The sheets a drag moved hear where it left them.
                    if ws.store.sheets_due() {
                        KeyRouter::route_sheets(
                            &mut ws.store,
                            &mut ws.states,
                            &ws.bindings,
                            &mut ws.text_edits,
                        );
                        ws.tell_focus();
                    }
                    // The targets and sources a drag passed hear what it did.
                    if ws.store.drops_due() {
                        KeyRouter::route_drops(
                            &mut ws.store,
                            &mut ws.states,
                            &ws.bindings,
                            &mut ws.text_edits,
                        );
                        ws.tell_focus();
                    }
                    // Poll the tasks spawned or woken since the last frame and
                    // run each finished one's continuation; what it writes joins
                    // this frame's transaction. No woken task (the steady case)
                    // takes no lock and polls nothing.
                    // Off-loop jobs a build without worker threads runs here:
                    // all of them headless, else an idle slice.
                    viso_ui::work::run_inline(if cfg!(feature = "headless") {
                        std::time::Duration::MAX
                    } else {
                        std::time::Duration::from_millis(4)
                    });
                    if ws.store.tasks_woken() {
                        ws.store.poll_tasks(&mut ws.continuations);
                        for then in ws.continuations.drain(..).flatten() {
                            then(&mut UpdateCx::__new(
                                &mut ws.states,
                                &ws.bindings,
                                &mut ws.store,
                            ));
                        }
                    }
                    ws.store.set_dirty_cause(DirtyCause::Other);
                }
            }
            #[cfg(feature = "hot-reload")]
            FramePhase::ApplyDevPatches => {
                // Commit the edits the watcher staged at the frame boundary, so
                // a reload's writes join this frame's transaction and paint
                // this frame; a snapshot the host asked for takes where each
                // window is.
                {
                    if self.hot_reload.snapshot_asked() {
                        let positions = self
                            .windows
                            .iter()
                            .map(|ws| (ws.window, cx.window_position(ws.window)))
                            .collect();
                        self.hot_reload.set_positions(positions);
                    }
                    for ws in &mut self.windows {
                        ws.store.set_dirty_cause(DirtyCause::Reload);
                    }
                    let before = self.hot_reload.revision();
                    self.hot_reload.reload(&mut self.windows, &mut self.assets);
                    if let (Some(recorder), Some(revision)) =
                        (&mut self.recorder, self.hot_reload.revision())
                        && before != Some(revision)
                    {
                        recorder.patch(revision);
                    }
                    for ws in &mut self.windows {
                        ws.store.set_dirty_cause(DirtyCause::Other);
                    }
                }
            }
            FramePhase::AdvanceAnimations => {
                for ws in &mut self.windows {
                    // Start any animations a handler requested this frame — a
                    // `TranslateAnim` queued through `EventCx`, which the router
                    // handed to the store's queue; drain it into the registry
                    // now. Then advance every live animation by this frame's
                    // delta, writing each node's interpolated translate through
                    // `set_translate` (a TRANSFORM write, plus one PAINT when it
                    // first makes the node a transform root). This runs before
                    // the state flush and the Layout phase, so the moved translate is
                    // resolved into `world` and painted this same frame. Both
                    // steps are no-ops when nothing is animating (the steady
                    // case) — the queue is empty and the registry has nothing to
                    // tick.
                    ws.store.set_dirty_cause(DirtyCause::Animation);
                    ws.store.take_animation_requests(&mut ws.anim_requests);
                    for anim in ws.anim_requests.drain(..) {
                        ws.animations.start(anim);
                    }
                    if !ws.animations.is_empty() {
                        ws.animations.tick(&mut ws.store, cx.frame_delta());
                    }
                    if ws.store.is_transitioning() {
                        ws.store.tick_transitions(cx.frame_delta());
                    }
                    // The viewports a flick left coasting move on.
                    if ws.store.is_scroll_moving() {
                        ws.store.tick_scroll_motion(cx.frame_delta());
                    }
                    // The spinning arcs chase on.
                    if ws.store.is_spinning() {
                        ws.store.tick_spinners(cx.frame_delta());
                    }
                    // The placeholders shimmer on.
                    if ws.store.is_shimmering() {
                        ws.store.tick_shimmers(cx.frame_delta());
                    }
                    // The press ripples spread and fade.
                    if ws.store.is_rippling() {
                        ws.store.tick_ripples(cx.frame_delta());
                    }
                    // The progress indicators run, or ease to their values.
                    if ws.store.is_progressing() {
                        ws.store.tick_progress(cx.frame_delta());
                    }
                    // The sliders' thumbs ease to where they were told.
                    if ws.store.is_sliding() {
                        ws.store.tick_sliders(cx.frame_delta());
                    }
                    ws.store.set_dirty_cause(DirtyCause::Other);
                    // Advance the games the window's views run — the ticks
                    // their clocks owe, then their frame hooks — after the dev
                    // patches committed, so a patch staged during the last
                    // frame's ticks runs from the next tick on.
                    ws.store.run_frame_clients(cx.frame_delta().as_secs_f64());
                    // A game's rumble plays on every pad of its local
                    // player, while its window holds input focus.
                    if let Some(rumble) = ws.store.take_frame_client_rumble()
                        && ws.active
                    {
                        for pad in &self.gamepads {
                            cx.rumble_gamepad(*pad, rumble);
                        }
                    }
                }
            }
            FramePhase::FlushStateTransactions => {
                for ws in &mut self.windows {
                    // Flush this frame's one transaction — the handlers',
                    // continuations', dev patches' and animations' writes —
                    // through the derivations, bindings, structure hooks and
                    // semantic-state projections, re-flushing any write a hook
                    // makes while settling, and queue the effects it wakes for
                    // `RunEffects`. Many writes collapse into one round; a frame
                    // with no writes touches nothing. A flush that never settles
                    // is a reactive cycle: it stops at a bound and drops the
                    // rest.
                    if let Err(cycle) = viso_ui::settle_states(
                        &mut ws.store,
                        &mut ws.states,
                        &mut ws.bindings,
                        &mut ws.computeds,
                        &mut ws.projectors,
                        &mut ws.effects,
                        &mut ws.virtual_lists,
                        &mut ws.changed,
                    ) {
                        eprintln!("[viso] {cycle}");
                    }
                }
                // Serve the assets the settle offered, before layout: what
                // decoded since the last frame reaches every window, and each
                // window whose table moved gets its decodes started and its
                // rasters uploaded, so this frame measures and paints them.
                for ws in &mut self.windows {
                    ws.store.set_dirty_cause(DirtyCause::Asset);
                }
                self.assets
                    .provide(self.windows.iter_mut().map(|ws| &mut ws.store));
                for ws in &mut self.windows {
                    if ws.store.assets().revision() == ws.assets_served {
                        continue;
                    }
                    let gpu = ws.gpu.as_mut().map(|g| (&mut g.backend, &mut g.renderer));
                    if self.assets.serve(&mut ws.store, gpu, &|| cx.loop_waker()) {
                        // Decodes left for the next frame's slice.
                        cx.request_redraw(ws.window);
                    } else {
                        ws.assets_served = ws.store.assets().revision();
                    }
                }
                self.sweep_assets();
                self.play_animations(cx.frame_now(), &|| cx.loop_waker());
                self.play_videos(cx.frame_now());
                for ws in &mut self.windows {
                    ws.store.set_dirty_cause(DirtyCause::Other);
                }
                // Install the programs the settle's shader views took and
                // release the ones no node paints with any more, so this
                // frame paints with them.
                for ws in &mut self.windows {
                    if let Some(gpu) = ws.gpu.as_mut() {
                        ws.shaders
                            .sync(ws.store.shaders(), &mut gpu.backend, &mut gpu.renderer);
                    }
                }

                // The links no handler kept open in the apps their URLs
                // belong to, and the haptic the frame's controls asked for;
                // what the system answers is not the app's to see.
                let services = self
                    .services
                    .as_deref()
                    .and_then(|any| any.downcast_ref::<services::Services>());
                for ws in &mut self.windows {
                    for url in ws.store.take_link_opens() {
                        if let Some(services) = services {
                            drop(services.launcher().open_url(&url));
                        }
                    }
                    if let Some(haptic) = ws.store.take_haptic()
                        && let Some(services) = services
                    {
                        drop(services.haptics().play(play(haptic)));
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
                    if let Some(data) = ws.store.take_copied() {
                        cx.set_clipboard(data);
                    }
                    ws.store.take_paste_asks(&mut ws.scratch_pastes);
                    for i in 0..ws.scratch_pastes.len() {
                        let (node, kinds) = ws.scratch_pastes[i];
                        let token = ws.paste_token(Some(node));
                        cx.request_paste(ws.window, kinds, token);
                    }
                    if let Some(on) = ws.store.take_fullscreen_ask() {
                        cx.set_fullscreen(ws.window, on);
                    }
                    ws.push_pointer_lock(cx);
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
                    let mut config = to_platform_config(&req.config);
                    let opening = self.places.open(
                        req.config.persist.as_deref(),
                        cx.framed_windows(),
                        &mut config,
                    );
                    // Values the build closure moves in to wrap the deferred tree
                    // in the default caption band, exactly as the launch path does.
                    let title = req.config.title.clone();
                    let framed = cx.framed_windows();
                    let caption = req.config.caption && framed;
                    let build = req.build;
                    let id_slot = req.id_slot;
                    if let Ok(id) = cx.create_window(config) {
                        if let Some(opening) = opening {
                            self.places.opened(cx, id, opening);
                        }
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
                        let bar = drawn_menu_bar(
                            self.app_menu.as_ref(),
                            &self.menu_picks,
                            cx,
                            id,
                            caption,
                        );
                        // Capture the caption root during the synchronous build, as
                        // the launch path does, to hold for fullscreen hide.
                        let caption_cell = Cell::new(None);
                        let mut ws = WindowState::open(
                            cx,
                            id,
                            &self.fonts,
                            !self.gamepads.is_empty(),
                            self.services.clone(),
                            ui_chrome,
                            chrome_geom,
                            |cx| {
                                if framed {
                                    wrap_root_with_caption(
                                        cx,
                                        &title,
                                        caption,
                                        &caption_cell,
                                        bar,
                                        build,
                                    )
                                } else {
                                    wrap_root_in_safe_area(cx, build)
                                }
                            },
                        );
                        ws.caption = caption_cell.get();
                        if !framed {
                            ws.safe_area_root = ws.root;
                        }
                        #[cfg(feature = "hot-reload")]
                        self.hot_reload.adopt(|| cx.loop_waker(), &mut ws);
                        #[cfg(feature = "headless")]
                        self.harness.adopt(&mut ws);
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
                    ws.store.set_dirty_cause(DirtyCause::Layout);
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
                    ws.store.set_dirty_cause(DirtyCause::Text);
                    {
                        let _text = profile::time(Span::Text);
                        ws.commit_text_work();
                        ws.settle_text_edits();
                        // Shape any text (re)declared this frame — a rebuilt
                        // list row, an applied text edit, or a future reactive
                        // text update leaves a pending `TextRequest` — into a
                        // content payload before measure, so a `Fit` text node
                        // sizes to its run. A no-op when nothing declared text
                        // (the steady case).
                        ws.shape_pending_text();
                    }
                    // Incrementally re-place invalidated subtrees; a clean frame
                    // touches nothing.
                    ws.store.set_dirty_cause(DirtyCause::Layout);
                    ws.relayout();
                    // Width-aware text reflow (DL1): the layout just above records
                    // a reflow for each wrap-eligible leaf whose assigned box width
                    // differs from the width it was shaped at. Drain and reshape
                    // those at their assigned width, then relayout so a run whose
                    // wrapped height grew re-places its siblings — bounded, and a
                    // no-op on the steady frame that enqueued none. Runs before
                    // `absorb_measurements` so a virtualized list's height model
                    // sees the final wrapped row heights, not the unconstrained
                    // single-line ones.
                    ws.store.set_dirty_cause(DirtyCause::Text);
                    let reflow_passes = {
                        let _text = profile::time(Span::Text);
                        let passes = ws.reflow_wrapped_text();
                        // Every layout this frame asked for goes to the worker
                        // at once, so it lays them out while the frame renders.
                        ws.dispatch_text_work();
                        passes
                    };
                    // First-frame double-shape visibility: a wrapped `Fill`
                    // paragraph is shaped once unconstrained then once at its width
                    // on the frame it appears/resizes; this surfaces that one-time
                    // cost, gated so a steady frame (zero passes) pays only the
                    // env-var check and nothing prints.
                    if reflow_passes > 0 && std::env::var_os("VISO_DEBUG").is_some() {
                        eprintln!("[viso] text reflow: {reflow_passes} pass(es)");
                    }
                    ws.store.set_dirty_cause(DirtyCause::Layout);
                    // Feed measured row heights back into each list's height model
                    // so a variable-height list corrects its extent and anchor
                    // next frame. Bounded to this frame's newly-mounted rows — no
                    // full-list sweep.
                    virtual_list::absorb_measurements(&ws.store, &mut ws.virtual_lists);
                }
            }
            FramePhase::ResolveStyle => {
                for ws in &mut self.windows {
                    ws.resolve_style();
                }
            }
            FramePhase::UpdateTransforms => {
                self.take_menu_picks(cx);
                for ws in &mut self.windows {
                    ws.update_transforms();
                    // Push the caption's draggable regions to the platform, but only
                    // when this frame's layout moved them (or first established them).
                    // The world boxes are final now; a caption that held still — the
                    // steady case, and every window without one — recomputes a tiny
                    // candidate list, finds it unchanged, and skips the cold
                    // back-channel entirely.
                    if ws.draggable_regions_changed() {
                        cx.set_draggable_regions(ws.window, &ws.draggable_cache);
                    }
                    ws.update_field_marks(cx.frame_now());
                    // Tell the platform where text entry is, so an IME candidate
                    // window and the soft keyboard track the focused control. The
                    // keyboard is toggled only when focus crosses into or out of
                    // text entry, not when the control merely moves.
                    let was_editing = matches!(ws.text_entry, Some(Some(_)));
                    if ws.text_entry_changed() {
                        let entry = ws.text_entry.flatten();
                        cx.set_text_entry(ws.window, entry);
                        if was_editing != entry.is_some() {
                            cx.show_soft_keyboard(ws.window, entry.is_some());
                        }
                    }
                    // A drag carried past the window's edge goes on as the
                    // OS's, where the OS takes one.
                    if let Some(data) = ws.store.drag_out_asked() {
                        let started = cx.start_drag(ws.window, drag_out(&data));
                        ws.store.drag_out_started(started);
                    }
                    ws.push_cursor(cx);
                    // The edit menu follows what a touch made of the focused
                    // field's selection, where the platform has one.
                    let caps = cx.capabilities();
                    if caps.edit_menu && ws.edit_menu_changed(!caps.context_menu) {
                        cx.set_edit_menu(ws.window, ws.edit_menu);
                    }
                    // A secondary click or the menu key asks for the context
                    // menu once; where the system draws one it pops up.
                    if caps.context_menu
                        && ws.context_menu_changed()
                        && let Some(menu) = ws.context_menu
                    {
                        let labels = viso_platform::EditLabels::for_locale(
                            &ws.states.env().environment().locale,
                        );
                        cx.show_context_menu(ws.window, menu, labels);
                    }
                    ws.update_drawn_menu(cx);
                    if caps.loupe && ws.loupe_changed() {
                        cx.set_loupe(ws.window, ws.loupe);
                    }
                }
            }
            FramePhase::BuildPaintChanges => {
                for ws in &mut self.windows {
                    ws.repaint();
                }
            }
            FramePhase::RunEffects => {
                for ws in &mut self.windows {
                    // The effects this frame's flush queued run now, after the
                    // frame presented; what they write is the next frame's
                    // transaction, so that frame is scheduled.
                    ws.store.set_dirty_cause(DirtyCause::Task);
                    // Edits the frame itself asked for (a number field the
                    // focus left settling) apply now; what they report is the
                    // next frame's.
                    if ws.store.has_edit_requests() {
                        ws.settle_text_edits();
                        cx.request_redraw(ws.window);
                    }
                    // What the frame's layout moved reaches the nodes that
                    // report it — a viewport's scroll, a list's visible rows;
                    // their handlers' writes are the next frame's, as the
                    // effects' are.
                    if viso_ui::deliver_notices(&mut ws.store, &ws.virtual_lists, &mut ws.states)
                        > 0
                        && ws.states.has_pending()
                    {
                        cx.request_state_flush(ws.window);
                    }
                    let done = viso_ui::run_effects(&mut ws.store, &mut ws.states, &mut ws.effects);
                    if let Some(cycle) = done.cycle {
                        eprintln!("[viso] {cycle}");
                    }
                    if done.pending {
                        cx.request_redraw(ws.window);
                    }
                    ws.store.set_dirty_cause(DirtyCause::Other);
                }
            }
            FramePhase::UpdateSemantics => {
                for ws in &mut self.windows {
                    let (Some(access), Some(root)) = (&mut ws.access, ws.root) else {
                        continue;
                    };
                    if let Some(update) = access.update(&ws.store, root, ws.dpi.max(1.0)) {
                        cx.update_accessibility(ws.window, update);
                    }
                }
            }
            FramePhase::UploadGpuChanges => {
                for ws in &mut self.windows {
                    ws.upload_gpu_changes();
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
                        // The clock brushes read: this frame's instant, from the
                        // window's first frame, so a headless clock stays
                        // deterministic and a reload keeps it running.
                        let now = cx.frame_now();
                        let origin = *ws.clock_origin.get_or_insert(now);
                        gpu.renderer
                            .set_time(now.saturating_duration_since(origin).as_secs_f32());
                        let (w, h) = gpu.size;
                        let viewport = [w as f32 / scale, h as f32 / scale];
                        // One-shot proof the first frame reached the GPU, gated on
                        // an env var so it costs a single bool check per
                        // steady-state frame and nothing else. Read once, then the
                        // branch dies. Stats must be read before `submit` consumes
                        // the segments.
                        if ws.awaiting_first_frame {
                            ws.awaiting_first_frame = false;
                            if std::env::var_os("VISO_DEBUG").is_some() {
                                let stats = gpu.renderer.frame_stats();
                                let recompute = ws.recompute;
                                eprintln!(
                                    "viso: first frame submitting {w}x{h} {stats:?} {recompute:?}"
                                );
                                let a = self.assets.counters();
                                eprintln!(
                                    "viso: assets decodes={} decode_failures={} uploads={} upload_bytes={} evictions={} animation_frames={} video_frames={}",
                                    a.decodes,
                                    a.decode_failures,
                                    a.uploads,
                                    a.upload_bytes,
                                    a.evictions,
                                    self.assets.playback.frames(),
                                    ws.videos.frames,
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
                        #[cfg(any(feature = "hot-reload", feature = "headless"))]
                        if self.profiler.is_on() {
                            let stats = gpu.renderer.frame_stats();
                            for (count, value) in [
                                (Count::Primitives, u64::from(stats.visible_primitives)),
                                (Count::DrawCalls, stats.draw_calls as u64),
                                (Count::Batches, stats.batches as u64),
                                (Count::Instances, stats.instances as u64),
                                (Count::UploadBytes, stats.gpu_upload_bytes as u64),
                            ] {
                                self.profiler.count(count, value);
                            }
                        }
                        #[cfg(feature = "headless")]
                        __test_support::FRAME_PROBE.with(|probe| {
                            if let Ok(mut probe) = probe.try_borrow_mut()
                                && let Some(probe) = probe.as_mut()
                            {
                                probe(&gpu.renderer.frame_stats(), ws.uploaded);
                            }
                        });
                        if let Some(surface) = gpu.surface {
                            #[cfg(any(feature = "hot-reload", feature = "headless"))]
                            let before = gpu.renderer.damage_stats();
                            gpu.renderer.submit(
                                &mut gpu.backend,
                                surface,
                                [0.1, 0.1, 0.1, 1.0],
                                viewport,
                            );
                            #[cfg(any(feature = "hot-reload", feature = "headless"))]
                            if self.profiler.is_on() {
                                let after = gpu.renderer.damage_stats();
                                for (count, value) in [
                                    (Count::DamagePx, after.damage_px),
                                    (Count::DamageRects, u64::from(after.damage_rects)),
                                    (
                                        Count::FullFrameFallbacks,
                                        after.full_frame_fallbacks - before.full_frame_fallbacks,
                                    ),
                                ] {
                                    self.profiler.count(count, value);
                                }
                            }
                        }
                    }
                }
                // The launch window's GPU stands for the frame's.
                #[cfg(any(feature = "hot-reload", feature = "headless"))]
                if let Some(gpu) = self.windows.first().and_then(|ws| ws.gpu.as_ref()) {
                    self.profiler.presented(gpu.backend.frame_epoch());
                }
            }
            FramePhase::PostFrameCleanup => {
                #[cfg(any(feature = "hot-reload", feature = "headless"))]
                if self.profiler.is_on() {
                    self.count_frame();
                }
                for ws in &mut self.windows {
                    // A timer the frame's own passes asked for (a scroll bar's
                    // fade) is armed now, so an idle loop wakes for it.
                    ws.store.take_timer_requests(&mut ws.timer_requests);
                    for req in ws.timer_requests.drain(..) {
                        ws.timers.arm_request(req, cx.frame_now());
                    }
                    // What the frame marked, for the inspector following the
                    // window.
                    ws.store.set_dirty_cause(DirtyCause::Other);
                    #[cfg(any(feature = "hot-reload", feature = "headless"))]
                    if let Some(journal) = &mut ws.journal {
                        journal.settle(&mut ws.store, &ws.states);
                    }
                    // The incremental passes have consumed this frame's
                    // invalidation; clear every node's dirty set so the next frame
                    // starts clean and an idle frame recomputes nothing.
                    ws.store.clear_dirty();
                    ws.style_seen = 0;
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
                    if ws.is_animating()
                        || ws.text_work_pending()
                        || viso_ui::work::inline_pending()
                    {
                        cx.request_redraw(ws.window);
                    }
                }
                self.watch_budget(cx);
                #[cfg(feature = "pdf")]
                self.drive_pdfs(cx);
                #[cfg(feature = "headless")]
                if let (Some(script), Some(ws)) = (&mut self.script, self.windows.first()) {
                    let mounts = self.harness.mounts();
                    let assets = &self.assets;
                    script.after_frame(
                        ws,
                        &|| headless::census(ws, mounts),
                        Some(&|| inspect::window_detail(ws, assets)),
                        cx,
                    );
                }
                #[cfg(any(feature = "hot-reload", feature = "headless"))]
                if let Some(player) = &mut self.player {
                    player.after_frame(cx);
                }
                #[cfg(feature = "hot-reload")]
                self.hot_reload.play(&self.windows, cx);
                #[cfg(any(feature = "hot-reload", feature = "headless"))]
                self.end_profiled_frame(cx);
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
        self.places.closed(window);
        if let Some(ws) = self.window_mut(window) {
            persist::suspend(&mut ws.store, &ws.states);
            ws.effects.cancel_all();
        }
        #[cfg(feature = "hot-reload")]
        self.hot_reload.close(window);
        #[cfg(any(feature = "hot-reload", feature = "headless"))]
        if let Some(recorder) = &mut self.recorder {
            recorder.closed(window);
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
        let scripted = {
            #[cfg(feature = "headless")]
            {
                self.script
                    .as_ref()
                    .is_some_and(scenario::ScriptRunner::wants_frames)
            }
            #[cfg(not(feature = "headless"))]
            {
                false
            }
        } || {
            #[cfg(feature = "hot-reload")]
            {
                self.hot_reload.wants_frames()
            }
            #[cfg(not(feature = "hot-reload"))]
            {
                false
            }
        };
        // A PDF document being written lays out and settles over frames.
        #[cfg(feature = "pdf")]
        let writing = !self.pdf_jobs.is_empty();
        #[cfg(not(feature = "pdf"))]
        let writing = false;
        scripted
            || writing
            || self.windows.iter().any(|w| {
                w.is_animating() || w.text_work_pending() || viso_ui::work::inline_pending()
            })
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
            .flat_map(|w| {
                w.timers
                    .earliest()
                    .into_iter()
                    .chain(w.caret_due)
                    .chain(w.animation_due)
                    .chain(w.video_step.due)
            })
            .chain(self.places.deadline())
            .min()
    }
}

/// Lower runtime-tier modifier state onto the UI-tier mirror (same fields, a
/// crate-boundary copy).
/// Whether Alt reaches the access keys and the menu bar: off Apple systems,
/// where Option types characters instead.
fn alt_access() -> bool {
    #[cfg(feature = "headless")]
    if let Some(on) = __test_support::ALT_ACCESS.with(std::cell::Cell::get) {
        return on;
    }
    !viso_runtime::primary_is_logo()
}

/// What a key reaches among the access keys: a letter or digit with Alt
/// alone held, or the menu bar's F10.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccessKey {
    Char(char),
    MenuBar,
}

/// The access key `key` with `modifiers` held presses, if any.
fn access_key_of(key: viso_runtime::Key, modifiers: Modifiers) -> Option<AccessKey> {
    use viso_runtime::Key as K;
    let plain = !(modifiers.control || modifiers.logo || modifiers.shift);
    if key == K::F10 && plain && !modifiers.alt {
        return Some(AccessKey::MenuBar);
    }
    if !modifiers.alt || modifiers.control || modifiers.logo {
        return None;
    }
    const LETTERS: [K; 26] = [
        K::A,
        K::B,
        K::C,
        K::D,
        K::E,
        K::F,
        K::G,
        K::H,
        K::I,
        K::J,
        K::K,
        K::L,
        K::M,
        K::N,
        K::O,
        K::P,
        K::Q,
        K::R,
        K::S,
        K::T,
        K::U,
        K::V,
        K::W,
        K::X,
        K::Y,
        K::Z,
    ];
    const DIGITS: [K; 10] = [
        K::Digit0,
        K::Digit1,
        K::Digit2,
        K::Digit3,
        K::Digit4,
        K::Digit5,
        K::Digit6,
        K::Digit7,
        K::Digit8,
        K::Digit9,
    ];
    if let Some(at) = LETTERS.iter().position(|&l| l == key) {
        return Some(AccessKey::Char((b'a' + at as u8) as char));
    }
    DIGITS
        .iter()
        .position(|&d| d == key)
        .map(|at| AccessKey::Char((b'0' + at as u8) as char))
}

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
    // `cx.services()` on a handler's context: the file dialog, share sheet and
    // permission prompts an ordinary app reaches for.
    pub use crate::services::ServicesExt;
    pub use viso_ui::context::AppCx;
    pub use viso_ui::dirty::DirtyClass;
    pub use viso_ui::node::NodeId;
    // Scene authoring: the mental model for a real app is `Application::build`
    // declaring nodes and wiring reactive state, so the authoring context, the
    // layout styles, the semantics facts, and the state cell handles belong in
    // the default set (commonly used, stable, unambiguous).
    pub use viso_ui::{
        AdaptiveColumns, BuildCx, CursorIcon, FlexStyle, GridPlacement, GridStyle, InputKind,
        LeafStyle, Role, Semantics, SpanStyle, StateId, StateValue, TextRequest, TrackMax,
        TrackSizing, VirtualListStyle,
    };

    // Tier 1 widgets: the base layout container, the static text control, the
    // texture-backed image control, and the vector icon control — plus the Tier 2
    // interactive controls, Button, CheckBox, Toggle, Chip, Stepper, RadioGroup,
    // Slider, and TextInput, the Tier 3 layout structures — the Scroll viewport, the
    // virtualized VirtualList, the two-dimensional Grid, and the draggable-pane
    // Splitter — and the Tier 4 navigation/overlay controls, the panel-switching
    // Tabs, the page-stack NavigationStack (with its app-captured NavHandle for
    // programmatic push/pop), the Popup (a persistent anchor with a floating
    // top-layer content, with its app-captured PopupHandle for programmatic
    // open/close/toggle), the Modal (a full-surface dialog that dims the scene
    // and traps focus while open, with its app-captured ModalHandle), and the
    // Sheet (an edge panel that slides in over a dimming scrim, rests at its
    // detents and drags off, trapping focus while open, with its app-captured
    // SheetHandle for programmatic open/close/toggle). A
    // widget is a `Component` an app authors and builds into its `BuildCx`, so
    // these belong in the default set as they land.
    pub use viso_widgets::{
        Avatar, Badge, Button, ButtonStyle, Calendar, CheckBox, CheckBoxStyle, Chip, ChipStyle,
        ColorPicker, DatePicker, Grid, GridViewStyle, Icon, IconStyle, Image, ImageStyle, Label,
        LabelStyle, Modal, ModalHandle, ModalHandleSlot, ModalStyle, NavHandle, NavHandleSlot,
        NavigationStack, NavigationStackStyle, NumberField, Popup, PopupHandle, PopupHandleSlot,
        PopupStyle, ProgressIndicator, RadioGroup, RadioStyle, RangeSlider, Scroll,
        ScrollViewStyle, SearchField, SearchFieldHandle, SearchFieldHandleSlot, SegmentedControl,
        SegmentedStyle, Sheet, SheetDetent, SheetEdge, SheetHandle, SheetHandleSlot, SheetStyle,
        Slider, SliderStyle, Spinner, SplitAt, Splitter, SplitterStyle, Stepper, StepperStyle,
        Tabs, TabsStyle, TextInput, TextInputStyle, TimePicker, Toast, ToastEdge, ToastHandle,
        ToastHandleSlot, ToastStyle, Toggle, ToggleStyle, View, ViewStyle, VirtualList,
        VirtualListViewStyle, avatar, badge, button, calendar, checkbox, chip, color_picker,
        date_picker, dot_badge, grid, icon, image, knob, label, link_label, modal,
        navigation_stack, number_field, popup, progress_bar, progress_ring, radio_group,
        range_slider, rich_text, scroll, search_field, segmented_control, sheet, slider, spinner,
        splitter, stepper, tabs, text_input, time_picker, toast, toggle, view, virtual_list,
    };
    // Forms: a checked field, the form gathering fields, a group of fields.
    pub use viso_widgets::{Field, FieldValue, Fieldset, Form, field, fieldset};
    // The data grid: a table that scrolls both ways, with its handle.
    pub use viso_widgets::{
        DataGrid, DataGridHandle, DataGridHandleSlot, GridSelect, GridSelection, data_grid,
        grid_column,
    };
    // The top-level Window control (public application lifecycle): a window
    // is not a node control — it does not enter the UI tree — so it appears as an
    // application-level handle, not a `Component`. `window(cfg).content(..).open(ev)`
    // opens another OS window mid-session and hands back a `WindowHandle` to close
    // it. `WindowConfig` is the (title, size) shape the author fills in. These are
    // commonly used, stable, and unambiguous, so they belong in the default set.
    pub use crate::{WindowBuilder, WindowChrome, WindowConfig, WindowHandle, window};
    // The application-menu model (application lifecycle): an app returns a `Menu` tree
    // from `Application::menu`, wiring custom items to `MenuCommandId`s it picks
    // and shortcuts through `Accel`; standard entries use `SystemAction`. Menus
    // are an application-level concern like `window`, so they belong here.
    pub use viso_platform::{Accel, Menu, MenuCommandId, SystemAction};
    // The declarative view-fragment entry point: a small local `ui! { … }`
    // fragment lowers, at Rust compile time, to a static `BuildCx` builder closure.
    pub use crate::ui;
    // 2D geometry, owned by `viso-math`.
    pub use viso_math::{Point, Rect, Vec2};
    // `component!`, `view!` and `asset!` are exported from the crate root
    // (`viso::component!`, …), not from here.
}

// -- Advanced escape hatches. Opt-in, clearly namespaced. --
pub mod ui {
    pub use viso_ui::*;
}
pub mod widgets {
    pub use viso_widgets::*;
}
pub mod math {
    pub use viso_math::*;
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
        let _root = wrap_root_with_caption(&mut cx, "Title", caption, &caption_cell, None, |cx| {
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
