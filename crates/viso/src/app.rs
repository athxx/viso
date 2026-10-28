//! The application contract and its entry point.

use viso_runtime::Scheduler;
use viso_ui::BuildCx;

use crate::driver::AppDriver;
use crate::{AppCx, ImageLoader, Lifecycle, MemoryBudget, WindowConfig, services, shell};

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

    /// Answer a change in where the app stands in its OS lifecycle. `cx` is
    /// the launch window's, as [`on_open_url`](Self::on_open_url) gets.
    ///
    /// - [`Lifecycle::Suspended`]: called before the views' `@persist`
    ///   states are made durable, so what it writes to one is kept.
    /// - [`Lifecycle::Resumed`]: called once every window has asked for a
    ///   fresh frame.
    /// - [`Lifecycle::LowMemory`]: called once the framework has dropped
    ///   its own rebuildable caches; drop the app's here.
    ///
    /// The default ignores every change.
    fn on_lifecycle(&mut self, cx: &mut viso_ui::EventCx<'_>, event: Lifecycle) {
        let _ = (cx, event);
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
/// exists — CI, tests), builds an `AppDriver` that bridges the runtime's
/// UI-agnostic [`viso_runtime::FrameDriver`] to the user's [`Application`] and
/// its [`AppCx`], and runs the scheduler until the last window closes.
pub fn run<A: Application>() {
    // `viso run` executes a new build once before restarting on it, for the
    // OS to check the executable while the old app runs on.
    #[cfg(feature = "hot-reload")]
    if viso_platform::launch_var(viso_view::dev::wire::DEV_PREWARM_ENV).is_some() {
        return;
    }
    if let Some(api) = viso_platform::launch_var(viso_gpu::API_VAR) {
        viso_gpu::ask_api(&api);
    }
    #[cfg(feature = "headless")]
    {
        crate::headless::run::<A>();
        return;
    }
    // A page's DOM is the browser's: no window opens and no frame runs.
    #[cfg(feature = "web-dom")]
    {
        crate::dom::run::<A>();
        return;
    }
    // A headless fallback has no OS behind its services.
    #[cfg_attr(
        any(feature = "headless", feature = "web-dom"),
        allow(unreachable_code)
    )]
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

/// The version of Viso the app is built with, as its package names it.
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_version_is_the_package_version() {
        assert_eq!(super::version(), env!("CARGO_PKG_VERSION"));
        assert!(!super::version().is_empty());
    }
}
