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
//!
//! # Break safety
//!
//! A candidate line break that Unicode allows may still fall inside a shaping
//! context — Arabic joining, a ligature, an Indic cluster — where splitting the
//! glyph array in two and drawing each half would be wrong. The shaper carries
//! the provider's break-safety signal onto each glyph: [`ShapedGlyph::unsafe_to_break`]
//! is HarfBuzz's `UNSAFE_TO_BREAK`, set on the starting glyph of a cluster whose
//! boundary needs both sides reshaped if the input is cut there. When it is
//! clear, the fragments on either side of that cluster boundary can be reused
//! as-is. [`ShapedRun::is_safe_break`] answers this for a source byte offset,
//! and [`ShapedRun::reshape_span`] gives the minimal span a paragraph layer must
//! reshape around an unsafe candidate break rather than splitting mechanically.
//!
//! `rustybuzz` produces `UNSAFE_TO_BREAK` by default, so this signal is always
//! present — Viso keeps it conservatively and never guesses that a boundary is
//! safe. The actual worker reshape orchestration (spec 12.9's reshape-and-fit
//! flow) belongs to the paragraph layer, not here; this layer only supplies the
//! metadata and the boundary queries it reads.

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
    /// Whether breaking the input at the start of this glyph's cluster would
    /// require reshaping both sides (HarfBuzz `UNSAFE_TO_BREAK`). When false,
    /// the shaped fragments on either side of this cluster boundary can be
    /// reused as-is; when true, a line break landing here must reshape the
    /// boundary runs rather than mechanically splitting the glyph array.
    pub unsafe_to_break: bool,
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
    /// Byte length of the run's source substring. Clusters are offsets into
    /// this `[0, text_len)` range; the length fixes the run's trailing boundary
    /// (the last cluster's extent is otherwise unknown) so break-safety queries
    /// have an unambiguous end regardless of direction.
    pub text_len: u32,
}

impl ShapedRun {
    /// Whether any glyph in the run is `.notdef` (glyph id 0), i.e. the face
    /// could not cover some cluster and the caller must resolve it via fallback.
    pub fn has_coverage_miss(&self) -> bool {
        self.glyphs.iter().any(|g| g.glyph_id == 0)
    }

    /// Whether a line break at source byte offset `byte` is shaping-safe: the
    /// run's glyphs can be split there and each side reused without reshaping.
    ///
    /// The run's endpoints (`0` and `text_len`) are always safe. An interior
    /// offset is safe only when it is a cluster boundary — some glyph has that
    /// offset as its `cluster` — and the starting glyph of that cluster is not
    /// [`ShapedGlyph::unsafe_to_break`]. An offset inside a cluster (not any
    /// glyph's `cluster`) is never safe: splitting there would cut a cluster.
    pub fn is_safe_break(&self, byte: u32) -> bool {
        if byte == 0 || byte == self.text_len {
            return true;
        }
        // A cluster boundary is safe when the glyphs starting there are not
        // flagged unsafe. Marks in one cluster share a `cluster` value; the
        // flag rides the whole group, so `all` and `any` agree — use `all`.
        let mut found = false;
        for glyph in self.glyphs.iter().filter(|g| g.cluster == byte) {
            found = true;
            if glyph.unsafe_to_break {
                return false;
            }
        }
        found
    }

    /// The minimal source-byte span `[start, end)` a paragraph layer must
    /// reshape to break at `byte`. When `byte` is already a safe break the span
    /// is empty (`(byte, byte)`): nothing needs reshaping. Otherwise the span
    /// grows left and right to the nearest safe breaks (bounded by `0` and
    /// `text_len`), enclosing the unsafe boundary so both sides can be reshaped
    /// in isolation instead of splitting the glyph array mechanically.
    pub fn reshape_span(&self, byte: u32) -> (u32, u32) {
        if self.is_safe_break(byte) {
            return (byte, byte);
        }
        // Ordered, de-duplicated set of every candidate boundary in the run.
        let mut boundaries: std::collections::BTreeSet<u32> = self
            .glyphs
            .iter()
            .map(|g| g.cluster)
            .filter(|&c| c <= self.text_len)
            .collect();
        boundaries.insert(0);
        boundaries.insert(self.text_len);

        let start = boundaries
            .iter()
            .rev()
            .find(|&&b| b <= byte && self.is_safe_break(b))
            .copied()
            .unwrap_or(0);
        let end = boundaries
            .iter()
            .find(|&&b| b >= byte && self.is_safe_break(b))
            .copied()
            .unwrap_or(self.text_len);
        (start, end)
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
                unsafe_to_break: info.unsafe_to_break(),
            });
            width_ems += x_advance;
        }

        // Reclaim the buffer's allocation for the next run.
        self.reusable_buffer = Some(glyph_buffer.clear());

        Some(ShapedRun {
            face,
            glyphs,
            width_ems,
            text_len: text.len() as u32,
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
        assert!(!run.glyphs[0].unsafe_to_break);

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

    #[test]
    fn text_len_is_source_byte_length() {
        // The run covers [0, text_len); the length is the source substring's
        // byte count, not its glyph count. "你好" is two 3-byte chars: len 6,
        // two glyphs.
        assert_eq!(shape("AVA").text_len, 3);
        assert_eq!(shape("").text_len, 0);
        assert_eq!(shape("\u{4F60}\u{597D}").text_len, 6);
    }

    #[test]
    fn run_endpoints_are_always_safe() {
        // Both boundaries of the run are safe breaks regardless of contents —
        // including the empty run, whose only offset (0 == text_len) is safe.
        let run = shape("AVA");
        assert!(run.is_safe_break(0));
        assert!(run.is_safe_break(run.text_len));

        let empty = shape("");
        assert!(empty.is_safe_break(0));
        assert_eq!(empty.text_len, 0);
    }

    #[test]
    fn latin_cluster_starts_are_safe_breaks() {
        // Every ASCII letter is its own one-byte cluster, and the DejaVu subset
        // shapes none of them unsafe, so every cluster boundary 0..=text_len is
        // a safe break.
        let run = shape("AVA");
        for byte in 0..=run.text_len {
            assert!(run.is_safe_break(byte), "byte {byte} should be safe");
        }
    }

    #[test]
    fn interior_of_multibyte_cluster_is_unsafe() {
        // "你好" shapes to clusters at bytes 0 and 3 (each char is 3 bytes).
        // Bytes 1,2,4,5 fall inside a cluster — not any glyph's `cluster` — so
        // breaking there would cut a shaping cluster and is never safe.
        let run = shape("\u{4F60}\u{597D}");
        assert_eq!(run.text_len, 6);
        assert!(run.is_safe_break(0));
        assert!(run.is_safe_break(3));
        assert!(run.is_safe_break(6));
        for byte in [1, 2, 4, 5] {
            assert!(!run.is_safe_break(byte), "byte {byte} is cluster-interior");
        }
    }

    #[test]
    fn combining_sequence_is_one_cluster() {
        // "e\u{301}" (e + combining acute) shapes to two glyphs that share
        // cluster 0: one grapheme, one shaping cluster spanning [0, 3). The
        // interior byte 1 (between the base and the mark) is not a cluster
        // boundary, so it is not a safe break.
        let run = shape("e\u{301}");
        assert_eq!(run.text_len, 3);
        assert!(run.is_safe_break(0));
        assert!(run.is_safe_break(3));
        assert!(!run.is_safe_break(1));
    }

    #[test]
    fn reshape_span_is_empty_at_safe_break() {
        // A safe break needs no reshaping: the span is empty at that offset.
        let run = shape("AVA");
        for byte in [0u32, 1, 2, 3] {
            assert_eq!(run.reshape_span(byte), (byte, byte));
        }
    }

    #[test]
    fn reshape_span_encloses_unsafe_boundary() {
        // Breaking inside the first cluster of "你好" (byte 1) is unsafe. The
        // reshape span expands to the enclosing safe cluster boundaries [0, 3):
        // a paragraph layer reshapes exactly that span rather than splitting the
        // glyph array. (The DejaVu subset never sets `unsafe_to_break`, so this
        // exercises the cluster-interior branch — the span semantics are the
        // same as for a shaper-flagged unsafe boundary.)
        let run = shape("\u{4F60}\u{597D}");
        assert_eq!(run.reshape_span(1), (0, 3));
        assert_eq!(run.reshape_span(2), (0, 3));
        // The second cluster's interior expands to [3, 6).
        assert_eq!(run.reshape_span(4), (3, 6));
        assert_eq!(run.reshape_span(5), (3, 6));
    }

    #[test]
    fn unsafe_to_break_flag_is_populated_from_shaper() {
        // The field is read straight from rustybuzz, not fabricated. Re-shape
        // the same text and assert each glyph's flag equals what rustybuzz
        // reports for the same buffer — proving fidelity, not a hardcoded value.
        let text = "AVA";
        let run = shape(text);
        let rb_face = rustybuzz::Face::from_slice(DEJAVU, 0).expect("fixture parses");
        let mut buffer = rustybuzz::UnicodeBuffer::new();
        buffer.set_direction(rustybuzz::Direction::LeftToRight);
        buffer.push_str(text);
        let glyphs = rustybuzz::shape(&rb_face, &[], buffer);
        let expected: Vec<bool> = glyphs
            .glyph_infos()
            .iter()
            .map(|i| i.unsafe_to_break())
            .collect();
        let got: Vec<bool> = run.glyphs.iter().map(|g| g.unsafe_to_break).collect();
        assert_eq!(got, expected);
    }
}
