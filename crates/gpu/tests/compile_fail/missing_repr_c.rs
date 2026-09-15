//! A `GpuPod` struct without `#[repr(C)]` must be rejected: the layout
//! would not be stable enough to describe to the GPU.

use viso_gpu::GpuPod;

#[derive(Clone, Copy, GpuPod)]
struct NoRepr {
    pos: [f32; 2],
}

fn main() {}
