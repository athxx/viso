//! `GpuPod` is only meaningful for structs; deriving on an enum is an
//! error.

use viso_gpu::GpuPod;

#[repr(C)]
#[derive(Clone, Copy, GpuPod)]
enum NotAStruct {
    A,
    B,
}

fn main() {}
