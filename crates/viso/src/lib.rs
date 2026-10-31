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
pub mod paged;
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
pub use videos::cameras;

#[cfg(feature = "hot-reload")]
mod hot_reload;
#[cfg(any(feature = "hot-reload", feature = "headless"))]
mod tape;
mod window;
pub use footprint::{BudgetExceeded, BudgetKind, Footprint, MemoryBudget};
pub use window::{WindowBuilder, WindowHandle, window};

pub use viso_ui::context::AppCx;

// Where the app stands in its OS lifecycle, as `Application::on_lifecycle`
// is told.
pub use viso_runtime::Lifecycle;

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
pub use viso_ui::{WindowChrome, WindowConfig, WindowMaterial};

// The DSL entry points `ui!`, `component!` and `view!` live in the compile-time-only
// `viso-ui-macros` crate, which depends on neither crate its expansion builds
// with; the expansion names them through `viso::__private`, so an app that
// depends on `viso` alone expands them.
pub use viso_ui_macros::{asset, component, ui, view};

/// The crates the expansion of `ui!`, `component!`, `view!` and `asset!`
/// builds with. Not an API: its paths change with the macros.
#[doc(hidden)]
pub mod __private {
    #[cfg(feature = "web-hybrid")]
    pub use crate::dom::{__island_values, __link_island};
    #[cfg(any(feature = "web-dom", feature = "web-hybrid"))]
    pub use crate::dom::{__offer_page, __offer_view};
    pub use viso_behavior;
    #[cfg(any(feature = "web-dom", feature = "web-hybrid"))]
    pub use viso_dom;

    /// [`viso_view::__link_game`], with the games' audio linked first.
    pub fn __link_game(
        cx: &mut viso_ui::BuildCx<'_>,
        host: &std::rc::Rc<std::cell::RefCell<viso_view::ViewHost>>,
        root: viso_ui::NodeId,
        reads: &[(usize, u8, &str, &str)],
    ) {
        crate::audio::link();
        viso_view::__link_game(cx, host, root, reads);
    }
    pub use viso_ende;
    pub use viso_platform;
    pub use viso_shader;
    pub use viso_ui;
    pub use viso_view;
}

mod behavior;
#[cfg(any(feature = "web-dom", feature = "web-hybrid"))]
mod dom;
#[cfg(all(feature = "web-dom", feature = "web-hybrid"))]
compile_error!("`web-dom` and `web-hybrid` are two builds: turn one on");
mod stdio;

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

mod app;
pub use app::{Application, run, version};

#[doc(hidden)]
#[path = "test_support.rs"]
pub mod __test_support;

mod driver;

/// The curated default prelude. Kept to a small, stable, low-ambiguity set —
/// GPU/backend/internal-compiler types never appear here.
pub mod prelude;

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
