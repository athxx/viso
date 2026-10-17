//! The runtime's answers to an inspector past the census: the detail of
//! chosen nodes of a window, and what a window's last frame drew and holds.
//!
//! Both read the window as its last frame left it, at the boundary before
//! the next one flushes, and allocate fresh: a tool asks, the frame path
//! never does.

use viso_text::GlyphImageKind;
use viso_ui::{AssetState, PaintRanges, paint_ranges};
#[cfg(feature = "headless")]
use viso_view::inspect::WindowDetail;
use viso_view::inspect::{BatchDetail, FrameDetail, NodeDetails, ResourceDetail, node_detail};

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
