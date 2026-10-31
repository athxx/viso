//! Glass follows its box as the window resizes: a renderer that drew the
//! frame at one size and then another draws the last as a fresh one does.

mod devices;

use devices::{Device, Draw, differing, on_devices};
use viso_gpu::{CpuRaster, TextureDesc, TextureFormat};
use viso_render::{Corners, FrostedMaterial, Primitive, Quad, Rect, Renderer, Rgba};

/// Stripes to blur, and a glass over a third of the window.
fn scene(w: f32, h: f32) -> Vec<Primitive> {
    let mut out: Vec<Primitive> = (0..(w / 8.0) as u32)
        .map(|i| {
            Primitive::Quad(Quad {
                rect: Rect::new(i as f32 * 8.0, 0.0, 4.0, h),
                color: Rgba::new(0.9, 0.3 + 0.02 * (i % 8) as f32, 0.2, 1.0),
                radius: 0.0,
                border: viso_render::Border::NONE,
            })
        })
        .collect();
    let mut glass = FrostedMaterial::new(
        Rect::new(w / 3.0, h / 4.0, w / 3.0, h / 2.0),
        Corners::uniform(8.0),
        4.0,
    );
    glass.noise = 0.05;
    out.push(Primitive::Frosted(Box::new(glass)));
    out
}

struct Resized(&'static [(u32, u32)]);

impl Draw for Resized {
    fn draw<B: Device>(&self, gpu: &mut B) -> Vec<u8> {
        let mut renderer = Renderer::new(gpu, TextureFormat::Bgra8Unorm);
        let mut last = Vec::new();
        for &(w, h) in self.0 {
            renderer.set_surface_size([w as f32, h as f32], 1.0);
            renderer.upload(gpu, &scene(w as f32, h as f32));
            let target = gpu.create_texture(&TextureDesc {
                width: w,
                height: h,
                format: TextureFormat::Bgra8Unorm,
                render_target: true,
                label: "resized",
            });
            renderer.render_to_texture(gpu, target, [0.0, 0.0, 0.0, 1.0], [w as f32, h as f32]);
            last = gpu.read(target);
        }
        last
    }
}

const SIZES: &[(u32, u32)] = &[(160, 120), (200, 140), (120, 100), (176, 132)];

#[test]
fn glass_follows_each_resize_on_every_device_here() {
    let last = *SIZES.last().unwrap();
    let fresh_cpu = Resized(&[(176, 132)]).draw(&mut CpuRaster::new());
    let resized_cpu = Resized(SIZES).draw(&mut CpuRaster::new());
    assert_eq!(differing(&fresh_cpu, &resized_cpu, 0), 0, "the CPU raster");
    for (device, px) in on_devices(&Resized(SIZES)) {
        let off = differing(&fresh_cpu, &px, 4);
        assert!(
            off * 200 < (last.0 * last.1) as usize,
            "{off} pixels differ between a fresh frame and {device}'s resized one"
        );
    }
}
