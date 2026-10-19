//! The CPU raster as an embedded panel's renderer (§30.4): an 800×480 UI
//! drawn whole, and redrawn where 5% of it changed, on an XRGB8888 and an
//! RGB565 panel, on one thread and on four.
//!
//! Before timing, it prints what each configuration holds — the pixels of
//! the framebuffer and every target, and the workers' scratch — and the heap
//! allocations of a warmed damage frame.
//!
//! Run release (`cargo bench -p viso-render --bench cpu_raster`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use criterion::{Criterion, criterion_group, criterion_main};
use viso_gpu::{ColorSpace, CpuRaster, GpuBackend, RawWindowHandle, SurfaceId, TextureFormat};
use viso_render::{
    Border, ExtendMode, Gradient, GradientKind, GradientStop, InterpolationSpace, LayerClip, Point,
    Primitive, Quad, Rect, Renderer, Rgba,
};

struct CountingAlloc;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static ARMED: AtomicBool = AtomicBool::new(false);

// SAFETY: forwards every call to the system allocator unchanged; the only added
// behavior is a relaxed counter increment on allocation while armed.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

const W: u32 = 800;
const H: u32 = 480;
const CLEAR: [f32; 4] = [0.07, 0.08, 0.1, 1.0];

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

/// A dashboard: a header gradient, a grid of rounded cards with rows of
/// "text" bars, a translucent overlay panel, and a `highlight` tile of 5%
/// of the screen whose color is the frame's one change.
fn dashboard(highlight: f32) -> Vec<Primitive> {
    let mut scene = vec![Primitive::Gradient(Gradient {
        rect: rect(0.0, 0.0, W as f32, 56.0),
        kind: GradientKind::Linear,
        extend: ExtendMode::Clamp,
        p0: Point { x: 0.0, y: 0.0 },
        p1: Point {
            x: W as f32,
            y: 56.0,
        },
        stops: vec![
            GradientStop {
                offset: 0.0,
                color: Rgba::new(0.1, 0.3, 0.7, 1.0),
            },
            GradientStop {
                offset: 1.0,
                color: Rgba::new(0.5, 0.2, 0.6, 1.0),
            },
        ],
        interp: InterpolationSpace::LinearRgb,
    })];
    for row in 0..3 {
        for col in 0..4 {
            let x = 16.0 + col as f32 * 196.0;
            let y = 72.0 + row as f32 * 136.0;
            scene.push(quad(
                rect(x, y, 180.0, 120.0),
                Rgba::new(0.16, 0.17, 0.2, 1.0),
                10.0,
            ));
            for line in 0..5 {
                let w = 150.0 - (line * 17 % 60) as f32;
                scene.push(quad(
                    rect(x + 14.0, y + 14.0 + line as f32 * 20.0, w, 10.0),
                    Rgba::new(0.75, 0.77, 0.8, 1.0),
                    3.0,
                ));
            }
        }
    }
    // 196×98 is 5% of 800×480.
    scene.push(quad(
        rect(16.0, 360.0, 196.0, 98.0),
        Rgba::new(highlight, 0.6, 0.3, 1.0),
        12.0,
    ));
    scene.push(Primitive::Layer(LayerClip {
        clip: rect(480.0, 300.0, 300.0, 160.0),
        opacity: 0.85,
        blur_sigma: 0.0,
        backdrop_sigma: 0.0,
    }));
    scene.push(quad(
        rect(480.0, 300.0, 300.0, 160.0),
        Rgba::new(0.95, 0.95, 0.97, 1.0),
        14.0,
    ));
    scene.push(quad(
        rect(500.0, 320.0, 120.0, 120.0),
        Rgba::new(0.2, 0.6, 0.9, 1.0),
        60.0,
    ));
    scene.push(Primitive::LayerEnd);
    scene
}

struct Panel {
    gpu: CpuRaster,
    surface: SurfaceId,
    r: Renderer,
    frame: u32,
}

impl Panel {
    fn new(format: TextureFormat, workers: usize) -> Panel {
        let mut gpu = CpuRaster::with_workers(workers);
        let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
        gpu.set_surface_color_target(surface, format, ColorSpace::Srgb);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.set_surface_size([W as f32, H as f32], 1.0);
        Panel {
            gpu,
            surface,
            r,
            frame: 0,
        }
    }

    fn present(&mut self, scene: &[Primitive]) {
        self.r.upload(&mut self.gpu, scene);
        self.r
            .submit(&mut self.gpu, self.surface, CLEAR, [W as f32, H as f32]);
    }

    /// A frame where only the highlight tile changed.
    fn damage_frame(&mut self, scenes: &[Vec<Primitive>; 2]) {
        self.frame += 1;
        self.present(&scenes[(self.frame % 2) as usize]);
    }

    /// The whole frame, redrawn: a fresh surface every time.
    fn full_frame(&mut self, scene: &[Primitive]) {
        self.gpu.resize_surface(self.surface, W, H);
        self.present(scene);
    }
}

fn report(name: &str, format: TextureFormat, workers: usize, scenes: &[Vec<Primitive>; 2]) {
    let mut p = Panel::new(format, workers);
    for _ in 0..4 {
        p.damage_frame(scenes);
    }
    ALLOCS.store(0, Ordering::Relaxed);
    ARMED.store(true, Ordering::Relaxed);
    p.damage_frame(scenes);
    ARMED.store(false, Ordering::Relaxed);
    eprintln!(
        "{name}: pixel_bytes {} scratch_bytes {} deferred_layers {} damage-frame allocations {}",
        p.gpu.pixel_bytes(),
        p.gpu.scratch_bytes(),
        p.gpu.deferred_layers(),
        ALLOCS.load(Ordering::Relaxed),
    );
}

fn bench(c: &mut Criterion) {
    let scenes = [dashboard(0.9), dashboard(0.3)];
    let configs = [
        ("xrgb8888", TextureFormat::Bgra8Unorm, 0),
        ("xrgb8888_4t", TextureFormat::Bgra8Unorm, 3),
        ("rgb565", TextureFormat::Rgb565Unorm, 0),
    ];
    for (name, format, workers) in configs {
        report(name, format, workers, &scenes);
        let mut p = Panel::new(format, workers);
        c.bench_function(&format!("cpu_raster_800x480_full_{name}"), |b| {
            b.iter(|| p.full_frame(black_box(&scenes[0])));
        });
        let mut p = Panel::new(format, workers);
        p.present(&scenes[0]);
        c.bench_function(&format!("cpu_raster_800x480_damage_5pct_{name}"), |b| {
            b.iter(|| p.damage_frame(black_box(&scenes)));
        });
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
