//! Font bytes: probing them never panics, and the coverage and color rasters
//! of a face that parses never panic for its first glyphs.

#![no_main]

use libfuzzer_sys::fuzz_target;
use viso_text::font_format;
use viso_text::raster_a8::rasterize_coverage;
use viso_text::raster_color::rasterize_color;

fuzz_target!(|data: &[u8]| {
    let _ = font_format::detect(data);
    if !font_format::parses(data) {
        return;
    }
    for glyph in 0..8 {
        let _ = rasterize_coverage(data, 0, glyph, 24.0);
        let _ = rasterize_color(data, 0, glyph, 24);
    }
});
