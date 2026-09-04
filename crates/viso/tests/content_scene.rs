//! End-to-end headless golden for the paint-all-primitives slice: build a real
//! retained tree with a text leaf and a textured-image leaf, run measure +
//! layout, paint through the UI `paint_tree` path (which lowers `Content::Text`
//! and `Content::Image` to renderer primitives), rasterize on the headless
//! backend, and compare the pixels against a baseline. The baseline is a
//! `BLESS=1`-regenerated raw dump kept out of version control (`*.bgra*` is
//! gitignored, same as the render crate's `quad_scene` golden), so a fresh
//! checkout regenerates it on first `BLESS=1` run.
//!
//! This is the counterpart to `crates/render/tests/golden.rs`, but driven from
//! the *UI* side: the render golden hand-assembles a `test_scene`, whereas this
//! proves the `NodeStore` → measure/layout → `paint_tree` → renderer → raster
//! pipeline handles content-bearing nodes. The glyph run and texture come from
//! the shared `viso-render` test fixtures so the baseline is deterministic and
//! font/atlas-stable.
//!
//! Set `BLESS=1` to (re)generate the baseline. Comparison is per-channel with a
//! small tolerance so it survives rounding and, later, the Metal readback.

use std::path::PathBuf;

use viso::gpu::{GpuBackend, HeadlessRaster, RawWindowHandle, TextureDesc, TextureFormat};
use viso::render::{GlyphInstanceData, Rect, Renderer, Rgba, test_glyphs, test_texture};
use viso::ui::{
    Axis, BoxStyle, BuildCx, Content, FlexStyle, Inset, LeafStyle, Length, NodeId, NodeStore, Size,
    Vec2, paint_tree,
};

const W: u32 = 160;
const H: u32 = 96;
/// Per-channel tolerance (in 0..=255) for the golden comparison.
const TOL: u8 = 2;

const DARK: Rgba = Rgba {
    r: 0.1,
    g: 0.1,
    b: 0.12,
    a: 1.0,
};
const WHITE: Rgba = Rgba {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};

/// The intrinsic extent of the glyph run, so its `Fit` leaf sizes to it.
fn glyph_run_natural(glyphs: &[GlyphInstanceData]) -> Vec2 {
    let mut n = Vec2::ZERO;
    for g in glyphs {
        n.x = n.x.max(g.rect.x + g.rect.w);
        n.y = n.y.max(g.rect.y + g.rect.h);
    }
    n
}

/// Render the content scene through the headless backend and return BGRA8 bytes.
fn render_scene() -> Vec<u8> {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);

    // Upload the shared checkerboard image texture and glyph SDF atlas.
    let (tw, th, texels) = test_texture();
    let texture = gpu.create_texture(&TextureDesc {
        width: tw,
        height: th,
        format: TextureFormat::Bgra8Unorm,
        render_target: false,
        label: "content-checkerboard",
    });
    gpu.write_texture(texture, 0, 0, tw, th, &texels);

    let tg = test_glyphs([0.0, 0.0], 26.0);
    let atlas = gpu.create_texture(&TextureDesc {
        width: tg.atlas_size,
        height: tg.atlas_size,
        format: TextureFormat::R8Unorm,
        render_target: false,
        label: "content-glyph-atlas",
    });
    gpu.write_texture(atlas, 0, 0, tg.atlas_size, tg.atlas_size, &tg.atlas_pixels);

    // Build the tree and attach content payloads to the two leaves.
    let mut store = NodeStore::new();
    let (root, text_id, image_id) = build(&mut store);

    store.set_content_payload(
        text_id,
        Content::Text {
            glyphs: tg.glyphs.clone(),
            atlas,
            color: WHITE,
            natural: glyph_run_natural(&tg.glyphs),
        },
    );
    store.set_content_payload(
        image_id,
        Content::Image {
            texture,
            uv: Rect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
            },
            tint: WHITE,
            natural: Vec2 {
                x: tw as f32,
                y: th as f32,
            },
        },
    );

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

    renderer.upload(&mut gpu, &primitives);
    renderer.submit(
        &mut gpu,
        surface,
        [0.0, 0.0, 0.0, 1.0],
        [W as f32, H as f32],
    );
    gpu.read_pixels_bgra8(surface)
}

/// A padded Row on a dark background: a `Fit` text leaf, then a fixed-size image
/// leaf. Returns the root plus the two content leaves.
fn build(store: &mut NodeStore) -> (NodeId, NodeId, NodeId) {
    let mut text_id = None;
    let mut image_id = None;
    let mut cx = BuildCx::new(store);
    cx.flex(
        FlexStyle {
            axis: Axis::Row,
            gap: 12.0,
            padding: Inset::all(10.0),
            size: Size::fill(),
            style: BoxStyle::solid(DARK),
            ..Default::default()
        },
        |cx| {
            // Text leaf sizes to the shaped run (Fit both axes).
            text_id = Some(
                cx.leaf(LeafStyle {
                    size: Size {
                        width: Length::Fit,
                        height: Length::Fit,
                    },
                    style: BoxStyle::NONE,
                })
                .id(),
            );
            // Image leaf is a fixed 48x48 box the checkerboard fills.
            image_id = Some(
                cx.leaf(LeafStyle {
                    size: Size::fixed(48.0, 48.0),
                    style: BoxStyle::NONE,
                })
                .id(),
            );
        },
    );
    let root = cx.root().expect("scene has a root");
    (root, text_id.unwrap(), image_id.unwrap())
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/content_scene.bgra8")
}

#[test]
fn content_scene_matches_golden() {
    let actual = render_scene();
    let path = golden_path();

    if std::env::var("BLESS").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
        eprintln!("blessed golden: {}", path.display());
        return;
    }

    let expected = std::fs::read(&path).unwrap_or_else(|_| {
        panic!(
            "missing golden {}; run with BLESS=1 to generate it",
            path.display()
        )
    });
    assert_eq!(
        actual.len(),
        expected.len(),
        "golden size mismatch: {} vs {}",
        actual.len(),
        expected.len()
    );

    let mut worst = 0u8;
    let mut worst_at = 0usize;
    for (i, (&a, &e)) in actual.iter().zip(&expected).enumerate() {
        let diff = a.abs_diff(e);
        if diff > worst {
            worst = diff;
            worst_at = i;
        }
    }
    assert!(
        worst <= TOL,
        "golden mismatch: max per-channel diff {worst} at byte {worst_at} \
         (pixel {}, channel {}) exceeds tolerance {TOL}",
        worst_at / 4,
        worst_at % 4,
    );
}
