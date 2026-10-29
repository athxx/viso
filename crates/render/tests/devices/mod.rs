//! The GPU devices a test can draw on here, each read back the same way: the
//! Metal device on Apple, Vulkan where it is native (Linux, Android) or built
//! (`vulkan`: MoltenVK on macOS, the Vulkan runtime on Windows), GLES 3 on
//! Linux with `gles`, and Direct3D 12 on Windows. A device the machine lacks
//! is skipped.

#![allow(dead_code)]

use viso_gpu::{GpuBackend, TextureId};

/// A picture drawn on a device into a texture of `TextureFormat::Bgra8Unorm`.
pub trait Draw {
    fn draw<B: Device>(&self, gpu: &mut B) -> Vec<u8>;
}

/// A device whose textures read back as BGRA8 rows.
pub trait Device: GpuBackend {
    fn read(&mut self, texture: TextureId) -> Vec<u8>;
}

impl Device for viso_gpu::CpuRaster {
    fn read(&mut self, texture: TextureId) -> Vec<u8> {
        self.read_texture(texture)
    }
}

#[cfg(target_vendor = "apple")]
impl Device for viso_gpu::MetalBackend {
    fn read(&mut self, texture: TextureId) -> Vec<u8> {
        self.read_texture(texture)
    }
}

#[cfg(any(feature = "vulkan", target_os = "linux", target_os = "android"))]
impl Device for viso_gpu::VulkanBackend {
    fn read(&mut self, texture: TextureId) -> Vec<u8> {
        self.read_texture(texture)
    }
}

#[cfg(all(target_os = "linux", feature = "gles"))]
impl Device for viso_gpu::Gles3Backend {
    fn read(&mut self, texture: TextureId) -> Vec<u8> {
        self.read_texture(texture)
    }
}

#[cfg(target_os = "windows")]
impl Device for viso_gpu::D3D12Backend {
    fn read(&mut self, texture: TextureId) -> Vec<u8> {
        self.read_texture(texture)
    }
}

/// The picture `draw` draws on each device here, by the device's name.
#[cfg_attr(
    not(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "windows",
        feature = "vulkan"
    )),
    allow(unused_mut, unused_variables)
)]
pub fn on_devices(draw: &impl Draw) -> Vec<(&'static str, Vec<u8>)> {
    let mut drawn = Vec::new();
    #[cfg(target_vendor = "apple")]
    if let Some(mut gpu) = viso_gpu::MetalBackend::try_new() {
        drawn.push(("metal", draw.draw(&mut gpu)));
    }
    #[cfg(any(feature = "vulkan", target_os = "linux", target_os = "android"))]
    match viso_gpu::VulkanBackend::try_new_with(true) {
        Some(mut gpu) => {
            drawn.push(("vulkan", draw.draw(&mut gpu)));
            assert_eq!(
                gpu.validation_errors(),
                0,
                "the validation layer reported errors"
            );
        }
        None => eprintln!("no Vulkan device; skipped"),
    }
    #[cfg(all(target_os = "linux", feature = "gles"))]
    {
        // Two GLES devices on one display at once crash Mesa.
        static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        match viso_gpu::Gles3Backend::try_new() {
            Some(mut gpu) => drawn.push(("gles", draw.draw(&mut gpu))),
            None => eprintln!("no GLES driver; skipped"),
        }
    }
    #[cfg(target_os = "windows")]
    if let Some(mut gpu) = viso_gpu::D3D12Backend::try_new() {
        drawn.push(("d3d12", draw.draw(&mut gpu)));
    }
    drawn
}

/// How many pixels of `a` and `b` differ by more than `tolerance` in a
/// channel.
pub fn differing(a: &[u8], b: &[u8], tolerance: u8) -> usize {
    a.as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_chunks::<4>().0)
        .filter(|(a, b)| {
            a.iter()
                .zip(b.iter())
                .any(|(x, y)| x.abs_diff(*y) > tolerance)
        })
        .count()
}
