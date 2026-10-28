//! Pictures from outside drawn into GLES textures: NV12 planes uploaded or
//! in a dma-buf, converted with their matrix into sRGB-encoded RGBA. Run
//! where Mesa or a driver opens a display (`EGL_PLATFORM=surfaceless`).

#![cfg(all(target_os = "linux", feature = "gles"))]

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::{Arc, Mutex};

use viso_gpu::GpuBackend;
use viso_gpu::gles::Gles3Backend;
use viso_handle::{ExternalHolds, ExternalImage, Nv12, YuvMatrix};

/// Four by two, rows eight bytes apart: a red block and a white one,
/// studio range in BT.601; chroma one sample a two-by-two block.
const LUMA: [u8; 16] = [
    81, 81, 235, 235, 0, 0, 0, 0, //
    81, 81, 235, 235, 0, 0, 0, 0,
];
const CHROMA: [u8; 8] = [90, 240, 128, 128, 0, 0, 0, 0];

/// A process draws with one backend: two opening the default display at
/// once trip Mesa.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn texel(px: &[u8], x: usize, y: usize) -> [u8; 4] {
    let i = (y * 4 + x) * 4;
    px[i..i + 4].try_into().unwrap()
}

/// The outer columns are the blocks' own; linear chroma blends their edge.
fn red_and_white(px: &[u8]) {
    for row in 0..2 {
        let red = texel(px, 0, row);
        assert!(
            red[0] >= 250 && red[1] <= 4 && red[2] <= 4 && red[3] == 255,
            "{red:?}"
        );
        let white = texel(px, 3, row);
        assert!(white.iter().all(|&c| c >= 250), "{white:?}");
    }
}

/// NV12 planes at hand convert into the picture's codes, a second picture
/// rewrites the same texture, and rows too short are refused.
#[test]
fn nv12_planes_at_hand_convert_into_an_srgb_texture() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut gpu) = Gles3Backend::try_new() else {
        eprintln!("no GLES driver; skipped");
        return;
    };
    let picture = Nv12 {
        width: 4,
        height: 2,
        y: &LUMA,
        y_stride: 8,
        uv: &CHROMA,
        uv_stride: 8,
        matrix: YuvMatrix::Bt601,
        full_range: false,
        left_chroma: false,
    };
    let id = gpu.write_nv12(None, &picture).expect("GLES converts NV12");
    red_and_white(&gpu.read_texture(id));
    let black = [0u8; 16];
    let grey = [128u8; 8];
    let next = Nv12 {
        y: &black,
        uv: &grey,
        full_range: true,
        ..picture
    };
    assert_eq!(gpu.write_nv12(Some(id), &next), Some(id));
    for t in gpu.read_texture(id).chunks(4) {
        assert_eq!(t, [0, 0, 0, 255]);
    }
    let short = Nv12 {
        y: &black[..11],
        ..picture
    };
    assert_eq!(gpu.write_nv12(Some(id), &short), None);
    gpu.destroy_texture(id);
}

/// A dma-buf of `bytes` from the system heap, or `None` without one.
fn dma_buf(bytes: &[u8]) -> Option<OwnedFd> {
    #[repr(C)]
    struct Allocation {
        len: u64,
        fd: u32,
        fd_flags: u32,
        heap_flags: u64,
    }
    // `DMA_HEAP_IOCTL_ALLOC`: `_IOWR('H', 0, struct dma_heap_allocation_data)`.
    const ALLOC: libc::c_ulong = 0xc018_4800;
    let heap = std::fs::File::open("/dev/dma_heap/system").ok()?;
    let mut alloc = Allocation {
        len: bytes.len() as u64,
        fd: 0,
        fd_flags: (libc::O_RDWR | libc::O_CLOEXEC) as u32,
        heap_flags: 0,
    };
    // SAFETY: the heap's allocation ioctl over a local of its layout; the
    // descriptor it returns is owned here, mapped for the length allocated,
    // written and unmapped.
    unsafe {
        if libc::ioctl(heap.as_raw_fd(), ALLOC, &mut alloc) != 0 {
            return None;
        }
        let fd = OwnedFd::from_raw_fd(alloc.fd as i32);
        let map = libc::mmap(
            std::ptr::null_mut(),
            bytes.len(),
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd.as_raw_fd(),
            0,
        );
        if map == libc::MAP_FAILED {
            return None;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), map.cast::<u8>(), bytes.len());
        libc::munmap(map, bytes.len());
        Some(fd)
    }
}

/// NV12 planes in a dma-buf are read where they lie, held while the GPU
/// reads them, and a second picture adopted into the same texture.
#[test]
fn nv12_planes_in_a_dma_buf_are_converted_where_they_lie() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut gpu) = Gles3Backend::try_new() else {
        eprintln!("no GLES driver; skipped");
        return;
    };
    // Rows 64 bytes apart, the chroma plane at 256.
    let mut bytes = vec![0u8; 4096];
    for row in 0..2 {
        bytes[row * 64..row * 64 + 4].copy_from_slice(&LUMA[..4]);
    }
    bytes[256..260].copy_from_slice(&CHROMA[..4]);
    let Some(fd) = dma_buf(&bytes) else {
        eprintln!("no dma-buf heap here; skipped");
        return;
    };
    let holds = ExternalHolds::new();
    let picture = |width: u32| ExternalImage::DmaBuf {
        fd: fd.as_raw_fd(),
        len: bytes.len() as u64,
        offsets: [0, 256],
        strides: [64, 64],
        holds: Arc::as_ptr(&holds),
        matrix: YuvMatrix::Bt601,
        full_range: false,
        left_chroma: false,
        width,
        height: 2,
    };
    let image = picture(4);
    let Some(id) = gpu.import_external(&image) else {
        eprintln!("no dma-buf import here; skipped");
        return;
    };
    red_and_white(&gpu.read_texture(id));
    assert!(gpu.adopt_external(id, &image), "the same texture again");
    assert!(!gpu.adopt_external(id, &picture(12)), "another size");
    assert!(
        gpu.import_external(&picture(80)).is_none(),
        "rows wider than their stride"
    );
    gpu.read_texture(id);
    assert!(!holds.is_held(), "let go once the GPU has read it");
    gpu.destroy_texture(id);
}
