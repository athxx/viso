//! The CPU raster as a production backend (§30.4): a layer composited from
//! tiles draws the pixels a whole offscreen target would, in a fraction of
//! the memory; a panel without alpha still composites groups; a rounded clip
//! cuts its group's corners; a 2D accelerator's fills land where the
//! raster's would.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use viso_gpu::cpu::{Blitter, Fill, FillPixels};
use viso_gpu::{ColorSpace, CpuRaster, GpuBackend, RawWindowHandle, SurfaceId, TextureFormat};
use viso_render::{
    AnalyticEllipse, Border, ColorEffect, Corners, LayerClip, Primitive, Quad, Rect, Renderer, Rgba,
};

const W: u32 = 200;
const H: u32 = 150;
const CLEAR: [f32; 4] = [0.05, 0.05, 0.08, 1.0];

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

fn quad(r: Rect, color: Rgba, radius: f32) -> Primitive {
    Primitive::Quad(Quad {
        rect: r,
        color,
        radius,
        border: Border::NONE,
    })
}

fn layer(clip: Rect, opacity: f32, blur_sigma: f32) -> Primitive {
    Primitive::Layer(LayerClip {
        clip,
        opacity,
        blur_sigma,
        backdrop_sigma: 0.0,
    })
}

/// A background, two overlapping translucent groups (one color-filtered),
/// and a blurred group, whose layer is read by the blur and must be drawn
/// whole.
fn scene() -> Vec<Primitive> {
    let group = rect(20.0, 15.0, 150.0, 110.0);
    vec![
        quad(
            rect(0.0, 0.0, W as f32, 60.0),
            Rgba::new(0.9, 0.8, 0.2, 1.0),
            0.0,
        ),
        layer(group, 0.6, 0.0),
        quad(
            rect(30.5, 20.25, 90.0, 70.0),
            Rgba::new(0.2, 0.5, 0.9, 1.0),
            12.0,
        ),
        Primitive::AnalyticEllipse(AnalyticEllipse {
            rect: rect(70.0, 50.0, 90.0, 66.0),
            color: Rgba::new(0.9, 0.2, 0.3, 0.8),
            border: Border::NONE,
        }),
        Primitive::LayerEnd,
        layer(rect(100.0, 70.0, 90.0, 70.0), 0.8, 0.0),
        Primitive::ColorEffect(Box::new(ColorEffect::Saturation(0.3))),
        quad(
            rect(110.0, 80.0, 70.0, 50.0),
            Rgba::new(0.1, 0.9, 0.4, 1.0),
            6.0,
        ),
        quad(
            rect(130.0, 95.0, 40.0, 30.0),
            Rgba::new(0.9, 0.1, 0.9, 1.0),
            0.0,
        ),
        Primitive::LayerEnd,
        layer(rect(5.0, 90.0, 60.0, 50.0), 1.0, 3.0),
        quad(
            rect(15.0, 100.0, 40.0, 30.0),
            Rgba::new(1.0, 1.0, 1.0, 1.0),
            0.0,
        ),
        Primitive::LayerEnd,
    ]
}

struct Kit {
    gpu: CpuRaster,
    surface: SurfaceId,
    r: Renderer,
}

impl Kit {
    fn new(mut gpu: CpuRaster, format: TextureFormat) -> Kit {
        let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
        gpu.set_surface_color_target(surface, format, ColorSpace::Srgb);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.set_surface_size([W as f32, H as f32], 1.0);
        Kit { gpu, surface, r }
    }

    fn draw(&mut self, scene: &[Primitive]) -> Vec<u8> {
        self.r.upload(&mut self.gpu, scene);
        self.r
            .submit(&mut self.gpu, self.surface, CLEAR, [W as f32, H as f32]);
        self.gpu.read_pixels_bgra8(self.surface)
    }
}

fn raster(workers: usize, defer: bool) -> CpuRaster {
    let mut gpu = CpuRaster::with_workers(workers);
    gpu.set_layer_deferral(defer);
    gpu
}

#[test]
fn deferred_layers_draw_the_pixels_whole_targets_do() {
    let scene = scene();
    let mut whole = Kit::new(raster(0, false), TextureFormat::Bgra8Unorm);
    let expected = whole.draw(&scene);
    assert_eq!(whole.gpu.deferred_layers(), 0);
    for workers in [0, 3] {
        let mut tiles = Kit::new(raster(workers, true), TextureFormat::Bgra8Unorm);
        // Twice: the second frame reuses every target and scratch.
        for frame in 0..2 {
            assert!(
                tiles.draw(&scene) == expected,
                "workers {workers}, frame {frame}: deferred layers changed pixels"
            );
        }
        // The two composited groups defer, and so does the blur's last step;
        // the blurred group's content, which the blur reads, is drawn whole.
        assert_eq!(tiles.gpu.deferred_layers(), 3, "workers {workers}");
        assert!(
            tiles.gpu.pixel_bytes() < whole.gpu.pixel_bytes(),
            "a deferred layer holds no target: {} vs {} bytes",
            tiles.gpu.pixel_bytes(),
            whole.gpu.pixel_bytes()
        );
    }
}

#[test]
fn a_rounded_clip_cuts_its_groups_corners() {
    let clip = rect(40.0, 30.0, 100.0, 80.0);
    let scene = [
        layer(clip, 1.0, 0.0),
        Primitive::ClipCorners(Corners::uniform(10.0)),
        quad(
            rect(0.0, 0.0, W as f32, H as f32),
            Rgba::new(1.0, 1.0, 1.0, 1.0),
            0.0,
        ),
        Primitive::LayerEnd,
    ];
    for defer in [false, true] {
        let mut kit = Kit::new(raster(0, defer), TextureFormat::Bgra8Unorm);
        let pixels = kit.draw(&scene);
        let at = |x: u32, y: u32| {
            let i = ((y * W + x) * 4) as usize;
            [pixels[i], pixels[i + 1], pixels[i + 2]]
        };
        let clear = at(0, 0);
        assert_eq!(at(90, 70), [255; 3], "defer {defer}: inside");
        assert_eq!(at(41, 70), [255; 3], "defer {defer}: along an edge");
        assert_eq!(at(90, 31), [255; 3], "defer {defer}: along the top");
        for (x, y) in [(41, 31), (138, 31), (138, 108), (41, 108), (30, 70)] {
            assert_eq!(
                at(x, y),
                clear,
                "defer {defer}: past a corner at ({x}, {y})"
            );
        }
        // On the arc, coverage blends the group over what is under it.
        let arc = at(46, 36);
        assert!(arc[0] > clear[0] && arc[0] < 255, "defer {defer}: {arc:?}");
    }
}

#[test]
fn an_rgb565_panel_composites_groups_through_alpha_intermediates() {
    let mut panel = Kit::new(CpuRaster::new(), TextureFormat::Rgb565Unorm);
    assert_eq!(panel.r.intermediate_format(), TextureFormat::Bgra8UnormSrgb);
    let group = rect(20.0, 20.0, 60.0, 40.0);
    let green = Rgba::new(0.0, 1.0, 0.0, 1.0);
    let px = panel.draw(&[
        layer(group, 0.5, 0.0),
        quad(group, green, 0.0),
        quad(rect(30.0, 30.0, 20.0, 20.0), green, 0.0),
        Primitive::LayerEnd,
    ]);
    let at = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    };
    // Half green over the clear color in linear light, sRGB-encoded in 565
    // steps (green 0.525 is 191, red 0.025 is 44, blue 0.04 is 56); opaque
    // outside the group, the clear's 0.05 there 63.
    let [b, g, r, a] = at(60, 50);
    assert_eq!(a, 255);
    assert!((183..=199).contains(&g), "green {g}");
    assert!(r < 56 && b < 72, "red {r}, blue {b}");
    assert_eq!(at(5, 5)[3], 255);
    assert!((55..=72).contains(&at(5, 5)[1]), "clear {:?}", at(5, 5));
}

/// A blitter doing the fills itself, counting them.
struct Counting(Arc<AtomicUsize>);

impl Blitter for Counting {
    fn fill(&mut self, fill: Fill<'_>) -> bool {
        let [x0, y0, x1, y1] = fill.rect;
        let w = fill.width as usize;
        match fill.pixels {
            FillPixels::Bytes4 { pixels, value } => {
                for y in y0 as usize..y1 as usize {
                    pixels[y * w + x0 as usize..y * w + x1 as usize].fill(value);
                }
            }
            FillPixels::Rgb565 { pixels, value } => {
                for y in y0 as usize..y1 as usize {
                    pixels[y * w + x0 as usize..y * w + x1 as usize].fill(value);
                }
            }
        }
        self.0.fetch_add(1, Ordering::Relaxed);
        true
    }
}

#[test]
fn a_blitter_takes_the_clears_and_the_pixels_do_not_change() {
    let scene = scene();
    for format in [TextureFormat::Bgra8Unorm, TextureFormat::Rgb565Unorm] {
        let mut cpu = Kit::new(CpuRaster::new(), format);
        let fills = Arc::new(AtomicUsize::new(0));
        let mut gpu = CpuRaster::new();
        gpu.set_blitter(Some(Box::new(Counting(Arc::clone(&fills)))));
        let mut blit = Kit::new(gpu, format);
        for frame in 0..3 {
            let moved: Vec<Primitive> = scene
                .iter()
                .cloned()
                .chain([quad(
                    rect(frame as f32 * 10.0, 140.0, 8.0, 8.0),
                    Rgba::new(1.0, 0.0, 0.0, 1.0),
                    0.0,
                )])
                .collect();
            assert!(
                cpu.draw(&moved) == blit.draw(&moved),
                "{format:?} frame {frame}"
            );
        }
        assert!(
            fills.load(Ordering::Relaxed) > 0,
            "{format:?}: no clear reached the blitter"
        );
    }
}
