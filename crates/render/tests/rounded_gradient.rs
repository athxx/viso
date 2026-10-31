//! A gradient masked to a rounded box: the fill a rounded box's gradient
//! background paints, its corners cut away and antialiased.

use viso_gpu::{CpuRaster, GpuBackend, TextureDesc, TextureFormat};
use viso_render::{
    Corners, ExtendMode, Gradient, GradientKind, GradientStop, InterpolationSpace, Point,
    Primitive, Rect, Renderer, Rgba, RoundMask,
};

const W: u32 = 64;
const H: u32 = 64;

fn stop(offset: f32, r: f32, b: f32) -> GradientStop {
    GradientStop {
        offset,
        color: Rgba {
            r,
            g: 0.0,
            b,
            a: 1.0,
        },
    }
}

/// The BGRA8 picture of `gradient` over white.
fn draw(gradient: Gradient) -> Vec<u8> {
    let mut gpu = CpuRaster::new();
    let mut renderer = Renderer::new(&mut gpu, TextureFormat::Bgra8Unorm);
    renderer.set_surface_size([W as f32, H as f32], 1.0);
    renderer.upload(&mut gpu, &[Primitive::Gradient(gradient)]);
    let target = gpu.create_texture(&TextureDesc {
        width: W,
        height: H,
        format: TextureFormat::Bgra8Unorm,
        render_target: true,
        label: "gradient",
    });
    renderer.render_to_texture(&mut gpu, target, [1.0, 1.0, 1.0, 1.0], [W as f32, H as f32]);
    gpu.read_texture(target)
}

fn pixel(picture: &[u8], x: u32, y: u32) -> [u8; 4] {
    let at = ((y * W + x) * 4) as usize;
    picture[at..at + 4].try_into().unwrap()
}

fn gradient(mask: Option<RoundMask>) -> Gradient {
    let rect = Rect::new(0.0, 0.0, W as f32, H as f32);
    Gradient {
        rect,
        kind: GradientKind::Linear,
        extend: ExtendMode::Clamp,
        p0: Point::new(0.0, 0.0),
        p1: Point::new(W as f32, 0.0),
        stops: vec![stop(0.0, 1.0, 0.0), stop(1.0, 0.0, 1.0)],
        interp: InterpolationSpace::LinearRgb,
        mask,
    }
}

#[test]
fn a_masked_gradient_cuts_its_corners_and_keeps_its_ramp() {
    let plain = draw(gradient(None));
    let rounded = draw(gradient(Some(RoundMask {
        rect: Rect::new(0.0, 0.0, W as f32, H as f32),
        radius: Corners::uniform(8.0),
    })));
    // A sharp box fills its corner; the rounded one leaves it white.
    assert_ne!(pixel(&plain, 1, 1), [255, 255, 255, 255]);
    assert_eq!(pixel(&rounded, 1, 1), [255, 255, 255, 255]);
    assert_eq!(pixel(&rounded, 62, 62), [255, 255, 255, 255]);
    // Inside the curve both draw the same ramp: red at the start, blue at
    // the end.
    for (x, y) in [(4, 32), (32, 32), (60, 32), (32, 2)] {
        assert_eq!(pixel(&rounded, x, y), pixel(&plain, x, y), "({x}, {y})");
    }
    let [b, _, r, _] = pixel(&rounded, 4, 32);
    assert!(r > 200 && b < 60, "{r} {b}");
    // The curve, the quad's (a 16 radius here), antialiases: a pixel on it
    // is part covered.
    let edge = pixel(&rounded, 5, 4);
    assert!(
        edge != [255, 255, 255, 255] && edge != pixel(&plain, 5, 4),
        "{edge:?}"
    );
}
