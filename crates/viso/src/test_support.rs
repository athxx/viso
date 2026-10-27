//! Deterministic headless-loop harness for facade integration tests.
//!
//! Not part of the public API — this is the first-class headless
//! seam, hidden from docs and only meant to be reached by this crate's own
//! integration tests. It drives the *real* `AppDriver` frame loop (the same
//! one [`run`](crate::run) uses) through a [`Scheduler`] over a scripted `HeadlessApp`,
//! injecting a self-stepping [`FixedStepClock`] so every animation frame
//! observes a fixed, reproducible delta with no test intervention between
//! beats (the pump loops internally while `wants_animation` holds). It returns
//! the driver after the pump exits so a test can inspect the settled retained
//! tree — world rects, per-frame recompute counts, and whether the loop halted.

pub use crate::game_sheet::render as render_game_sheet;
#[cfg(any(feature = "hot-reload", feature = "headless"))]
pub use viso_view::tape::UiTape;

use std::time::Duration;

use viso_runtime::{FixedStepClock, Instant, Scheduler};
use viso_ui::{FrameRecompute, NodeId, NodeStore, StateStore};

use crate::Application;
use crate::driver::AppDriver;

/// Inspection view over a settled `AppDriver`, returned from
/// [`drive_scripted`]. Exposes exactly what the Commit-4 facade tests read:
/// the retained store (for `world`/`dirty`/`translate`), the tree root, the
/// last frame's recompute counts, and the loop's animation state.
pub struct DrivenApp<A: Application> {
    driver: AppDriver<A>,
}

impl<A: Application> DrivenApp<A> {
    /// The retained tree of the first (launch) window. Read
    /// `world`/`bounds`/`translate` off it.
    pub fn store(&self) -> &NodeStore {
        &self.driver.windows[0].store
    }

    /// The first window's declared root node, if any.
    pub fn root(&self) -> Option<NodeId> {
        self.driver.windows[0].root
    }

    /// What the first window last published to an assistive technology,
    /// in id order; `None` while none listens.
    pub fn published(&self) -> Option<Vec<viso_platform::AccessNode>> {
        let access = self.driver.windows[0].access.as_ref()?;
        let mut nodes: Vec<_> = access.published().cloned().collect();
        nodes.sort_unstable_by_key(|node| node.id);
        Some(nodes)
    }

    /// The first window's reactive state cells.
    pub fn states(&self) -> &StateStore {
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
    pub fn store_at(&self, index: usize) -> &NodeStore {
        &self.driver.windows[index].store
    }

    /// The declared root of the window at `index`, if any.
    pub fn root_at(&self, index: usize) -> Option<NodeId> {
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
    pub fn field_text(&self, node: NodeId) -> Option<&str> {
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

    /// The underlines the focused field draws under its composition, in
    /// the field's own coordinates.
    pub fn focused_underlines(&self) -> Option<&[viso_ui::Rect]> {
        let ws = &self.driver.windows[0];
        let node = ws.focused_text_node()?;
        Some(&ws.store.field_marks(node)?.underlines)
    }

    /// The text around the focused field's selection last shown to the
    /// first window's input method.
    pub fn surrounding_text(&self) -> Option<&viso_platform::SurroundingText> {
        self.driver.windows[0].surrounding.as_ref()?.1.as_ref()
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

    /// What the launch window's last submit redrew: nothing, the whole
    /// surface, or a few rects in device pixels; `None` without a surface.
    pub fn damage(&self) -> Option<viso_render::damage::Region> {
        let gpu = self.driver.windows[0].gpu.as_ref()?;
        Some(*gpu.renderer.last_damage())
    }

    /// How many devices the launch window lost and drew on anew.
    pub fn devices_replaced(&self) -> u32 {
        self.driver.windows[0].devices_replaced
    }

    /// How much each layer recomputed on the first window's most recent
    /// frame. A pure TRANSFORM animation frame has `laid_out == 0` yet
    /// `painted > 0` — the observable proof that world moved without a
    /// relayout.
    pub fn recompute(&self) -> FrameRecompute {
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
    /// come from the window's live [`Renderer`](viso_render::Renderer) when it has
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

/// The id node `node` is published to an assistive technology under.
pub fn semantics_id(node: NodeId) -> viso_platform::SemanticsId {
    crate::accessibility::semantics_id(node)
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
    let driver = fresh_driver::<A>(Some(crate::services::Services::unsupported()));
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
    let driver = fresh_driver::<A>(Some(crate::services::Services::unsupported()));
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
    let mut driver = fresh_driver::<A>(Some(crate::services::Services::unsupported()));
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
    let mut driver = fresh_driver::<A>(Some(crate::services::Services::unsupported()));
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

/// A driver for a run that numbers its trees as every run on this thread
/// does, so identical scripts give identical node handles.
fn fresh_driver<A: Application>(services: Option<crate::services::Services>) -> AppDriver<A> {
    viso_ui::node::restart_owners();
    AppDriver::<A>::new(services)
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
    let driver = fresh_driver::<A>(Some(services));
    let clock = FixedStepClock::new(Instant::now(), step);
    let driver = Scheduler::with_clock(app, driver, clock).run_returning();
    DrivenApp { driver }
}

pub use crate::text_harness::{LineShape, TextDraw, TextHarness, TextStats, text_commit_budget};

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

thread_local! {
    static LOSE_DEVICE: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

/// Loses the device of the window this thread's next drive submits a frame
/// on after `submits` frames, once, as a GPU reset would; `None` loses none.
pub fn lose_device_after(submits: Option<u32>) {
    LOSE_DEVICE.with(|l| l.set(submits));
}

/// Whether the frame just submitted is the one to lose the device after.
#[cfg(feature = "headless")]
pub(crate) fn device_lost_now() -> bool {
    LOSE_DEVICE.with(|l| match l.get() {
        Some(0) | None => false,
        Some(1) => {
            l.set(None);
            true
        }
        Some(n) => {
            l.set(Some(n - 1));
            false
        }
    })
}
