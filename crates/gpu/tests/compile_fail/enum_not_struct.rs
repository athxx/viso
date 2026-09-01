//! `GpuInstance` is only meaningful for structs; deriving on an enum is an
//! error.

use viso_gpu::GpuInstance;

#[repr(C)]
#[derive(Clone, Copy, GpuInstance)]
enum NotAStruct {
    A,
    B,
}

fn main() {}
