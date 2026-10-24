//! An IOSurface imported as a texture is sampled where it lies; adopting
//! another points the same texture at it, and the one before is let go once
//! no frame samples it.

#![cfg(target_vendor = "apple")]

use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_io_surface::{
    IOSurfaceLockOptions, IOSurfaceRef, kIOSurfaceBytesPerElement, kIOSurfaceHeight,
    kIOSurfacePixelFormat, kIOSurfaceWidth,
};
use viso_gpu::{CpuRaster, ExternalImage, GpuBackend, MetalBackend, TextureDesc, TextureFormat};

/// A `side` × `side` BGRA surface every texel of which is `bgra`.
fn surface(side: u32, bgra: [u8; 4]) -> CFRetained<IOSurfaceRef> {
    let number = |n: i32| CFNumber::new_i32(n);
    let (w, h, bytes, format) = (
        number(side as i32),
        number(side as i32),
        number(4),
        number(i32::from_be_bytes(*b"BGRA")),
    );
    // SAFETY: the keys are IOSurface's own statics.
    let keys: [&CFString; 4] = unsafe {
        [
            kIOSurfaceWidth,
            kIOSurfaceHeight,
            kIOSurfaceBytesPerElement,
            kIOSurfacePixelFormat,
        ]
    };
    let values: [&CFType; 4] = [&w, &h, &bytes, &format];
    let properties = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
    // SAFETY: the properties are CFStrings to CFNumbers, as IOSurfaceCreate
    // takes them; the surface is locked while its rows are written, each
    // `bytes_per_row` apart for its height.
    unsafe {
        let surface = IOSurfaceRef::new(properties.as_ref()).expect("an IOSurface");
        surface.lock(IOSurfaceLockOptions::empty(), std::ptr::null_mut());
        let base = surface.base_address().as_ptr().cast::<u8>();
        let stride = surface.bytes_per_row();
        for y in 0..side as usize {
            for x in 0..side as usize {
                std::ptr::copy_nonoverlapping(bgra.as_ptr(), base.add(y * stride + x * 4), 4);
            }
        }
        surface.unlock(IOSurfaceLockOptions::empty(), std::ptr::null_mut());
        surface
    }
}

fn image(surface: &IOSurfaceRef, side: u32) -> ExternalImage {
    ExternalImage::IoSurface {
        surface: std::ptr::from_ref(surface).cast_mut().cast(),
        width: side,
        height: side,
    }
}

#[test]
fn an_imported_surface_is_sampled_in_place_and_another_adopted_into_its_texture() {
    let mut gpu = MetalBackend::new();
    let (red, blue) = (surface(4, [0, 0, 255, 255]), surface(4, [255, 0, 0, 255]));
    let texture = gpu.import_external(&image(&red, 4)).expect("imported");
    assert_eq!(gpu.read_texture(texture), [0, 0, 255, 255].repeat(16));
    assert_eq!(red.use_count(), 1, "in use while sampled");

    assert!(gpu.adopt_external(texture, &image(&blue, 4)));
    assert_eq!(gpu.read_texture(texture), [255, 0, 0, 255].repeat(16));
    assert_eq!(blue.use_count(), 1);
    // No frame in flight samples the red one: the next adoption lets it go.
    assert!(gpu.adopt_external(texture, &image(&blue, 4)), "already");
    assert_eq!(red.use_count(), 0);

    // The red one again reuses the texture made over it.
    assert!(gpu.adopt_external(texture, &image(&red, 4)));
    assert_eq!(gpu.read_texture(texture), [0, 0, 255, 255].repeat(16));

    // Another size, or a texture of its own, is not adopted.
    let large = surface(8, [0, 255, 0, 255]);
    assert!(!gpu.adopt_external(texture, &image(&large, 8)));
    let own = gpu.create_texture(&TextureDesc {
        width: 4,
        height: 4,
        format: TextureFormat::Bgra8UnormSrgb,
        render_target: false,
        label: "own",
    });
    assert!(!gpu.adopt_external(own, &image(&blue, 4)));
    assert_eq!(large.use_count(), 0);
}

#[test]
fn the_cpu_raster_adopts_nothing() {
    let mut gpu = CpuRaster::new();
    let red = surface(4, [0, 0, 255, 255]);
    assert!(gpu.import_external(&image(&red, 4)).is_none());
}
