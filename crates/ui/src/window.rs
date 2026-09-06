//! UI-tier window-open requests: the deferred payload a handler produces to ask
//! the application to open another top-level OS window mid-session.
//!
//! A control cannot open a window itself — the UI tier holds no platform seam
//! (`viso-ui` does not depend on `viso-platform`, section 3.5), and even if it
//! did, the new window's node store does not exist yet at the moment a handler
//! runs. So opening a window rides the same deferred-request seam as an
//! animation or a timer: a handler records a [`WindowOpenRequest`] through
//! [`EventCx::request_open_window`](crate::context::EventCx::request_open_window),
//! the router hands it to the store's handoff queue, and the facade drains that
//! queue on the next frame — where it *does* hold a live scheduling context —
//! creates the OS window, builds its fresh store, and runs the deferred [`build`]
//! closure against it.
//!
//! [`build`]: WindowOpenRequest::build
//!
//! Because the UI tier has no `WindowId`/`WindowConfig` of its own, this module
//! carries UI-tier mirrors: a small [`WindowConfig`] value the facade translates
//! into the platform config at the drain point, and a raw `u32` window id for
//! close requests (the facade wraps it back into the platform `WindowId`). The
//! `build` closure takes a [`BuildCx`](crate::component::BuildCx) — itself a
//! UI-tier type — so it can construct the new window's tree exactly as
//! [`Application::build`](../../viso/trait.Application.html) does for the first
//! window, deferred until the store is ready.

use crate::component::BuildCx;
use crate::node::NodeId;

/// UI-tier mirror of the platform window configuration. The UI tier cannot name
/// the platform `WindowConfig` (section 3.5 forbids the `viso-ui -> viso-platform`
/// edge), so a handler describes the window it wants with this small value and
/// the facade translates it into the platform config when it opens the window.
///
/// Kept deliberately minimal — title and logical size — matching the platform
/// config's current surface. Future window attributes (min/max size, resizable)
/// extend both in step.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowConfig {
    /// The window title.
    pub title: String,
    /// The initial logical (pre-scale) size, in points: `(width, height)`.
    pub size: (f64, f64),
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            title: "Viso".to_string(),
            size: (800.0, 600.0),
        }
    }
}

/// The deferred build of a new window's tree, run once by the facade after it
/// creates the OS window and its fresh node store. Boxed because each capture is
/// distinct (the closure the app author passed to `window(...).content(...)`),
/// and invoked at most once — off the per-frame hot path.
///
/// It returns the new tree's root [`NodeId`] (or `None` for an empty window),
/// exactly as a first-window build does; the facade records it as that window's
/// root and marks it dirty for the next frame.
type BuildFn = Box<dyn FnOnce(&mut BuildCx) -> Option<NodeId>>;

/// A deferred request to open a top-level window, produced by a handler through
/// [`EventCx::request_open_window`](crate::context::EventCx::request_open_window)
/// and carried — like a [`TranslateAnim`](crate::animation::TranslateAnim)
/// request — through the store's handoff queue to the facade, which opens the OS
/// window and builds its tree the next frame.
///
/// It holds the UI-tier [`WindowConfig`] rather than a resolved platform config:
/// the handler that records it cannot name the platform type, so the facade
/// translates it where it holds the platform seam. The [`build`](Self::build)
/// closure is deferred for the same reason the config is — the new window's
/// store does not exist when the handler runs, so the tree is built later,
/// against the store the facade creates.
pub struct WindowOpenRequest {
    /// How the new window should be configured (title, size).
    pub config: WindowConfig,
    /// Builds the new window's tree once its store exists, returning its root.
    pub build: BuildFn,
}

impl WindowOpenRequest {
    /// A request to open a window configured by `config`, whose tree `build`
    /// constructs once the facade has created the window's store.
    pub fn new(
        config: WindowConfig,
        build: impl FnOnce(&mut BuildCx) -> Option<NodeId> + 'static,
    ) -> Self {
        Self {
            config,
            build: Box::new(build),
        }
    }
}
