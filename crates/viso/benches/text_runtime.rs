//! Regression gate for the wired text runtime (section 36, `text`).
//!
//! `viso-text`'s `high_refresh` and `incremental_edit` benches gate the
//! paragraph and scheduling primitives on their own; they stay as the floor.
//! This drives what an app's frames run — the shaper, its worker thread, the
//! per-frame commit budget, glyph residency over the headless raster backend —
//! through `__test_support::TextHarness`, and asserts on the counters the
//! runtime itself keeps:
//!
//! 1. **High refresh**: settled frames shape, raster, and upload nothing and
//!    draw the same instances every frame; an edit every frame at 60, 120, 144,
//!    and 240Hz shapes nothing on the main thread, and the main thread's frame
//!    cost (queue the edit, commit within the tier's budget) stays under the
//!    tier's budget with headroom. The timing gate runs in release only.
//! 2. **Incremental edit**: one same-width token edit in the middle of 500,
//!    2000, and 8000 words shapes a bounded handful of runs whatever the
//!    document size, and lands on the lines a fresh runtime lays out.
//! 3. **Section 26 matrix**, the rows this runtime drives headlessly: startup
//!    and first system resolve, Latin/CJK cold and warm resolution, mixed
//!    Latin/CJK/emoji and a ZWJ sequence, the face SLRU hot set, text input,
//!    a 10k-line code editor scroll, caret queries, steady A8 scroll, and page
//!    eviction without a full reset. Rows that need system fonts run on macOS
//!    and print a skip elsewhere.
//! 4. **Steady state**: a scrolling CJK document and a mixed-direction editing
//!    session hold constant draw, upload, and shaping counts across identical
//!    frames.
//!
//! The gates run once at startup so a regression fails the bench binary
//! immediately; criterion then measures a steady frame and an edit frame for
//! trend reporting. Run release: debug timing is not a perf result.

use std::hint::black_box;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use viso::__test_support::{TextDraw, TextHarness, TextStats, text_commit_budget};

/// The DejaVu Sans subset carrying the Latin the gates lay out.
const DEJAVU: &[u8] = include_bytes!("../fixtures/DejaVuSans-subset.ttf");
const SIZE: f32 = 16.0;

/// A refresh tier and the commit budget spec 14.4 derives for it.
struct Tier {
    hz: u32,
    budget_us: u64,
}

const TIERS: [Tier; 4] = [
    Tier {
        hz: 60,
        budget_us: 500,
    },
    Tier {
        hz: 120,
        budget_us: 416,
    },
    Tier {
        hz: 144,
        budget_us: 347,
    },
    Tier {
        hz: 240,
        budget_us: 208,
    },
];

const PARAGRAPHS: [&str; 4] = [
    "the quick brown fox jumps over the lazy dog again and again",
    "pack my box with five dozen liquor jugs",
    "how vexingly quick daft zebras jump",
    "sphinx of black quartz judge my vow",
];

fn frame_interval(hz: u32) -> Duration {
    Duration::from_secs(1) / hz
}

/// Draw every paragraph of `texts` this frame at `wrap`, commit within
/// `budget`, and read the frame's counters before closing it.
fn frame(
    text: &mut TextHarness,
    texts: &[&str],
    wrap: Option<f32>,
    budget: Duration,
) -> (Vec<TextDraw>, TextStats) {
    let draws = texts
        .iter()
        .enumerate()
        .map(|(at, body)| text.draw(at as u32, body, SIZE, wrap))
        .collect();
    text.commit(budget);
    let stats = text.stats();
    text.end_frame();
    (draws, stats)
}

/// Draw `texts`, let the worker finish, and draw again so every glyph is
/// resident: the frame a steady run starts from.
fn warm(text: &mut TextHarness, texts: &[&str], wrap: Option<f32>) {
    for _ in 0..2 {
        for (at, body) in texts.iter().enumerate() {
            text.draw(at as u32, body, SIZE, wrap);
        }
        text.settle();
        text.end_frame();
    }
}

/// Assert `frames` identical frames of `texts` draw what the first did and
/// shape, raster, and upload nothing.
fn assert_steady(
    label: &str,
    text: &mut TextHarness,
    texts: &[&str],
    wrap: Option<f32>,
    frames: u32,
) {
    let budget = text_commit_budget(frame_interval(60));
    let (first, _) = frame(text, texts, wrap, budget);
    assert!(
        first.iter().any(|draw| draw.glyphs + draw.color_glyphs > 0),
        "{label}: a warmed frame drew nothing"
    );
    for at in 0..frames {
        let (draws, stats) = frame(text, texts, wrap, budget);
        assert_eq!(draws, first, "{label}: steady frame {at} drew differently");
        assert_eq!(stats.reshapes, 0, "{label}: steady frame {at} laid out");
        assert_eq!(stats.shaped_runs, 0, "{label}: steady frame {at} shaped");
        assert_eq!(stats.rasters, 0, "{label}: steady frame {at} rasterized");
        assert_eq!(
            stats.atlas_upload_bytes, 0,
            "{label}: steady frame {at} uploaded"
        );
        assert!(!text.pending(), "{label}: steady frame {at} queued work");
    }
}

/// GATE 1a — settled frames do no text work and draw the same instances.
fn assert_steady_frames_do_no_text_work() {
    let mut text = TextHarness::with_font(DEJAVU);
    warm(&mut text, &PARAGRAPHS, Some(120.0));
    assert_steady(
        "static paragraphs",
        &mut text,
        &PARAGRAPHS,
        Some(120.0),
        240,
    );
}

/// GATE 1b — an edit every frame at each tier never shapes on the main
/// thread, and the main thread's frame stays under the tier's budget.
fn assert_edit_cadence_holds_each_tier() {
    for tier in &TIERS {
        let budget = text_commit_budget(frame_interval(tier.hz));
        assert_eq!(
            budget.as_micros() as u64,
            tier.budget_us,
            "{}Hz commit budget",
            tier.hz
        );
        let mut text = TextHarness::with_font(DEJAVU);
        let base = PARAGRAPHS[0];
        let edited = format!("{base}s");
        warm(&mut text, &[base, &edited], Some(120.0));

        let mut spent = Duration::ZERO;
        let mut commits = 0;
        for step in 0..tier.hz {
            let body = if step % 2 == 0 { edited.as_str() } else { base };
            let start = Instant::now();
            text.draw(0, body, SIZE, Some(120.0));
            text.commit(budget);
            spent += start.elapsed();
            let stats = text.stats();
            assert_eq!(
                stats.main_shaped_runs, 0,
                "{}Hz edit {step} shaped on the main thread",
                tier.hz
            );
            commits += stats.commits;
            text.end_frame();
        }
        text.settle();
        assert!(commits > 0, "{}Hz edits committed nothing", tier.hz);

        let per_frame_us = spent.as_secs_f64() * 1e6 / f64::from(tier.hz);
        println!(
            "text runtime: {}Hz budget {}us, edit frame {per_frame_us:.2}us",
            tier.hz, tier.budget_us
        );
        if cfg!(debug_assertions) {
            continue;
        }
        let ceiling = tier.budget_us as f64 * 2.0;
        assert!(
            per_frame_us < ceiling,
            "{}Hz edit frame {per_frame_us:.2}us exceeds the {ceiling:.0}us gate \
             (budget {}us)",
            tier.hz,
            tier.budget_us
        );
    }
}

/// `word0000 word0001 …`: fixed-width tokens, so a same-width token edit keeps
/// every line's width.
fn large_text(words: usize) -> String {
    let mut text = String::with_capacity(words * 9);
    for word in 0..words {
        if word > 0 {
            text.push(' ');
        }
        text.push_str(&format!("word{word:04}"));
    }
    text
}

/// About two tokens a line at [`SIZE`].
const TWO_TOKENS: f32 = 180.0;

/// The shape calls one middle-token edit of a `words`-word paragraph costs,
/// and whether its lines equal a fresh runtime's.
fn edit_in_the_middle(words: usize) -> (u64, bool) {
    let text = large_text(words);
    let mut runtime = TextHarness::with_font(DEJAVU);
    runtime.draw(0, &text, SIZE, Some(TWO_TOKENS));
    runtime.settle();
    let before = runtime.shape_calls(0);
    runtime.end_frame();

    let target = format!("word{:04}", words / 2);
    let mid = text.find(&target).expect("middle token");
    let mut edited = text.clone();
    edited.replace_range(mid..mid + target.len(), "word9999");
    runtime.draw(0, &edited, SIZE, Some(TWO_TOKENS));
    runtime.settle();
    let cost = runtime.shape_calls(0) - before;

    let mut fresh = TextHarness::with_font(DEJAVU);
    fresh.draw(0, &edited, SIZE, Some(TWO_TOKENS));
    fresh.settle();
    let equal = runtime.line_structure(0) == fresh.line_structure(0);
    (cost, equal)
}

/// GATE 2 — a middle-token edit shapes a bounded constant of runs at every
/// document size and lands on a full recompute's lines.
fn assert_incremental_edit_is_bounded() {
    let sizes = [500usize, 2000, 8000];
    let mut costs = Vec::new();
    for &words in &sizes {
        let (cost, equal) = edit_in_the_middle(words);
        assert!(
            equal,
            "the {words}-word edit diverged from a full recompute"
        );
        costs.push(cost);
    }
    let smallest = *costs.iter().min().expect("three sizes");
    let largest = *costs.iter().max().expect("three sizes");
    assert!(
        largest <= smallest + 4 && largest < 32,
        "a middle-token edit shaped {costs:?} runs at {sizes:?} words: it must \
         cost the edited lines, not the document"
    );
    println!("text runtime: middle-token edit shaped {costs:?} runs at {sizes:?} words");
}

/// Whether the system-font rows can run: they need the platform's font
/// provider.
fn system_fonts() -> bool {
    cfg!(target_os = "macos")
}

fn skip(row: &str) {
    println!("text runtime: {row} skipped: no system font provider on this target");
}

/// §26 startup and resolution: the first frame resolves the UI face with one
/// system query; warm frames never query again, CJK included.
fn assert_resolution_rows() {
    if !system_fonts() {
        skip("native_no_packaged_fonts_startup");
        skip("latin/cjk/mixed/emoji resolution");
        return;
    }
    let start = Instant::now();
    let mut text = TextHarness::system();
    let latin = ["system fonts only"];
    warm(&mut text, &latin, None);
    println!(
        "text runtime: native_system_ui_first_resolve {:.2}ms",
        start.elapsed().as_secs_f64() * 1e3
    );
    let cold = text.stats();
    assert_eq!(
        cold.system_queries, 1,
        "native_no_packaged_fonts_startup: a Latin first frame queries the system once"
    );
    assert_steady("latin_warm", &mut text, &latin, None, 30);
    assert_eq!(
        text.stats().system_queries,
        cold.system_queries,
        "latin_warm queried the system"
    );

    let cjk = cjk_text(10_000);
    let cjk_rows = [cjk.as_str()];
    let start = Instant::now();
    warm(&mut text, &cjk_rows, Some(480.0));
    println!(
        "text runtime: cjk_10k_cold {:.2}ms",
        start.elapsed().as_secs_f64() * 1e3
    );
    let cjk_cold = text.stats();
    assert!(
        cjk_cold.system_queries > cold.system_queries,
        "cjk_10k_cold: Han needs a fallback face"
    );
    assert_steady("cjk_10k_warm", &mut text, &cjk_rows, Some(480.0), 30);
    // The same text in a new paragraph resolves through the retained plans.
    text.draw(1, &cjk, SIZE, Some(480.0));
    text.settle();
    assert_eq!(
        text.stats().system_queries,
        cjk_cold.system_queries,
        "cjk_10k_warm: a second CJK paragraph queried the system"
    );
    text.retain(|at| at == 0);
    text.end_frame();

    let mut mixed = TextHarness::system();
    let rows = ["latin 中文 emoji 😀", "👩\u{200d}💻"];
    warm(&mut mixed, &rows, None);
    let draws: Vec<TextDraw> = rows
        .iter()
        .enumerate()
        .map(|(at, body)| mixed.draw(at as u32, body, SIZE, None))
        .collect();
    assert!(
        draws[0].glyphs > 0 && draws[0].color_glyphs == 1,
        "mixed_latin_cjk_emoji drew {:?}",
        draws[0]
    );
    assert_eq!(
        (draws[1].glyphs, draws[1].color_glyphs),
        (0, 1),
        "emoji_zwj: the sequence draws as one color glyph"
    );
    mixed.end_frame();
    let queries = mixed.stats().system_queries;
    assert_steady("mixed_latin_cjk_emoji", &mut mixed, &rows, None, 30);
    assert_eq!(mixed.stats().system_queries, queries);
}

/// `count` Han characters cycling over the first thousand of the block, in
/// sentences of forty.
fn cjk_text(count: usize) -> String {
    (0..count)
        .map(|at| {
            if at % 40 == 39 {
                '。'
            } else {
                char::from_u32(0x4e00 + (at % 1000) as u32).expect("Han")
            }
        })
        .collect()
}

/// §26 face cache: a steady hot set touches recency at most once per face per
/// frame and never misses.
fn assert_face_slru_hot_set() {
    let mut text = TextHarness::with_font(DEJAVU);
    warm(&mut text, &PARAGRAPHS, None);
    let start = text.stats();
    const FRAMES: u64 = 60;
    for _ in 0..FRAMES {
        frame(&mut text, &PARAGRAPHS, None, Duration::MAX);
    }
    let end = text.stats();
    assert_eq!(
        end.face_misses, start.face_misses,
        "face_slru_hot_set missed"
    );
    assert_eq!(end.face_evictions, start.face_evictions);
    assert!(
        end.face_recency_updates - start.face_recency_updates <= FRAMES * end.face_resident,
        "face_slru_hot_set: recency updates are not coalesced per frame \
         ({} over {FRAMES} frames, {} faces)",
        end.face_recency_updates - start.face_recency_updates,
        end.face_resident
    );
}

/// §26 text input: typing at the end of a paragraph costs each keystroke the
/// last line, not the paragraph; the caret never shapes.
fn assert_text_input_and_caret_rows() {
    let mut text = TextHarness::with_font(DEJAVU);
    let mut body = large_text(400);
    text.draw(0, &body, SIZE, Some(TWO_TOKENS));
    text.settle();
    text.end_frame();
    let mut worst = 0;
    for key in "typing more words".chars() {
        let before = text.shape_calls(0);
        body.push(key);
        text.draw(0, &body, SIZE, Some(TWO_TOKENS));
        text.settle();
        text.end_frame();
        worst = worst.max(text.shape_calls(0) - before);
    }
    assert!(
        worst < 16,
        "text_input_incremental: a keystroke shaped {worst} runs"
    );

    // Caret move and hit test read the drawn lines.
    text.draw(0, &body, SIZE, Some(TWO_TOKENS));
    let mut previous = [f32::MIN; 2];
    for offset in (0..body.len()).step_by(97) {
        let caret = text
            .caret(0, &body, SIZE, offset)
            .expect("the caret of the drawn text");
        assert!(
            caret[1] > previous[1] || (caret[1] == previous[1] && caret[0] > previous[0]),
            "the caret went backwards at {offset}"
        );
        previous = [caret[0], caret[1]];
    }
    let stats = text.stats();
    assert_eq!(
        (stats.reshapes, stats.shaped_runs),
        (0, 0),
        "caret queries shaped"
    );
    assert!(!text.pending(), "caret queries queued work");
    text.end_frame();
}

const LINES: usize = 10_000;
const VISIBLE: usize = 48;

fn code_line(at: usize) -> String {
    format!("let value{at:05} be the sum of line {at} and more")
}

/// §26 code editor scroll: a 10k-line document scrolled a line a frame, each
/// line a paragraph, mounted only while visible. Every line is laid out once
/// as it enters, the glyph set stays resident so nothing uploads after the
/// first screen, and the main thread never shapes.
fn assert_code_editor_scroll() {
    let mut text = TextHarness::with_font(DEJAVU);
    let lines: Vec<String> = (0..LINES).map(code_line).collect();
    let budget = text_commit_budget(frame_interval(120));
    let draw_window = |text: &mut TextHarness, top: usize| {
        for (at, line) in lines.iter().enumerate().skip(top).take(VISIBLE) {
            text.draw(at as u32, line, SIZE, None);
        }
        text.retain(|at| (top..top + VISIBLE).contains(&(at as usize)));
    };
    draw_window(&mut text, 0);
    text.settle();
    draw_window(&mut text, 0);
    text.settle();
    text.end_frame();

    let mut reshapes = 0;
    let mut uploaded = 0;
    let scrolled = LINES - VISIBLE;
    for top in 1..=scrolled {
        draw_window(&mut text, top);
        text.commit(budget);
        let stats = text.stats();
        assert_eq!(
            stats.main_shaped_runs, 0,
            "scroll frame {top} shaped on main"
        );
        reshapes += stats.reshapes;
        uploaded += stats.atlas_upload_bytes;
        text.end_frame();
    }
    text.settle();
    reshapes += text.stats().reshapes;
    text.end_frame();
    assert_eq!(
        reshapes, scrolled as u64,
        "code_editor_scroll_10k_lines: each entering line lays out exactly once"
    );
    assert_eq!(
        uploaded, 0,
        "atlas_a8_steady_scroll: scrolling resident glyphs uploaded"
    );
}

/// §26 atlas: turning pages over evicts cold glyphs without a full reset — the
/// hot pair stays resident and drawn every frame.
fn assert_page_eviction_without_reset() {
    let mut text = TextHarness::with_font_and_atlas(DEJAVU, 96, 32);
    text.draw(0, "bd", 30.0, None);
    text.settle();
    text.draw(0, "bd", 30.0, None);
    text.end_frame();
    let mut evictions = 0;
    for size in [27.0, 28.0, 29.0, 31.0, 32.0, 33.0, 34.0] {
        text.draw(0, "bd", 30.0, None);
        text.draw(1, "bd", size, None);
        text.settle();
        let hot = text.draw(0, "bd", 30.0, None);
        text.draw(1, "bd", size, None);
        assert_eq!(
            hot.glyphs, 2,
            "atlas_no_full_reset: the hot pair was dropped"
        );
        let stats = text.stats();
        assert!(
            stats.coverage_resident >= 2,
            "atlas_no_full_reset: the pool was reset"
        );
        evictions += stats.evictions;
        text.end_frame();
    }
    assert!(evictions > 0, "atlas_page_eviction: no page turned over");
}

/// Steady state — a CJK document scrolled back and forth: once every line
/// has been seen, frames at the same scroll position draw the same instances
/// and do no text work.
fn assert_cjk_scroll_steady() {
    if !system_fonts() {
        skip("steady CJK scroll");
        return;
    }
    let mut text = TextHarness::system();
    let lines: Vec<String> = (0..120)
        .map(|at| {
            let mut line = cjk_text(24 + at % 7);
            line.push_str(&format!("{at}"));
            line
        })
        .collect();
    let rows: Vec<&str> = lines.iter().map(String::as_str).collect();
    // Mount the document once so every line is laid out and resident.
    warm(&mut text, &rows, None);
    let window = 30;
    let tops: Vec<usize> = (0..rows.len() - window)
        .chain((0..rows.len() - window).rev())
        .collect();
    let budget = text_commit_budget(frame_interval(120));
    let mut first_pass = Vec::new();
    for pass in 0..3 {
        for (step, &top) in tops.iter().enumerate() {
            let draws: Vec<TextDraw> = (top..top + window)
                .map(|at| text.draw(at as u32, rows[at], SIZE, None))
                .collect();
            text.commit(budget);
            let stats = text.stats();
            assert_eq!(
                (
                    stats.reshapes,
                    stats.shaped_runs,
                    stats.rasters,
                    stats.atlas_upload_bytes
                ),
                (0, 0, 0, 0),
                "CJK scroll pass {pass} at {top} did text work"
            );
            text.end_frame();
            if pass == 0 {
                first_pass.push(draws);
            } else {
                assert_eq!(
                    draws, first_pass[step],
                    "CJK scroll pass {pass} at {top} drew differently"
                );
            }
        }
    }
}

/// Steady state — a mixed-direction editing session: typing into and out of
/// a Latin/Hebrew/Arabic paragraph shapes nothing on the main thread, and once
/// the edits stop, identical frames do no text work.
fn assert_mixed_direction_session_steady() {
    if !system_fonts() {
        skip("steady mixed-direction session");
        return;
    }
    let mut text = TextHarness::system();
    let base = "edit שלום עולם and مرحبا بالعالم here";
    warm(&mut text, &[base], Some(200.0));
    let budget = text_commit_budget(frame_interval(120));
    let mut body = base.to_string();
    for key in "typed שלום مرحبا"
        .chars()
        .chain(std::iter::repeat_n('\u{8}', 16))
    {
        if key == '\u{8}' {
            body.pop();
        } else {
            body.push(key);
        }
        text.draw(0, &body, SIZE, Some(200.0));
        text.commit(budget);
        assert_eq!(
            text.stats().main_shaped_runs,
            0,
            "the mixed-direction session shaped on main"
        );
        text.end_frame();
    }
    text.settle();
    text.end_frame();
    assert_steady(
        "mixed-direction session",
        &mut text,
        &[body.as_str()],
        Some(200.0),
        60,
    );
}

fn bench_steady_frame(c: &mut Criterion) {
    let mut text = TextHarness::with_font(DEJAVU);
    warm(&mut text, &PARAGRAPHS, Some(120.0));
    let budget = text_commit_budget(frame_interval(240));
    c.bench_function("text_runtime_steady_frame", |b| {
        b.iter(|| black_box(frame(&mut text, &PARAGRAPHS, Some(120.0), budget)));
    });
}

fn bench_edit_frame(c: &mut Criterion) {
    let mut text = TextHarness::with_font(DEJAVU);
    let base = PARAGRAPHS[0];
    let edited = format!("{base}s");
    warm(&mut text, &[base, &edited], Some(120.0));
    let budget = text_commit_budget(frame_interval(240));
    let mut toggle = false;
    c.bench_function("text_runtime_edit_frame", |b| {
        b.iter(|| {
            toggle = !toggle;
            let body = if toggle { edited.as_str() } else { base };
            black_box(text.draw(0, body, SIZE, Some(120.0)));
            black_box(text.commit(budget));
            text.end_frame();
        });
    });
    text.settle();
}

fn gates(c: &mut Criterion) {
    assert_steady_frames_do_no_text_work();
    assert_edit_cadence_holds_each_tier();
    assert_incremental_edit_is_bounded();
    assert_resolution_rows();
    assert_face_slru_hot_set();
    assert_text_input_and_caret_rows();
    assert_code_editor_scroll();
    assert_page_eviction_without_reset();
    assert_cjk_scroll_steady();
    assert_mixed_direction_session_steady();
    bench_steady_frame(c);
    bench_edit_frame(c);
}

criterion_group!(benches, gates);
criterion_main!(benches);
