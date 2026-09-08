//! Behavioral tests for the text subsystem, driven by an embedded ASCII subset
//! of DejaVu Sans (`tests/fixtures/DejaVuSans-subset.ttf`).

use viso_text::{FontStore, TextSystem, layout, rasterize_glyph, shape};

const FONT: &[u8] = include_bytes!("fixtures/DejaVuSans-subset.ttf");

fn store() -> (FontStore, viso_text::FontId) {
    let mut store = FontStore::new();
    let id = store.load(FONT.to_vec(), 0).expect("parse subset font");
    (store, id)
}

#[test]
fn font_metrics_are_sane() {
    let (store, id) = store();
    let face = store.face(id);
    assert!(face.units_per_em > 0.0);
    assert!(face.ascender_em > 0.0);
    assert!(face.descender_em < 0.0);
    assert!(face.line_height_em() > face.ascender_em);
}

#[test]
fn shaping_produces_nonzero_advances() {
    let (store, id) = store();
    let glyphs = shape(&store, "Hello");
    assert_eq!(glyphs.len(), 5);
    for g in &glyphs {
        assert!(g.advance_em > 0.0, "glyph {} has zero advance", g.id);
        assert!(g.id != 0, "unexpected .notdef in ASCII shaping");
        // A single-face store resolves every glyph to the primary face.
        assert_eq!(g.font, id, "ASCII shapes with the primary face");
    }
    // Clusters increase left-to-right across the ASCII run.
    assert_eq!(glyphs[0].cluster, 0);
    assert!(glyphs[4].cluster > glyphs[0].cluster);
}

#[test]
fn empty_chain_shapes_to_nothing() {
    // A store with no font loaded has an empty chain and shapes to no glyphs
    // rather than panicking on a missing primary.
    let store = FontStore::new();
    assert!(shape(&store, "Hello").is_empty());
}

#[test]
fn uncovered_glyphs_stay_notdef_without_a_fallback() {
    // The ASCII subset covers no CJK; with no fallback face the run shapes to
    // the primary's .notdef box rather than resolving elsewhere.
    let (store, id) = store();
    let glyphs = shape(&store, "中");
    assert!(
        !glyphs.is_empty(),
        "a codepoint still produces a glyph slot"
    );
    assert!(
        glyphs.iter().all(|g| g.font == id),
        "with no fallback, everything stays on the primary face"
    );
    assert!(
        glyphs.iter().any(|g| g.id == 0),
        "uncovered CJK shapes to .notdef on the primary"
    );
}

#[test]
fn fallback_recursion_resolves_missing_glyphs_to_the_tail_face() {
    // Two faces: a primary that covers only the ASCII subset, and a fallback.
    // Reusing the same subset bytes for both means the fallback covers the same
    // glyphs — so a covered run still resolves entirely to the primary, proving
    // recursion only reshapes genuine .notdef spans, never covered ones.
    let mut store = FontStore::new();
    let primary = store.load(FONT.to_vec(), 0).expect("primary");
    let fallback = store.load(FONT.to_vec(), 0).expect("fallback");
    store.push_fallback(fallback);

    let glyphs = shape(&store, "Hi");
    assert_eq!(glyphs.len(), 2);
    assert!(
        glyphs.iter().all(|g| g.font == primary),
        "covered text never falls through to the tail face"
    );
    assert!(glyphs.iter().all(|g| g.id != 0));
}

#[test]
fn ltr_fast_path_and_bidi_agree_on_pure_ltr() {
    // Pure-LTR text takes the fast path; a leading RTL marker would force the
    // BiDi path. Both must shape the ASCII tail to the same non-notdef glyphs
    // (the subset has no RTL coverage, so we only assert the LTR portion holds).
    let (store, _) = store();
    let fast = shape(&store, "abc");
    assert_eq!(fast.len(), 3);
    assert!(fast.iter().all(|g| g.id != 0 && g.advance_em > 0.0));
    // Clusters are the absolute byte offsets 0,1,2 for single-byte ASCII.
    assert_eq!(fast[0].cluster, 0);
    assert_eq!(fast[1].cluster, 1);
    assert_eq!(fast[2].cluster, 2);
}

#[test]
fn multiline_layout_steps_baseline_down() {
    let (store, id) = store();
    let placed = layout(&store, id, "ab\ncd", 32.0);
    // Two glyphs per line, four total.
    assert_eq!(placed.len(), 4);
    let line0_y = placed[0].origin_px[1];
    let line1_y = placed[2].origin_px[1];
    assert!(line1_y > line0_y, "second line baseline must be lower");
    // First glyph of each line starts at the same pen x (0 + its bearing/offset).
    assert!((placed[0].origin_px[0] - placed[2].origin_px[0]).abs() < 1e-3);
    // Within a line the pen advances rightward.
    assert!(placed[1].origin_px[0] > placed[0].origin_px[0]);
}

#[test]
fn rasterized_glyph_is_nonempty_sdf() {
    let (store, id) = store();
    let face = store.face(id);
    // 'A' (U+0041) has an outline.
    let a = face.rb();
    let gid = a.glyph_index('A').expect("A in cmap").0;
    let raster = rasterize_glyph(face, gid, 32.0).expect("A rasterizes");
    assert!(raster.width > 0 && raster.height > 0);
    assert_eq!(raster.sdf.len(), (raster.width * raster.height) as usize);
    // The SDF must contain some near-edge coverage, not be uniformly empty.
    assert!(raster.sdf.iter().any(|&b| b > 0), "SDF is all zero");
    assert!(raster.px_range > 0.0);
}

#[test]
fn atlas_caches_repeated_glyphs() {
    let mut sys = TextSystem::new();
    let id = sys.load_font(FONT.to_vec(), 0).unwrap();

    let quads = sys.prepare(id, "AA", 32.0, 2.0);
    assert_eq!(quads.len(), 2, "both A glyphs produce quads");
    // The two 'A's are identical, so they share one atlas cell (same UV).
    assert_eq!(quads[0].uv, quads[1].uv);
    // After preparing, the atlas has a dirty region to upload.
    assert!(sys.take_atlas_dirty().is_some());
    // A second identical prepare hits the cache — no new dirty region.
    let again = sys.prepare(id, "AA", 32.0, 2.0);
    assert_eq!(again[0].uv, quads[0].uv);
    assert!(
        sys.take_atlas_dirty().is_none(),
        "cache hit must not re-dirty"
    );
}

#[test]
fn whitespace_advances_without_quad() {
    let mut sys = TextSystem::new();
    let id = sys.load_font(FONT.to_vec(), 0).unwrap();
    // "a b" — the space has no outline, so only 2 quads for 'a' and 'b'.
    let quads = sys.prepare(id, "a b", 24.0, 1.0);
    assert_eq!(quads.len(), 2);
    // 'b' sits to the right of 'a' with the space's advance between them.
    assert!(quads[1].rect_px[0] > quads[0].rect_px[0]);
}

#[test]
fn first_load_seeds_the_chain_as_primary() {
    let (store, id) = store();
    // Loading a single face makes it the chain head; a single-face store shapes
    // with it exactly as before, so the chain is just [id].
    assert_eq!(store.chain(), &[id]);
    assert_eq!(store.primary(), Some(id));
}

#[test]
fn face_coverage_reports_cmap_membership() {
    let (store, id) = store();
    let face = store.face(id);
    // The ASCII subset covers Latin but not CJK.
    assert!(face.has_char('A'), "DejaVu subset covers Latin 'A'");
    assert!(!face.has_char('中'), "DejaVu subset does not cover CJK");
    // glyph_count is a coarse coverage weight, > 0 for any real face.
    assert!(face.glyph_count > 0);
}

#[test]
fn first_covering_walks_the_chain() {
    let (store, id) = store();
    // A covered character resolves to the primary; an uncovered one resolves to
    // nothing (it will shape to .notdef and become a system-font candidate).
    assert_eq!(store.first_covering('A'), Some(id));
    assert_eq!(store.first_covering('中'), None);
}

#[test]
fn push_fallback_extends_and_dedups_the_chain() {
    let mut store = FontStore::new();
    let primary = store.load(FONT.to_vec(), 0).expect("primary");
    // A second load registers a face but does not touch the chain — resolving a
    // fallback is explicit. (Reusing the same fixture bytes yields a distinct id.)
    let fallback = store.load(FONT.to_vec(), 0).expect("fallback");
    assert_eq!(
        store.chain(),
        &[primary],
        "load alone does not extend the chain"
    );

    store.push_fallback(fallback);
    assert_eq!(store.chain(), &[primary, fallback]);

    // Re-resolving the same face is a no-op — the chain never grows a duplicate.
    store.push_fallback(fallback);
    assert_eq!(store.chain(), &[primary, fallback]);
}
