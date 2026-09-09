//! The Inspector facade end to end: build a real retained tree, lay it out,
//! paint it, and drive it through the headless renderer, then aggregate every
//! architecture section 62 introspection surface into one `InspectSnapshot` and
//! serialize it to JSON — the one model Studio transport (Slice B) and
//! `viso inspect --json` (Slice C) share (architecture section 34), proven with
//! a *live* renderer producing *real* batches rather than a fabricated snapshot.
//!
//! Like `headless_scene`, it assembles the scene from public building blocks and
//! rasterizes through the first-class headless backend (section 66) — the same
//! Component → Node → Flex → paint → renderer pipeline the facade drives on
//! Metal, but deterministic and display-free. The snapshot is a cold-path
//! readout (section 7.2): it only reads `&self` accessors and the renderer's own
//! batch/stats snapshot, mutating nothing.

use viso::gpu::{GpuBackend, HeadlessRaster, RawWindowHandle};
use viso::render::{Rect, Renderer, Rgba};
use viso::snapshot_ui;
use viso::ui::{
    Align, Axis, BoxStyle, BuildCx, FlexStyle, Inset, Justify, LeafStyle, Length, NodeId,
    NodeStore, Size, paint_tree,
};

const W: u32 = 200;
const H: u32 = 120;

const DARK: Rgba = Rgba {
    r: 0.15,
    g: 0.16,
    b: 0.20,
    a: 1.0,
};
const RED: Rgba = Rgba {
    r: 0.9,
    g: 0.1,
    b: 0.1,
    a: 1.0,
};
const GREEN: Rgba = Rgba {
    r: 0.1,
    g: 0.7,
    b: 0.3,
    a: 1.0,
};

/// A padded Row with a dark background holding two colored leaves — the same
/// shape family as `headless_scene`, small enough to reason about the snapshot.
fn build(store: &mut NodeStore) -> NodeId {
    let mut cx = BuildCx::new(store);
    cx.flex(
        FlexStyle {
            axis: Axis::Row,
            gap: 8.0,
            padding: Inset::all(12.0),
            align: Align::Center,
            justify: Justify::Start,
            size: Size::fill(),
            style: BoxStyle::solid(DARK),
        },
        |cx| {
            cx.leaf(LeafStyle {
                size: Size::fixed(48.0, 40.0),
                style: BoxStyle::solid(RED).with_radius(8.0),
            });
            cx.leaf(LeafStyle {
                size: Size {
                    width: Length::fill(),
                    height: Length::Fixed(56.0),
                },
                style: BoxStyle::solid(GREEN).with_radius(4.0),
            });
        },
    );
    cx.root().expect("scene has a root")
}

/// Lay out `store`'s tree, paint it, and drive it through a headless renderer so
/// its batch/stats snapshot reflects a real encoded frame. Returns the live
/// renderer alongside the scene so the snapshot reads them in the same window.
fn settled_scene() -> (NodeStore, NodeId, Renderer, HeadlessRaster) {
    let mut store = NodeStore::new();
    let root = build(&mut store);

    let surface_rect = Rect {
        x: 0.0,
        y: 0.0,
        w: W as f32,
        h: H as f32,
    };
    let mut scratch = Vec::new();
    store.layout(root, surface_rect, &mut scratch);

    let mut primitives = Vec::new();
    paint_tree(&store, root, &mut primitives);

    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);
    renderer.upload(&mut gpu, &primitives);

    (store, root, renderer, gpu)
}

#[test]
fn snapshot_aggregates_a_live_frame() {
    let (store, root, renderer, _gpu) = settled_scene();
    let snap = snapshot_ui(
        &store,
        root,
        renderer.inspect_batches(),
        renderer.frame_stats(),
    );

    // Container background + two leaves = three visible nodes, batched into one
    // quad draw of three instances (the automatic-batching contract).
    assert_eq!(snap.tree.len(), 3, "root + two leaves");
    assert_eq!(snap.batches.len(), 1, "the three quads batch into one draw");
    assert_eq!(snap.stats.draw_calls, 1);
    assert_eq!(snap.stats.instances, 3);
    // The batch/stats snapshots agree — read in the same window.
    assert_eq!(snap.batches.draw_calls(), snap.stats.draw_calls);
    assert_eq!(snap.batches.instances(), snap.stats.instances);
}

#[test]
fn snapshot_json_round_trips_a_live_frame() {
    let (store, root, renderer, _gpu) = settled_scene();
    let json = snapshot_ui(
        &store,
        root,
        renderer.inspect_batches(),
        renderer.frame_stats(),
    )
    .to_json();

    // A valid, non-empty document carrying every top-level surface and the
    // section 61 counter vocabulary. (Byte-exact schema is asserted in the
    // viso-ui unit test; here we prove the facade path produces the live wire
    // form.)
    assert!(json.starts_with('{') && json.ends_with('}'));
    for key in [
        r#""tree":"#,
        r#""paint":"#,
        r#""semantics":"#,
        r#""batches":"#,
        r#""counters":"#,
        r#""draw_calls":1"#,
        r#""instances":3"#,
        r#""node_count":3"#,
        r#""pipeline":"quad""#,
    ] {
        assert!(json.contains(key), "JSON missing {key}: {json}");
    }
}
