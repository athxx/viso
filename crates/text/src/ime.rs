//! IME composition: the preedit / composition region a platform input method
//! drives, expressed in typed logical positions and bridged to UTF-16.
//!
//! A composition is a temporary edit state *on the logical text*, never a
//! separate glyph overlay: it stores a logical byte range, the platform's clause
//! segments, the selected subrange within the preedit, the range it will replace
//! on commit, and the anchor the candidate window pins to — all as
//! [`TextOffset`] / [`TextPosition`], never as visual glyph indices. Every update
//! carries a revision so a slow worker's stale shaping can never overwrite newer
//! input.
//!
//! # UTF-16 is composition-local
//!
//! A platform IME (macOS `NSTextInputClient`, Android `InputConnection`, iOS
//! `UITextView`) speaks UTF-16 code units; the logical source and every
//! [`TextOffset`] are UTF-8 bytes. The conversion runs through a
//! [`Utf16Bridge`] built over the *composition (or paragraph) slice*, not the
//! whole document, and dropped when the composition ends — the spec's
//! no-global-table rule, and O(1) per query after one O(n) build instead of the
//! O(document) rescan a reference implementation does on every keystroke. The
//! bridge reads the source bytes verbatim, so a CJK composition is never
//! silently NFC/NFKC-normalized: the recovered range always lands on the
//! original source.
//!
//! # Candidate geometry comes from the visual caret map
//!
//! The candidate-window rectangle is derived from the retained visual caret map
//! — the [`LineLayout`] the caret / hit-test / selection readers share — at the
//! composition's anchor position, never from a guessed average character width.
//! For an RTL composition the anchor's inline x is the visual position of that
//! logical offset in its run, so the window sits where the caret visually is,
//! not where byte order would wrongly place it. The inline x is the composition
//! layer's contribution; the line's vertical band (top and height) is the
//! paragraph's, supplied when the rect is placed.

use crate::paragraph::LineLayout;
use crate::text_position::{TextOffset, TextPosition, Utf16Bridge};

/// A monotonically increasing edit revision. A composition update stamps the
/// revision it was computed against; a worker result carrying an older revision
/// than the live composition is stale and must be dropped rather than applied,
/// so slow shaping of an abandoned preedit can never overwrite newer input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Revision(pub u64);

impl Revision {
    /// The next revision after this one.
    pub fn next(self) -> Self {
        Revision(self.0 + 1)
    }
}

/// One clause / segment the platform reports within the preedit, as a logical
/// byte range. Japanese and Korean IMEs split a composition into clauses the
/// user converts independently; the range is logical (source-truth) so its
/// underline/style can be painted per clause without touching the source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompositionClause {
    /// The clause's logical byte range `[start, end)` within the source.
    pub range: (TextOffset, TextOffset),
}

impl CompositionClause {
    /// Whether the clause range is non-empty and well-ordered.
    fn is_valid(&self) -> bool {
        self.range.0 < self.range.1
    }
}

/// The active IME composition over a paragraph: a spec-12.17 logical edit state.
///
/// Every field is a logical position or range, and the whole state carries a
/// [`Revision`]. Nothing here is a visual glyph index. When there is no active
/// composition the range is empty (`start == end`) and [`Self::is_active`] is
/// false; the default is the inactive composition at the origin.
#[derive(Debug, Clone)]
pub struct ImeComposition {
    /// The preedit's logical byte range `[start, end)`. Empty when inactive.
    range: (TextOffset, TextOffset),
    /// The platform's clause segments within `range`, in logical order. Empty
    /// when the platform reports none (a single implicit clause spanning the
    /// whole preedit).
    clauses: Vec<CompositionClause>,
    /// The subrange within `range` the platform marks as the actively selected
    /// (currently-converting) clause, if any. A logical byte range inside the
    /// preedit; `None` when nothing is selected.
    selected: Option<(TextOffset, TextOffset)>,
    /// The range in the source this composition replaces on commit. Usually the
    /// preedit `range` itself, but a platform may target a wider span (e.g.
    /// reconversion of already-committed text). `None` means the preedit range.
    replacement: Option<(TextOffset, TextOffset)>,
    /// The logical position the candidate window anchors to — where the platform
    /// should place its candidate list. A [`TextPosition`] so affinity resolves
    /// the anchor's visual side at a BiDi seam.
    anchor: TextPosition,
    /// The revision this composition state was last updated at.
    revision: Revision,
}

impl Default for ImeComposition {
    fn default() -> Self {
        Self {
            range: (TextOffset(0), TextOffset(0)),
            clauses: Vec::new(),
            selected: None,
            replacement: None,
            anchor: TextPosition::downstream(TextOffset(0)),
            revision: Revision::default(),
        }
    }
}

/// The candidate-window rectangle, in the line's inline/cross space (em units).
///
/// The inline coordinate is the visual x of the composition anchor from the
/// retained caret map; the cross coordinates are the line's vertical band, passed
/// in by the paragraph when the rect is placed. It is deliberately not a screen
/// rect: the paragraph adds the line's origin to lift it into screen space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CandidateRect {
    /// Inline x of the anchor within the line, em units, left-to-right visual.
    pub inline_x: f32,
    /// Top of the line's vertical band, em units.
    pub top: f32,
    /// Height of the line's vertical band, em units.
    pub height: f32,
}

impl ImeComposition {
    /// Begin (or replace) a composition from platform-reported UTF-16 bounds,
    /// mapped through a composition-local bridge, at the given revision.
    ///
    /// `bridge` is built over the composition/paragraph slice the platform's
    /// offsets are relative to; the caller owns its lifetime and drops it when
    /// the composition ends. The anchor defaults to the preedit start
    /// (downstream); [`Self::set_anchor`] refines it. Existing clauses and the
    /// selected/replacement subranges are cleared — a fresh platform composition
    /// update supersedes them.
    pub fn begin(
        &mut self,
        bridge: &Utf16Bridge,
        utf16_start: usize,
        utf16_end: usize,
        revision: Revision,
    ) {
        let start = bridge.utf16_to_offset(utf16_start);
        let end = bridge.utf16_to_offset(utf16_end);
        // Normalize order: the platform reports ascending bounds, but guard so a
        // range is always well-ordered.
        self.range = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        self.clauses.clear();
        self.selected = None;
        self.replacement = None;
        self.anchor = TextPosition::downstream(self.range.0);
        self.revision = revision;
    }

    /// Set the active composition range directly from typed logical bounds
    /// (already UTF-8), at the given revision. The bridge-mapping entry point is
    /// [`Self::begin`]; this is the already-converted form for callers that hold
    /// UTF-8 offsets.
    pub fn set_range(&mut self, start: TextOffset, end: TextOffset, revision: Revision) {
        self.range = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        self.clauses.clear();
        self.selected = None;
        self.replacement = None;
        self.anchor = TextPosition::downstream(self.range.0);
        self.revision = revision;
    }

    /// Record the platform's clause segments, given as UTF-16 code-unit bounds
    /// mapped through `bridge`. Bounds outside the preedit range are clamped into
    /// it; empty or reversed clauses are dropped.
    pub fn set_clauses_utf16(&mut self, bridge: &Utf16Bridge, clauses_utf16: &[(usize, usize)]) {
        self.clauses.clear();
        for &(u_start, u_end) in clauses_utf16 {
            let start = self.clamp_to_range(bridge.utf16_to_offset(u_start));
            let end = self.clamp_to_range(bridge.utf16_to_offset(u_end));
            let clause = CompositionClause {
                range: (start.min(end), start.max(end)),
            };
            if clause.is_valid() {
                self.clauses.push(clause);
            }
        }
    }

    /// Mark the actively selected (converting) subrange from UTF-16 bounds mapped
    /// through `bridge`. Clamped into the preedit range; an empty selection
    /// clears it.
    pub fn set_selected_utf16(
        &mut self,
        bridge: &Utf16Bridge,
        utf16_start: usize,
        utf16_end: usize,
    ) {
        let start = self.clamp_to_range(bridge.utf16_to_offset(utf16_start));
        let end = self.clamp_to_range(bridge.utf16_to_offset(utf16_end));
        let (lo, hi) = (start.min(end), start.max(end));
        self.selected = if lo < hi { Some((lo, hi)) } else { None };
    }

    /// Set the range in the source this composition replaces on commit, from
    /// UTF-16 bounds mapped through `bridge`. Unlike the preedit range this may
    /// fall outside the preedit (reconversion), so it is not clamped to it.
    pub fn set_replacement_utf16(
        &mut self,
        bridge: &Utf16Bridge,
        utf16_start: usize,
        utf16_end: usize,
    ) {
        let start = bridge.utf16_to_offset(utf16_start);
        let end = bridge.utf16_to_offset(utf16_end);
        self.replacement = Some((start.min(end), start.max(end)));
    }

    /// Set the candidate-window anchor to a typed logical position. Affinity
    /// disambiguates the anchor's visual side at a direction boundary, so an RTL
    /// composition's candidate window pins to the correct visual edge.
    pub fn set_anchor(&mut self, anchor: TextPosition) {
        self.anchor = anchor;
    }

    /// Whether a composition is currently active (its preedit range is
    /// non-empty).
    pub fn is_active(&self) -> bool {
        self.range.0 < self.range.1
    }

    /// The preedit's logical byte range `[start, end)`.
    pub fn range(&self) -> (TextOffset, TextOffset) {
        self.range
    }

    /// The platform clause segments within the preedit, in logical order.
    pub fn clauses(&self) -> &[CompositionClause] {
        &self.clauses
    }

    /// The actively selected subrange within the preedit, if any.
    pub fn selected(&self) -> Option<(TextOffset, TextOffset)> {
        self.selected
    }

    /// The range this composition replaces on commit — the explicit replacement
    /// range when set, otherwise the preedit range.
    pub fn replacement(&self) -> (TextOffset, TextOffset) {
        self.replacement.unwrap_or(self.range)
    }

    /// The candidate-window anchor position.
    pub fn anchor(&self) -> TextPosition {
        self.anchor
    }

    /// The revision this composition was last updated at.
    pub fn revision(&self) -> Revision {
        self.revision
    }

    /// Whether a worker result computed at `result_revision` is still current
    /// for this composition. A result older than the live revision is stale and
    /// must be dropped so it cannot overwrite newer input.
    pub fn accepts(&self, result_revision: Revision) -> bool {
        result_revision >= self.revision
    }

    /// End the composition, returning to the inactive empty state. The
    /// composition-local [`Utf16Bridge`] is the caller's to drop; this only
    /// clears the logical state.
    pub fn clear(&mut self) {
        self.range = (TextOffset(0), TextOffset(0));
        self.clauses.clear();
        self.selected = None;
        self.replacement = None;
        self.anchor = TextPosition::downstream(TextOffset(0));
        // The revision is left intact: it is a document-edit clock, not part of
        // the per-composition state, so clearing a composition must not rewind
        // it and let a stale worker result look current again.
    }

    /// The candidate-window rectangle for this composition on `line`, given the
    /// line's vertical band (`top`, `height` in em units).
    ///
    /// The inline x is the visual position of the anchor offset resolved through
    /// the retained caret map — the same geometry the caret and hit test read —
    /// so it is a real visual position, never an average-width guess, and is
    /// correct for RTL and mixed-direction lines. Returns `None` when there is no
    /// active composition.
    pub fn candidate_rect(
        &self,
        line: &LineLayout,
        top: f32,
        height: f32,
    ) -> Option<CandidateRect> {
        if !self.is_active() {
            return None;
        }
        let inline_x = anchor_inline_x(line, self.anchor);
        Some(CandidateRect {
            inline_x,
            top,
            height,
        })
    }

    /// Clamp an offset into the preedit range so a clause / selected subrange
    /// derived from possibly-out-of-range platform bounds stays inside it.
    fn clamp_to_range(&self, offset: TextOffset) -> TextOffset {
        offset.max(self.range.0).min(self.range.1)
    }
}

/// The inline x of a logical anchor position within `line`'s retained caret map.
///
/// The anchor's offset is looked up in the run that owns it, using the run's
/// caret-stop inline x — so an RTL run reports the visually-left-to-right-correct
/// position, not a byte-order estimate. When the offset is not an exact stop (a
/// composition anchor should sit on a grapheme boundary, so this is a safety
/// net), or the line is empty, the hit tester resolves the nearest stop instead
/// of inventing a position.
fn anchor_inline_x(line: &LineLayout, anchor: TextPosition) -> f32 {
    // The anchor offset is a caret stop in exactly one run (its own logical
    // range, endpoints included at the appropriate side). Read that stop's
    // resolved visual x directly from the retained caret map — this is a real
    // visual position for both LTR and RTL runs.
    if let Some(x) = line
        .runs
        .iter()
        .find_map(|run| run.inline_x_of(anchor.offset))
    {
        return x;
    }
    // The anchor is not an exact stop (a safety net — a composition anchor should
    // sit on a grapheme boundary) or the line is empty. Snap to the nearest stop
    // at or before the anchor in each run so the result still comes from retained
    // geometry rather than a guessed width; an empty line anchors to its leading
    // edge.
    line.runs
        .iter()
        .flat_map(|run| run.caret_stops.iter())
        .take_while(|s| s.offset <= anchor.offset)
        .last()
        .map(|s| s.inline_x)
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FontFaceId;
    use crate::bidi::{BaseDirection, BidiInfo};
    use crate::shaping::{ShapedRun, Shaper};
    use crate::text_position::CaretAffinity;

    const DEJAVU: &[u8] = include_bytes!("../tests/fixtures/DejaVuSans-subset.ttf");

    fn layout_line(text: &str, base: BaseDirection) -> LineLayout {
        let bidi = BidiInfo::resolve(text, base);
        let logical_runs = bidi.direction_runs_in(TextOffset(0), TextOffset(text.len()));
        let mut shaper = Shaper::new();
        let shaped: Vec<ShapedRun> = logical_runs
            .iter()
            .map(|run| {
                let sub = &text[run.start.0..run.end.0];
                shaper
                    .shape_run(FontFaceId(0), DEJAVU, 0, sub, run.direction)
                    .expect("fixture parses")
            })
            .collect();
        LineLayout::single_line(text, &bidi, &shaped)
    }

    #[test]
    fn default_composition_is_inactive() {
        let ime = ImeComposition::default();
        assert!(!ime.is_active());
        assert_eq!(ime.range(), (TextOffset(0), TextOffset(0)));
        assert!(ime.clauses().is_empty());
        assert!(ime.selected().is_none());
    }

    #[test]
    fn begin_maps_utf16_bounds_to_logical_range() {
        // A CJK preedit "你好" is 6 UTF-8 bytes but 2 UTF-16 units. The platform
        // reports units 0..2; the bridge maps that to bytes 0..6, never to a
        // byte range that would slice a scalar.
        let text = "\u{4F60}\u{597D}";
        let bridge = Utf16Bridge::new(text);
        let mut ime = ImeComposition::default();
        ime.begin(&bridge, 0, 2, Revision(1));
        assert!(ime.is_active());
        assert_eq!(ime.range(), (TextOffset(0), TextOffset(6)));
        // The anchor defaults to the preedit start, downstream.
        assert_eq!(ime.anchor(), TextPosition::downstream(TextOffset(0)));
    }

    #[test]
    fn astral_composition_maps_surrogate_units() {
        // A single emoji preedit is 4 UTF-8 bytes and 2 UTF-16 units (a surrogate
        // pair). Units 0..2 map to bytes 0..4 — the whole scalar, not a middle.
        let text = "\u{1F600}";
        let bridge = Utf16Bridge::new(text);
        let mut ime = ImeComposition::default();
        ime.begin(&bridge, 0, 2, Revision(1));
        assert_eq!(ime.range(), (TextOffset(0), TextOffset(4)));
    }

    #[test]
    fn clauses_map_and_clamp_into_range() {
        // "あいう" = 3 BMP chars, 3 bytes each = 9 bytes, 3 UTF-16 units. Preedit
        // is the whole thing; two clauses [0,1) and [1,3) in units map to byte
        // ranges [0,3) and [3,9).
        let text = "\u{3042}\u{3044}\u{3046}";
        let bridge = Utf16Bridge::new(text);
        let mut ime = ImeComposition::default();
        ime.begin(&bridge, 0, 3, Revision(1));
        ime.set_clauses_utf16(&bridge, &[(0, 1), (1, 3)]);
        assert_eq!(ime.clauses().len(), 2);
        assert_eq!(ime.clauses()[0].range, (TextOffset(0), TextOffset(3)));
        assert_eq!(ime.clauses()[1].range, (TextOffset(3), TextOffset(9)));
    }

    #[test]
    fn selected_subrange_maps_from_utf16() {
        let text = "\u{3042}\u{3044}\u{3046}";
        let bridge = Utf16Bridge::new(text);
        let mut ime = ImeComposition::default();
        ime.begin(&bridge, 0, 3, Revision(1));
        // The middle clause (unit 1..2 -> byte 3..6) is the actively converting one.
        ime.set_selected_utf16(&bridge, 1, 2);
        assert_eq!(ime.selected(), Some((TextOffset(3), TextOffset(6))));
    }

    #[test]
    fn replacement_defaults_to_range_and_can_be_wider() {
        let text = "abcdef";
        let bridge = Utf16Bridge::new(text);
        let mut ime = ImeComposition::default();
        ime.begin(&bridge, 2, 4, Revision(1));
        // No explicit replacement: the preedit range is what commit replaces.
        assert_eq!(ime.replacement(), (TextOffset(2), TextOffset(4)));
        // Reconversion targets a wider span than the preedit.
        ime.set_replacement_utf16(&bridge, 0, 6);
        assert_eq!(ime.replacement(), (TextOffset(0), TextOffset(6)));
    }

    #[test]
    fn revision_gates_stale_worker_results() {
        let text = "abc";
        let bridge = Utf16Bridge::new(text);
        let mut ime = ImeComposition::default();
        ime.begin(&bridge, 0, 3, Revision(5));
        // A result from the current or a newer revision is accepted.
        assert!(ime.accepts(Revision(5)));
        assert!(ime.accepts(Revision(6)));
        // An older result (from an abandoned preedit) is stale and rejected.
        assert!(!ime.accepts(Revision(4)));
    }

    #[test]
    fn clear_returns_to_inactive_but_keeps_revision() {
        let text = "abc";
        let bridge = Utf16Bridge::new(text);
        let mut ime = ImeComposition::default();
        ime.begin(&bridge, 0, 3, Revision(7));
        ime.clear();
        assert!(!ime.is_active());
        assert!(ime.clauses().is_empty());
        // The edit clock is not rewound: a stale worker result stays stale.
        assert_eq!(ime.revision(), Revision(7));
        assert!(!ime.accepts(Revision(6)));
    }

    #[test]
    fn candidate_rect_uses_visual_caret_map_ltr() {
        // In "AVA" a composition anchored at offset 1 must place its candidate
        // window at that stop's real inline x from the caret map, not an
        // average-width guess.
        let line = layout_line("AVA", BaseDirection::LeftToRight);
        let run = &line.runs[0];
        let x1 = run.inline_x_of(TextOffset(1)).unwrap();
        let mut ime = ImeComposition::default();
        ime.set_range(TextOffset(1), TextOffset(3), Revision(1));
        ime.set_anchor(TextPosition::downstream(TextOffset(1)));
        let rect = ime.candidate_rect(&line, 0.0, 1.0).unwrap();
        assert!((rect.inline_x - x1).abs() < 1e-4);
        assert_eq!(rect.top, 0.0);
        assert_eq!(rect.height, 1.0);
    }

    #[test]
    fn candidate_rect_rtl_uses_visual_position_not_byte_order() {
        // Pure Hebrew resolves RTL. A composition anchored at a mid-text logical
        // offset must place the candidate window at that offset's *visual* x —
        // which in an RTL run is not where byte order would put it. Assert the
        // anchor x matches the run's own caret-stop x for that offset.
        let text = "\u{05D0}\u{05D1}\u{05D2}";
        let line = layout_line(text, BaseDirection::RightToLeft);
        let run = &line.runs[0];
        // Offset 2 is the boundary after the first Hebrew letter (2 bytes each).
        let expected = run.inline_x_of(TextOffset(2)).unwrap();
        let mut ime = ImeComposition::default();
        ime.set_range(TextOffset(0), TextOffset(4), Revision(1));
        ime.set_anchor(TextPosition::upstream(TextOffset(2)));
        let rect = ime.candidate_rect(&line, 0.0, 1.0).unwrap();
        assert!(
            (rect.inline_x - expected).abs() < 1e-4,
            "RTL candidate x {} should equal the run's visual stop x {}",
            rect.inline_x,
            expected
        );
        // And it is genuinely a visual (not byte-order) position: the RTL run's
        // stop x for a higher logical offset is further left.
        let x0 = run.inline_x_of(TextOffset(0)).unwrap();
        assert!(expected < x0, "RTL: higher offset sits visually left");
    }

    #[test]
    fn inactive_composition_has_no_candidate_rect() {
        let line = layout_line("AVA", BaseDirection::LeftToRight);
        let ime = ImeComposition::default();
        assert!(ime.candidate_rect(&line, 0.0, 1.0).is_none());
    }

    #[test]
    fn composition_does_not_normalize_source_range() {
        // "e\u{301}" (e + combining acute) is NFC-composable to "é" (2 bytes).
        // The bridge keeps the source as given (3 bytes, 2 units); a composition
        // over units 0..2 must land on byte 0..3, proving the source range is not
        // silently normalized.
        let text = "e\u{301}";
        assert_eq!(text.len(), 3);
        let bridge = Utf16Bridge::new(text);
        let mut ime = ImeComposition::default();
        ime.begin(&bridge, 0, 2, Revision(1));
        assert_eq!(ime.range(), (TextOffset(0), TextOffset(3)));
    }

    #[test]
    fn anchor_affinity_is_preserved() {
        // The anchor keeps its affinity so a BiDi-seam candidate window resolves
        // to the correct visual side.
        let mut ime = ImeComposition::default();
        ime.set_range(TextOffset(0), TextOffset(3), Revision(1));
        ime.set_anchor(TextPosition::upstream(TextOffset(2)));
        assert_eq!(ime.anchor().affinity, CaretAffinity::Upstream);
        assert_eq!(ime.anchor().offset, TextOffset(2));
    }
}
