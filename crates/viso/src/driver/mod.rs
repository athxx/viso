//! The frame driver that bridges the runtime to the user's application, and
//! the per-window state it keeps.

use std::any::Any;
use std::rc::Rc;

use viso_gpu::{Backend, SurfaceId};
use viso_platform::{LogicalInsets, LogicalRect, SurroundingText, TextEntry, WindowId};
use viso_render::{Primitive, Rect, Renderer};
use viso_runtime::{Instant, RuntimeCx};
use viso_ui::{
    BindingTable, ComputedStore, Continuation, EffectStore, FrameRecompute, KeyRouter, NodeId,
    NodeStore, SemanticProjector, StateId, StateStore, TextEdits, TextRequest, TimerRegistry,
    TimerRequest, VirtualLists, WindowOpenRequest,
};

use crate::text_content::{ParagraphSlot, TextShaper};
use crate::{
    AppCx, Application, Lifecycle, WindowChrome, accessibility, alloc, app_menu, assets, edit_menu,
    fonts, footprint, persist, places, playback, services, shaders, shell, videos,
};

mod access_keys;
mod convert;
mod frame;
mod pace;
mod surrounding;
mod window_state;

/// What a field's surrounding text was made from: the field, its text's
/// revision, the selection's anchor and focus and the composition's range.
pub(crate) type SurroundingKey = (
    NodeId,
    viso_text::ime::Revision,
    (usize, usize),
    (usize, usize),
);

/// Bridges the UI-agnostic runtime [`viso_runtime::FrameDriver`] to the user's
/// [`Application`].
///
/// It owns the user app and its [`AppCx`], and lives above the runtime in the
/// DAG (the facade legally depends on both `viso-runtime` and `viso-ui`), so it
/// is the only place where the two meet. The user `Application` is constructed
/// lazily on launch — after the pump is live — matching AppKit's
/// finish-launching-then-loop ordering.
pub(crate) struct AppDriver<A: Application> {
    pub(crate) app: Option<A>,
    /// The application-scope context. `AppCx` is a lifetime-marked capability
    /// handle; the facade owns it for the whole session, so it is `'static`
    /// here.
    pub(crate) cx: AppCx<'static>,
    /// The live windows. One `Application` drives every window, but each window
    /// owns its own retained tree, reactive state, GPU surface, animations, and
    /// timers — an independent [`WindowState`]. A desktop session has a
    /// single-digit window count, and every frame *iterates all windows* (it
    /// never keys a lookup by id on the hot path), so a linear `Vec` is the
    /// most compact, cache-friendly layout — a `HashMap` would only add a hash
    /// and a heap bucket per access with nothing to lookup against.
    /// Ordered by open time; `windows[0]` is the launch window.
    pub(crate) windows: Vec<WindowState>,
    /// Reusable scratch the flush phase drains each window's queued window-open
    /// requests into, so collecting a frame's `window()` requests off every
    /// window's store releases the `&mut windows` borrow before the driver opens
    /// the new windows (which needs `windows` mutably again to push). Empty on
    /// the steady path — a frame that opens no window allocates nothing.
    pub(crate) pending_opens: Vec<WindowOpenRequest>,
    /// Reusable scratch the flush phase drains each window's queued window-close
    /// requests into (raw ids from the UI tier), mirroring `pending_opens`.
    pub(crate) pending_closes: Vec<u32>,
    /// The raw sfnt faces the app registered at init through `AppCx::load_font`,
    /// drained from the cx once after `Application::new`. Session-scoped: loaded
    /// into every window's shaper as the window opens (ahead of the system
    /// fallbacks), so a face registered at init applies to the launch window and
    /// to any window opened later alike. Empty for an app that loads no font and
    /// relies on the system default.
    pub(crate) fonts: SessionFonts,
    /// The session's service registry, lent type-erased to every window's
    /// store. Created on launch unless injected (a headless session, a test).
    pub(crate) services: Option<Rc<dyn Any>>,
    /// The assets the windows show, decoded once for all of them.
    pub(crate) assets: assets::Assets,
    /// The measured frame interval, and the share of it every window's
    /// inline work draws from.
    pub(crate) pace: pace::FramePace,
    /// The session's hard memory limits, checked after every frame.
    pub(crate) budget_watch: footprint::BudgetWatch,
    /// Whether the backend announces semantics to assistive technology
    /// ([`PlatformCaps::accessibility`](viso_platform::PlatformCaps::accessibility)),
    /// read on launch. Without a bridge no window derives deltas; the
    /// semantics tree still serves tests and the Inspector.
    pub(crate) accessibility: bool,
    /// The application's menu, which a captioned window draws as its menu
    /// bar where the platform shows none.
    pub(crate) app_menu: Option<viso_platform::Menu>,
    /// The picks of the windows' drawn menu bars, until a frame takes them.
    pub(crate) menu_picks: app_menu::Picks,
    /// The links the app was asked to open that it has no window for yet.
    pub(crate) pending_urls: Vec<String>,
    /// What the session shows in the desktop shell.
    pub(crate) shell: shell::Shown,
    /// The PDF documents being written, each in the window it is built in.
    #[cfg(feature = "pdf")]
    pub(crate) pdf_jobs: Vec<(WindowId, crate::pdf::Running)>,
    /// The windows whose size and place are kept across launches.
    pub(crate) places: places::Places,
    /// The development session: the mounted views, their watched files and
    /// the reloads staged for the next frame.
    #[cfg(feature = "hot-reload")]
    pub(crate) hot_reload: crate::hot_reload::HotReloadSession,
    /// The UI scenario a headless run plays.
    #[cfg(feature = "headless")]
    pub(crate) script: Option<crate::scenario::ScriptRunner>,
    /// The view a headless run mounts alone in place of the app's.
    #[cfg(feature = "headless")]
    pub(crate) harness: crate::headless::Harness,
    /// The capture of the app's frames a tool asked for, if any.
    #[cfg(any(feature = "hot-reload", feature = "headless"))]
    pub(crate) profiler: crate::profile::Profiler,
    /// The first window's input, recorded while the launch names a tape.
    #[cfg(any(feature = "hot-reload", feature = "headless"))]
    pub(crate) recorder: Option<crate::tape::TapeRecorder>,
    /// The tape a headless run replays.
    #[cfg(any(feature = "hot-reload", feature = "headless"))]
    pub(crate) player: Option<crate::tape::TapePlayer>,
    /// The gamepads connected, in connection order.
    pub(crate) gamepads: Vec<viso_runtime::GamepadId>,
}

/// The fonts an app registered at init, installed into every window's shaper.
#[derive(Default)]
pub(crate) struct SessionFonts {
    /// Raw sfnt faces from `AppCx::load_font`, in call order.
    pub(crate) loaded: Vec<Box<[u8]>>,
    /// Faces packaged from `assets/fonts/` via `AppCx::use_packaged_fonts`.
    pub(crate) packaged: fonts::PackagedFonts,
}

/// Everything one window owns: its retained tree, reactive state, GPU surface,
/// live animations/timers, and the reusable scratch the frame hot path drains
/// into. The session-scope `Application` and `AppCx` stay on [`AppDriver`]; each
/// window holds an independent instance of everything below, so closing a window
/// drops exactly its tree and GPU resources (its `EffectStore` cancelled first,
/// so scoped effects release their resources — cleanup then drop).
/// How long a text field's caret shows, and then hides, while it blinks.
pub(crate) const CARET_BLINK: std::time::Duration = std::time::Duration::from_millis(530);

/// How long after it last moved a caret stops blinking and shows steadily, so
/// an idle field wakes no frames.
pub(crate) const CARET_IDLE: std::time::Duration = std::time::Duration::from_secs(10);

/// A focused text field as its caret last stood: the field, its text's
/// revision and its selection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct FieldKey {
    pub(crate) node: NodeId,
    pub(crate) revision: viso_text::ime::Revision,
    pub(crate) sel: viso_text::selection::Selection,
}

pub(crate) struct WindowState {
    /// The window this state drives. Input, geometry, and close events name a
    /// [`WindowId`]; the driver routes each to the matching state by this field.
    pub(crate) window: WindowId,
    /// The GPU state, created on launch once the window exposes a native
    /// windowing handle. `None` until then, and `None` for the whole session
    /// under a headless window (a `RawWindowHandle::Headless`): the tree still
    /// builds and lays out against `surface_size`, it just paints to no surface.
    pub(crate) gpu: Option<GpuState>,
    /// The current surface size in physical pixels, held independently of
    /// [`gpu`](Self::gpu). Layout resolves against this every frame, so the
    /// retained tree is measured/placed and its world rects re-derived even
    /// when there is no GPU (a headless window on any platform, including the
    /// Metal target where a headless surface cannot be created). `on_geometry`
    /// keeps it and the swapchain in step; seeded from the window's launch size.
    pub(crate) surface_size: (u32, u32),
    /// The window's device-pixel density (logical→physical scale), seeded from
    /// the platform at open and refreshed on every geometry change. Text shapes
    /// and rasterizes glyphs at this density, so a HiDPI surface gets crisp
    /// glyphs instead of 1x SDFs upscaled by the compositor. Independent of the
    /// GPU: a headless window still carries its reported scale.
    pub(crate) dpi: f32,
    /// The native chrome affordances' bounding box for a self-drawn-chrome window,
    /// in logical points (top-left origin, relative to the content area) — on
    /// macOS the traffic-light buttons. `None` until the platform reports one and
    /// for the whole life of a native-chrome window (which never fires the event).
    /// The app reads it to align its own caption around the native buttons; the
    /// facade also derives the caption's draggable strip from it and pushes that
    /// back to the platform so a press on the caption begins a native window drag.
    pub(crate) chrome_buttons: Option<LogicalRect>,
    /// The caption's draggable regions as last pushed to the platform, in logical
    /// points. The Layout phase collects the world boxes of the nodes a caption
    /// widget registered as draggable, converts them to logical rects, and pushes
    /// them through the `set_draggable_regions` back-channel only when they differ
    /// from this cache — so a steady frame whose caption did not move re-pushes
    /// nothing (the back-channel is a cold path). Empty until a
    /// caption first lays out; a window with no caption never fills it.
    pub(crate) draggable_cache: Vec<LogicalRect>,
    /// Who draws this window's chrome, known when the window opens (from its
    /// `WindowConfig`). Seeded into every (re)build's [`ChromeContext`] so a
    /// caption widget can decide whether self-drawn min/max/close buttons are
    /// allowed — the build-time half of the chrome data contract, paired with the
    /// later-frame [`chrome_buttons`](Self::chrome_buttons) width.
    pub(crate) chrome: WindowChrome,
    /// The root node of this window's self-drawn caption bar, captured when the
    /// tree was built (`None` for a native-chrome window that draws no caption).
    /// Held so a fullscreen transition can hide the caption in place — on macOS
    /// the OS draws its own auto-hiding title bar in fullscreen and removes the
    /// traffic lights — and restore it on exit, by toggling this node's
    /// visibility rather than rebuilding the tree.
    pub(crate) caption: Option<NodeId>,
    /// The root container a full-screen window pads to keep content clear of
    /// system UI (`None` on a framed desktop window), with the last reported
    /// safe area and on-screen keyboard height, in logical points.
    pub(crate) safe_area_root: Option<NodeId>,
    pub(crate) safe_area: LogicalInsets,
    pub(crate) keyboard_inset: f64,
    /// The accessibility tree published to the OS, present only while an
    /// assistive technology listens to this window.
    pub(crate) access: Option<accessibility::AccessBridge>,
    /// The retained UI tree: real nodes built once on launch, then relaid only
    /// where invalidated each frame and painted to primitives.
    pub(crate) store: NodeStore,
    /// Reactive state cells. Writes record a pending change; the frame's flush
    /// phase turns each changed cell into targeted node dirtying via `bindings`.
    pub(crate) states: StateStore,
    /// Compiled state→node edges. Built alongside the tree; read every flush.
    pub(crate) bindings: BindingTable,
    /// Pure cached derivations. The flush wakes those whose dependencies changed
    /// and dirties their downstream nodes only when the derived value changed.
    pub(crate) computeds: ComputedStore,
    /// Side effects scoped to nodes. The flush re-runs those whose dependencies
    /// changed; freeing a node cancels its effects (cleanup then drop).
    pub(crate) effects: EffectStore,
    /// Semantic-state projections keyed by node. A control registers one at build
    /// time (`bind_semantic_state`); the flush wakes those whose dependencies
    /// changed and writes each affected node's `semantic_state` column, so the
    /// semantics derive pass reads live accessibility state (checked / value /
    /// range) from a node column without a cross-layer read.
    pub(crate) projectors: SemanticProjector,
    /// Virtualized lists keyed by viewport node. Reconciled each frame before
    /// layout: reads each list's scroll, remounts only the visible range's hosts,
    /// keeps the content extent (so `scroll_range` is right) with ~40 mounted
    /// nodes instead of one per logical item. Driver-owned so a with-reactive
    /// build cx can register lists and the frame can drive reconcile.
    pub(crate) virtual_lists: VirtualLists,
    /// Retained text-edit buffers keyed by text-control node. A control registers
    /// its buffer at build time; a key/IME handler records `EditIntent`s that the
    /// router queues onto the node's buffer, and `text_edit::reconcile` (before
    /// text shaping each layout phase) applies them and re-declares the node's
    /// `TextRequest` when the text changed. Driver-owned, mirroring `virtual_lists`.
    pub(crate) text_edits: TextEdits,
    /// Live one-shot UI timers (a toast's auto-dismiss). Unlike a moving look, a
    /// live timer costs *no* frame while it waits: the driver surfaces its
    /// earliest deadline through `next_timer_deadline`, which the scheduler turns
    /// into a `ControlFlow::WaitUntil` so the loop blocks until the deadline
    /// instead of spinning. Fired once at the head of the flush phase with the
    /// frame instant; empties itself as timers fire so the loop can halt again.
    pub(crate) timers: TimerRegistry,
    /// Reusable buffer the flush drains the store's queued timer requests into
    /// before arming each on `timers` against the frame instant, so handing a
    /// frame's toast timer to the registry allocates nothing on the steady path.
    pub(crate) timer_requests: Vec<TimerRequest>,
    /// Reusable buffer the flush drains this frame's pending state ids into, so
    /// the steady path allocates nothing while draining the transaction.
    pub(crate) changed: Vec<StateId>,
    /// The tree root declared by the application's `build`, if it authored one.
    pub(crate) root: Option<NodeId>,
    /// The text-entry area last pushed to the platform, in logical points:
    /// `Some(Some(rect))` while a text control has focus, `Some(None)` once
    /// pushed clear, `None` before the first push. The Layout phase compares
    /// against it so a steady frame pushes nothing and the soft keyboard is
    /// shown or hidden only when focus moves into or out of text entry.
    pub(crate) text_entry: Option<Option<TextEntry>>,
    /// The text around the focused field's selection last pushed to the
    /// platform, with the field, its text's revision, selection and
    /// composition it was made from; `None` while none is shown.
    pub(crate) surrounding: Option<(SurroundingKey, Option<SurroundingText>)>,
    /// The edit menu last pushed to the platform, while it shows.
    pub(crate) edit_menu: Option<viso_platform::EditMenu>,
    /// The context menu last asked for, while the field asks for it.
    pub(crate) context_menu: Option<viso_platform::EditMenu>,
    /// The menus of edits the window draws where the platform draws none.
    pub(crate) drawn_menu: edit_menu::DrawnMenu,
    /// The magnifier last pushed to the platform, while it shows.
    pub(crate) loupe: Option<viso_platform::Loupe>,
    /// The cursor last pushed to the platform for the mouse.
    pub(crate) cursor: viso_platform::CursorIcon,
    /// The text field drawn with the focus last frame.
    pub(crate) marked: Option<NodeId>,
    /// The focused field's text and selection when its caret last moved, and
    /// when: its caret blinks from then.
    pub(crate) blink: Option<(FieldKey, Instant)>,
    /// The focused field's text and selection it last scrolled to show.
    pub(crate) revealed: Option<FieldKey>,
    /// When the caret next blinks, while it blinks.
    pub(crate) caret_due: Option<Instant>,
    /// When the next frame of an animated image playing in the window is due.
    pub(crate) animation_due: Option<Instant>,
    /// The animated images playing in the window.
    pub(crate) players: playback::Players,
    /// The players of the window's video nodes.
    pub(crate) videos: videos::Videos,
    /// What the videos' last step left: whether one plays on screen, and
    /// when one is next polled.
    pub(crate) video_step: videos::Step,
    /// Whether the window is the active one, taking the keys.
    pub(crate) active: bool,
    /// The marks a field drew before the ones it was just given, kept so
    /// marking a field each frame allocates nothing.
    pub(crate) marks: viso_ui::FieldMarks,
    /// Reusable ancestry buffer the pointer router fills each event, owned here
    /// so routing a pointer allocates nothing on the steady path.
    pub(crate) route_chain: Vec<NodeId>,
    /// Whether Alt is down with nothing pressed since: let go, it asks for
    /// the menu bar.
    pub(crate) alt_alone: bool,
    /// What happens in the window while an inspector follows it; `None`
    /// while none does.
    #[cfg(any(feature = "hot-reload", feature = "headless"))]
    pub(crate) journal: Option<Box<crate::inspect::Journal>>,
    /// Reusable scratch for [`WindowState::settle_text_edits`]: the edited
    /// nodes drained from the registry, and the text of the one being reported.
    pub(crate) edited: Vec<NodeId>,
    pub(crate) edited_text: String,
    /// Reusable primitive buffer. Rebuilt only on a paint-affecting frame; reused
    /// verbatim (and re-uploaded) on frames with no paint invalidation.
    pub(crate) primitives: Vec<Primitive>,
    /// Reusable child-id scratch for the layout passes.
    pub(crate) scratch: Vec<u32>,
    /// Reusable buffer of redo roots for incremental relayout, owned here so a
    /// relayout allocates nothing on the hot path.
    pub(crate) redo_roots: Vec<NodeId>,
    /// The failed-reload panel drawn over the tree, while an edit of a view
    /// this window mounts does not compile.
    #[cfg(feature = "hot-reload")]
    pub(crate) dev_overlay: Option<NodeId>,
    /// The frame time drawn over the tree, while a capture asks for it.
    #[cfg(feature = "hot-reload")]
    pub(crate) dev_hud: Option<NodeId>,
    /// How much each layer recomputed on the most recent frame — surfaced for
    /// diagnostics and asserted by tests to confirm only the dirty subtree moved.
    pub(crate) recompute: FrameRecompute,
    /// What this frame's paint did to `primitives`, for its upload.
    pub(crate) painted: viso_ui::Painted,
    /// Whether this frame's upload phase wrote primitives to the renderer.
    pub(crate) uploaded: bool,
    /// The measure/layout counts of the frame in progress, folded into
    /// `recompute` when its paint phase runs.
    pub(crate) laid: (u32, u32),
    /// How far into this frame's STYLE worklist the style phase resolved.
    pub(crate) style_seen: usize,
    /// True until the first frame has been submitted. Lets the Submit phase emit
    /// a one-shot diagnostic (gated on `VISO_DEBUG`) proving the first
    /// frame reached the GPU, then fall dark for every steady-state frame.
    pub(crate) awaiting_first_frame: bool,
    /// The revision of the store's asset table the app last served: a frame
    /// that moved nothing in it serves nothing.
    pub(crate) assets_served: u64,
    /// The device was lost and no new one opened yet.
    pub(crate) lost_gpu: Option<LostGpu>,
    /// How many lost devices were replaced.
    pub(crate) devices_replaced: u32,
    /// What the window's renderer holds of the programs its nodes paint
    /// with.
    pub(crate) shaders: shaders::InstalledShaders,
    /// The instant of the window's first submitted frame, which the clock
    /// its brushes read counts from.
    pub(crate) clock_origin: Option<viso_runtime::Instant>,
    /// Whether the frame last uploaded draws a brush reading the clock, which
    /// keeps the window drawing frames, as an animation does.
    pub(crate) draws_clock: bool,
    /// The facade's font stack + glyph atlas. Shapes each node's `TextRequest`
    /// into a `Content::Text` payload (`viso-ui` holds no font stack). `None`
    /// until launch, when a GPU backend exists to allocate the atlas; the app's
    /// own faces (from `AppCx::load_font`) are loaded into it then, ahead of the
    /// system fallbacks the first shape resolves. No font is bundled.
    pub(crate) text: Option<TextShaper>,
    /// Reusable buffer the text seam drains pending requests into, so re-shaping
    /// text allocates only the shaped payloads, not the request list.
    pub(crate) text_scratch: Vec<(NodeId, Box<TextRequest>)>,
    /// Reusable buffer the text commit drains the slots whose layout the
    /// worker finished into, so committing allocates nothing on the steady
    /// path.
    pub(crate) text_updated: Vec<ParagraphSlot>,
    /// What the text representation pass gathers each frame it runs: the
    /// text under a turn, as nodes and as runs, and the walk's stack.
    pub(crate) text_turned: Vec<(NodeId, viso_render::Affine2)>,
    pub(crate) text_turned_runs: Vec<(ParagraphSlot, f32, bool)>,
    pub(crate) text_walk: Vec<NodeId>,
    /// The instant the text representations count time from, set on the
    /// first frame they run.
    pub(crate) text_epoch: Option<Instant>,
    /// When a text representation decides anew with nothing moving.
    pub(crate) text_due: Option<Instant>,
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
    pub(crate) text_sources: std::collections::HashMap<NodeId, Box<TextRequest>>,
    /// Reusable buffer the Layout phase drains the store's queued text reflows
    /// into (`take_text_reflows`), so servicing a reflow allocates only the
    /// reshaped payloads. Empty and allocation-free on the steady path with no
    /// wrap-eligible text whose assigned width changed.
    pub(crate) reflow_scratch: Vec<(NodeId, f32)>,
    /// Reusable buffer the flush drains this window's queued window-open requests
    /// into before the driver appends them to its session-level pending list, so
    /// servicing a `window()` call reuses this window's own scratch rather than
    /// allocating one per frame. Empty on the steady path.
    pub(crate) scratch_opens: Vec<WindowOpenRequest>,
    /// Reusable buffer for this window's queued window-close ids, mirroring
    /// [`scratch_opens`](Self::scratch_opens).
    pub(crate) scratch_closes: Vec<u32>,
    /// Reusable buffer the flush polls this window's woken tasks into: one
    /// entry per task that finished, its continuation if it writes back.
    pub(crate) continuations: Vec<Option<Continuation>>,
    /// The pastes asked of the platform and not yet answered, by token: the
    /// node each goes to, or the focused text field (`None`).
    pub(crate) pastes: Vec<(u32, Option<NodeId>)>,
    /// The token the next paste request carries.
    pub(crate) next_paste: u32,
    /// Reusable buffer the flush drains this window's paste asks into.
    pub(crate) scratch_pastes: Vec<(NodeId, viso_ui::ClipboardKinds)>,
}

/// The facade-owned GPU state: the concrete backend, the renderer, and the
/// per-window surface.
///
/// The backend is the compile-time-selected concrete [`Backend`] (Metal on
/// macOS, software raster elsewhere), held by value so the frame hot path is
/// monomorphized — no `dyn GpuBackend`.
pub(crate) struct GpuState {
    pub(crate) backend: Backend,
    pub(crate) renderer: Renderer,
    /// The window's surface; `None` while the platform has taken the native
    /// window away (a backgrounded mobile app).
    pub(crate) surface: Option<SurfaceId>,
    /// Current surface size in physical pixels `(width, height)`.
    pub(crate) size: (u32, u32),
}

/// A window waiting for a device to replace the one it lost.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LostGpu {
    /// How many times none opened.
    pub(crate) tries: u32,
    /// When to try again.
    pub(crate) due: viso_runtime::Instant,
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

/// Run `f` with `ws`'s context placed at its root, as a handler there is
/// run: what it asks of the tree is applied as a handler's requests are.
pub(crate) fn at_root(ws: &mut WindowState, mut f: impl FnMut(&mut viso_ui::EventCx<'_>)) {
    match ws.root {
        Some(root) => {
            KeyRouter::run_at(
                &mut ws.store,
                &mut ws.states,
                &ws.bindings,
                &mut ws.text_edits,
                root,
                f,
            );
        }
        // A window that builds nothing has no tree to ask anything of: only
        // its state is there.
        None => f(&mut viso_ui::EventCx::__new(&mut ws.states, &ws.bindings)),
    }
}

impl<A: Application> AppDriver<A> {
    /// Hand the app each link it was asked to open, in order, once it has a
    /// window: with the launch window's context placed at its root, applying
    /// what the app asked of the tree as a handler's requests are, and
    /// framing the window.
    pub(crate) fn open_pending_urls(&mut self, cx: &mut RuntimeCx<'_>) {
        let (Some(app), Some(ws)) = (self.app.as_mut(), self.windows.first_mut()) else {
            return;
        };
        for url in std::mem::take(&mut self.pending_urls) {
            at_root(ws, |ev| app.on_open_url(ev, &url));
        }
        cx.request_redraw(ws.window);
    }

    /// Tell the app of `event` on the launch window's context.
    pub(crate) fn tell_lifecycle(&mut self, event: Lifecycle) {
        if let (Some(app), Some(ws)) = (self.app.as_mut(), self.windows.first_mut()) {
            at_root(ws, |ev| app.on_lifecycle(ev, event));
        }
    }

    /// Do what the drawn menu bars' picks ask: a command reaches the app,
    /// a standard action is done as the system's menu would.
    pub(crate) fn take_menu_picks(&mut self, cx: &mut RuntimeCx<'_>) {
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

    pub(crate) fn new(services: Option<services::Services>) -> Self {
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
            pace: pace::FramePace::default(),
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
            hot_reload: crate::hot_reload::HotReloadSession::default(),
            #[cfg(feature = "headless")]
            script: None,
            #[cfg(feature = "headless")]
            harness: crate::headless::Harness::default(),
            #[cfg(any(feature = "hot-reload", feature = "headless"))]
            profiler: crate::profile::Profiler::default(),
            #[cfg(any(feature = "hot-reload", feature = "headless"))]
            recorder: viso_platform::launch_var(viso_view::tape::RECORD_ENV)
                .map(|path| crate::tape::TapeRecorder::new(Some(path.into()))),
            #[cfg(any(feature = "hot-reload", feature = "headless"))]
            player: None,
            gamepads: Vec::new(),
        }
    }

    /// Hands the first window to the tape being recorded or replayed.
    #[allow(unused_variables)]
    pub(crate) fn adopt_tape(&mut self, ws: &WindowState) {
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
    pub(crate) fn drive_pdfs(&mut self, cx: &mut RuntimeCx<'_>) {
        for (pdf, done) in crate::pdf::take_asked() {
            let job = crate::pdf::Running::new(pdf, done);
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
    pub(crate) fn watch_budget(&mut self, cx: &mut RuntimeCx<'_>) {
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
    pub(crate) fn sweep_assets(&mut self) {
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
    pub(crate) fn play_animations(
        &mut self,
        now: Instant,
        waker: &dyn Fn() -> viso_platform::LoopWaker,
    ) {
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
    pub(crate) fn play_videos(&mut self, now: Instant) {
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
    pub(crate) fn window_mut(&mut self, id: WindowId) -> Option<&mut WindowState> {
        self.windows.iter_mut().find(|w| w.window == id)
    }
}
