//! End-to-end headless slice test: build a real retained tree with the public
//! building blocks, run measure + layout, paint to primitives, and rasterize
//! through the headless backend — the same Component → Node → Flex → paint →
//! renderer pipeline the facade drives on Metal, but deterministic and
//! display-free (the headless backend is a first-class test surface).
//!
//! It asserts two things the live window cannot cheaply prove in CI: the tree
//! lowers to exactly the expected draw work (one batched quad draw, one
//! instance per visible node), and the colored leaves land where layout places
//! them (sampled pixels match their fills).

use viso::gpu::{GpuBackend, HeadlessRaster, RawWindowHandle};
use viso::render::Rect;
use viso::render::{Renderer, Rgba};
use viso::ui::{
    Align, Axis, BoxStyle, BuildCx, FlexStyle, Inset, LeafStyle, Length, NodeStore, Size,
    paint_tree,
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
const BLUE: Rgba = Rgba {
    r: 0.2,
    g: 0.4,
    b: 0.95,
    a: 1.0,
};

/// Build the demo tree: a padded, cross-centered Row with a dark background,
/// holding a fixed red box, a width-filling green box, and a fixed blue box.
/// Mirrors the facade's `Scene`, assembled from public API so the test proves
/// the same pipeline without reaching into facade internals.
fn build(store: &mut NodeStore) -> viso::ui::NodeId {
    let mut cx = BuildCx::new(store);
    cx.flex(
        FlexStyle {
            axis: Axis::Row,
            gap: 8.0,
            padding: Inset::all(12.0),
            align: Align::Center,
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
            cx.leaf(LeafStyle {
                size: Size::fixed(64.0, 48.0),
                style: BoxStyle::solid(BLUE).with_radius(6.0),
            });
        },
    );
    cx.root().expect("scene has a root")
}

/// Read the BGRA8 pixel at `(x, y)` (top-left origin) as an `(r, g, b)` triple.
fn pixel(buf: &[u8], x: u32, y: u32) -> (u8, u8, u8) {
    let i = ((y * W + x) * 4) as usize;
    (buf[i + 2], buf[i + 1], buf[i]) // BGRA -> RGB
}

/// A channel is "close" to an expected 0..=1 float fill (allowing raster/round).
fn near(actual: u8, expected: f32) -> bool {
    let e = (expected * 255.0).round() as i32;
    (actual as i32 - e).abs() <= 6
}

#[test]
fn demo_tree_lays_out_and_rasterizes() {
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
    // Container background + three leaves = four visible quads.
    assert_eq!(primitives.len(), 4, "one quad per visible node");

    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);
    renderer.upload(&mut gpu, &primitives);

    // All four quads share the built-in quad pipeline, so they batch into a
    // single draw of four instances (the automatic-batching contract).
    let stats = renderer.frame_stats();
    assert_eq!(stats.draw_calls, 1, "quads batch into one draw");
    assert_eq!(stats.instances, 4, "one instance per visible quad");

    renderer.submit(
        &mut gpu,
        surface,
        [0.0, 0.0, 0.0, 1.0],
        [W as f32, H as f32],
    );
    let buf = gpu.read_pixels_bgra8(surface);

    // The red leaf is 48x40 fixed at the padding origin (12, 12), cross-centered
    // in the 96px-tall content band: y = 12 + (96 - 40) / 2 = 40. Its center is
    // (12 + 24, 40 + 20) = (36, 60).
    let (r, g, b) = pixel(&buf, 36, 60);
    assert!(
        near(r, RED.r) && near(g, RED.g) && near(b, RED.b),
        "red leaf center: got {r},{g},{b}"
    );

    // The blue leaf is 64x48 fixed, anchored at the far right of the content
    // band: x spans [W-12-64, W-12] = [124, 188], center x = 156; cross-centered
    // y = 12 + (96 - 48)/2 = 36, center y = 60.
    let (r, g, b) = pixel(&buf, 156, 60);
    assert!(
        near(r, BLUE.r) && near(g, BLUE.g) && near(b, BLUE.b),
        "blue leaf center: got {r},{g},{b}"
    );

    // A point in the padded border (top-left corner inset) shows the dark
    // container background, proving the container quad painted behind the leaves.
    let (r, g, b) = pixel(&buf, 4, 4);
    assert!(
        near(r, DARK.r) && near(g, DARK.g) && near(b, DARK.b),
        "container bg: got {r},{g},{b}"
    );
}
