//! A field whose type is not a supported GPU scalar/vector (`f64` here) must be
//! rejected at the field span.

use viso_gpu::GpuPod;

#[repr(C)]
#[derive(Clone, Copy, GpuPod)]
struct BadField {
    pos: [f32; 2],
    weird: f64,
}

fn main() {}
