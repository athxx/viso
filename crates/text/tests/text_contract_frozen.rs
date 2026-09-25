//! Frozen contract for the text runtime's public surface.
//!
//! Six contracts are pinned here, because the runtime, the renderer's glyph
//! lanes, the editing controls and the Inspector all build on them and must not
//! silently redefine them:
//!
//! 1. **Glyph image kinds and the representation state machine.** Exactly five
//!    kinds and no plain single-channel SDF. A run starts on exact coverage at
//!    its raster bucket, a steady run is never re-evaluated, a one-frame spike
//!    promotes nothing, a sustained zoom asks for a scalable representation that
//!    is switched to only at the next resolve, and the switch hands the previous
//!    representation back as a release. Thresholds are private and not pinned.
//! 2. **Glyph residency pools and eviction.** Four pools, the two vector kinds
//!    sharing one; a hit uploads nothing; a full pool reclaims one page of its
//!    own and reports it, never another pool's and never the whole atlas;
//!    pressure sheds one named pool; a promoted pool that would have to evict
//!    admits A8 coverage instead.
//! 3. **Typed positions.** `TextOffset` is a UTF-8 byte offset, `TextPosition`
//!    is an offset plus a `CaretAffinity`.
//! 4. **Paragraph pipeline entry.** `Paragraph::new(text, base, style_epoch)`,
//!    shaping injected through a `ShapeFn`, `layout` returning whether a
//!    (re)layout ran, and an incremental layout that equals `layout_full`.
//! 5. **Face and shaping cache budgets.** One byte budget per cache from a
//!    memory class, never a shared ceiling; the face cache is byte-accounted and
//!    never evicts a pin. The MiB figures are provisional and not pinned.
//! 6. **MTSDF buckets and quality windows.** The ladder, the field encoding
//!    constants baked into the sampling shader, and the plan that takes the next
//!    bucket or hands off to the outline rather than stretching a field.
//!
//! The in-crate unit tests cover the details; this integration test sees only
//! the public surface, which is precisely what "frozen" has to mean.

use std::time::Duration;

use viso_text::mtsdf::{self, BUCKETS, DISTANCE_RANGE, FIELD_PAD, MtsdfPlan};
use viso_text::paragraph::{LineLayout, Paragraph, ShapedSegment, ShapedSpan};
use viso_text::progressive::FontRevision;
use viso_text::{
    Admission, BaseDirection, CaretAffinity, DEFAULT_PAGE_BYTES, Direction, FontCache, FontFaceId,
    GlyphImageKind, GlyphKey, GlyphResidency, MemoryClass, PoolBudget, Reclaimed, Representation,
    RepresentationState, RunClass, ShapedGlyph, ShapedRun, TextOffset, TextPosition,
    TransformSample,
};

const FRAME: Duration = Duration::from_micros(16_667);

fn sample(frame: u32, px_per_em: f32) -> TransformSample {
    TransformSample {
        now: FRAME * frame,
        px_per_em,
        rotated: false,
        world_space: false,
    }
}

#[test]
fn glyph_image_kinds_are_exactly_five_with_no_plain_sdf() {
    let kind = |k: GlyphImageKind| match k {
        GlyphImageKind::MaskA8 => 0,
        GlyphImageKind::ScalableMtsdf => 1,
        GlyphImageKind::OutlineVector => 2,
        GlyphImageKind::ColorRgba8 => 3,
        GlyphImageKind::ColorVector => 4,
    };
    assert_eq!(kind(GlyphImageKind::ColorVector), 4);
    assert_eq!(Representation::coverage(16).kind, GlyphImageKind::MaskA8);
    assert_eq!(
        Representation::mtsdf(32).kind,
        GlyphImageKind::ScalableMtsdf
    );
    assert_eq!(
        Representation::OUTLINE,
        Representation {
            kind: GlyphImageKind::OutlineVector,
            bucket: 0
        }
    );
    assert!(!Representation::coverage(16).is_scalable());
    assert!(Representation::mtsdf(32).is_scalable());
    assert!(Representation::OUTLINE.is_scalable());
}

#[test]
fn a_run_starts_on_coverage_and_switches_only_at_a_resolve() {
    let revision = FontRevision(1);
    let mut state = RepresentationState::new(RunClass::Text, revision, &sample(0, 16.0));
    assert_eq!(state.drawn(), Representation::coverage(16));

    // Steady: nothing is evaluated, nothing is asked for.
    for frame in 1..30 {
        let resolution = state.resolve(&sample(frame, 16.0));
        assert_eq!(resolution.draw, Representation::coverage(16));
        assert_eq!((resolution.request, resolution.release), (None, None));
    }
    assert_eq!(state.evaluations(), 0, "a steady run is never re-evaluated");

    // A one-frame spike promotes nothing.
    let spike = state.resolve(&sample(30, 40.0));
    assert!(spike.request.is_none_or(|r| !r.is_scalable()));
    for frame in 31..60 {
        let resolution = state.resolve(&sample(frame, 16.0));
        assert!(resolution.request.is_none_or(|r| !r.is_scalable()));
    }
    assert!(!state.drawn().is_scalable());

    // A sustained zoom asks for a scalable representation, still drawing
    // coverage while it is pending.
    let mut asked = None;
    for (step, frame) in (60..180).enumerate() {
        let resolution = state.resolve(&sample(frame, 16.0 + step as f32));
        if let Some(request) = resolution.request.filter(Representation::is_scalable) {
            asked = Some((request, frame, 16.0 + step as f32));
            break;
        }
        assert!(!resolution.draw.is_scalable());
    }
    let (request, frame, px) = asked.expect("a sustained zoom promotes");
    assert_eq!(state.pending(), Some(request));
    assert!(!state.drawn().is_scalable(), "pending draws the previous");

    // Produced off the frame: the switch waits for the next resolve and
    // releases what the run stopped drawing.
    let before = state.drawn();
    assert!(state.ready(request, revision));
    assert_eq!(state.drawn(), before, "no switch mid-frame");
    let switch = state.resolve(&sample(frame + 1, px));
    assert_eq!(switch.draw, request);
    assert_eq!(switch.release, Some(before));

    // A stale revision is refused.
    assert!(!state.ready(Representation::coverage(16), FontRevision(0)));
}

fn key(face: u32, glyph: u16, kind: GlyphImageKind) -> GlyphKey {
    GlyphKey {
        face: FontFaceId(face),
        glyph,
        kind,
        bucket: 16,
    }
}

#[test]
fn residency_evicts_one_page_of_one_pool_and_never_resets() {
    let page = 64;
    let small = PoolBudget::new(2, page);
    let mut residency = GlyphResidency::with_pool_budgets(small, small, small, small);
    assert_eq!(small.bytes(), 2 * page);
    assert_eq!(
        PoolBudget::default(),
        PoolBudget::new(64, DEFAULT_PAGE_BYTES)
    );

    let color = key(0, 1, GlyphImageKind::ColorRgba8);
    assert!(matches!(
        residency.get_or_admit(color, page),
        Admission::Admitted {
            kind: GlyphImageKind::ColorRgba8,
            ..
        }
    ));
    let first = key(0, 1, GlyphImageKind::MaskA8);
    assert!(matches!(
        residency.get_or_admit(first, page),
        Admission::Admitted {
            kind: GlyphImageKind::MaskA8,
            page: 0,
            offset_bytes: 0
        }
    ));
    let uploaded = residency.upload_bytes_total();
    assert!(matches!(
        residency.get_or_admit(first, page),
        Admission::Cached {
            kind: GlyphImageKind::MaskA8,
            page: 0
        }
    ));
    assert_eq!(
        residency.upload_bytes_total(),
        uploaded,
        "a hit uploads nothing"
    );

    residency.get_or_admit(key(0, 2, GlyphImageKind::MaskA8), page);
    for _ in 0..3 {
        residency.advance_epoch();
    }
    residency.get_or_admit(key(0, 3, GlyphImageKind::MaskA8), page);

    let mut reclaims = Vec::new();
    residency.take_reclaims(&mut reclaims);
    assert_eq!(reclaims.len(), 1, "one page, not the atlas");
    assert!(matches!(
        reclaims[0],
        Reclaimed {
            kind: GlyphImageKind::MaskA8,
            ..
        }
    ));
    assert_eq!(residency.pool_evictions(GlyphImageKind::MaskA8), 1);
    assert_eq!(residency.pool_resident_glyphs(GlyphImageKind::MaskA8), 2);
    assert_eq!(residency.pool_evictions(GlyphImageKind::ColorRgba8), 0);
    assert_eq!(
        residency.pool_resident_glyphs(GlyphImageKind::ColorRgba8),
        1
    );

    // The two vector kinds share one pool.
    residency.get_or_admit(key(0, 9, GlyphImageKind::ColorVector), page);
    assert_eq!(
        residency.pool_resident_glyphs(GlyphImageKind::OutlineVector),
        1
    );

    // Pressure sheds the named pool only.
    residency.shed_pool_to_pressure(GlyphImageKind::MaskA8, 0);
    assert_eq!(residency.pool_resident_glyphs(GlyphImageKind::MaskA8), 0);
    assert_eq!(
        residency.pool_resident_glyphs(GlyphImageKind::ColorRgba8),
        1
    );
    assert_eq!(
        residency.pool_resident_glyphs(GlyphImageKind::OutlineVector),
        1
    );
}

#[test]
fn a_promoted_pool_that_would_evict_admits_coverage_instead() {
    let page = 64;
    let mut residency = GlyphResidency::with_pool_budgets(
        PoolBudget::new(4, page),
        PoolBudget::new(1, page),
        PoolBudget::new(1, page),
        PoolBudget::new(1, page),
    );
    residency.get_or_admit(key(0, 1, GlyphImageKind::ScalableMtsdf), page);
    let admission =
        residency.get_or_admit_with_fallback(key(0, 2, GlyphImageKind::ScalableMtsdf), page, 8);
    assert!(matches!(
        admission,
        Admission::Admitted {
            kind: GlyphImageKind::MaskA8,
            ..
        }
    ));
    assert_eq!(residency.pool_evictions(GlyphImageKind::ScalableMtsdf), 0);
}

#[test]
fn positions_are_typed_byte_offsets_with_affinity() {
    let source = "a\u{00e9}";
    assert!(TextOffset(1).is_char_boundary(source));
    assert!(
        !TextOffset(2).is_char_boundary(source),
        "offsets are UTF-8 bytes"
    );
    assert_eq!(
        TextPosition::upstream(TextOffset(3)),
        TextPosition {
            offset: TextOffset(3),
            affinity: CaretAffinity::Upstream
        }
    );
    assert_eq!(
        TextPosition::downstream(TextOffset(3)).affinity,
        CaretAffinity::Downstream
    );
}

/// One glyph per scalar, half an em each: shaping is the caller's.
fn shape(text: &str, _direction: Direction) -> ShapedSpan {
    let glyphs: Vec<ShapedGlyph> = text
        .char_indices()
        .map(|(at, _)| ShapedGlyph {
            glyph_id: 1,
            cluster: at as u32,
            x_advance: 0.5,
            x_offset: 0.0,
            y_offset: 0.0,
            unsafe_to_break: false,
        })
        .collect();
    ShapedSpan {
        segments: vec![ShapedSegment {
            start: 0,
            run: ShapedRun {
                face: FontFaceId(0),
                width_ems: 0.5 * glyphs.len() as f32,
                glyphs,
                text_len: text.len() as u32,
                ligature_carets: Vec::new(),
            },
        }],
    }
}

fn structure(lines: &[LineLayout]) -> Vec<(usize, usize, u32)> {
    lines
        .iter()
        .map(|l| (l.logical_range.0.0, l.logical_range.1.0, l.width.to_bits()))
        .collect()
}

#[test]
fn paragraph_entry_is_injected_shaping_and_incremental_equals_full() {
    let text = "one two three four five six seven eight nine ten";
    let mut paragraph = Paragraph::new(text, BaseDirection::Auto, 0);
    assert!(paragraph.layout(6.0, &mut shape), "the first layout runs");
    assert!(paragraph.lines().len() > 1, "a narrow width wraps");
    let calls = paragraph.shape_call_count();
    assert!(
        !paragraph.layout(6.0, &mut shape),
        "an unchanged layout is a hit"
    );
    assert_eq!(paragraph.shape_call_count(), calls);

    paragraph.edit((TextOffset(4), TextOffset(7)), "TWO");
    assert!(paragraph.layout(6.0, &mut shape));
    let incremental = structure(paragraph.lines());
    let full = structure(&paragraph.layout_full(6.0, &mut shape));
    assert_eq!(incremental, full);
    assert_eq!(paragraph.text(), text.replacen("two", "TWO", 1));
}

#[test]
fn every_cache_has_its_own_byte_budget_from_a_memory_class() {
    let [compact, standard, large] = [
        MemoryClass::Compact,
        MemoryClass::Standard,
        MemoryClass::Large,
    ]
    .map(MemoryClass::text_budgets);
    for budgets in [compact, standard, large] {
        assert!(budgets.face_cache_bytes > 0 && budgets.shaping_cache_bytes > 0);
    }
    assert!(compact.face_cache_bytes < standard.face_cache_bytes);
    assert!(standard.shaping_cache_bytes < large.shaping_cache_bytes);

    let mut faces = FontCache::with_budget(100);
    assert_eq!(faces.budget_bytes(), 100);
    faces.pin(FontFaceId(0));
    faces.admit(FontFaceId(0), 80);
    faces.admit(FontFaceId(1), 40);
    faces.advance_epoch();
    faces.admit(FontFaceId(2), 40);
    faces.advance_epoch();
    assert!(faces.total_bytes() <= 100 || faces.total_bytes() == faces.pinned_bytes());
    assert!(faces.contains(FontFaceId(0)), "a pin is never evicted");
    assert!(
        faces.evictions() > 0,
        "bytes, not a face count, bound the cache"
    );
}

#[test]
fn mtsdf_buckets_and_quality_windows() {
    assert_eq!(BUCKETS, [16.0, 32.0, 64.0, 128.0]);
    assert_eq!(DISTANCE_RANGE, 4.0);
    assert_eq!(FIELD_PAD, 3);
    assert!(FIELD_PAD as f32 >= DISTANCE_RANGE / 2.0);

    // Windows overlap: every scale up to the ladder's top has a bucket, and the
    // bucket planned for a scale always serves it.
    for pair in BUCKETS.windows(2) {
        assert!(mtsdf::window(pair[0]).1 >= mtsdf::window(pair[1]).0);
    }
    let top = mtsdf::window(BUCKETS[BUCKETS.len() - 1]).1;
    let mut px = 1.0;
    while px <= top {
        let MtsdfPlan::Bucket(bucket) = mtsdf::plan(px) else {
            panic!("{px} px-per-em is inside the ladder");
        };
        assert!(px <= mtsdf::window(bucket).1);
        px += 0.5;
    }
    // Past the top, hand off to the outline rather than stretch.
    assert_eq!(mtsdf::plan(top + 1.0), MtsdfPlan::Outline);
}
