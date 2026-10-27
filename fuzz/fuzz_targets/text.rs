//! Text segmentation: grapheme, word and line boundaries and bidi levels of
//! any text never panic, and every boundary lies on a character boundary.

#![no_main]

use libfuzzer_sys::fuzz_target;
use viso_text::segment::Segmenter;
use viso_text::{BaseDirection, BidiInfo, LineBreaker};

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let segmenter = Segmenter::new(&text);
    for offset in segmenter
        .grapheme_boundaries()
        .chain(segmenter.word_boundaries())
    {
        assert!(text.is_char_boundary(offset.0));
    }
    for (offset, _) in LineBreaker::new().break_opportunities(&text) {
        assert!(text.is_char_boundary(offset.0));
    }
    for base in [BaseDirection::Auto, BaseDirection::AutoRightToLeft] {
        let bidi = BidiInfo::resolve(&text, base);
        let _ = bidi.direction_runs();
        for paragraph in bidi.paragraphs() {
            let _ = bidi.visual_order(paragraph.start, paragraph.end);
        }
    }
});
