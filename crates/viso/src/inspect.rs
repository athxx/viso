//! The runtime's answers to an inspector past the census: the detail of
//! chosen nodes of a window, what a window's last frame drew and holds, and
//! the journal of what happens in a window while an inspector follows it.
//!
//! The details read the window as its last frame left it, at the boundary
//! before the next one flushes, and allocate fresh: a tool asks, the frame
//! path never does. The journal exists only once an inspector asks for it,
//! and ends when it stops asking.

use std::collections::VecDeque;

use viso_text::GlyphImageKind;
use viso_ui::{
    AssetState, DirtyCause, DirtyMark, NodeId, NodeStore, PaintRanges, StateStore, paint_ranges,
};
#[cfg(feature = "headless")]
use viso_view::inspect::WindowDetail;
use viso_view::inspect::{
    BatchDetail, Cause, EventKind, EventLog, FrameDetail, InspectEvent, NodeDetails,
    ResourceDetail, node_detail,
};

use crate::WindowState;
use crate::assets::Assets;

/// The detail of the live `nodes` of `ws`, by index and generation.
pub(crate) fn nodes(ws: &WindowState, nodes: &[(u32, u32)]) -> NodeDetails {
    let mut primitives = Vec::new();
    let ranges = match ws.root {
        Some(root) => paint_ranges(&ws.store, root, &mut primitives),
        None => PaintRanges::default(),
    };
    let nodes = nodes
        .iter()
        .filter_map(|&(index, generation)| {
            let id = ws
                .store
                .arena()
                .live_id(index)
                .filter(|id| id.generation() == generation)?;
            node_detail(
                &ws.store,
                &ws.bindings,
                &ws.states,
                &ranges,
                &primitives,
                id,
            )
        })
        .collect();
    NodeDetails { nodes }
}

/// Every node of `ws`, the app's first window, and its last frame.
#[cfg(feature = "headless")]
pub(crate) fn window_detail(ws: &WindowState, assets: &Assets) -> WindowDetail {
    let all: Vec<(u32, u32)> = ws
        .root
        .map(|root| ws.store.inspect_tree(root).nodes)
        .unwrap_or_default()
        .iter()
        .map(|node| (node.id.index(), node.id.generation()))
        .collect();
    WindowDetail {
        nodes: nodes(ws, &all),
        frame: frame(ws, 0, assets),
    }
}

/// What the last frame of `ws`, the app's window `window`, drew and holds.
pub(crate) fn frame(ws: &WindowState, window: u32, assets: &Assets) -> FrameDetail {
    let mut counters: Vec<(String, u64)> = vec![
        ("nodes".into(), ws.store.arena().live_count() as u64),
        ("primitives".into(), ws.primitives.len() as u64),
    ];
    let mut batches = Vec::new();
    let mut resources = Vec::new();
    let mut add = |group: &str, name: &str, count: u64, bytes: Option<u64>, budget: Option<u64>| {
        resources.push(ResourceDetail {
            group: group.to_owned(),
            name: name.to_owned(),
            count,
            bytes,
            budget,
            note: None,
        });
    };
    if let Some(gpu) = &ws.gpu {
        let renderer = &gpu.renderer;
        let stats = renderer.frame_stats();
        counters.extend(
            [
                ("draw_calls", stats.draw_calls as u64),
                ("batches", stats.batches as u64),
                ("instances", stats.instances as u64),
                ("visible_primitives", u64::from(stats.visible_primitives)),
                ("dirty_primitives", u64::from(stats.dirty_primitives)),
                ("quad_instances", u64::from(stats.quad_instances)),
                ("glyph_instances", u64::from(stats.glyph_instances)),
                ("shader_instances", u64::from(stats.shader_instances)),
                ("render_chunks", stats.render_chunks as u64),
                ("render_passes", stats.render_passes as u64),
                ("offscreen_passes", stats.offscreen_passes as u64),
                ("pipeline_switches", u64::from(stats.pipeline_switches)),
                (
                    "texture_binding_switches",
                    u64::from(stats.texture_binding_switches),
                ),
                ("uploaded_ranges", stats.uploaded_ranges as u64),
                ("upload_bytes", stats.gpu_upload_bytes as u64),
                ("path_tessellations", u64::from(stats.path_tessellations)),
                ("clip_mask_builds", u64::from(stats.clip_mask_builds)),
                ("culled_primitives", stats.culled_primitives as u64),
            ]
            .map(|(name, value)| (name.to_owned(), value)),
        );
        batches = renderer
            .inspect_batches()
            .batches
            .iter()
            .map(|batch| BatchDetail {
                pipeline: batch.pipeline.label().to_owned(),
                range: batch.range,
                clip: batch.clip,
                offscreen: batch.offscreen,
                cost: format!("{:?}", batch.cost).to_lowercase(),
            })
            .collect();
        let held = renderer.resources();
        add("gpu", "pipelines", u64::from(held.pipelines), None, None);
        add(
            "gpu",
            "program_pipelines",
            u64::from(held.program_pipelines),
            None,
            None,
        );
        add(
            "gpu",
            "buffers",
            u64::from(held.buffers),
            Some(held.buffer_bytes),
            None,
        );
        add(
            "gpu",
            "gradient_lut",
            1,
            Some(held.gradient_lut_bytes),
            None,
        );
        add(
            "gpu",
            "mask_page",
            u64::from(held.masks),
            Some(held.mask_page_bytes),
            None,
        );
        add(
            "gpu",
            "offscreen_targets",
            u64::from(held.transient_targets),
            Some(held.transient_bytes),
            None,
        );
    }
    if let Some(text) = &ws.text {
        let residency = text.residency();
        for (pool, name) in [
            (GlyphImageKind::MaskA8, "glyph_atlas_mask"),
            (GlyphImageKind::ScalableMtsdf, "glyph_atlas_mtsdf"),
            (GlyphImageKind::ColorRgba8, "glyph_atlas_color"),
            (GlyphImageKind::OutlineVector, "glyph_outlines"),
        ] {
            let budget = residency.pool_budget(pool);
            let pages = residency.pool_page_count(pool) as u64;
            if pages > 0 {
                add(
                    "gpu",
                    name,
                    pages,
                    Some(pages * budget.page_bytes as u64),
                    Some(budget.pages as u64 * budget.page_bytes as u64),
                );
            }
            let glyphs = residency.pool_resident_glyphs(pool) as u64;
            if glyphs > 0 {
                add(
                    "cache",
                    &format!("{name}_glyphs"),
                    glyphs,
                    Some(residency.pool_resident_bytes(pool) as u64),
                    None,
                );
            }
        }
        let fonts = text.face_cache();
        add(
            "cache",
            "fonts",
            fonts.len() as u64,
            Some(fonts.total_bytes()),
            Some(fonts.budget_bytes()),
        );
        let shaping = text.shaping_cache();
        add(
            "cache",
            "shaped_runs",
            shaping.len() as u64,
            Some(shaping.bytes()),
            Some(shaping.budget_bytes()),
        );
    }
    let decoded = assets.counters();
    add("cache", "asset_decodes", decoded.decodes, None, None);
    add(
        "cache",
        "asset_decode_failures",
        decoded.decode_failures,
        None,
        None,
    );
    add(
        "cache",
        "asset_uploads",
        decoded.uploads,
        Some(decoded.upload_bytes),
        None,
    );
    for (id, state) in ws.store.assets().states() {
        let pixels = |[w, h]: [f32; 2]| (w.max(0.0) as u64) * (h.max(0.0) as u64) * 4;
        let sized = |state: &str, [w, h]: [f32; 2]| format!("{state} {w}x{h}");
        let (note, bytes) = match state {
            AssetState::Wanted => ("wanted".to_owned(), None),
            AssetState::Encoded { bytes } => ("encoded".to_owned(), Some(bytes)),
            AssetState::Decoded { size } => (sized("decoded", size), Some(pixels(size))),
            AssetState::Resident { size } => (sized("resident", size), Some(pixels(size))),
            AssetState::Vector { size } => (sized("vector", size), None),
        };
        resources.push(ResourceDetail {
            group: "asset".into(),
            name: format!("{:016x}", id.0),
            count: 1,
            bytes,
            budget: None,
            note: Some(note),
        });
    }
    FrameDetail {
        window,
        counters,
        batches,
        resources,
    }
}

/// What happens in a window while an inspector follows it: the input it
/// routes, focus moving and each direct dirty mark with its cause, numbered
/// from 1 and bounded, the oldest dropped first.
pub(crate) struct Journal {
    events: VecDeque<InspectEvent>,
    capacity: usize,
    /// The number the next event takes.
    next: u64,
    /// The frames since the journal started.
    frame: u64,
    /// The frames since an inspector last asked; `None` for a journal kept
    /// to the end of a headless run.
    idle: Option<u32>,
    focused: Option<NodeId>,
    marks: Vec<DirtyMark>,
}

impl Journal {
    /// The events a followed window keeps between two asks.
    #[cfg(feature = "hot-reload")]
    const FOLLOWED: usize = 8_192;
    /// The events a headless run keeps for the log it writes at its end.
    #[cfg(feature = "headless")]
    const KEPT: usize = 65_536;
    /// The frames without an ask after which a followed journal ends: about
    /// twenty seconds of continuous drawing; an idle app draws none.
    const UNASKED_FRAMES: u32 = 1_200;

    /// Starts following `store`, whose focus is where it is.
    #[cfg(feature = "hot-reload")]
    pub(crate) fn follow(store: &mut NodeStore) -> Box<Journal> {
        Self::start(store, Self::FOLLOWED, Some(0))
    }

    /// Starts a journal of `store` kept until the run ends.
    #[cfg(feature = "headless")]
    pub(crate) fn keep(store: &mut NodeStore) -> Box<Journal> {
        Self::start(store, Self::KEPT, None)
    }

    fn start(store: &mut NodeStore, capacity: usize, idle: Option<u32>) -> Box<Journal> {
        store.journal_dirty(true);
        Box::new(Journal {
            events: VecDeque::new(),
            capacity,
            next: 1,
            frame: 0,
            idle,
            focused: store.focused(),
            marks: Vec::new(),
        })
    }

    /// Stops recording `store`'s marks: the journal ends.
    pub(crate) fn end(store: &mut NodeStore) {
        store.journal_dirty(false);
    }

    /// A frame begins; whether the journal has gone unasked too long.
    pub(crate) fn begin_frame(&mut self) -> bool {
        self.frame += 1;
        match &mut self.idle {
            Some(idle) => {
                *idle += 1;
                *idle > Self::UNASKED_FRAMES
            }
            None => false,
        }
    }

    fn push(&mut self, kind: EventKind) {
        if self.events.len() == self.capacity {
            self.events.pop_front();
        }
        self.events.push_back(InspectEvent {
            seq: self.next,
            frame: self.frame,
            kind,
        });
        self.next += 1;
    }

    /// An input event `input`, at `at` in the window for a pointer, was
    /// routed through `route`, root first.
    pub(crate) fn input(&mut self, input: String, at: Option<[f32; 2]>, route: &[NodeId]) {
        let route = route.iter().map(|&node| id(node)).collect();
        self.push(EventKind::Input { input, at, route });
    }

    /// Records what `store` marked since the last call and where its focus
    /// moved, `states` naming the cells whose bindings fired.
    pub(crate) fn settle(&mut self, store: &mut NodeStore, states: &StateStore) {
        // Marks the bound dropped leave a gap in the numbering.
        self.next += u64::from(store.take_dirty_marks(&mut self.marks));
        let mut marks = std::mem::take(&mut self.marks);
        for mark in marks.drain(..) {
            let cause = match mark.cause {
                DirtyCause::Other => Cause::Other,
                DirtyCause::Input => Cause::Input,
                DirtyCause::State(cell) => Cause::State {
                    cell: (cell.index(), cell.generation()),
                    key: states.key_of(cell).map(|key| (key.hi, key.lo)),
                    value: states
                        .get(cell)
                        .map(viso_view::inspect::spell_value)
                        .unwrap_or_default(),
                },
                DirtyCause::Computed => Cause::Computed,
                DirtyCause::Animation => Cause::Animation,
                DirtyCause::Task => Cause::Task,
                DirtyCause::Reload => Cause::Reload,
                DirtyCause::Text => Cause::Text,
                DirtyCause::Asset => Cause::Asset,
                DirtyCause::Window => Cause::Window,
                DirtyCause::Layout => Cause::Layout,
            };
            self.push(EventKind::Dirty {
                node: id(mark.node),
                class: mark.class.bits(),
                cause,
            });
        }
        self.marks = marks;
        let focused = store.focused();
        if focused != self.focused {
            let from = self.focused.map(id);
            self.focused = focused;
            self.push(EventKind::Focus {
                from,
                to: focused.map(id),
            });
        }
    }

    /// The events of window `window` after number `after`, an inspector
    /// asking.
    #[cfg(feature = "hot-reload")]
    pub(crate) fn log(&mut self, window: u32, after: u64) -> EventLog {
        if let Some(idle) = &mut self.idle {
            *idle = 0;
        }
        self.read(window, after)
    }

    /// [`log`](Self::log), read without asking.
    pub(crate) fn read(&self, window: u32, after: u64) -> EventLog {
        let events: Vec<InspectEvent> = self
            .events
            .iter()
            .filter(|event| event.seq > after)
            .cloned()
            .collect();
        let first = events.first().map_or(self.next, |event| event.seq);
        EventLog {
            window,
            next: self.next,
            dropped: first.saturating_sub(after.saturating_add(1)),
            events,
        }
    }
}

fn id(node: NodeId) -> (u32, u32) {
    (node.index(), node.generation())
}

/// What input `sample` is, as a journal names it, and where it is in the
/// window for a pointer, `scale` its pixels per point: `None` for a pointer
/// move, which the hover it changes speaks for, and for a copy.
pub(crate) fn input_name(
    sample: &viso_runtime::InputSample,
    scale: f32,
) -> Option<(String, Option<[f32; 2]>)> {
    use viso_runtime::{InputSample, PointerPhase};
    Some(match sample {
        InputSample::Pointer(p) => {
            let phase = match p.phase {
                PointerPhase::Down => "pointer_down",
                PointerPhase::Up => "pointer_up",
                PointerPhase::Leave => "pointer_leave",
                PointerPhase::Cancel => "pointer_cancel",
                PointerPhase::Move => return None,
            };
            (phase.to_owned(), Some([p.x / scale, p.y / scale]))
        }
        InputSample::Key(k) => {
            let phase = match (k.pressed, k.repeat) {
                (true, false) => "key_down",
                (true, true) => "key_repeat",
                (false, _) => "key_up",
            };
            (format!("{phase} {:?}", k.key), None)
        }
        InputSample::Text(t) => (format!("text {:?}", t.text), None),
        InputSample::Paste(_) => ("paste".to_owned(), None),
        InputSample::ImePreedit(_) => ("ime_preedit".to_owned(), None),
        InputSample::Scroll(s) => (
            format!("scroll {} {}", s.delta_x / scale, s.delta_y / scale),
            Some([s.x / scale, s.y / scale]),
        ),
        InputSample::Copy(_) => return None,
    })
}
