//! Raster image bytes (PNG, JPEG, GIF, WebP, AVIF, BMP, QOI): decoding never
//! panics and never exceeds the pixel cap, and an animated image steps through
//! its frames without panicking.

#![no_main]

use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use viso_image::{Animation, MAX_PIXELS, decode_raster};
use viso_schema::resource::AssetBytes;

fuzz_target!(|data: &[u8]| {
    if let Ok(image) = decode_raster(data) {
        let [w, h] = image.size();
        assert!(f64::from(w) * f64::from(h) <= MAX_PIXELS as f64);
    }
    let bytes = AssetBytes::Shared(Arc::from(data));
    if let Ok(Some(mut animation)) = Animation::open(&bytes) {
        for _ in 0..8 {
            match animation.next_frame() {
                Ok(Some(_)) => {}
                _ => break,
            }
        }
    }
});
