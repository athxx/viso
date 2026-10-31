//! An inset shadow falls inside its box, outside the box moved by its offset
//! and shrunk by its spread: a sharp one is a line along the edge it falls
//! from, a soft one shades the rim and leaves the middle.

use viso_gpu::{CpuRaster, GpuBackend, TextureDesc, TextureFormat};
use viso_render::{
    AnalyticShadow, Border, Corners, Primitive, Quad, Rect, Renderer, Rgba, ShadowShape,
};

const W: u32 = 64;
const H: u32 = 48;
const GREY: Rgba = Rgba::new(0.5, 0.5, 0.5, 1.0);

/// The red channel down column `x` of a grey box at (8, 8, 48, 32) under
/// an inset shadow of black.
fn column(offset: [f32; 2], sigma: f32, spread: f32, x: u32) -> Vec<u8> {
    let rect = Rect::new(8.0, 8.0, 48.0, 32.0);
    let prims = [
        Primitive::Quad(Quad {
            rect,
            color: GREY,
            radius: 0.0,
            border: Border::NONE,
        }),
        Primitive::AnalyticShadow(AnalyticShadow {
            rect,
            color: Rgba::new(0.0, 0.0, 0.0, 1.0),
            radius: Corners::uniform(0.0),
            offset,
            sigma,
            spread,
            shape: ShadowShape::RoundedBox,
            inner: true,
        }),
    ];
    let mut gpu = CpuRaster::new();
    let mut renderer = Renderer::new(&mut gpu, TextureFormat::Bgra8Unorm);
    renderer.set_surface_size([W as f32, H as f32], 1.0);
    renderer.upload(&mut gpu, &prims);
    let target = gpu.create_texture(&TextureDesc {
        width: W,
        height: H,
        format: TextureFormat::Bgra8Unorm,
        render_target: true,
        label: "inset",
    });
    renderer.render_to_texture(&mut gpu, target, [1.0; 4], [W as f32, H as f32]);
    let px = gpu.read_texture(target);
    (0..H).map(|y| px[((y * W + x) * 4 + 2) as usize]).collect()
}

#[test]
fn a_sharp_inset_shadow_is_a_line_along_the_edge_it_falls_from() {
    let red = column([0.0, 2.0], 0.0, 0.0, 32);
    let grey = red[24];
    assert!(grey > 100, "the middle keeps the box: {red:?}");
    // The box's edge row is half covered, as its fill is; the hole's edge
    // ramps over a pixel.
    assert!(red[9] < 10, "two rows of shadow at the top: {red:?}");
    assert!(red[10] < grey, "its edge ramps: {red:?}");
    assert_eq!(red[11], grey, "then the box: {red:?}");
    assert_eq!(red[38], grey, "nothing at the bottom: {red:?}");
    assert_eq!(red[4], 255, "nothing outside the box: {red:?}");
}

#[test]
fn a_soft_inset_shadow_shades_the_rim_and_leaves_the_middle() {
    let red = column([0.0, 0.0], 3.0, 0.0, 32);
    let grey = red[24];
    // A pixel in from the edge, a third of the way to black, as CSS has it.
    assert!(
        u32::from(red[9]) * 4 < u32::from(grey) * 3,
        "dark at the rim: {red:?}"
    );
    assert!(
        red[12] < red[18] && red[18] <= grey,
        "lighter inward: {red:?}"
    );
    assert!(grey > 100, "the middle all but untouched: {red:?}");
    assert_eq!(red[4], 255, "nothing outside the box: {red:?}");
    // A spread shrinks the hole: the shade reaches further in.
    let spread = column([0.0, 0.0], 3.0, 4.0, 32);
    assert!(spread[12] < red[12], "{spread:?} vs {red:?}");
}
