//! What the driver does per window: open, lay out, paint, route input.

use std::any::Any;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use viso_gpu::{Backend, GpuBackend};
use viso_platform::{
    LogicalInsets, LogicalRect, RawWindowHandle, SurroundingText, TextEntry, WindowId,
};
use viso_render::Rect;
use viso_runtime::{Instant, RuntimeCx};
use viso_ui::{
    Axis, BindingTable, BuildCx, ChromeContext, ComputedStore, DirtyClass, EffectStore, FlexStyle,
    FrameRecompute, KeyEvent, KeyRouter, Modifiers, NodeId, NodeStore, SemanticProjector, Sizing,
    StateStore, TextEdits, TimerRegistry, VirtualLists, text_edit,
};
use viso_view::profile::Span;
use viso_widgets::caption_bar;

use super::access_keys::AccessKey;
use super::surrounding;
use super::{
    CARET_BLINK, CARET_IDLE, FieldKey, GpuState, LostGpu, PasteTo, SessionFonts, WindowState,
};
use crate::text_content::{self, ParagraphSlot, TextShaper};
use crate::{
    WindowChrome, WindowConfig, app_menu, edit_menu, environment, footprint, playback, shaders,
    videos,
};

impl WindowState {
    /// Shows the cursor the node under the mouse names; the platform hears of
    /// it only when it changes.
    pub(crate) fn push_cursor(&mut self, cx: &mut RuntimeCx<'_>) {
        let cursor = self.store.mouse_cursor();
        if cursor != self.cursor {
            self.cursor = cursor;
            cx.set_cursor(self.window, cursor);
        }
    }

    /// Hands the platform the pointer lock a node asked for, or its release;
    /// a platform without pointer lock answers the ask with the lock ended.
    pub(crate) fn push_pointer_lock(&mut self, cx: &mut RuntimeCx<'_>) {
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
    pub(crate) fn new(window: WindowId) -> Self {
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
            // An embedded profile's window holds its tree in a store of fixed
            // capacity, allocated once.
            store: footprint::budget()
                .nodes
                .map_or_else(NodeStore::new, NodeStore::with_fixed_capacity),
            states: StateStore::new(),
            bindings: BindingTable::new(),
            computeds: ComputedStore::new(),
            effects: EffectStore::new(),
            projectors: SemanticProjector::new(),
            virtual_lists: VirtualLists::new(),
            text_edits: TextEdits::new(),
            timers: TimerRegistry::new(),
            timer_requests: Vec::new(),
            changed: Vec::new(),
            root: None,
            text_entry: None,
            surrounding: None,
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
            lost_gpu: None,
            devices_replaced: 0,
            paint_whole: false,
            shaders: shaders::InstalledShaders::default(),
            clock_origin: None,
            draws_clock: false,
            text: None,
            text_scratch: Vec::new(),
            text_sources: std::collections::HashMap::new(),
            text_updated: Vec::new(),
            text_turned: Vec::new(),
            text_turned_runs: Vec::new(),
            text_walk: Vec::new(),
            text_epoch: None,
            text_due: None,
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
    pub(crate) fn open(
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
        // the renderer for its format.
        if let Some(raw) = native_handle(cx, window) {
            ws.attach_gpu(viso_gpu::create_device(), raw, fonts);
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
    pub(crate) fn shape_pending_text(&mut self) {
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
    /// one. Inline shaping spends from `pace`. A no-op when the worker owes
    /// nothing (the steady case).
    pub(crate) fn commit_text_work(&mut self, pace: &mut super::pace::FramePace) {
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
            text_content::text_commit_budget(pace.interval())
        };
        let shaping = text.pump(
            &mut gpu.backend,
            budget,
            pace.slice(),
            &mut self.text_updated,
        );
        pace.charge(shaping);
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
    pub(crate) fn replace_packaged_font(
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

    /// Hand the text worker every layout this frame asked for, in one batch;
    /// what layout settled on draws from now on.
    pub(crate) fn dispatch_text_work(&mut self) {
        if let Some(text) = self.text.as_mut() {
            text.settle_priors();
            text.dispatch();
        }
    }

    /// Whether a node of the window is mid-slide or a look property mid-move.
    pub(crate) fn is_animating(&self) -> bool {
        self.store.is_transitioning()
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
            // A list still owes the rows its viewport covers.
            || viso_ui::virtual_list::unsettled(&self.store, &self.virtual_lists)
            || self.video_step.beating
            || self.draws_clock
            // A device lost in the last frame is replaced in the next.
            || self.gpu.as_ref().is_some_and(|g| g.backend.is_lost())
    }

    /// Whether the text worker still owes results a later frame commits.
    pub(crate) fn text_work_pending(&self) -> bool {
        self.text.as_ref().is_some_and(TextShaper::has_pending_work)
    }

    /// Tell the nodes the focus moved between, and settle the edits what
    /// they did about it asked for.
    pub(crate) fn tell_focus(&mut self) {
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
    pub(crate) fn settle_text_edits(&mut self) {
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
    pub(crate) fn device_bytes(&self) -> u64 {
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

    /// Bring the window's GPU up on `backend`: a surface on the native window
    /// `raw`, the renderer for the surface's format and color space, and the
    /// text stack on that device, the session's fonts registered into it.
    fn attach_gpu(&mut self, mut backend: Backend, raw: RawWindowHandle, fonts: &SessionFonts) {
        let (w, h) = self.surface_size;
        #[cfg(not(feature = "headless"))]
        backend.set_cpu_workers(footprint::workers());
        let surface = backend.create_surface(raw, w, h);
        // Both the format and the color space come from the surface, so the
        // renderer's intermediate targets are planned for the domain this
        // window actually composites in.
        let renderer = footprint::renderer(&mut backend, surface);
        self.gpu = Some(GpuState {
            backend,
            renderer,
            surface: Some(surface),
            size: (w, h),
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
            // sfnt bytes rather than moving the shared registration out.
            shaper.load_font(face.clone(), 0);
        }
        self.text = Some(shaper);
    }

    /// Whether the window's device was lost, its GPU state let go of and
    /// waiting for a new one.
    pub(crate) fn gpu_lost(&self) -> bool {
        self.lost_gpu.is_some() || self.gpu.as_ref().is_some_and(|g| g.backend.is_lost())
    }

    /// Whether the window's device was lost and the window has not started
    /// on a new one: the next frame does.
    pub(crate) fn gpu_newly_lost(&self) -> bool {
        self.lost_gpu.is_none() && self.gpu.as_ref().is_some_and(|g| g.backend.is_lost())
    }

    /// Whether the window has work its next frame does unasked: an asset
    /// offered after the frame served it (a list row mounted at layout shows
    /// one), or a device lost to replace.
    pub(crate) fn owes_frame(&self) -> bool {
        self.store.assets().revision() != self.assets_served || self.gpu_newly_lost()
    }

    /// Replaces a lost device, once a new one opens and the platform has a
    /// native window `raw` to draw in. What the old device held — the window's
    /// GPU state, its glyph atlases, the textures, models and scenes its
    /// tables uploaded, its programs — is let go of unreleased, and built again
    /// on the new one from what the window keeps: the tree is painted whole,
    /// its text shaped again, its assets decoded again. While no device opens
    /// (the GPU still resetting, an external one unplugged), the window draws
    /// nothing and retries, a quarter second after the first try and doubling
    /// up to four. Returns whether it has a device again.
    pub(crate) fn recover_gpu(
        &mut self,
        raw: Option<RawWindowHandle>,
        fonts: &SessionFonts,
        now: Instant,
    ) -> bool {
        if self.lost_gpu.is_none() {
            eprintln!("[viso] the GPU device was lost; drawing the window again on a new one");
            self.gpu = None;
            self.text = None;
            self.store.forget_device();
            self.videos.forget_device();
            self.shaders = shaders::InstalledShaders::default();
            self.lost_gpu = Some(LostGpu { tries: 0, due: now });
        }
        let Some(lost) = &mut self.lost_gpu else {
            return true;
        };
        if now < lost.due {
            return false;
        }
        let Some(raw) = raw else {
            return false;
        };
        let Some(backend) = viso_gpu::try_create_device() else {
            lost.due = now + Duration::from_millis(250 << lost.tries.min(4));
            lost.tries = lost.tries.saturating_add(1);
            return false;
        };
        self.lost_gpu = None;
        self.devices_replaced += 1;
        self.paint_whole = true;
        self.attach_gpu(backend, raw, fonts);
        // Every table is served again, uploading into the new device.
        self.assets_served = u64::MAX;
        for (&id, request) in &self.text_sources {
            self.store.set_text_request(id, (**request).clone());
        }
        self.shape_pending_text();
        if let Some(root) = self.root {
            self.store.mark_dirty(root, DirtyClass::PAINT);
        }
        true
    }

    /// Give back every GPU cache the next frame can rebuild: idle pooled
    /// targets, and every glyph page no mounted text draws from. Unmounted
    /// text is pruned first so it holds no page. Mounted text is then
    /// reshaped, which re-admits anything a shed page held — none of its own
    /// pages is shed, so that costs no raster and nothing on screen goes
    /// missing.
    pub(crate) fn trim_memory(&mut self) {
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
    pub(crate) fn follow_scale_factor(&mut self) {
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
    pub(crate) fn logical_surface(&self) -> (f32, f32) {
        let (w, h) = self.surface_size;
        let scale = self.dpi.max(1.0);
        (w as f32 / scale, h as f32 / scale)
    }

    /// Every render phase at once, outside the frame loop: style, layout,
    /// transforms and paint — what a test of a commit landing between frames
    /// shows its result with.
    #[cfg(all(test, feature = "hot-reload"))]
    pub(crate) fn relayout_and_paint(&mut self) {
        self.resolve_style();
        self.relayout();
        self.update_transforms();
        self.repaint();
    }

    /// The `ResolveStyle` phase: re-select interaction-state boxes when a
    /// STYLE class is pending. A press/hover flip marks its node STYLE (and
    /// PAINT) through the flush; this folds the winning variant (pressed >
    /// hover > resting) into the warm `style` the measure and paint passes
    /// read. The STYLE worklist is empty on a frame with no interaction change, which
    /// then pays one length check.
    ///
    /// It resolves only what gained STYLE since it last ran this frame, so the
    /// paint phase calls it again to pick up a node a virtual list mounted
    /// during layout.
    pub(crate) fn resolve_style(&mut self) {
        let listed = self.store.dirty_lists().of(DirtyClass::STYLE).len();
        // A clear outside the frame loop emptied the lists under the cursor.
        if listed < self.style_seen {
            self.style_seen = 0;
        }
        if listed > self.style_seen {
            let _style = crate::profile::time(Span::Style);
            let (_, seen) = self
                .store
                .resolve_interaction_styles_since(&self.states, self.style_seen);
            self.style_seen = seen;
        }
    }

    /// The `Measure` and `Layout` phases: re-measure and re-place the dirty
    /// subtrees, then settle the adaptive environment against the boxes just
    /// placed. Counts accumulate into the frame's recompute until its paint.
    pub(crate) fn relayout(&mut self) {
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
        let _layout = crate::profile::time(Span::Layout);
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
    pub(crate) fn update_transforms(&mut self) {
        // A scroll or a translate moved anchors without a layout: the layers
        // anchored to them follow.
        if self.root.is_some() && self.store.transforms_pending() {
            self.store.follow_anchors();
        }
        if self.root.is_some() && self.store.transforms_pending() {
            let _layout = crate::profile::time(Span::Layout);
            self.store.update_transforms();
        }
    }

    /// Resolve how the text under a turn draws: a run scaled or rotated for
    /// long enough draws from distance fields, and settles back to coverage
    /// at the size it holds still at. Each run whose representation switched
    /// draws again at the extent it has. A frame with nothing turned and no
    /// run settling back does nothing.
    pub(crate) fn represent_text(&mut self, now: Instant) {
        let Some(text) = self.text.as_mut() else {
            return;
        };
        self.store.transforms().turned_text(
            &self.store,
            &mut self.text_turned,
            &mut self.text_walk,
        );
        self.text_turned_runs.clear();
        self.text_turned_runs
            .extend(self.text_turned.iter().map(|&(id, linear)| {
                (
                    ParagraphSlot::from(id),
                    linear.mean_scale(),
                    !linear.is_scale_translate(),
                )
            }));
        let epoch = *self.text_epoch.get_or_insert(now);
        text.represent(
            now.saturating_duration_since(epoch),
            &self.text_turned_runs,
            &mut self.text_updated,
        );
        self.text_due = text.representation_deadline().map(|due| epoch + due);
        if self.text_updated.is_empty() {
            return;
        }
        let Some(gpu) = self.gpu.as_mut() else {
            self.text_updated.clear();
            return;
        };
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
            self.store.set_painted_content(id, content);
        }
    }

    /// The `BuildPaintChanges` phase: re-emit the primitives when a
    /// paint-affecting class is pending, and close the frame's recompute
    /// counts.
    pub(crate) fn repaint(&mut self) {
        let whole = std::mem::take(&mut self.paint_whole);
        self.paint(whole);
    }

    /// Re-emit the primitives — when a paint-affecting class is pending, or
    /// whatever is dirty when `force`d — and close the frame's recompute counts.
    pub(crate) fn paint(&mut self, force: bool) {
        let Some(root) = self.root else {
            return;
        };
        self.resolve_style();
        let _paint = crate::profile::time(Span::Paint);
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
                viso_ui::Painted::Whole(n + crate::hot_reload::overlay::paint(self, surface, n))
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
    /// Puts the videos drawn plainly and on top on `display`'s planes, as
    /// the frame just uploaded draws them.
    pub(crate) fn scan_out_videos(&mut self, display: &dyn viso_gpu::Scanout, now: Instant) {
        let Some(gpu) = &self.gpu else {
            return;
        };
        let scale = self.dpi.max(1.0);
        let (w, h) = gpu.size;
        let viewport = [w as f32 / scale, h as f32 / scale];
        self.videos
            .scan_out(display, &gpu.renderer, viewport, scale, now);
    }

    pub(crate) fn upload_gpu_changes(&mut self) {
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
    /// A fresh token for a paste that goes to `to`, kept until its answer
    /// arrives.
    pub(crate) fn paste_token(&mut self, to: PasteTo) -> u32 {
        let token = self.next_paste;
        self.next_paste = self.next_paste.wrapping_add(1);
        // A request the platform never answered is dropped, not kept; a read
        // waiting on it finishes empty.
        if self.pastes.len() >= 64
            && let (_, PasteTo::Read(read)) = self.pastes.remove(0)
        {
            read.answer(None);
        }
        self.pastes.push((token, to));
        token
    }

    /// Where a paste gesture goes and what it takes: the focused text field
    /// takes plain text; otherwise the nearest paste target from the focus
    /// (from the root with nothing focused) takes its kinds; with none, the
    /// focused node hears plain text.
    pub(crate) fn paste_route(&self, root: NodeId) -> (Option<NodeId>, viso_ui::ClipboardKinds) {
        if self.focused_text_node().is_some() {
            return (None, viso_ui::ClipboardKinds::TEXT);
        }
        let from = self.store.focused().unwrap_or(root);
        match self.store.paste_target_from(from) {
            Some((node, kinds)) => (Some(node), kinds),
            None => (None, viso_ui::ClipboardKinds::TEXT),
        }
    }

    pub(crate) fn focused_text_node(&self) -> Option<NodeId> {
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
    pub(crate) fn edit_menu_changed(&mut self, with_context: bool) -> bool {
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
    pub(crate) fn loupe_changed(&mut self) -> bool {
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
    pub(crate) fn context_menu_changed(&mut self) -> bool {
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
    pub(crate) fn update_drawn_menu(&mut self, cx: &mut RuntimeCx<'_>) {
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
    pub(crate) fn apply_menu_edit(
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
                let token = self.paste_token(PasteTo::Field);
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
    pub(crate) fn route_access_key(
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
    pub(crate) fn close_drawn_menu(&mut self) {
        if let Some(node) = self.focused_text_node() {
            self.store.queue_edit(
                node,
                text_edit::EditIntent::Touch(text_edit::TouchEdit::MenuClosed),
            );
        }
    }

    /// The menu of edits field `id` offers, pointing at `at` in its text
    /// space, kept inside the field's box.
    pub(crate) fn edit_menu_at(
        &self,
        id: NodeId,
        at: viso_ui::Rect,
    ) -> Option<viso_platform::EditMenu> {
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
    pub(crate) fn text_entry_changed(&mut self) -> bool {
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

    /// Recompute the text around the focused field's selection that the
    /// input method reads, and return it when it differs from what was last
    /// pushed. Only a field that shows its text shows it: not a secret, a
    /// read-only field or a control that keeps its own document.
    pub(crate) fn surrounding_changed(&mut self) -> Option<Option<SurroundingText>> {
        let field = self.focused_text_node().and_then(|id| {
            let buffer = self.text_edits.get(id)?;
            (!buffer.look.read_only && !buffer.look.secure).then_some((id, buffer))
        });
        let Some((id, buffer)) = field else {
            return self.surrounding.take().map(|_| None);
        };
        let composition = buffer.composition();
        let composition = composition
            .is_active()
            .then(|| composition.range())
            .map(|(a, b)| (a.0, b.0));
        let key = (
            id,
            buffer.revision(),
            (buffer.sel.anchor.offset.0, buffer.sel.focus.offset.0),
            composition.unwrap_or_default(),
        );
        let kept = match &self.surrounding {
            Some((last, _)) if *last == key => return None,
            Some((last, Some(shown))) if last.0 == id => Some(shown.offset),
            _ => None,
        };
        let shown = surrounding::surrounding(&buffer.text, key.2.0, key.2.1, composition, kept);
        let changed = self.surrounding.as_ref().is_none_or(|(_, s)| *s != shown);
        self.surrounding = Some((key, shown.clone()));
        changed.then_some(shown)
    }

    /// Give the focused text field its caret, selection and composition
    /// marks and scroll it to show its caret, and take them from the field
    /// that lost the focus. The caret shows steadily for [`CARET_BLINK`]
    /// after it moves, then blinks until [`CARET_IDLE`] passes, then shows
    /// steadily again; it never blinks under reduced motion, and shows only
    /// while the window is active.
    pub(crate) fn update_field_marks(&mut self, now: Instant) {
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
    pub(crate) fn mark_field(&mut self, node: NodeId, focused: bool, now: Instant) {
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

    pub(crate) fn draggable_regions_changed(&mut self) -> bool {
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
    pub(crate) fn reflow_wrapped_text(&mut self) -> u32 {
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
pub(crate) fn to_platform_config(cfg: &WindowConfig) -> viso_platform::WindowConfig {
    viso_platform::WindowConfig {
        title: cfg.title.clone(),
        logical_size: cfg.size,
        chrome: match cfg.chrome {
            WindowChrome::Native => viso_platform::WindowChrome::Native,
            WindowChrome::SelfDrawn => viso_platform::WindowChrome::SelfDrawn,
        },
        position: None,
        maximized: false,
        cpu_frames: cpu_frames(),
    }
}

/// Whether a window's frames are drawn on the CPU and shown by the platform:
/// in a CPU-drawn build, and in a browser that opened no WebGPU device.
fn cpu_frames() -> bool {
    #[cfg(all(
        target_arch = "wasm32",
        target_os = "unknown",
        not(any(feature = "headless", feature = "cpu"))
    ))]
    {
        !viso_gpu::webgpu::is_prepared()
    }
    #[cfg(not(all(
        target_arch = "wasm32",
        target_os = "unknown",
        not(any(feature = "headless", feature = "cpu"))
    )))]
    {
        !cfg!(feature = "headless")
            && <viso_gpu::Backend as viso_gpu::GpuBackend>::SHADER_LANG
                == viso_gpu::ShaderLang::None
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
pub(crate) fn wrap_root_with_caption(
    cx: &mut BuildCx<'_>,
    title: &str,
    caption: bool,
    caption_out: &Cell<Option<NodeId>>,
    bar: Option<viso_widgets::MenuBar>,
    build_content: impl FnOnce(&mut BuildCx<'_>) -> Option<NodeId>,
) -> Option<NodeId> {
    if super::PANEL_ONLY || !caption {
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
            size: Sizing::fill(),
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
                    size: Sizing::fill(),
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
pub(crate) fn drawn_menu_bar(
    menu: Option<&viso_platform::Menu>,
    picks: &app_menu::Picks,
    cx: &RuntimeCx<'_>,
    window: WindowId,
    caption: bool,
) -> Option<viso_widgets::MenuBar> {
    if super::PANEL_ONLY || !caption || cx.capabilities().menus {
        return None;
    }
    app_menu::bar(menu?, window, picks)
}

/// Wrap a full-screen window's content (mobile, a browser tab) in a fill
/// Column the facade pads by the safe area and keyboard, so content stays
/// clear of the status bar, notch, home indicator and on-screen keyboard. The
/// Column is the window root; such a window draws no caption.
pub(crate) fn wrap_root_in_safe_area(
    cx: &mut BuildCx<'_>,
    build_content: impl FnOnce(&mut BuildCx<'_>) -> Option<NodeId>,
) -> Option<NodeId> {
    let root = cx.flex(
        FlexStyle {
            axis: Axis::Column,
            align: viso_ui::Align::Stretch,
            size: Sizing::fill(),
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
    pub(crate) fn apply_safe_area(&mut self) {
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

/// The text-entry area for a control at `world` whose caret, relative to the
/// run's origin, is `caret`: a one-pixel column on the caret's line, kept inside
/// the control, or the whole control when no caret geometry exists.
pub(crate) fn caret_area(world: Rect, caret: Option<Rect>) -> LogicalRect {
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
    use viso_ui::{BoxStyle, LeafStyle, Sizing};

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
                size: Sizing::fixed(logical.0, logical.1),
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
        ws.store.set_fixed_size(root, Sizing::fixed(300.0, 80.0));
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

/// The native window `window` draws in, when it has one a device can draw
/// in. A headless window reports a `RawWindowHandle::Headless` (or no handle
/// at all) — there is no surface to attach, and on a real-GPU target such as
/// Metal `create_surface` would reject the non-native handle — so it stays
/// without a GPU for the session and draws nothing; its retained tree is built
/// and driven regardless: the headless backend is a first-class path, not a
/// degraded one. A headless build draws every window through the software
/// raster, which takes the headless handle.
pub(crate) fn native_handle(cx: &RuntimeCx<'_>, window: WindowId) -> Option<RawWindowHandle> {
    match cx.raw_handle(window) {
        Some(RawWindowHandle::Headless) if cfg!(feature = "headless") => {
            Some(RawWindowHandle::Headless)
        }
        Some(RawWindowHandle::Headless) | None => None,
        Some(handle) => Some(handle),
    }
}
