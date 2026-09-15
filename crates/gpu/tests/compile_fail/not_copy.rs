//! A `GpuPod` struct must be `Copy` (§7.3): it is uploaded as a plain byte
//! range, so it can carry no move-only state or `Drop` glue. Every field here is
//! a valid GPU scalar/vector, but the struct does not derive `Copy`, so the
//! derive's compile-time `Copy` assertion must reject it.

use viso_gpu::GpuPod;

#[repr(C)]
#[derive(GpuPod)]
struct NotCopy {
    pos: [f32; 2],
    color: [f32; 4],
}

fn main() {}
