//! `bool` has no fixed-width GPU representation (§7.3), so a `bool` field must be
//! rejected at the field span like any other unsupported type.

use viso_gpu::GpuPod;

#[repr(C)]
#[derive(Clone, Copy, GpuPod)]
struct BoolField {
    pos: [f32; 2],
    enabled: bool,
}

fn main() {}
