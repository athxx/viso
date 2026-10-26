//! Image objects: straight sRGB pixels as an RGB image and, where any pixel
//! is not opaque, its alpha as a soft mask.

use std::io::Write;

use crate::object::{Ref, Writer};

/// Writes `pixels` (straight sRGB RGBA8, `width × height`, rows top first)
/// as image object `at`.
pub(crate) fn write_rgba(w: &mut Writer, at: Ref, width: u32, height: u32, pixels: &[u8]) {
    let count = (width * height) as usize;
    let mut rgb = Vec::with_capacity(count * 3);
    let mut alpha = Vec::with_capacity(count);
    for px in pixels.as_chunks::<4>().0.iter().take(count) {
        rgb.extend_from_slice(&px[..3]);
        alpha.push(px[3]);
    }
    let mask = alpha.iter().any(|&a| a != 255).then(|| {
        let mask = w.alloc();
        w.stream(
            mask,
            |out| {
                let _ = write!(
                    out,
                    "/Type/XObject/Subtype/Image/Width {width}/Height {height}/ColorSpace/DeviceGray/BitsPerComponent 8/Interpolate true"
                );
            },
            &alpha,
        );
        mask
    });
    w.stream(
        at,
        |out| {
            let _ = write!(
                out,
                "/Type/XObject/Subtype/Image/Width {width}/Height {height}/ColorSpace/DeviceRGB/BitsPerComponent 8/Interpolate true"
            );
            if let Some(mask) = mask {
                out.extend_from_slice(b"/SMask ");
                mask.put(out);
            }
        },
        &rgb,
    );
}
