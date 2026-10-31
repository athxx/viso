//! An erasing layer: a `DstOut` layer's coverage cuts its parent's content,
//! and an isolated parent keeps the cut off what lies behind it.

use viso_gpu::{CpuRaster, GpuBackend, TextureDesc, TextureFormat};
use viso_render::{Blend, Border, LayerClip, Primitive, Quad, Rect, Renderer, Rgba};

const W: u32 = 64;
const H: u32 = 64;

fn quad(rect: Rect, color: Rgba) -> Primitive {
    Primitive::Quad(Quad {
        rect,
        color,
        radius: 0.0,
        border: Border::NONE,
    })
}

fn layer(clip: Rect, blur_sigma: f32) -> Primitive {
    Primitive::Layer(LayerClip {
        clip,
        opacity: 1.0,
        blur_sigma,
        backdrop_sigma: 0.0,
    })
}

/// The BGRA8 picture of `prims` over a blue ground.
fn draw(prims: &[Primitive]) -> Vec<u8> {
    let mut gpu = CpuRaster::new();
    let mut renderer = Renderer::new(&mut gpu, TextureFormat::Bgra8Unorm);
    renderer.set_surface_size([W as f32, H as f32], 1.0);
    let ground = quad(Rect::new(0.0, 0.0, W as f32, H as f32), BLUE);
    let mut all = vec![ground];
    all.extend_from_slice(prims);
    renderer.upload(&mut gpu, &all);
    let target = gpu.create_texture(&TextureDesc {
        width: W,
        height: H,
        format: TextureFormat::Bgra8Unorm,
        render_target: true,
        label: "erase",
    });
    renderer.render_to_texture(&mut gpu, target, [1.0; 4], [W as f32, H as f32]);
    gpu.read_texture(target)
}

fn pixel(picture: &[u8], x: u32, y: u32) -> [u8; 4] {
    let at = ((y * W + x) * 4) as usize;
    picture[at..at + 4].try_into().unwrap()
}

const RED: Rgba = Rgba {
    r: 1.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};
const BLUE: Rgba = Rgba {
    r: 0.0,
    g: 0.0,
    b: 1.0,
    a: 1.0,
};
const RED_PX: [u8; 4] = [0, 0, 255, 255];
const BLUE_PX: [u8; 4] = [255, 0, 0, 255];

/// Red over the whole box with its middle cut away by an erasing layer.
fn cut(blur: f32) -> Vec<Primitive> {
    let all = Rect::new(0.0, 0.0, W as f32, H as f32);
    vec![
        layer(all, 0.0),
        Primitive::Isolate,
        quad(all, RED),
        layer(all, blur),
        Primitive::Blend(Blend::DstOut),
        quad(Rect::new(16.0, 16.0, 32.0, 32.0), RED),
        Primitive::LayerEnd,
        Primitive::LayerEnd,
    ]
}

#[test]
fn an_erasing_layer_cuts_only_its_isolated_parent() {
    let picture = draw(&cut(0.0));
    assert_eq!(pixel(&picture, 4, 4), RED_PX, "outside the cut");
    assert_eq!(
        pixel(&picture, 32, 32),
        BLUE_PX,
        "the ground shows through the cut"
    );
}

#[test]
fn a_blurred_cut_fades_at_its_edge() {
    let picture = draw(&cut(4.0));
    assert_eq!(pixel(&picture, 32, 32), BLUE_PX, "deep inside the cut");
    assert_eq!(pixel(&picture, 2, 2), RED_PX, "far outside it");
    let [b, _, r, _] = pixel(&picture, 16, 32);
    assert!(r > 40 && b > 40, "half cut at the edge: {r} {b}");
}
