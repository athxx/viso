//! Complex-script shaping: turn a resolved, single-face, single-direction run
//! into positioned glyphs with clusters, driven by `rustybuzz`.
//!
//! Shaping is complex-text-complete: ligatures, marks, contextual forms, and
//! OpenType feature application. It runs on a worker, never the main thread, and
//! emits shaping clusters that map back to source byte offsets.
//!
//! # What this layer owns, and what it does not
//!
//! The shaper consumes a face it is handed — owned sfnt bytes plus a face index
//! — and produces positioned glyphs. It does not resolve faces, does not run
//! fallback, and does not own the Face Cache: those are separate lifecycles.
//! Reconstructing a memoized parsed face from a [`FontFaceId`] is the Face
//! Cache's job; here a [`rustybuzz::Face`] is built from the bytes for the run
//! and dropped after. The reusable Unicode buffer is retained across calls so a
//! steady stream of runs does not reallocate it.
//!
//! # Positions are in font design units per em
//!
//! Advances and offsets are normalized to em units (design units divided by
//! `units_per_em`), so a shaped run is resolution- and size-independent: line
//! layout multiplies by the pixel size later. This keeps a shaped run reusable
//! across sizes when text/features/face are unchanged.

use crate::FontFaceId;

/// One positioned glyph produced by shaping.
///
/// The face is implicit: a shaped run is single-face, so the run carries the
/// [`FontFaceId`] once rather than tagging every glyph with it. `cluster` is the
/// byte offset, within the shaped source text, of the character this glyph
/// derives from; a ligature and its several source characters share one
/// cluster, and several glyphs forming one character (marks) share one too.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapedGlyph {
    /// Glyph index within the face. Zero is `.notdef` — a coverage miss the
    /// caller resolves through fallback in a later stage.
    pub glyph_id: u16,
    /// Source byte offset of the cluster this glyph belongs to.
    pub cluster: u32,
    /// Horizontal advance in em units (design units / `units_per_em`).
    pub x_advance: f32,
    /// Horizontal glyph placement offset in em units.
    pub x_offset: f32,
    /// Vertical glyph placement offset in em units (marks, superscripts).
    pub y_offset: f32,
}

/// A single-face, single-direction run of positioned glyphs.
#[derive(Debug, Clone, PartialEq)]
pub struct ShapedRun {
    /// The face every glyph in this run was shaped with.
    pub face: FontFaceId,
    /// The positioned glyphs, in visual order for the run's direction.
    pub glyphs: Vec<ShapedGlyph>,
    /// Sum of glyph advances in em units: the run's laid-out width per em.
    pub width_ems: f32,
}

impl ShapedRun {
    /// Whether any glyph in the run is `.notdef` (glyph id 0), i.e. the face
    /// could not cover some cluster and the caller must resolve it via fallback.
    pub fn has_coverage_miss(&self) -> bool {
        self.glyphs.iter().any(|g| g.glyph_id == 0)
    }
}

/// The direction a run is shaped in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Left-to-right (Latin, and the P0 baseline).
    LeftToRight,
    /// Right-to-left (Arabic, Hebrew); filled in with the BiDi stage.
    RightToLeft,
}

/// The shaper over resolved faces.
///
/// It retains a reusable [`rustybuzz::UnicodeBuffer`] so that shaping a stream
/// of runs does not reallocate the buffer per call. The buffer is taken for a
/// shape and handed back by clearing the resulting glyph buffer.
#[derive(Debug, Default)]
pub struct Shaper {
    /// Reused across `shape_run` calls: taken on entry, restored on exit.
    reusable_buffer: Option<rustybuzz::UnicodeBuffer>,
}

impl Shaper {
    /// A shaper with no retained buffer yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Shape one single-face, single-direction run into positioned glyphs.
    ///
    /// `face` is the resolved identity carried onto the run; `sfnt` and `index`
    /// are the owned face bytes and face index handed in by the Face Cache;
    /// `text` is the run's source substring; `dir` is its resolved direction.
    /// Returns `None` when the bytes do not parse as a usable face.
    ///
    /// Advances and offsets come back in em units. Clusters are source byte
    /// offsets into `text`.
    pub fn shape_run(
        &mut self,
        face: FontFaceId,
        sfnt: &[u8],
        index: u32,
        text: &str,
        dir: Direction,
    ) -> Option<ShapedRun> {
        let rb_face = rustybuzz::Face::from_slice(sfnt, index)?;
        let units_per_em = rb_face.units_per_em() as f32;

        let mut buffer = self.reusable_buffer.take().unwrap_or_default();
        buffer.set_direction(match dir {
            Direction::LeftToRight => rustybuzz::Direction::LeftToRight,
            Direction::RightToLeft => rustybuzz::Direction::RightToLeft,
        });
        buffer.push_str(text);

        // No explicit OpenType feature overrides at the P0 baseline; the shaper
        // applies the face's default feature set.
        let glyph_buffer = rustybuzz::shape(&rb_face, &[], buffer);

        let infos = glyph_buffer.glyph_infos();
        let positions = glyph_buffer.glyph_positions();
        let mut glyphs = Vec::with_capacity(infos.len());
        let mut width_ems = 0.0f32;
        for (info, pos) in infos.iter().zip(positions.iter()) {
            let x_advance = pos.x_advance as f32 / units_per_em;
            glyphs.push(ShapedGlyph {
                glyph_id: info.glyph_id as u16,
                cluster: info.cluster,
                x_advance,
                x_offset: pos.x_offset as f32 / units_per_em,
                y_offset: pos.y_offset as f32 / units_per_em,
            });
            width_ems += x_advance;
        }

        // Reclaim the buffer's allocation for the next run.
        self.reusable_buffer = Some(glyph_buffer.clear());

        Some(ShapedRun {
            face,
            glyphs,
            width_ems,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A subset of DejaVu Sans carrying the Latin letters the goldens use.
    const DEJAVU: &[u8] = include_bytes!("../tests/fixtures/DejaVuSans-subset.ttf");

    fn shape(text: &str) -> ShapedRun {
        let mut shaper = Shaper::new();
        shaper
            .shape_run(FontFaceId(0), DEJAVU, 0, text, Direction::LeftToRight)
            .expect("fixture parses")
    }

    /// Two advances in em units compare equal within a design-unit rounding
    /// tolerance (the fixture is 2048 upm, so one unit is ~0.0005 em).
    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn latin_run_shapes_to_golden_glyphs_and_advances() {
        // Golden for the fixture subset: "AV" maps to glyph ids 34 and 55, each
        // one Latin cluster, each advancing ~0.684 em, in left-to-right order.
        let run = shape("AV");
        assert_eq!(run.face, FontFaceId(0));
        assert_eq!(run.glyphs.len(), 2);

        assert_eq!(run.glyphs[0].glyph_id, 34);
        assert_eq!(run.glyphs[0].cluster, 0);
        assert!(approx(run.glyphs[0].x_advance, 0.684_082_03));
        assert_eq!(run.glyphs[0].x_offset, 0.0);
        assert_eq!(run.glyphs[0].y_offset, 0.0);

        assert_eq!(run.glyphs[1].glyph_id, 55);
        assert_eq!(run.glyphs[1].cluster, 1);
        assert!(approx(run.glyphs[1].x_advance, 0.684_082_03));

        // The run width is the sum of glyph advances.
        assert!(approx(
            run.width_ems,
            run.glyphs.iter().map(|g| g.x_advance).sum()
        ));
        assert!(approx(run.width_ems, 1.368_164_1));
    }

    #[test]
    fn clusters_are_source_byte_offsets_in_order() {
        // Each ASCII letter is one byte, so clusters run 0,1,2 and stay
        // monotonic in left-to-right visual order.
        let run = shape("AVA");
        let clusters: Vec<u32> = run.glyphs.iter().map(|g| g.cluster).collect();
        assert_eq!(clusters, vec![0, 1, 2]);
        // The repeated 'A' shapes to the same glyph id both times.
        assert_eq!(run.glyphs[0].glyph_id, run.glyphs[2].glyph_id);
    }

    #[test]
    fn empty_run_shapes_to_no_glyphs() {
        let run = shape("");
        assert!(run.glyphs.is_empty());
        assert_eq!(run.width_ems, 0.0);
    }

    #[test]
    fn covered_latin_has_no_coverage_miss() {
        // Every glyph resolves to a real (non-notdef) id.
        let run = shape("AV");
        assert!(!run.has_coverage_miss());
        assert!(run.glyphs.iter().all(|g| g.glyph_id != 0));
    }

    #[test]
    fn uncovered_codepoint_is_a_coverage_miss() {
        // A CJK character the Latin subset cannot cover shapes to .notdef, the
        // signal the fallback stage keys on.
        let run = shape("\u{4F60}");
        assert!(run.has_coverage_miss());
    }

    #[test]
    fn buffer_is_reused_across_runs() {
        // Shaping many runs through one shaper must reuse the retained buffer
        // rather than leaving it taken; a second run still shapes correctly.
        let mut shaper = Shaper::new();
        let first = shaper
            .shape_run(FontFaceId(0), DEJAVU, 0, "AV", Direction::LeftToRight)
            .expect("fixture parses");
        assert!(shaper.reusable_buffer.is_some());
        let second = shaper
            .shape_run(FontFaceId(0), DEJAVU, 0, "AV", Direction::LeftToRight)
            .expect("fixture parses");
        assert_eq!(first, second);
    }

    #[test]
    fn bad_bytes_do_not_parse() {
        let mut shaper = Shaper::new();
        assert!(
            shaper
                .shape_run(FontFaceId(0), b"not a font", 0, "A", Direction::LeftToRight)
                .is_none()
        );
    }
}
