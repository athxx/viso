//! The box looks drawn on each GPU device here as on the CPU raster: a
//! gradient masked to rounded corners, a destination-out cut inside an
//! isolated layer (the inset text shadow's), and a liquid glass surface that
//! bends, splits, lights and evens its backdrop.

mod devices;

use devices::{Device, Draw, differing, on_devices};
use viso_gpu::{CpuRaster, TextureDesc, TextureFormat};
use viso_render::{
    Blend, Border, Corners, ExtendMode, FrostedMaterial, GlassUnion, Glint, Glow, Gradient,
    GradientKind, GradientStop, InterpolationSpace, LayerClip, Lens, Luminosity, Point, Primitive,
    Quad, Rect, Renderer, Rgba, RoundMask,
};

const W: u32 = 128;
const H: u32 = 96;

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

/// Stripes to bend, a rounded gradient, a cut and a glass over them.
fn scene() -> Vec<Primitive> {
    let all = Rect::new(0.0, 0.0, W as f32, H as f32);
    let mut out = vec![quad(all, Rgba::new(0.05, 0.05, 0.1, 1.0))];
    for x in (0..W).step_by(16) {
        out.push(quad(
            Rect::new(x as f32, 0.0, 8.0, H as f32),
            Rgba::new(0.9, 0.8, 0.2, 1.0),
        ));
    }
    let card = Rect::new(4.0, 4.0, 56.0, 40.0);
    out.push(Primitive::Gradient(Gradient {
        rect: card,
        kind: GradientKind::Linear,
        extend: ExtendMode::Clamp,
        p0: Point::new(4.0, 4.0),
        p1: Point::new(60.0, 44.0),
        stops: vec![
            GradientStop {
                offset: 0.0,
                color: Rgba::new(1.0, 0.2, 0.2, 1.0),
            },
            GradientStop {
                offset: 1.0,
                color: Rgba::new(0.2, 0.2, 1.0, 1.0),
            },
        ],
        interp: InterpolationSpace::LinearRgb,
        mask: Some(RoundMask {
            rect: card,
            radius: Corners::uniform(6.0),
        }),
    }));
    let cut = Rect::new(68.0, 4.0, 56.0, 40.0);
    out.extend([
        layer(cut, 0.0),
        Primitive::Isolate,
        quad(cut, Rgba::new(0.1, 0.8, 0.3, 1.0)),
        layer(cut, 3.0),
        Primitive::Blend(Blend::DstOut),
        quad(
            Rect::new(80.0, 14.0, 32.0, 20.0),
            Rgba::new(1.0, 1.0, 1.0, 1.0),
        ),
        Primitive::LayerEnd,
        Primitive::LayerEnd,
    ]);
    let mut glass = FrostedMaterial::new(
        Rect::new(16.0, 50.0, 96.0, 40.0),
        Corners::uniform(10.0),
        2.0,
    );
    glass.lens = Lens {
        refraction: 8.0,
        bevel: 12.0,
        dispersion: 0.3,
    };
    glass.glint = Glint {
        strength: 0.8,
        light: 7.0 * core::f32::consts::FRAC_PI_4,
        width: 1.5,
    };
    glass.adaptive_tint = Some(Rgba::new(0.4, 0.6, 1.0, 0.4));
    glass.luminosity = Luminosity {
        amount: 0.3,
        luminance: 0.6,
    };
    out.push(Primitive::Frosted(Box::new(glass)));
    // Two pills melting, a press glowing across both.
    let union = GlassUnion::of(
        &[
            (Rect::new(70.0, 6.0, 24.0, 16.0), 4.0),
            (Rect::new(98.0, 6.0, 24.0, 16.0), 4.0),
        ],
        10.0,
    )
    .unwrap();
    let mut pills = FrostedMaterial::new(union.bounds(), Corners::uniform(0.0), 2.0);
    pills.merged = Some(union);
    pills.lens = Lens {
        refraction: 4.0,
        bevel: 6.0,
        dispersion: 0.0,
    };
    pills.glow = Some(Glow {
        at: Point::new(90.0, 14.0),
        radius: 30.0,
        strength: 0.5,
    });
    out.push(Primitive::Frosted(Box::new(pills)));
    out
}

struct Looks;

impl Draw for Looks {
    fn draw<B: Device>(&self, gpu: &mut B) -> Vec<u8> {
        let mut renderer = Renderer::new(gpu, TextureFormat::Bgra8Unorm);
        renderer.set_surface_size([W as f32, H as f32], 1.0);
        renderer.upload(gpu, &scene());
        let target = gpu.create_texture(&TextureDesc {
            width: W,
            height: H,
            format: TextureFormat::Bgra8Unorm,
            render_target: true,
            label: "looks",
        });
        renderer.render_to_texture(gpu, target, [0.0, 0.0, 0.0, 1.0], [W as f32, H as f32]);
        gpu.read(target)
    }
}

#[test]
fn the_box_looks_draw_alike_on_every_device_here() {
    let cpu = Looks.draw(&mut CpuRaster::new());
    for (device, px) in on_devices(&Looks) {
        let off = differing(&cpu, &px, 4);
        assert!(
            off * 200 < (W * H) as usize,
            "{off} pixels differ between the CPU raster and {device}"
        );
    }
}
