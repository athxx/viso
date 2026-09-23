//! Real-Metal verification of the "no runtime shader compilation" contract (§7.1).
//!
//! The standard pipelines are a fixed, known set. They must be compiled once, at
//! device init, from the frozen manifest MSL — never lazily on the first draw
//! that needs them (the old `newLibraryWithSource` on the first Button paint).
//!
//! `MetalBackend` counts every `newLibraryWithSource` in `library_compiles()`.
//! This test builds a `Renderer` on the system's default Metal device, which
//! prewarms every standard pipeline, then drives a Button-style paint frame and
//! asserts the compile count did not move: the draw path triggered zero shader
//! compilation.
//!
//! macOS-only: it needs a real `MTLDevice`. On CI without a GPU this is skipped.

#![cfg(target_vendor = "apple")]

use viso_gpu::{MetalBackend, TextureFormat};
use viso_render::{Border, Primitive, Quad, Rect, Renderer, Rgba};

#[test]
fn standard_pipelines_compile_at_device_init_not_on_a_draw() {
    let mut gpu = MetalBackend::new();

    // A fresh backend has compiled nothing.
    assert_eq!(
        gpu.library_compiles(),
        0,
        "a new backend has not compiled any MSL yet"
    );

    // Building the renderer is the device-init prewarm: it creates every standard
    // pipeline from the manifest, so every standard shader compiles exactly once
    // here. The count after construction is the fixed set's size.
    let mut renderer = Renderer::new(&mut gpu, TextureFormat::Bgra8Unorm);
    let after_prewarm = gpu.library_compiles();
    assert!(
        after_prewarm > 0,
        "constructing the renderer prewarms the standard pipelines"
    );

    // A Button-style scene: one rounded, bordered filled rect. Uploading it stages
    // instances and writes the persistent buffers — the per-frame draw path.
    let scene = [Primitive::Quad(Quad {
        rect: Rect {
            x: 8.0,
            y: 8.0,
            w: 120.0,
            h: 40.0,
        },
        color: Rgba {
            r: 0.2,
            g: 0.5,
            b: 0.9,
            a: 1.0,
        },
        radius: 6.0,
        border: Border {
            width: 1.0,
            color: Rgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        },
    })];

    // Drive the frame path several times: a steady stream of paints must never
    // reach a shader compiler.
    for _ in 0..3 {
        renderer.upload(&mut gpu, &scene);
    }

    assert_eq!(
        gpu.library_compiles(),
        after_prewarm,
        "painting must not compile any shader — the standard set is prewarmed at \
         device init (§7.1), and no draw may trigger `newLibraryWithSource`"
    );
}
