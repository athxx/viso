//! Phase-specific contexts.
//!
//! Instead of one omnipotent context, each frame phase gets a purpose-specific
//! one. This is both an architectural constraint and a performance / reasoning
//! tool: a `LayoutCx` cannot launch a network request, create a window, or
//! submit arbitrary GPU work.
//!
//! Phase 0 defines these as marker types so signatures like
//! `fn layout(&mut self, cx: &mut LayoutCx<'_>, ..)` can be written against a
//! stable contract before the real capabilities are filled in.

use core::marker::PhantomData;
use std::any::Any;
use std::future::Future;

use crate::animation::TranslateAnim;
use crate::binding::BindingTable;
use crate::component::NodeStore;
use crate::input::{DispatchPhase, ImeEvent, KeyEvent, PointerEvent, PointerId};
use crate::layout::Vec2;
use crate::node::NodeId;
use crate::state::{StateId, StateStore, StateValue};
use crate::task::{self, TaskFuture, TaskId, TaskOps};
use crate::text_edit::EditIntent;
use crate::timer::TimerRequest;
use crate::window::{WindowConfig, WindowIdSlot, WindowOpenRequest};
use viso_render::Rect;

macro_rules! phase_cx {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        pub struct $name<'a> {
            _life: PhantomData<&'a mut ()>,
        }
        impl<'a> $name<'a> {
            #[doc(hidden)]
            pub fn __new() -> Self {
                Self { _life: PhantomData }
            }
        }
    };
}

/// Application-scope context: create windows, register services, and load the
/// fonts the app wants to render with.
///
/// Unlike the marker phase contexts, `AppCx` holds live session state: the raw
/// font faces the app registers at init. Viso bundles no default font — the
/// default UI text is resolved from the OS at runtime — so an app that wants a
/// specific typeface loads it here, ahead of the system fallbacks. A loaded face
/// applies to every window the session opens, including windows opened later.
///
/// Requests are *recorded*, not applied: [`load_font`](Self::load_font) pushes
/// the bytes onto a queue the facade drains after [`Application::new`] returns,
/// mirroring how every other init/handler intent (window opens, timers) reaches
/// the driver. The cx holds no font stack itself — that lives per window, one
/// layer down, which the cx cannot reach.
///
/// [`Application::new`]: the app entry point that receives this context.
pub struct AppCx<'a> {
    _life: PhantomData<&'a mut ()>,
    /// Raw sfnt face bytes the app asked to load, in call order. The facade
    /// drains this after `Application::new` and loads each into every window's
    /// shaper as the window opens, so the first-loaded face becomes the primary
    /// (a user face sits ahead of any system fallback). Empty for an app that
    /// loads no font and relies entirely on the system default.
    fonts: Vec<Box<[u8]>>,
    /// The faces packaged from `assets/fonts/`, if the app asked for them.
    packaged: viso_text::PackagedFonts,
    /// What the app keeps across a Rust warm restart, by name.
    #[cfg(feature = "hot-reload")]
    dev_states: Vec<DevState>,
}

/// State an app keeps across a Rust warm restart of `viso run`: the bytes
/// `capture` returns in the old process are handed to `restore` in the new one.
#[cfg(feature = "hot-reload")]
#[doc(hidden)]
pub struct DevState {
    pub name: String,
    pub capture: Box<dyn Fn() -> Vec<u8>>,
    pub restore: DevRestore,
}

/// What takes a [`DevState`]'s bytes back in the new process.
#[cfg(feature = "hot-reload")]
#[doc(hidden)]
pub type DevRestore = Box<dyn FnOnce(&[u8])>;

impl<'a> AppCx<'a> {
    #[doc(hidden)]
    pub fn __new() -> Self {
        Self {
            _life: PhantomData,
            fonts: Vec::new(),
            packaged: viso_text::PackagedFonts::default(),
            #[cfg(feature = "hot-reload")]
            dev_states: Vec::new(),
        }
    }

    /// Register a font face from raw sfnt bytes (a `.ttf` / `.otf`, or the
    /// decompressed bytes of a web font), to render with instead of the system
    /// default. The first face loaded becomes the primary; later faces and the
    /// system fallbacks sit behind it, covering scripts the primary lacks.
    ///
    /// The bytes are taken now and parsed later, once a window exists to hold the
    /// font stack — so this cannot report a parse error here. A face whose bytes
    /// are not a valid sfnt is skipped when the window loads it, leaving the
    /// system default in place. This applies to every window the session opens.
    pub fn load_font(&mut self, bytes: impl Into<Vec<u8>>) {
        self.fonts.push(bytes.into().into_boxed_slice());
    }

    /// Register a font face read from a file on disk. Reads the whole file now
    /// and records its bytes like [`load_font`](Self::load_font); the parse
    /// happens later when a window loads the stack. Returns the read error if the
    /// file cannot be read, so a missing path surfaces at the call site rather
    /// than silently falling back to the system default.
    pub fn load_font_file(&mut self, path: impl AsRef<std::path::Path>) -> std::io::Result<()> {
        let bytes = std::fs::read(path)?;
        self.fonts.push(bytes.into_boxed_slice());
        Ok(())
    }

    /// Render with the faces packaged from the crate's `assets/fonts/`:
    /// `cx.use_packaged_fonts(viso::packaged_fonts!())`.
    ///
    /// The UI text binds to a packaged text family (one covering Latin if any
    /// does) ahead of the system UI face, and every packaged face is tried
    /// before the system when a run needs a fallback, so a packaged emoji or
    /// CJK face draws those runs. Nothing is parsed until a face is first
    /// used. A face registered with [`load_font`](Self::load_font) still
    /// becomes the primary ahead of the packaged UI family. Calling this again
    /// replaces the earlier set.
    pub fn use_packaged_fonts(&mut self, fonts: viso_text::PackagedFonts) {
        self.packaged = fonts;
    }

    /// The packaged faces, for the facade to install into each window's
    /// shaper.
    #[doc(hidden)]
    pub fn __packaged_fonts(&self) -> viso_text::PackagedFonts {
        self.packaged
    }

    /// Take the registered font faces for the facade to load into each window's
    /// shaper. Called once after `Application::new`; leaves the queue empty.
    #[doc(hidden)]
    pub fn __take_fonts(&mut self) -> Vec<Box<[u8]>> {
        core::mem::take(&mut self.fonts)
    }

    /// Keep state of the app's own across a Rust warm restart: when `viso run`
    /// restarts the app after a Rust edit, the bytes `capture` returns in the
    /// old process are handed to `restore` in the new one, before its first
    /// frame, for the registration of the same `name`. State the views and
    /// games hold is carried already; this is for what the app keeps itself,
    /// such as a navigation stack. A later registration of a name replaces
    /// the earlier one.
    ///
    /// Only a development build run by `viso run` keeps anything: elsewhere
    /// the closures are dropped here and never called. The bytes stay in the
    /// memory of `viso run`, never written to disk or logged.
    pub fn dev_state(
        &mut self,
        name: impl Into<String>,
        capture: impl Fn() -> Vec<u8> + 'static,
        restore: impl FnOnce(&[u8]) + 'static,
    ) {
        #[cfg(feature = "hot-reload")]
        {
            let name = name.into();
            self.dev_states.retain(|state| state.name != name);
            self.dev_states.push(DevState {
                name,
                capture: Box::new(capture),
                restore: Box::new(restore),
            });
        }
        #[cfg(not(feature = "hot-reload"))]
        let _ = (name, capture, restore);
    }

    /// Take what [`dev_state`](Self::dev_state) registered, for the
    /// development session. Called once after `Application::new`.
    #[cfg(feature = "hot-reload")]
    #[doc(hidden)]
    pub fn __take_dev_states(&mut self) -> Vec<DevState> {
        core::mem::take(&mut self.dev_states)
    }
}

phase_cx!(/// Layout context. No network, no window creation, no GPU submit.
    LayoutCx);
phase_cx!(/// Paint-data production context.
    PaintCx);
phase_cx!(/// Render/submit context (internal).
    RenderCx);
phase_cx!(/// Async task context.
    TaskCx);

/// What writes state cells: an event handler's context, an update's, an
/// effect's, or the store itself. A handle that owns a cell — a document a
/// widget shows, say — takes one, so whichever context holds it can write.
pub trait StateWriter {
    /// Write a state value; returns whether the write landed (the handle was
    /// live).
    fn set(&mut self, id: StateId, value: StateValue) -> bool;
}

impl StateWriter for StateStore {
    #[inline]
    fn set(&mut self, id: StateId, value: StateValue) -> bool {
        StateStore::set(self, id, value)
    }
}

impl StateWriter for UpdateCx<'_> {
    #[inline]
    fn set(&mut self, id: StateId, value: StateValue) -> bool {
        UpdateCx::set(self, id, value)
    }
}

impl StateWriter for EventCx<'_> {
    #[inline]
    fn set(&mut self, id: StateId, value: StateValue) -> bool {
        EventCx::set(self, id, value)
    }
}

impl StateWriter for crate::reactive::EffectCx<'_> {
    #[inline]
    fn set(&mut self, id: StateId, value: StateValue) -> bool {
        crate::reactive::EffectCx::set(self, id, value)
    }
}

/// State-flush / reactive update context.
///
/// This is the write end handed to component update / action handlers. Unlike
/// the marker contexts, it holds live borrows of the reactive stores so a
/// handler can read and write state. Writes are deferred: [`UpdateCx::set`]
/// records the change in the state store's per-frame pending set, and the
/// frame's flush phase turns each changed state into targeted node dirtying via
/// the [`BindingTable`]. Nothing recomputes at write time, so many writes in
/// one action collapse into one flush.
///
/// The context deliberately does *not* expose `mark_dirty` or layout entry
/// points: a handler declares intent by writing state, and invalidation follows
/// from the compiled bindings. It holds `nodes` for reads and for the flush's
/// own use, not as a manual-invalidation escape hatch.
pub struct UpdateCx<'a> {
    states: &'a mut StateStore,
    bindings: &'a BindingTable,
    nodes: &'a mut NodeStore,
}

impl<'a> UpdateCx<'a> {
    /// Assemble the update context from the frame's reactive stores.
    #[doc(hidden)]
    pub fn __new(
        states: &'a mut StateStore,
        bindings: &'a BindingTable,
        nodes: &'a mut NodeStore,
    ) -> Self {
        Self {
            states,
            bindings,
            nodes,
        }
    }

    /// Read the current value of a state cell. `None` for a stale handle.
    #[inline]
    pub fn get(&self, id: StateId) -> Option<StateValue> {
        self.states.get(id)
    }

    /// Write a state value. The change is recorded in this frame's pending set
    /// and applied to bound nodes by the flush phase, not here. Returns whether
    /// the write landed (the handle was live). Setting the current value is a
    /// no-op that schedules no work.
    #[inline]
    pub fn set(&mut self, id: StateId, value: StateValue) -> bool {
        self.states.set(id, value)
    }

    /// The adaptive environment the frame resolved.
    #[inline]
    pub fn env(&self) -> &crate::adaptive::AdaptiveEnv {
        self.states.env()
    }

    /// The state store's [epoch](crate::StateStore::epoch): while it stands,
    /// every cell reads as it did.
    #[inline]
    pub fn state_epoch(&self) -> u64 {
        self.states.epoch()
    }

    /// Run `future` as a task owned by `owner`, first polled at the next frame
    /// boundary; `None` when `owner` was freed.
    #[doc(hidden)]
    pub fn __spawn_for(&mut self, owner: NodeId, future: TaskFuture) -> Option<TaskId> {
        self.nodes.spawn_task(owner, future)
    }

    /// Drop task `id`: its future is dropped and its continuation never runs.
    pub fn cancel_task(&mut self, id: TaskId) {
        self.nodes.cancel_task(id);
    }

    /// Make viewport `id` refreshing, or end its refresh: the work a pull
    /// started finished ([`NodeStore::set_refreshing`]).
    pub fn set_refreshing(&mut self, id: NodeId, on: bool) {
        self.nodes.set_refreshing(id, on);
    }

    /// The binding table backing this frame — the flush reads it to turn
    /// changed states into node dirtying.
    #[inline]
    pub fn bindings(&self) -> &BindingTable {
        self.bindings
    }
}

/// Input-dispatch context handed to a pointer/key/IME handler.
///
/// Like [`UpdateCx`] it holds the reactive stores so a handler can read and
/// write state; writes are deferred ([`EventCx::set`] records the change in the
/// per-frame pending set, and the frame's flush turns it into targeted node
/// dirtying via the [`BindingTable`]). It deliberately holds no node store and
/// exposes no `mark_dirty`: a handler declares intent by writing state, and
/// invalidation follows from the compiled bindings.
///
/// Whichever of the three input kinds is under dispatch, the cx carries a borrow
/// of that event so the handler can read *which* sample it is reacting to: the
/// pointer sample (its phase, position, buttons — so a handler acts on release,
/// not on every phase), the key transition, or the composition string a text
/// control needs. Exactly one of the three is `Some` per dispatch; the others
/// are `None`. A handler may also request a focus change: the request is
/// recorded here and applied by the router after the handler returns, since the
/// cx holds no node store to mutate the focus slot directly.
pub struct EventCx<'a> {
    states: &'a mut StateStore,
    #[allow(dead_code)]
    bindings: &'a BindingTable,
    /// The pointer event currently being dispatched, if this is a pointer
    /// dispatch.
    pointer: Option<&'a PointerEvent>,
    /// Which pointer the dispatched sample belongs to.
    pointer_id: PointerId,
    /// The key event currently being dispatched, if this is a key dispatch.
    key: Option<&'a KeyEvent>,
    /// The IME event currently being dispatched, if this is an IME dispatch.
    ime: Option<&'a ImeEvent>,
    /// The edited node's new text, if this dispatch reports a settled edit.
    text_change: Option<&'a str>,
    /// Where an activated link leads, if this dispatch reports one.
    link: Option<&'a str>,
    /// This dispatch tells a light-dismiss layer a press landed outside it.
    dismissed: bool,
    /// This dispatch tells a viewport a pull asked it to refresh.
    refresh: bool,
    /// The item a snapping viewport came to rest on, heads for, or shows
    /// nearest, if this dispatch tells it.
    snapped: Option<usize>,
    /// The token of the wake this dispatch delivers.
    woken: Option<u32>,
    /// Whether the focus came to or left the node this dispatch tells.
    focus_change: Option<FocusChange>,
    /// What this dispatch tells of the access keys.
    access: Option<crate::access::Access>,
    /// What a handler asked of the tree this dispatch beyond the requests
    /// above: applied by the router after the handler returns.
    store_requests: Vec<StoreRequest>,
    /// A focus request a handler made this dispatch: `Some(Some(id))` focuses
    /// `id`, `Some(None)` clears focus, `None` = no request. Applied by the
    /// router after the handler returns.
    focus_request: Option<Option<NodeId>>,
    /// A pointer-capture request a handler made this dispatch: `Some(Some(id))`
    /// captures the pointer to `id`, `Some(None)` releases capture, `None` = no
    /// request. Applied by the router after the handler returns.
    capture_request: Option<Option<NodeId>>,
    /// A focus-scope request a handler made this dispatch: `Some(Some(id))`
    /// confines focus traversal to `id`'s subtree (an opening modal traps the
    /// ring), `Some(None)` clears the scope (a closing modal releases it),
    /// `None` = no request. Applied by the router after the handler returns —
    /// the same deferred shape as [`focus_request`](Self::focus_request), since
    /// the cx holds no node store to touch the scope slot directly.
    focus_scope_request: Option<Option<NodeId>>,
    /// Set by [`EventCx::stop_propagation`]: when true, the router stops walking
    /// the remaining dispatch chain after this handler returns. Shared by the
    /// pointer, key, and IME paths.
    stop: bool,
    /// Edit intents a text-control handler recorded this dispatch, applied by the
    /// router to the dispatching node's edit buffer after the handler returns —
    /// the same deferred shape as [`focus_request`](Self::focus_request), since
    /// the cx holds no edit buffer to mutate directly. Empty (and allocation-free)
    /// for the overwhelmingly common non-text dispatch.
    edits: Vec<EditIntent>,
    /// Node visibility flips a handler recorded this dispatch, applied by the
    /// router to the node store after the handler returns — the same deferred
    /// shape as [`edits`](Self::edits), since the cx holds no node store to flip
    /// the flag directly. A hide/show control (Tabs and the wider Tier 4 family)
    /// records `(panel, hidden)` pairs to swap which panel shows. Empty (and
    /// allocation-free) for the overwhelmingly common dispatch that shows nothing.
    hidden_requests: Vec<(NodeId, bool)>,
    /// Transform animations a handler asked to start this dispatch, applied by
    /// the router to the node store's animation queue after the handler returns
    /// — the same deferred shape as [`hidden_requests`](Self::hidden_requests),
    /// since the cx holds neither the store's queue nor the driver's registry.
    /// A sheet's open/close records one slide here; the router hands it to the
    /// store queue, and the driver drains that into its live registry the next
    /// frame, before ticking. Empty (and allocation-free) for the overwhelmingly
    /// common dispatch that starts no animation.
    animation_requests: Vec<TranslateAnim>,
    /// One-shot timer arms a handler asked to start this dispatch, applied by the
    /// router to the node store's timer queue after the handler returns — the
    /// same deferred shape as [`animation_requests`](Self::animation_requests),
    /// since the cx holds neither the store's queue nor the driver's registry. A
    /// toast's show records one auto-dismiss arm here; the router hands it to the
    /// store queue, and the driver arms it on its live registry the next frame
    /// (against that frame's `now`). Empty (and allocation-free) for the
    /// overwhelmingly common dispatch that arms no timer.
    timer_requests: Vec<TimerRequest>,
    /// Window-open requests a handler recorded this dispatch, applied by the
    /// router to the node store's window queue after the handler returns — the
    /// same deferred shape as [`timer_requests`](Self::timer_requests), since the
    /// cx holds neither the store's queue nor the facade's platform seam. A
    /// handler that opens another top-level window records one request here; the
    /// router hands it to the store queue, and the facade drains it the next
    /// frame — where it holds a scheduling context — to create the OS window and
    /// build its tree. Empty (and allocation-free) for the overwhelmingly common
    /// dispatch that opens no window.
    window_opens: Vec<WindowOpenRequest>,
    /// Window-close requests a handler recorded this dispatch, carried as raw
    /// window ids (the UI tier has no platform `WindowId`), applied by the router
    /// to the node store's window queue after the handler returns — the same
    /// deferred shape as [`window_opens`](Self::window_opens). A handler that
    /// closes a window it opened records the window's id here; the facade drains
    /// it the next frame and asks the platform to close that window, which routes
    /// back through the single teardown path. Empty (and allocation-free) for the
    /// overwhelmingly common dispatch that closes no window.
    window_closes: Vec<u32>,
    /// The node that holds focus at the moment this dispatch began, lent by value
    /// from the node store's focus slot so a handler can read it via
    /// [`EventCx::focused`] without the cx holding the store. A `Copy` snapshot,
    /// not a live view: a `request_focus` this same dispatch does not change it
    /// (the router applies focus after the handler returns). `None` on the
    /// state-only [`__new`](Self::__new) path, and on any dispatch where nothing
    /// was focused. A modal reads it on open to remember where to send focus back
    /// when it closes (the WAI-ARIA dialog focus-restore contract).
    focused: Option<NodeId>,
    /// Tasks a handler spawned or cancelled this dispatch, handed by the router
    /// to the node store under the dispatching node after the handler returns.
    /// Empty (and allocation-free) for the common dispatch that spawns nothing.
    tasks: TaskOps,
    /// The session's service registry, type-erased; `None` on a store the
    /// driver lent none.
    services: Option<&'a dyn Any>,
    /// Where the dispatching node sits in the tree, lent by the router; `None`
    /// on the state-only paths.
    place: Option<Place<'a>>,
}

/// How the focus moved for the node a key-handler dispatch tells
/// ([`EventCx::focus_change`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusChange {
    /// The focus came to it.
    Gained,
    /// The focus left it.
    Lost,
}

/// Something a handler asks of the tree, applied by the router after it
/// returns.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq)]
pub enum StoreRequest {
    /// Anchor a layer at a window point of its anchor.
    AnchorAt(NodeId, Vec2),
    /// Put a node of a layer over its anchor.
    AnchorOver(NodeId, Option<NodeId>),
    /// Scroll a node into view.
    Scroll(
        NodeId,
        crate::component::ScrollRequest,
        crate::component::ScrollBehavior,
    ),
    /// Show another text on a text node.
    Text(NodeId, Box<crate::content::TextRequest>),
    /// Show a descendant of a node as its active one.
    Active(NodeId, Option<NodeId>),
    /// Edit another node's text.
    Edit(NodeId, EditIntent),
    /// Work a scroll bar.
    Bar(NodeId, crate::scroll_bar::BarInput),
    /// Start or end a viewport's refresh.
    Refreshing(NodeId, bool),
    /// Bring a snapping viewport to rest on an item.
    SnapTo(NodeId, usize),
    /// Turn a paged stack to a page.
    Page(NodeId, Option<usize>, crate::navigation::PageTransition),
}

impl StoreRequest {
    /// Apply the request to `store`, as the router does: for tests that
    /// drive a handler without one.
    #[doc(hidden)]
    pub fn __apply(self, store: &mut NodeStore) {
        self.apply(store);
    }

    /// Apply the request to `store`.
    pub(crate) fn apply(self, store: &mut NodeStore) {
        match self {
            StoreRequest::AnchorAt(layer, at) => store.set_anchor_point(layer, Some(at)),
            StoreRequest::AnchorOver(layer, over) => store.set_anchor_over(layer, over),
            StoreRequest::Scroll(id, request, behavior) => {
                store.request_scroll_with(id, request, behavior)
            }
            StoreRequest::Text(id, request) => store.set_text(id, *request),
            StoreRequest::Active(id, active) => store.set_active_descendant(id, active),
            StoreRequest::Edit(id, intent) => store.queue_edit(id, intent),
            StoreRequest::Bar(track, input) => store.bar_input(track, input),
            StoreRequest::Refreshing(id, on) => store.set_refreshing(id, on),
            StoreRequest::SnapTo(id, index) => store.snap_to_item(id, index),
            StoreRequest::Page(stack, page, transition) => store.set_page(stack, page, transition),
        }
    }
}

/// The dispatching node and the tree it sits in, lent to an [`EventCx`] so a
/// control can read its geometry and which of its children the event passed
/// through.
#[derive(Clone, Copy)]
struct Place<'a> {
    store: &'a NodeStore,
    node: NodeId,
    rect: Rect,
    phase: DispatchPhase,
    /// The dispatching node's child on the path to the target; `None` at the
    /// target itself.
    through: Option<NodeId>,
}

impl<'a> EventCx<'a> {
    /// Assemble the event context for the pointer path with no borrowed sample —
    /// a handler here can read and write state but not inspect the pointer. Used
    /// by state-only tests; the live router uses [`EventCx::__new_pointer`].
    #[doc(hidden)]
    pub fn __new(states: &'a mut StateStore, bindings: &'a BindingTable) -> Self {
        Self {
            states,
            bindings,
            pointer: None,
            key: None,
            ime: None,
            focus_request: None,
            capture_request: None,
            focus_scope_request: None,
            stop: false,
            edits: Vec::new(),
            hidden_requests: Vec::new(),
            animation_requests: Vec::new(),
            timer_requests: Vec::new(),
            window_opens: Vec::new(),
            window_closes: Vec::new(),
            focused: None,
            pointer_id: PointerId::MOUSE,
            text_change: None,
            link: None,
            dismissed: false,
            refresh: false,
            snapped: None,
            woken: None,
            focus_change: None,
            access: None,
            store_requests: Vec::new(),
            tasks: TaskOps::default(),
            services: None,
            place: None,
        }
    }

    /// Assemble the event context for a pointer dispatch, injecting the borrowed
    /// pointer sample so the handler can read it via [`EventCx::pointer`] — its
    /// phase in particular, so a click acts on `Up` rather than every sample.
    #[doc(hidden)]
    pub fn __new_pointer(
        states: &'a mut StateStore,
        bindings: &'a BindingTable,
        pointer: &'a PointerEvent,
    ) -> Self {
        Self {
            states,
            bindings,
            pointer: Some(pointer),
            key: None,
            ime: None,
            focus_request: None,
            capture_request: None,
            focus_scope_request: None,
            stop: false,
            edits: Vec::new(),
            hidden_requests: Vec::new(),
            animation_requests: Vec::new(),
            timer_requests: Vec::new(),
            window_opens: Vec::new(),
            window_closes: Vec::new(),
            focused: None,
            pointer_id: PointerId::MOUSE,
            text_change: None,
            link: None,
            dismissed: false,
            refresh: false,
            snapped: None,
            woken: None,
            focus_change: None,
            access: None,
            store_requests: Vec::new(),
            tasks: TaskOps::default(),
            services: None,
            place: None,
        }
    }

    /// Assemble the event context for a key dispatch, injecting the borrowed
    /// key event so the handler can read it via [`EventCx::key`].
    #[doc(hidden)]
    pub fn __new_key(
        states: &'a mut StateStore,
        bindings: &'a BindingTable,
        key: &'a KeyEvent,
    ) -> Self {
        Self {
            states,
            bindings,
            pointer: None,
            key: Some(key),
            ime: None,
            focus_request: None,
            capture_request: None,
            focus_scope_request: None,
            stop: false,
            edits: Vec::new(),
            hidden_requests: Vec::new(),
            animation_requests: Vec::new(),
            timer_requests: Vec::new(),
            window_opens: Vec::new(),
            window_closes: Vec::new(),
            focused: None,
            pointer_id: PointerId::MOUSE,
            text_change: None,
            link: None,
            dismissed: false,
            refresh: false,
            snapped: None,
            woken: None,
            focus_change: None,
            access: None,
            store_requests: Vec::new(),
            tasks: TaskOps::default(),
            services: None,
            place: None,
        }
    }

    /// Assemble the event context for an IME dispatch, injecting the borrowed
    /// IME event so the handler can read it via [`EventCx::ime`].
    #[doc(hidden)]
    pub fn __new_ime(
        states: &'a mut StateStore,
        bindings: &'a BindingTable,
        ime: &'a ImeEvent,
    ) -> Self {
        Self {
            states,
            bindings,
            pointer: None,
            key: None,
            ime: Some(ime),
            focus_request: None,
            capture_request: None,
            focus_scope_request: None,
            stop: false,
            edits: Vec::new(),
            hidden_requests: Vec::new(),
            animation_requests: Vec::new(),
            timer_requests: Vec::new(),
            window_opens: Vec::new(),
            window_closes: Vec::new(),
            focused: None,
            pointer_id: PointerId::MOUSE,
            text_change: None,
            link: None,
            dismissed: false,
            refresh: false,
            snapped: None,
            woken: None,
            focus_change: None,
            access: None,
            store_requests: Vec::new(),
            tasks: TaskOps::default(),
            services: None,
            place: None,
        }
    }

    /// Assemble the event context that reports a settled edit: the node's text
    /// changed, and the handler reads the new text via [`EventCx::text_change`].
    #[doc(hidden)]
    pub fn __new_text_change(
        states: &'a mut StateStore,
        bindings: &'a BindingTable,
        text: &'a str,
    ) -> Self {
        let mut cx = Self::__new(states, bindings);
        cx.text_change = Some(text);
        cx
    }

    /// Assemble the event context that reports an activated link: the
    /// handler reads where it leads via [`EventCx::link`].
    #[doc(hidden)]
    pub fn __new_link(
        states: &'a mut StateStore,
        bindings: &'a BindingTable,
        target: &'a str,
    ) -> Self {
        let mut cx = Self::__new(states, bindings);
        cx.link = Some(target);
        cx
    }

    /// Assemble the event context that tells a light-dismiss layer a press
    /// landed outside it: the handler reads it via [`EventCx::dismissed`].
    #[doc(hidden)]
    pub fn __new_dismiss(states: &'a mut StateStore, bindings: &'a BindingTable) -> Self {
        let mut cx = Self::__new(states, bindings);
        cx.dismissed = true;
        cx
    }

    /// Assemble the event context that tells a viewport a pull asked it to
    /// refresh: the handler reads it via [`EventCx::refresh`].
    #[doc(hidden)]
    pub fn __new_refresh(states: &'a mut StateStore, bindings: &'a BindingTable) -> Self {
        let mut cx = Self::__new(states, bindings);
        cx.refresh = true;
        cx
    }

    /// Assemble the event context that tells a snapping viewport the item it
    /// settles on: the handler reads it via [`EventCx::snapped`].
    #[doc(hidden)]
    pub fn __new_snap(
        states: &'a mut StateStore,
        bindings: &'a BindingTable,
        index: usize,
    ) -> Self {
        let mut cx = Self::__new(states, bindings);
        cx.snapped = Some(index);
        cx
    }

    /// Assemble the event context that delivers the wake `token` a handler
    /// asked for: the handler reads it via [`EventCx::woken`].
    #[doc(hidden)]
    pub fn __new_wake(states: &'a mut StateStore, bindings: &'a BindingTable, token: u32) -> Self {
        let mut cx = Self::__new(states, bindings);
        cx.woken = Some(token);
        cx
    }

    /// Assemble the event context that tells a node the focus came to it or
    /// left it: the handler reads it via [`EventCx::focus_change`].
    #[doc(hidden)]
    pub fn __new_focus(
        states: &'a mut StateStore,
        bindings: &'a BindingTable,
        change: FocusChange,
    ) -> Self {
        let mut cx = Self::__new(states, bindings);
        cx.focus_change = Some(change);
        cx
    }

    /// Assemble the event context that tells a node of the access keys: the
    /// handler reads it via [`EventCx::access`].
    #[doc(hidden)]
    pub fn __new_access(
        states: &'a mut StateStore,
        bindings: &'a BindingTable,
        access: crate::access::Access,
    ) -> Self {
        let mut cx = Self::__new(states, bindings);
        cx.access = Some(access);
        cx
    }

    /// Run `future` on the UI thread as a task owned by the dispatching node:
    /// unmounting the node drops it. The first poll comes at the next frame
    /// boundary, and later polls whenever its waker fires, from any thread.
    /// Hold no borrow of UI state across its `.await`s; to write state when it
    /// finishes, use [`spawn_then`](Self::spawn_then).
    pub fn spawn(&mut self, future: impl Future<Output = ()> + 'static) -> TaskId {
        self.tasks.spawn(None, task::detached(future))
    }

    /// Run `future` like [`spawn`](Self::spawn), then hand its output to
    /// `then` with an [`UpdateCx`] at the frame boundary after it finishes.
    ///
    /// ```ignore
    /// let picked = cx.services().files().open(OpenOptions::default());
    /// cx.spawn_then(picked, move |cx, files| {
    ///     cx.set(count, StateValue::Int(files.map_or(0, |f| f.len() as i32)));
    /// });
    /// ```
    pub fn spawn_then<T: 'static>(
        &mut self,
        future: impl Future<Output = T> + 'static,
        then: impl FnOnce(&mut UpdateCx<'_>, T) + 'static,
    ) -> TaskId {
        self.tasks.spawn(None, task::then(future, then))
    }

    /// Run `future` as a task owned by `owner` instead of the dispatching
    /// node: freeing `owner` drops it.
    #[doc(hidden)]
    pub fn __spawn_for(&mut self, owner: NodeId, future: TaskFuture) -> TaskId {
        self.tasks.spawn(Some(owner), future)
    }

    /// Drop task `id`, spawned by any handler of this tree. Its future is
    /// dropped and its continuation never runs; a finished or unknown id is a
    /// no-op.
    pub fn cancel_task(&mut self, id: TaskId) {
        self.tasks.cancels.push(id);
    }

    /// Lend the session's type-erased service registry to this dispatch.
    #[doc(hidden)]
    pub fn __set_services(&mut self, services: Option<&'a dyn Any>) {
        self.services = services;
    }

    /// The session's type-erased service registry, if one was lent.
    #[doc(hidden)]
    pub fn __services(&self) -> Option<&'a dyn Any> {
        self.services
    }

    /// Move out the task spawns and cancellations this dispatch recorded.
    #[doc(hidden)]
    pub fn __take_tasks(&mut self) -> TaskOps {
        std::mem::take(&mut self.tasks)
    }

    /// Read the current value of a state cell. `None` for a stale handle.
    #[inline]
    pub fn get(&self, id: StateId) -> Option<StateValue> {
        self.states.get(id)
    }

    /// The adaptive environment, as of the last settle.
    #[inline]
    pub fn env(&self) -> &crate::adaptive::AdaptiveEnv {
        self.states.env()
    }

    /// The state store's [epoch](crate::StateStore::epoch): while it stands,
    /// every cell reads as it did.
    #[inline]
    pub fn state_epoch(&self) -> u64 {
        self.states.epoch()
    }

    /// Write a state value. Deferred: recorded in this frame's pending set and
    /// applied to bound nodes by the flush phase, not here. Returns whether the
    /// write landed (the handle was live). Setting the current value is a no-op.
    #[inline]
    pub fn set(&mut self, id: StateId, value: StateValue) -> bool {
        self.states.set(id, value)
    }

    /// The pointer sample under dispatch, if this is a pointer dispatch; `None`
    /// on the key and IME paths. A pointer handler reads it to gate on phase —
    /// e.g. act on `PointerPhase::Up` so a down/up pair counts as one click —
    /// or to read the position, buttons, or modifiers of the sample.
    #[inline]
    pub fn pointer(&self) -> Option<&PointerEvent> {
        self.pointer
    }

    /// Which pointer the sample under dispatch belongs to: [`PointerId::MOUSE`]
    /// for the mouse (and on the key and IME paths), a per-contact id for each
    /// finger or pen. [`capture_pointer`](Self::capture_pointer) captures this
    /// pointer, so a multi-touch control tells its contacts apart by it.
    #[inline]
    pub fn pointer_id(&self) -> PointerId {
        self.pointer_id
    }

    #[doc(hidden)]
    #[inline]
    pub fn __set_pointer_id(&mut self, id: PointerId) {
        self.pointer_id = id;
    }

    /// The key event under dispatch, if this is a key dispatch; `None` on the
    /// pointer and IME paths.
    #[inline]
    pub fn key(&self) -> Option<&KeyEvent> {
        self.key
    }

    /// The IME event under dispatch, if this is an IME dispatch; `None` on the
    /// pointer and key paths.
    #[inline]
    pub fn ime(&self) -> Option<&ImeEvent> {
        self.ime
    }

    /// The new text of this node's edit buffer, when this dispatch reports that
    /// an edit changed it. Delivered to the edited node's key handler once per
    /// input sample that changed the text — typing, deletion, paste, and cut
    /// alike — after the edit is applied, so the text is what the user now sees.
    pub fn text_change(&self) -> Option<&'a str> {
        self.text_change
    }

    /// Where a link leads, when this dispatch reports that a pointer's click
    /// or Enter activated it: delivered to the key handler of the text that
    /// holds it, then of each of its ancestors until one stops it.
    pub fn link(&self) -> Option<&'a str> {
        self.link
    }

    /// Whether this dispatch tells a light-dismiss layer
    /// ([`NodeStore::set_light_dismiss`]) that a press landed outside it.
    pub fn dismissed(&self) -> bool {
        self.dismissed
    }

    /// Whether this dispatch tells a refreshable viewport
    /// ([`NodeStore::set_refreshable`]) that a pull, or an assistive
    /// technology, asked it to refresh. It shows refreshing until
    /// [`set_refreshing`](Self::set_refreshing) ends it.
    pub fn refresh(&self) -> bool {
        self.refresh
    }

    /// Make viewport `id` refreshing, or end its refresh
    /// ([`NodeStore::set_refreshing`]).
    pub fn set_refreshing(&mut self, id: NodeId, on: bool) {
        self.store_requests.push(StoreRequest::Refreshing(id, on));
    }

    /// Bring snapping viewport `id` to rest on item `index`
    /// ([`NodeStore::snap_to_item`]).
    pub fn snap_to_item(&mut self, id: NodeId, index: usize) {
        self.store_requests.push(StoreRequest::SnapTo(id, index));
    }

    /// Turn paged stack `stack` to `page`, the pages moving as `transition`
    /// says ([`NodeStore::set_page`]).
    pub fn set_page(
        &mut self,
        stack: NodeId,
        page: Option<usize>,
        transition: crate::navigation::PageTransition,
    ) {
        self.store_requests
            .push(StoreRequest::Page(stack, page, transition));
    }

    /// The item snapping viewport `id` rests on, heads for, or shows
    /// nearest; `None` on the state-only paths.
    pub fn snap_index(&self, id: NodeId) -> Option<usize> {
        self.place.and_then(|place| place.store.snap_index(id))
    }

    /// Node `id`'s page dots; `None` on the state-only paths.
    pub fn page_dots(&self, id: NodeId) -> Option<crate::page_dots::PageDots> {
        self.place.and_then(|place| place.store.page_dots(id))
    }

    /// The item a snapping viewport ([`NodeStore::tell_snaps`]) came to rest
    /// on, heads for, or shows nearest, if this dispatch tells one.
    pub fn snapped(&self) -> Option<usize> {
        self.snapped
    }

    /// The token of the wake ([`wake_after`](Self::wake_after)) this dispatch
    /// delivers, `None` for any other dispatch.
    pub fn woken(&self) -> Option<u32> {
        self.woken
    }

    /// Whether node `id` is `ancestor` or lies inside it.
    pub fn is_within(&self, id: NodeId, ancestor: NodeId) -> bool {
        self.place
            .is_some_and(|place| place.store.within(id, ancestor))
    }

    /// Whether the focus shows as the keyboard's: a key, not a press, brought
    /// it where it is.
    pub fn focus_visible(&self) -> bool {
        self.place
            .is_some_and(|place| place.store.focus_shows_visible())
    }

    /// Whether this dispatch tells that the focus came to or left a node: the
    /// dispatching node or one inside it. [`focused`](Self::focused) says
    /// where the focus is now, so a container learns whether it left it.
    pub fn focus_change(&self) -> Option<FocusChange> {
        self.focus_change
    }

    /// What this dispatch tells of the access keys: the dispatching node's
    /// was pressed, Alt alone or F10 asks for the menu bar it is, or the
    /// cues come or go ([`crate::access`]).
    pub fn access(&self) -> Option<crate::access::Access> {
        self.access
    }

    /// Whether the access keys' cues show: Alt is held.
    pub fn access_cues(&self) -> bool {
        self.place.is_some_and(|place| place.store.access_cues())
    }

    /// Where this dispatch asks for a context menu, in window coordinates: a
    /// secondary press (on Apple systems also a Control press), a long press,
    /// or off Apple systems the menu key or Shift+F10, which ask for it below
    /// the dispatching node's start. `None` for any other dispatch.
    pub fn context_menu(&self) -> Option<Vec2> {
        if let Some(hold) = self.long_press() {
            return Some(hold.at);
        }
        if let Some(p) = self.pointer {
            return crate::field_input::asks_for_menu(p, viso_runtime::primary_is_logo())
                .then_some(Vec2::new(p.x, p.y));
        }
        let k = self.key.filter(|k| k.pressed && !k.repeat)?;
        let m = k.modifiers;
        let asks = !viso_runtime::primary_is_logo()
            && match k.key {
                viso_runtime::Key::ContextMenu => true,
                viso_runtime::Key::F10 => m.shift && !m.control && !m.alt && !m.logo,
                _ => false,
            };
        let r = self.rect()?;
        asks.then_some(Vec2::new(r.x, r.y + r.h))
    }

    /// The on-screen rect of node `id` — the one hit testing uses — or `None`
    /// for a node gone or on the state-only paths.
    pub fn rect_of(&self, id: NodeId) -> Option<Rect> {
        let place = self.place?;
        place
            .store
            .arena()
            .is_live(id)
            .then(|| place.store.world(id))
    }

    /// The node that held focus when this dispatch began, or `None` if nothing
    /// was focused (or on the state-only test path). A `Copy` snapshot taken
    /// before the handler runs: a [`request_focus`](Self::request_focus) made in
    /// this same dispatch does not change what this returns, because the router
    /// applies the focus move only after the handler returns. A modal reads this
    /// on open to remember where focus was, so it can send focus back there when
    /// it closes.
    #[inline]
    pub fn focused(&self) -> Option<NodeId> {
        self.focused
    }

    /// The node whose handler is running, or `None` on the state-only paths.
    #[inline]
    pub fn node(&self) -> Option<NodeId> {
        self.place.map(|p| p.node)
    }

    /// The dispatching node's scrolled, world-space rect — the one hit testing
    /// uses, so a pointer sample's position is relative to its origin.
    #[inline]
    pub fn rect(&self) -> Option<Rect> {
        self.place.map(|p| p.rect)
    }

    /// Which leg of the capture → target → bubble walk this dispatch is on: an
    /// ancestor's handler runs twice per routed event, once capturing and once
    /// bubbling.
    #[inline]
    pub fn phase(&self) -> DispatchPhase {
        self.place.map_or(DispatchPhase::Target, |p| p.phase)
    }

    /// Which of the dispatching node's children the event passed through on its
    /// way to the target, counted from the first child. `None` when the
    /// dispatching node is the target itself.
    pub fn child_index(&self) -> Option<u32> {
        let place = self.place?;
        let through = place.through?;
        place
            .store
            .arena()
            .children(place.node)
            .position(|c| c == through)
            .map(|i| i as u32)
    }

    /// How many children the dispatching node has.
    pub fn child_count(&self) -> u32 {
        let Some(place) = self.place else {
            return 0;
        };
        place.store.arena().children(place.node).count() as u32
    }

    /// Where the screen point `(x, y)` falls in the dispatching node's box:
    /// from its top-left corner, along its own axes before any scale or
    /// rotation. The point itself on the state-only paths.
    pub fn local(&self, x: f32, y: f32) -> Vec2 {
        match self.place {
            Some(place) => place.store.to_local(place.node, x, y),
            None => Vec2 { x, y },
        }
    }

    /// Whether the dispatching node is in interaction state `state`; `false`
    /// on the state-only paths.
    pub fn in_state(&self, state: crate::interaction::Interaction) -> bool {
        self.place
            .is_some_and(|place| place.store.in_state(place.node, state))
    }

    /// The drag sample the pointer sample under dispatch carries, `None` for
    /// a sample no drag of this pointer sees.
    pub fn drag(&self) -> Option<crate::drag::DragSample> {
        self.place?.store.drag_sample(self.pointer_id)
    }

    /// How many clicks the press under dispatch is in its run: `1` for a
    /// single click, `2` for a double and `3` for a triple. A press comes
    /// within [`CLICK_INTERVAL`](crate::clicks::CLICK_INTERVAL) of the last,
    /// near it, to count on.
    pub fn clicks(&self) -> u8 {
        self.place
            .map_or(1, |place| place.store.click_count(self.pointer_id))
    }

    /// Whether the pointer under dispatch is a direct contact — a finger or
    /// a pen on the glass — rather than a mouse; `false` on the state-only
    /// paths.
    pub fn direct(&self) -> bool {
        self.place
            .is_some_and(|place| place.store.contacts().is_direct(self.pointer_id))
    }

    /// Whether the direct contact under dispatch has stayed within the touch
    /// slop with no handler capturing it, so that its lift is a tap.
    pub fn still(&self) -> bool {
        self.place
            .is_some_and(|place| place.store.contacts().is_still(self.pointer_id))
    }

    /// The long press under dispatch, `None` for any other dispatch.
    pub fn long_press(&self) -> Option<crate::long_press::LongPressSample> {
        self.place?.store.long_press_sample(self.pointer_id)
    }

    /// The dispatching node's laid-out size, along its own axes.
    pub fn size(&self) -> Option<Vec2> {
        self.place.map(|place| {
            let b = place.store.bounds(place.node);
            Vec2 { x: b.w, y: b.h }
        })
    }

    /// Lend the dispatching node's place in the tree: its store, id, the leg
    /// of the walk and the child on the path to the target.
    #[doc(hidden)]
    #[inline]
    pub fn __set_place(
        &mut self,
        store: &'a NodeStore,
        node: NodeId,
        phase: DispatchPhase,
        through: Option<NodeId>,
    ) {
        self.place = Some(Place {
            store,
            node,
            rect: store.world(node),
            phase,
            through,
        });
    }

    /// Lend the current focus slot into the cx so a handler can read it via
    /// [`focused`](Self::focused). The router calls this right after constructing
    /// the cx for a real dispatch; the state-only paths leave it `None`.
    #[doc(hidden)]
    #[inline]
    pub fn __set_focused(&mut self, focused: Option<NodeId>) {
        self.focused = focused;
    }

    /// Request that focus move to `id` after this handler returns. The router
    /// applies it (dirtying the old and new focused nodes' paint); the cx holds
    /// no node store, so nothing moves at call time.
    #[inline]
    pub fn request_focus(&mut self, id: NodeId) {
        self.focus_request = Some(Some(id));
    }

    /// Request that focus be cleared after this handler returns.
    #[inline]
    pub fn clear_focus(&mut self) {
        self.focus_request = Some(None);
    }

    /// Take the pending focus request out of the cx for the router to apply.
    /// `None` means the handler made no request this dispatch.
    #[doc(hidden)]
    pub fn __take_focus_request(&mut self) -> Option<Option<NodeId>> {
        self.focus_request.take()
    }

    /// Request that focus traversal be confined to `id`'s subtree after this
    /// handler returns — the focus trap an opening modal installs so Tab cycles
    /// only inside the dialog. Deferred exactly like
    /// [`request_focus`](Self::request_focus): the cx holds no node store, so the
    /// router applies it to the scope slot after the handler returns.
    #[inline]
    pub fn set_focus_scope(&mut self, id: NodeId) {
        self.focus_scope_request = Some(Some(id));
    }

    /// Request that the focus scope be cleared after this handler returns — a
    /// closing modal releasing the trap so Tab returns to the whole tree.
    #[inline]
    pub fn clear_focus_scope(&mut self) {
        self.focus_scope_request = Some(None);
    }

    /// Take the pending focus-scope request out of the cx for the router to
    /// apply. `None` means the handler made no request this dispatch.
    #[doc(hidden)]
    pub fn __take_focus_scope_request(&mut self) -> Option<Option<NodeId>> {
        self.focus_scope_request.take()
    }

    /// Record a text edit for the dispatching node. Deferred exactly like
    /// [`request_focus`](Self::request_focus): the cx holds no edit buffer, so the
    /// router applies the recorded intents to the node's buffer after the handler
    /// returns, and the layout phase folds them into the text. A text control's
    /// key/IME handler calls this to insert, delete, move the caret, or drive an
    /// IME composition without needing to read the buffer here.
    #[inline]
    pub fn record_edit(&mut self, intent: EditIntent) {
        self.edits.push(intent);
    }

    /// Record a text edit for text field `id`, which need not be the
    /// dispatching node: a combo box's list writes the chosen option into its
    /// field. Applied with the dispatch's other edits; the field's key
    /// handler hears the change like any other.
    pub fn edit(&mut self, id: NodeId, intent: EditIntent) {
        self.store_requests.push(StoreRequest::Edit(id, intent));
    }

    /// Take the edit intents recorded this dispatch for the router to apply to the
    /// dispatching node's buffer. Empty when the handler recorded none.
    #[doc(hidden)]
    pub fn __take_edits(&mut self) -> Vec<EditIntent> {
        std::mem::take(&mut self.edits)
    }

    /// Request that node `id` be shown or hidden after this handler returns.
    /// Deferred exactly like [`record_edit`](Self::record_edit): the cx holds no
    /// node store, so the router applies the recorded flips (each marking the node
    /// LAYOUT | PAINT dirty) after the handler returns. A hide/show control swaps
    /// panels by hiding the old and showing the new.
    #[inline]
    pub fn set_hidden(&mut self, id: NodeId, hidden: bool) {
        self.hidden_requests.push((id, hidden));
    }

    /// Take the visibility flips recorded this dispatch for the router to apply to
    /// the node store. Empty when the handler recorded none.
    #[doc(hidden)]
    pub fn __take_hidden_requests(&mut self) -> Vec<(NodeId, bool)> {
        std::mem::take(&mut self.hidden_requests)
    }

    /// Request that a transform animation start after this handler returns.
    /// Deferred exactly like [`set_hidden`](Self::set_hidden): the cx holds
    /// neither the store's animation queue nor the driver's live registry, so
    /// the router hands the recorded slides to the store queue after the handler
    /// returns, and the driver drains that queue into its registry the next
    /// frame (before ticking). A sheet's open/close records one slide here.
    #[inline]
    pub fn request_animation(&mut self, anim: TranslateAnim) {
        self.animation_requests.push(anim);
    }

    /// Take the animations requested this dispatch for the router to enqueue on
    /// the node store. Empty when the handler requested none.
    #[doc(hidden)]
    pub fn __take_animation_requests(&mut self) -> Vec<TranslateAnim> {
        std::mem::take(&mut self.animation_requests)
    }

    /// Arm a one-shot timer on `node` that fires `on_fire` once, `delay` after
    /// the frame that arms it. Deferred exactly like
    /// [`request_animation`](Self::request_animation): the cx holds neither the
    /// store's timer queue nor the driver's live registry, so the router hands
    /// the recorded arm to the store queue after the handler returns, and the
    /// driver arms it on its registry the next frame — against that frame's `now`,
    /// so the deadline stays deterministic under a headless clock.
    ///
    /// The callback receives only `&mut NodeStore` (an animation's `on_done`
    /// discipline): it may hide a node or restore state, but cannot itself arm
    /// another timer or animation. A toast's auto-dismiss records its hide here.
    #[inline]
    pub fn request_timer(
        &mut self,
        node: NodeId,
        delay: core::time::Duration,
        on_fire: impl FnOnce(&mut NodeStore) + 'static,
    ) {
        self.timer_requests
            .push(TimerRequest::new(node, delay, on_fire));
    }

    /// Deliver `token` to `node`'s key handler (read via
    /// [`woken`](Self::woken)) `delay` after the frame that arms it, where it
    /// may do anything a handler does — a menu opens a submenu the pointer
    /// rests on this way. Nothing cancels a wake: a handler that arms another
    /// gives it a new token and ignores the stale one.
    pub fn wake_after(&mut self, node: NodeId, delay: core::time::Duration, token: u32) {
        self.request_timer(node, delay, move |store| store.queue_wake(node, token));
    }

    /// Put anchored layer `layer` against the point `at` of its anchor, in
    /// window coordinates, rather than against the anchor's box: a context
    /// menu opens at the press that asked for it.
    pub fn anchor_at(&mut self, layer: NodeId, at: Vec2) {
        self.store_requests.push(StoreRequest::AnchorAt(layer, at));
    }

    /// Put node `over` of anchored layer `layer` level with its anchor
    /// ([`NodeStore::set_anchor_over`]), or `None`, on its side again.
    pub fn anchor_over(&mut self, layer: NodeId, over: Option<NodeId>) {
        self.store_requests
            .push(StoreRequest::AnchorOver(layer, over));
    }

    /// Work the scroll bar whose track is `track`.
    pub(crate) fn bar_input(&mut self, track: NodeId, input: crate::scroll_bar::BarInput) {
        self.store_requests.push(StoreRequest::Bar(track, input));
    }

    /// Scroll the viewports around `id` the least that shows it, once the
    /// next layout has placed it.
    pub fn scroll_into_view(&mut self, id: NodeId) {
        self.scroll(
            id,
            crate::component::ScrollRequest::Reveal,
            crate::component::ScrollBehavior::Instant,
        );
    }

    /// Scroll viewport `id` (or, revealing, the viewports around it) as
    /// `request` asks, at once or gliding, once the next layout has placed
    /// it ([`NodeStore::request_scroll_with`](crate::NodeStore::request_scroll_with)).
    pub fn scroll(
        &mut self,
        id: NodeId,
        request: crate::component::ScrollRequest,
        behavior: crate::component::ScrollBehavior,
    ) {
        self.store_requests
            .push(StoreRequest::Scroll(id, request, behavior));
    }

    /// Show `request` on text node `id` in place of its text
    /// ([`NodeStore::set_text`]).
    pub fn set_text(&mut self, id: NodeId, request: crate::content::TextRequest) {
        self.store_requests
            .push(StoreRequest::Text(id, Box::new(request)));
    }

    /// Show `active`, a descendant of `id`, as the one active while `id`
    /// keeps the focus ([`NodeStore::set_active_descendant`]).
    pub fn set_active_descendant(&mut self, id: NodeId, active: Option<NodeId>) {
        self.store_requests.push(StoreRequest::Active(id, active));
    }

    /// Take what the handler asked of the tree for the router to apply.
    #[doc(hidden)]
    pub fn __take_store_requests(&mut self) -> Vec<StoreRequest> {
        std::mem::take(&mut self.store_requests)
    }

    /// Take the timer arms requested this dispatch for the router to enqueue on
    /// the node store. Empty when the handler armed none.
    #[doc(hidden)]
    pub fn __take_timer_requests(&mut self) -> Vec<TimerRequest> {
        std::mem::take(&mut self.timer_requests)
    }

    /// Request that a new top-level window open after this handler returns.
    /// Deferred exactly like [`request_timer`](Self::request_timer): the cx holds
    /// neither the store's window queue nor the facade's platform seam, so the
    /// router hands the recorded request to the store queue after the handler
    /// returns, and the facade drains it the next frame — where it holds a
    /// scheduling context — to create the OS window, build its store, and run
    /// `build` against that store.
    ///
    /// `build` is deferred because the new window's store does not exist yet: it
    /// runs later, with a [`BuildCx`](crate::component::BuildCx) over the fresh
    /// store, exactly as the first window's build does, and returns the new
    /// tree's root.
    #[inline]
    pub fn request_open_window(
        &mut self,
        config: WindowConfig,
        build: impl FnOnce(&mut crate::component::BuildCx) -> Option<NodeId> + 'static,
    ) {
        self.window_opens
            .push(WindowOpenRequest::new(config, build));
    }

    /// Record a tracked open request: identical to
    /// [`request_open_window`](Self::request_open_window) but the facade writes
    /// the opened window's raw id into `id_slot`, the cell a facade-level
    /// `WindowHandle` shares so it can read the id back and close the window
    /// later. Used by the facade's `window(...).open(...)` builder; ordinary app
    /// code opens through that builder rather than calling this directly.
    #[doc(hidden)]
    #[inline]
    pub fn request_open_window_tracked(
        &mut self,
        config: WindowConfig,
        build: impl FnOnce(&mut crate::component::BuildCx) -> Option<NodeId> + 'static,
        id_slot: WindowIdSlot,
    ) {
        self.window_opens
            .push(WindowOpenRequest::tracked(config, build, id_slot));
    }

    /// Take the window-open requests recorded this dispatch for the router to
    /// enqueue on the node store. Empty when the handler opened none.
    #[doc(hidden)]
    pub fn __take_window_opens(&mut self) -> Vec<WindowOpenRequest> {
        std::mem::take(&mut self.window_opens)
    }

    /// Request that the window with raw id `window` close after this handler
    /// returns. Deferred exactly like [`request_open_window`](Self::request_open_window):
    /// the facade drains it the next frame and asks the platform to close that
    /// window, which routes back through the single teardown path (the
    /// `WindowClosed` event → `on_window_closed`) — the same path an OS-driven
    /// close takes, so no double teardown. The id is raw because the UI tier has
    /// no platform `WindowId`; the facade wraps it back.
    #[inline]
    pub fn request_close_window(&mut self, window: u32) {
        self.window_closes.push(window);
    }

    /// Take the window-close requests recorded this dispatch for the router to
    /// enqueue on the node store. Empty when the handler closed none.
    #[doc(hidden)]
    pub fn __take_window_closes(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.window_closes)
    }

    /// Request that the dispatched pointer be captured to `id` after this
    /// handler returns. While captured, that pointer's samples route straight to
    /// `id` (a drag, a slider grab) instead of hit-testing. Capturing a pointer
    /// another node holds takes it over: the previous holder receives a
    /// `Leave`, so an ancestor that claims a drag cancels its descendant's
    /// press. The router applies it; the cx holds no node store, so nothing
    /// moves at call time.
    #[inline]
    pub fn capture_pointer(&mut self, id: NodeId) {
        self.capture_request = Some(Some(id));
    }

    /// Request that pointer capture be released after this handler returns.
    #[inline]
    pub fn release_pointer(&mut self) {
        self.capture_request = Some(None);
    }

    /// Take the pending capture request out of the cx for the router to apply.
    /// `None` means the handler made no request this dispatch.
    #[doc(hidden)]
    pub fn __take_capture_request(&mut self) -> Option<Option<NodeId>> {
        self.capture_request.take()
    }

    /// Stop the event from propagating to the rest of the dispatch chain. The
    /// current handler still finishes; the router walks no further ancestors (or
    /// descendants) for this event. Shared by the pointer, key, and IME paths.
    #[inline]
    pub fn stop_propagation(&mut self) {
        self.stop = true;
    }

    /// Whether a handler called [`stop_propagation`](Self::stop_propagation) this
    /// dispatch. The router checks it after each handler to decide whether to
    /// continue the chain.
    #[doc(hidden)]
    pub fn __stop_requested(&self) -> bool {
        self.stop
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binding::BindingTable;
    use crate::component::{BuildCx, LeafStyle, NodeStore};
    use crate::input::{ImeEvent, Key, KeyEvent, PointerButtons, PointerEvent, PointerPhase};
    use crate::layout::Size;
    use crate::state::{StateStore, StateValue};

    /// Mint a live `NodeId` (its fields are private, so a test can't build one
    /// directly — allocate a real leaf and read its handle back).
    fn a_live_node() -> (NodeStore, NodeId) {
        let mut store = NodeStore::new();
        let id = {
            let mut cx = BuildCx::new(&mut store);
            let h = cx.leaf(LeafStyle {
                size: Size::fixed(1.0, 1.0),
                ..Default::default()
            });
            cx.root();
            h.id()
        };
        (store, id)
    }

    #[test]
    fn app_cx_records_fonts_in_call_order_and_drains_once() {
        let mut cx = AppCx::__new();
        // An app with no font registered hands the facade an empty queue — it
        // relies entirely on the system default.
        assert!(cx.__take_fonts().is_empty());

        // Faces load in call order: the facade makes the first one the primary,
        // so the order the app registers them in is the fallback order.
        cx.load_font(vec![1u8, 2, 3]);
        cx.load_font(vec![4u8, 5]);
        let fonts = cx.__take_fonts();
        assert_eq!(fonts.len(), 2);
        assert_eq!(&*fonts[0], &[1, 2, 3]);
        assert_eq!(&*fonts[1], &[4, 5]);

        // Draining empties the queue: a second window opened later gets the same
        // faces from the facade's own copy, not by re-draining the cx.
        assert!(cx.__take_fonts().is_empty());
    }

    #[test]
    fn event_cx_reads_and_defers_writes() {
        let mut states = StateStore::new();
        let id = states.alloc(StateValue::Int(1));
        let bindings = BindingTable::new();
        {
            let mut cx = EventCx::__new(&mut states, &bindings);
            assert_eq!(cx.get(id), Some(StateValue::Int(1)));
            assert!(cx.set(id, StateValue::Int(2)));
        }
        assert_eq!(states.get(id), Some(StateValue::Int(2)));
        assert!(states.has_pending());
    }

    #[test]
    fn no_event_cx_carries_no_sample() {
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let cx = EventCx::__new(&mut states, &bindings);
        assert!(cx.pointer().is_none());
        assert!(cx.key().is_none());
        assert!(cx.ime().is_none());
    }

    #[test]
    fn pointer_cx_exposes_the_pointer_event_only() {
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let ev = PointerEvent {
            x: 3.0,
            y: 4.0,
            phase: PointerPhase::Up,
            buttons: PointerButtons::PRIMARY,
            modifiers: Default::default(),
        };
        let cx = EventCx::__new_pointer(&mut states, &bindings, &ev);
        assert_eq!(cx.pointer(), Some(&ev));
        assert_eq!(cx.pointer().map(|p| p.phase), Some(PointerPhase::Up));
        assert!(cx.key().is_none());
        assert!(cx.ime().is_none());
    }

    #[test]
    fn key_cx_exposes_the_key_event_only() {
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let ev = KeyEvent {
            key: Key::Enter,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        };
        let cx = EventCx::__new_key(&mut states, &bindings, &ev);
        assert_eq!(cx.key(), Some(&ev));
        assert!(cx.pointer().is_none());
        assert!(cx.ime().is_none());
    }

    #[test]
    fn ime_cx_exposes_the_ime_event_only() {
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let ev = ImeEvent::Commit {
            text: "a".to_string(),
        };
        let cx = EventCx::__new_ime(&mut states, &bindings, &ev);
        assert_eq!(cx.ime(), Some(&ev));
        assert!(cx.pointer().is_none());
        assert!(cx.key().is_none());
    }

    #[test]
    fn request_focus_records_a_deferred_request() {
        let (_store, id) = a_live_node();
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let mut cx = EventCx::__new(&mut states, &bindings);
        assert_eq!(cx.__take_focus_request(), None, "no request by default");
        cx.request_focus(id);
        assert_eq!(cx.__take_focus_request(), Some(Some(id)));
        assert_eq!(cx.__take_focus_request(), None, "taking clears it");
    }

    #[test]
    fn clear_focus_records_a_clear_request() {
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let mut cx = EventCx::__new(&mut states, &bindings);
        cx.clear_focus();
        assert_eq!(cx.__take_focus_request(), Some(None));
    }

    #[test]
    fn focused_defaults_none_and_reflects_the_router_lent_value() {
        let (_store, id) = a_live_node();
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let mut cx = EventCx::__new(&mut states, &bindings);
        assert_eq!(cx.focused(), None, "no focus lent in by default");
        // The router lends the live focus slot right after constructing the cx; a
        // handler (e.g. a modal snapshotting the pre-open focus) reads it back.
        cx.__set_focused(Some(id));
        assert_eq!(cx.focused(), Some(id));
        cx.__set_focused(None);
        assert_eq!(
            cx.focused(),
            None,
            "clearing the lent value reads back None"
        );
    }

    #[test]
    fn set_focus_scope_records_a_deferred_scope_request() {
        let (_store, id) = a_live_node();
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let mut cx = EventCx::__new(&mut states, &bindings);
        assert_eq!(
            cx.__take_focus_scope_request(),
            None,
            "no scope request by default"
        );
        cx.set_focus_scope(id);
        assert_eq!(cx.__take_focus_scope_request(), Some(Some(id)));
        assert_eq!(
            cx.__take_focus_scope_request(),
            None,
            "taking clears the request"
        );
    }

    #[test]
    fn clear_focus_scope_records_a_clear_request() {
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let mut cx = EventCx::__new(&mut states, &bindings);
        cx.clear_focus_scope();
        assert_eq!(
            cx.__take_focus_scope_request(),
            Some(None),
            "clearing the scope is a Some(None) request"
        );
    }

    #[test]
    fn set_hidden_records_deferred_visibility_flips() {
        let (mut store, a) = a_live_node();
        // A second live node so the test drains an ordered pair.
        let b = {
            let mut cx = BuildCx::new(&mut store);
            let h = cx.leaf(LeafStyle {
                size: Size::fixed(1.0, 1.0),
                ..Default::default()
            });
            cx.root();
            h.id()
        };
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let mut cx = EventCx::__new(&mut states, &bindings);
        assert!(
            cx.__take_hidden_requests().is_empty(),
            "no flips recorded by default"
        );
        cx.set_hidden(a, true);
        cx.set_hidden(b, false);
        assert_eq!(cx.__take_hidden_requests(), vec![(a, true), (b, false)]);
        assert!(
            cx.__take_hidden_requests().is_empty(),
            "taking drains the recorded flips"
        );
    }
}
