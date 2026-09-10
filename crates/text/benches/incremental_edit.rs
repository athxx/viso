//! Incremental-reflow regression gate for very large paragraphs (section 36,
//! `text`; TF-P6.3).
//!
//! The paragraph pipeline reflows *incrementally*: an edit dirties a byte range,
//! and [`Paragraph::layout`](viso_text::paragraph::Paragraph::layout) reflows
//! forward from the dirtied line only until a line signature restabilizes
//! (spec 12.19). The result is defined to equal a full recompute, but the cost
//! is meant to be **O(edited lines), not O(document)** — a single keystroke in a
//! ten-thousand-line editor buffer must cost the same as one in a ten-line note.
//!
//! Makepad has no such incremental path: its layouter keys a whole-paragraph LRU
//! on the full layout params, so any edit is a cache miss that re-lays the entire
//! paragraph. This gate is the Viso-owned divergence made measurable — it asserts
//! that the incremental edit reshapes only a bounded handful of runs regardless
//! of document size, and that the wall-clock per-edit cost does not scale with
//! the document.
//!
//! Two invariants are gated (once at startup, like `high_refresh.rs`):
//!
//! 1. **Bounded reshape, independent of document size**: the same single-token
//!    edit at the middle of paragraphs of 500, 2000, and 8000 words reshapes the
//!    same small constant number of runs — it does not grow with the document.
//! 2. **Incremental equals full**: the incrementally reflowed lines match a full
//!    recompute of the edited text in line count and every line's source range
//!    and width (spec 12.19). The bit-for-bit run/caret identity is proven by the
//!    in-crate test `very_large_paragraph_incremental_edit_equals_full_recompute`;
//!    this gate checks the structure a downstream crate can observe.
//!
//! Then a criterion measurement tracks the per-edit relayout time on a large
//! (8000-word) paragraph over time. Run release: criterion defaults to a release
//! profile; debug timing is not a perf result (section 36).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_text::paragraph::{LineLayout, Paragraph};
use viso_text::shaping::{Direction, ShapedRun, Shaper};
use viso_text::{BaseDirection, FontFaceId, TextOffset};

/// The DejaVu Sans subset carrying the Latin the benches lay out.
const DEJAVU: &[u8] = include_bytes!("../tests/fixtures/DejaVuSans-subset.ttf");

/// Shape one substring through the fixture face (the same path the paragraph
/// tests and `high_refresh.rs` drive).
fn shape_run(sub: &str, dir: Direction) -> ShapedRun {
    Shaper::new()
        .shape_run(FontFaceId(0), DEJAVU, 0, sub, dir)
        .expect("fixture parses")
}

/// A large paragraph of fixed-width tokens: `word0000 word0001 ...`. Every token
/// is the same length, so a same-width single-token edit preserves the wrapping
/// structure and the stable-stop restabilizes within a line or two.
fn large_text(words: usize) -> String {
    let mut s = String::with_capacity(words * 9);
    for w in 0..words {
        if w > 0 {
            s.push(' ');
        }
        s.push_str(&format!("word{w:04}"));
    }
    s
}

/// A wrap width that fits about two tokens per line, so the document has one line
/// per ~two words — enough lines that O(document) and O(edited lines) diverge
/// sharply.
fn two_token_width() -> f32 {
    shape_run("word0000 word0001 ", Direction::LeftToRight).width_ems + 0.01
}

/// Lay out a fresh paragraph, apply one same-width single-token edit at the
/// middle, relayout incrementally, and return (reshape runs the edit cost, final
/// lines). The reshape count is measured as the shape calls the relayout issued.
fn edit_and_measure_reshape(words: usize) -> (u64, Vec<LineLayout>) {
    let text = large_text(words);
    let width = two_token_width();
    let mut shape_fn = |s: &str, d: Direction| shape_run(s, d);

    let mut p = Paragraph::new(&text, BaseDirection::LeftToRight, 0);
    p.layout(width, &mut shape_fn);
    let after_initial = p.shape_call_count();

    // Edit one token in the middle, same width so wrapping is preserved.
    let target = format!("word{:04}", words / 2);
    let mid = text.find(&target).expect("mid token present");
    p.edit(
        (TextOffset(mid), TextOffset(mid + target.len())),
        "word9999",
    );
    p.layout(width, &mut shape_fn);
    let reshape = p.shape_call_count() - after_initial;
    (reshape, p.lines().to_vec())
}

/// A full recompute of the same edited text, for the incremental-equals-full
/// check. Returns the fully laid-out lines.
fn full_lines_for(words: usize) -> Vec<LineLayout> {
    let text = large_text(words);
    let width = two_token_width();
    let target = format!("word{:04}", words / 2);
    let mid = text.find(&target).expect("mid token present");
    let mut edited = text.clone();
    edited.replace_range(mid..mid + target.len(), "word9999");
    let mut shape_fn = |s: &str, d: Direction| shape_run(s, d);
    let mut p = Paragraph::new(&edited, BaseDirection::LeftToRight, 0);
    p.layout_full(width, &mut shape_fn)
}

/// GATE 1 — the same single-token edit reshapes a bounded small constant of runs
/// at 500, 2000, and 8000 words. If the incremental reflow scaled with the
/// document, this constant would grow with word count; it must not.
fn assert_reshape_is_bounded_independent_of_document_size() {
    let sizes = [500usize, 2000, 8000];
    let mut counts = Vec::new();
    for &words in &sizes {
        let (reshape, _lines) = edit_and_measure_reshape(words);
        counts.push(reshape);
    }
    // The reshape count must not grow with the document: the 8000-word edit
    // reshapes no more than the 500-word edit plus a tiny slack. A cascade
    // (O(document)) would make counts scale ~16x across these sizes.
    let smallest = *counts.iter().min().expect("three sizes");
    let largest = *counts.iter().max().expect("three sizes");
    assert!(
        largest <= smallest + 4,
        "incremental reshape scaled with document size: {sizes:?} reshaped \
         {counts:?} runs — a single-token edit must be O(edited lines), not O(document)"
    );
    // And it must be a genuinely small constant, not a large fraction of a big
    // document (8000 words is ~4000 lines; a bounded edit reshapes a handful).
    assert!(
        largest < 32,
        "incremental reshape {largest} runs is not a small constant \
         (sizes {sizes:?} reshaped {counts:?})"
    );
}

/// Whether two line sequences agree on the structure a downstream crate can
/// observe: line count, and each line's source range and width bits (the
/// `LineSignature` the stable-stop keys on). Bit-for-bit run/caret identity is
/// gated in-crate; this is the externally visible equivalence.
fn line_structure_matches(a: &[LineLayout], b: &[LineLayout]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.logical_range == y.logical_range && x.width.to_bits() == y.width.to_bits()
        })
}

/// GATE 2 — the incrementally reflowed lines match a full recompute of the edited
/// text in line count, source ranges, and widths on a large paragraph (spec
/// 12.19: the incremental result is defined to equal a full recompute).
fn assert_incremental_equals_full() {
    for &words in &[500usize, 2000, 8000] {
        let (_reshape, incremental) = edit_and_measure_reshape(words);
        let full = full_lines_for(words);
        assert!(
            line_structure_matches(&incremental, &full),
            "incremental reflow diverged from full recompute at {words} words \
             ({} vs {} lines) — spec 12.19 requires them identical",
            incremental.len(),
            full.len()
        );
    }
}

/// Criterion measurement of one incremental single-token edit + relayout on a
/// large (8000-word) paragraph — the per-keystroke reflow cost that must stay a
/// small constant as documents grow.
fn bench_incremental_edit(c: &mut Criterion) {
    const WORDS: usize = 8000;
    let text = large_text(WORDS);
    let width = two_token_width();
    let target_a = format!("word{:04}", WORDS / 2);
    let mid = text.find(&target_a).expect("mid token present");
    let mut shape_fn = |s: &str, d: Direction| shape_run(s, d);

    let mut p = Paragraph::new(&text, BaseDirection::LeftToRight, 0);
    p.layout(width, &mut shape_fn);

    // Alternate the token between two same-width values so each iteration is a
    // real edit + incremental relayout, not a cache hit. Both tokens are eight
    // digit-width glyphs so the edit preserves wrapping and the stable-stop
    // restabilizes within a line or two (an unequal-width toggle would shift
    // every downstream line and cascade a full reflow, which is not what this
    // per-keystroke gate measures).
    let mut toggle = false;
    c.bench_function("text_incremental_single_token_edit_8000_words", |b| {
        b.iter(|| {
            let replacement = if toggle { "word9999" } else { "word8888" };
            toggle = !toggle;
            p.edit(
                (TextOffset(mid), TextOffset(mid + 8)),
                black_box(replacement),
            );
            black_box(p.layout(black_box(width), &mut shape_fn));
        });
    });
}

/// Run the two gates once before the criterion measurement, so a violated
/// incremental-reflow invariant fails the bench binary immediately.
fn gates(c: &mut Criterion) {
    assert_reshape_is_bounded_independent_of_document_size();
    assert_incremental_equals_full();
    bench_incremental_edit(c);
}

criterion_group!(benches, gates);
criterion_main!(benches);
