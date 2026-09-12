//! Public application-level window control: [`window`], [`WindowBuilder`], and
//! [`WindowHandle`].
//!
//! A window is not a node control. It does not enter the UI tree, has no
//! `build`/layout/paint, and emits no primitives — it is a top-level OS surface
//! that *hosts* a tree. So unlike Tabs, Popup, Modal, Sheet, or Toast (all node
//! controls an app builds into its `BuildCx`), a window is an *application-level
//! handle*: the app opens one from within an event handler and holds a
//! [`WindowHandle`] to close it later. This is the single place window authoring
//! appears in the public surface.
//!
//! Opening is deferred. A handler cannot open an OS window at call time — the UI
//! tier holds no platform seam, and the new window's node store does not exist
//! yet — so [`WindowBuilder::open`] records a tracked open request (the same
//! deferred seam an animation or a timer rides) and hands back a [`WindowHandle`]
//! immediately. The facade services the request on the next frame: it creates
//! the OS window, brings its state up, runs the deferred content closure against
//! the fresh store, and writes the new window's id into the handle's shared
//! slot. Until that frame runs, [`WindowHandle::id`] reads `None`.
//!
//! ```no_run
//! use viso::prelude::*;
//!
//! # fn demo(ev: &mut viso::ui::EventCx<'_>) {
//! // Open a second window from within an event handler and keep its handle.
//! let handle = window(WindowConfig {
//!     title: "Inspector".to_string(),
//!     size: (480.0, 640.0),
//!     ..Default::default()
//! })
//! .content(|build| {
//!     let root = build.flex(FlexStyle::default(), |_cx| {});
//!     Some(root.id())
//! })
//! .open(ev);
//!
//! // …later, from another handler, close it programmatically:
//! handle.close(ev);
//! # }
//! ```

use std::cell::Cell;
use std::rc::Rc;

use viso_platform::WindowId;
use viso_ui::{BuildCx, EventCx, NodeId, WindowConfig, WindowIdSlot};

/// Begin opening a top-level window configured by `config`. Returns a
/// [`WindowBuilder`]; set its tree with [`content`](WindowBuilder::content), then
/// [`open`](WindowBuilder::open) it from within an event handler to record the
/// deferred open and receive a [`WindowHandle`].
///
/// The window does not exist until the next frame services the request; see the
/// [module docs](self) for the deferred-open contract.
#[inline]
pub fn window(config: WindowConfig) -> WindowBuilder {
    WindowBuilder {
        config,
        content: None,
    }
}

/// The deferred build of a window's tree — the closure the app passed to
/// [`WindowBuilder::content`]. Run once by the facade against the new window's
/// fresh store, returning its root, exactly as a first-window build does.
type ContentFn = Box<dyn FnOnce(&mut BuildCx) -> Option<NodeId>>;

/// A builder for a top-level window, produced by [`window`]. Configure the tree
/// with [`content`](Self::content), then [`open`](Self::open) it.
///
/// Opening without content is allowed — an empty window (no root) — matching a
/// first window whose `Application::build` declares nothing.
pub struct WindowBuilder {
    config: WindowConfig,
    content: Option<ContentFn>,
}

impl WindowBuilder {
    /// Set the closure that builds the window's tree. It runs once, deferred,
    /// against the new window's store (which does not exist at call time), and
    /// returns the tree's root — exactly as `Application::build` does for the
    /// first window. Calling `content` again replaces the previous closure.
    #[inline]
    pub fn content(mut self, build: impl FnOnce(&mut BuildCx) -> Option<NodeId> + 'static) -> Self {
        self.content = Some(Box::new(build));
        self
    }

    /// Record the deferred open on `ev` and return a [`WindowHandle`] that tracks
    /// the window. The window is created — and its content built — on the next
    /// frame; until then [`WindowHandle::id`] reads `None`. Call from within an
    /// event handler.
    pub fn open(self, ev: &mut EventCx<'_>) -> WindowHandle {
        let id_slot: WindowIdSlot = Rc::new(Cell::new(None));
        let content = self.content.unwrap_or_else(|| Box::new(|_| None));
        ev.request_open_window_tracked(self.config, content, id_slot.clone());
        WindowHandle { id_slot }
    }
}

/// A cheap-to-clone handle to a window opened through [`window`]. It shares the
/// id-backfill slot the facade writes the opened window's id into, so it can
/// observe when the window exists ([`id`](Self::id)) and close it later
/// ([`close`](Self::close)).
///
/// The handle carries no borrow of window state — only the shared id cell — so it
/// is safe to clone into other handlers and outlive the frame that created it.
#[derive(Clone)]
pub struct WindowHandle {
    /// Shared with the open request: `None` until the facade opens the window,
    /// then the raw window id. Reading it is how the handle learns the id.
    id_slot: WindowIdSlot,
}

impl WindowHandle {
    /// The opened window's id, or `None` if the deferred open has not yet been
    /// serviced (the frame after [`WindowBuilder::open`]) — or if the window has
    /// since closed and the facade cleared the slot.
    #[inline]
    pub fn id(&self) -> Option<WindowId> {
        self.id_slot.get().map(WindowId)
    }

    /// Request that this window close. A no-op if the window has not opened yet
    /// (nothing to close) or has already closed. Deferred like the open: the
    /// facade drains it next frame and asks the platform to close the window,
    /// which routes through the single teardown path (`WindowClosed` →
    /// `on_window_closed`) — the same path a user-driven OS close takes. Call
    /// from within an event handler.
    pub fn close(&self, ev: &mut EventCx<'_>) {
        if let Some(raw) = self.id_slot.get() {
            ev.request_close_window(raw);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_ui::{BindingTable, StateStore};

    // A window is authored through the facade builder, so its unit tests exercise
    // the builder against a bare `EventCx` (no store, no router) and read back the
    // deferred requests it records — the same three-hop seam the facade drains.

    #[test]
    fn open_records_a_tracked_request_and_yields_an_unresolved_handle() {
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let mut ev = EventCx::__new(&mut states, &bindings);

        let handle = window(WindowConfig {
            title: "inspector".to_string(),
            size: (480.0, 640.0),
            ..Default::default()
        })
        .content(|_build| None)
        .open(&mut ev);

        // Before the facade services the open, the handle knows no id.
        assert_eq!(handle.id(), None);

        // Exactly one open request was recorded, carrying the config and a
        // tracking slot (the cell the handle shares).
        let opens = ev.__take_window_opens();
        assert_eq!(opens.len(), 1);
        assert_eq!(opens[0].config.title, "inspector");
        assert_eq!(opens[0].config.size, (480.0, 640.0));
        assert!(
            opens[0].id_slot.is_some(),
            "builder must record a tracked request so the handle can observe the id"
        );
    }

    #[test]
    fn the_facade_backfilling_the_slot_resolves_the_handle_id() {
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let mut ev = EventCx::__new(&mut states, &bindings);

        let handle = window(WindowConfig::default())
            .content(|_build| None)
            .open(&mut ev);

        // Simulate the facade opening the window: it writes the raw id into the
        // shared slot right after `create_window`.
        let opens = ev.__take_window_opens();
        opens[0].id_slot.as_ref().unwrap().set(Some(7));

        assert_eq!(handle.id(), Some(WindowId(7)));
    }

    #[test]
    fn close_on_an_open_handle_records_a_close_request() {
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let mut ev = EventCx::__new(&mut states, &bindings);

        let handle = window(WindowConfig::default())
            .content(|_build| None)
            .open(&mut ev);
        let opens = ev.__take_window_opens();
        opens[0].id_slot.as_ref().unwrap().set(Some(4));

        handle.close(&mut ev);

        let closes = ev.__take_window_closes();
        assert_eq!(closes, vec![4]);
    }

    #[test]
    fn close_before_the_window_opens_is_a_no_op() {
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let mut ev = EventCx::__new(&mut states, &bindings);

        let handle = window(WindowConfig::default())
            .content(|_build| None)
            .open(&mut ev);
        let _ = ev.__take_window_opens(); // slot never back-filled → id stays None.

        handle.close(&mut ev);

        // Nothing to close yet, so no close request is recorded.
        assert!(ev.__take_window_closes().is_empty());
    }

    #[test]
    fn a_window_may_open_without_content() {
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let mut ev = EventCx::__new(&mut states, &bindings);

        // No `.content(..)` — an empty window, matching a first window whose
        // build declares nothing.
        let _handle = window(WindowConfig::default()).open(&mut ev);

        let opens = ev.__take_window_opens();
        assert_eq!(opens.len(), 1);
    }
}
