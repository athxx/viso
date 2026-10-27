//! SVG bytes: parsing never panics, and a document that parses rasterizes
//! without panicking.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(tree) = viso_svg::parse(data) {
        let pixels = viso_image::vector::rasterize(&tree, 32, 32);
        assert_eq!(pixels.len(), 32 * 32);
    }
});
