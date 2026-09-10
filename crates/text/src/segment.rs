//! Text segmentation (UAX#29): extended grapheme clusters and word boundaries,
//! expressed as typed [`TextOffset`]s, plus the incremental checkpoint an editor
//! resumes a local edit from.
//!
//! Segmentation feeds two consumers with different needs. Caret and selection
//! need *grapheme-aware* movement: a caret never lands inside a user-perceived
//! character, so navigation snaps to extended-grapheme-cluster boundaries. Word
//! navigation needs UAX#29 word boundaries as its cross-language base — never
//! `split_ascii_whitespace`, which is wrong for every script without spaces.
//!
//! The UAX#29 boundary tables are proven, standards-heavy Unicode data, so this
//! module *wraps* `unicode-segmentation` rather than reimplementing the rules
//! (the Ownership Ladder: prefer a proven algorithm for a Unicode primitive).
//! What this module owns is the typing — boundaries are [`TextOffset`], never a
//! bare `usize` shared across index meanings — and the incremental contract.
//!
//! # Incremental correctness, never a fixed-character window
//!
//! A local edit must not force a whole-paragraph re-segmentation, but the naive
//! shortcut of "re-check the two code points around the edit" is wrong: rules
//! like regional-indicator parity depend on more context than a fixed window.
//! The correct unit is a *checkpoint* — a confirmed grapheme boundary. An
//! extended grapheme cluster boundary is a hard reset point for the segmentation
//! state machine: no UAX#29 grapheme rule reaches across a confirmed boundary,
//! and regional-indicator parity itself resets at each emitted cluster. So
//! resuming segmentation at a [`GraphemeCheckpoint`] yields boundaries
//! byte-identical to the tail of a full pass over the whole text. That is the
//! equivalence the incremental path relies on, and the conformance corpus tests
//! prove it holds on every UAX#29 sample.

use unicode_segmentation::UnicodeSegmentation;

use crate::text_position::TextOffset;

/// A confirmed grapheme-cluster boundary an incremental re-segmentation may
/// resume from without re-scanning the text before it.
///
/// It is only valid to construct one at a boundary that a full segmentation
/// pass would emit; [`Segmenter::checkpoint_at_or_before`] finds the nearest
/// such boundary at or before an offset, so an edit at an arbitrary offset can
/// still resume from a sound point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GraphemeCheckpoint(pub TextOffset);

/// Segmentation over a slice of paragraph source text.
///
/// The segmenter borrows the text rather than owning it: it is a thin typed view
/// onto the UAX#29 iterators, cheap to construct per query, and holds no mutable
/// state that a steady-state frame would have to reset.
#[derive(Debug, Clone, Copy)]
pub struct Segmenter<'a> {
    text: &'a str,
}

impl<'a> Segmenter<'a> {
    /// A segmenter over the given source text.
    pub fn new(text: &'a str) -> Self {
        Self { text }
    }

    /// The source text this segmenter views.
    pub fn text(&self) -> &'a str {
        self.text
    }

    /// Every extended-grapheme-cluster boundary, in order, including the leading
    /// `0` and the trailing `text.len()`. Empty text yields the single boundary
    /// `[0]`.
    ///
    /// These are the caret stops: a caret may sit at any of these offsets and
    /// never between them.
    pub fn grapheme_boundaries(&self) -> impl Iterator<Item = TextOffset> + '_ {
        std::iter::once(TextOffset(0)).chain(
            self.text
                .grapheme_indices(true)
                .map(|(offset, cluster)| TextOffset(offset + cluster.len())),
        )
    }

    /// Every UAX#29 word boundary, in order, including the leading `0` and the
    /// trailing `text.len()`. Empty text yields the single boundary `[0]`.
    ///
    /// This is the cross-language base for word navigation; locale/editor
    /// tailoring layers on top of it, never in place of it.
    pub fn word_boundaries(&self) -> impl Iterator<Item = TextOffset> + '_ {
        std::iter::once(TextOffset(0)).chain(
            self.text
                .split_word_bound_indices()
                .map(|(offset, word)| TextOffset(offset + word.len())),
        )
    }

    /// The next grapheme-cluster boundary strictly after `from`, or the end of
    /// the text if `from` is at or past the last boundary.
    ///
    /// Caret-forward movement: the result is always a legal caret stop, so a
    /// caret can never advance into a cluster interior. `from` need not itself be
    /// a boundary; the next boundary strictly greater than it is returned.
    pub fn next_grapheme(&self, from: TextOffset) -> TextOffset {
        self.grapheme_boundaries()
            .find(|&b| b > from)
            .unwrap_or(TextOffset(self.text.len()))
    }

    /// The previous grapheme-cluster boundary strictly before `from`, or `0` if
    /// `from` is at or before the first boundary.
    ///
    /// Caret-backward movement, the mirror of [`Self::next_grapheme`].
    pub fn prev_grapheme(&self, from: TextOffset) -> TextOffset {
        self.grapheme_boundaries()
            .take_while(|&b| b < from)
            .last()
            .unwrap_or(TextOffset(0))
    }

    /// The nearest grapheme-cluster boundary at or before `from`, as a
    /// checkpoint an incremental re-segmentation can resume from.
    ///
    /// An edit lands at an arbitrary offset; this snaps back to the last sound
    /// resume point so the incremental path never resumes mid-cluster.
    pub fn checkpoint_at_or_before(&self, from: TextOffset) -> GraphemeCheckpoint {
        let boundary = self
            .grapheme_boundaries()
            .take_while(|&b| b <= from)
            .last()
            .unwrap_or(TextOffset(0));
        GraphemeCheckpoint(boundary)
    }

    /// Grapheme boundaries from a checkpoint onward, as *absolute* offsets into
    /// the original text.
    ///
    /// This is the incremental entry point: re-segment only the tail after an
    /// edit, from a confirmed boundary. Because a grapheme boundary is a hard
    /// reset for the segmentation state machine, the offsets this yields are
    /// byte-identical to the corresponding tail of [`Self::grapheme_boundaries`]
    /// over the whole text — the equivalence the incremental path depends on.
    pub fn grapheme_boundaries_from(
        &self,
        checkpoint: GraphemeCheckpoint,
    ) -> impl Iterator<Item = TextOffset> + '_ {
        let base = checkpoint.0.0.min(self.text.len());
        let tail = &self.text[base..];
        std::iter::once(TextOffset(base)).chain(
            tail.grapheme_indices(true)
                .map(move |(offset, cluster)| TextOffset(base + offset + cluster.len())),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse one UAX#29 test line into its source text and the set of break
    /// offsets it asserts. The format is space-separated tokens: `÷` (U+00F7)
    /// marks a break opportunity, `×` (U+00D7) marks a non-break, and every
    /// other token is a hex code point. A trailing `#` begins a comment.
    fn parse_corpus_line(line: &str) -> Option<(String, Vec<TextOffset>)> {
        let data = line.split('#').next().unwrap_or("").trim();
        if data.is_empty() {
            return None;
        }
        let mut text = String::new();
        let mut breaks = Vec::new();
        for token in data.split_whitespace() {
            match token {
                "\u{00F7}" => breaks.push(TextOffset(text.len())),
                "\u{00D7}" => {}
                hex => {
                    let cp = u32::from_str_radix(hex, 16).ok()?;
                    text.push(char::from_u32(cp)?);
                }
            }
        }
        Some((text, breaks))
    }

    const GRAPHEME_CORPUS: &str = include_str!("../tests/fixtures/GraphemeBreakTest.txt");
    const WORD_CORPUS: &str = include_str!("../tests/fixtures/WordBreakTest.txt");

    #[test]
    fn grapheme_boundaries_match_uax29_corpus() {
        // Every sample in the official Unicode 17.0.0 GraphemeBreakTest must
        // reproduce exactly: extended grapheme clusters, no divergence.
        let mut cases = 0;
        for line in GRAPHEME_CORPUS.lines() {
            let Some((text, expected)) = parse_corpus_line(line) else {
                continue;
            };
            cases += 1;
            let got: Vec<TextOffset> = Segmenter::new(&text).grapheme_boundaries().collect();
            assert_eq!(
                got,
                expected,
                "grapheme boundaries diverge for {:?}",
                text.chars().map(|c| c as u32).collect::<Vec<_>>()
            );
        }
        assert!(cases > 700, "corpus should carry the full sample set");
    }

    #[test]
    fn word_boundaries_match_uax29_corpus() {
        // Every sample in the official Unicode 17.0.0 WordBreakTest must
        // reproduce exactly: UAX#29 word boundaries, the cross-language base.
        let mut cases = 0;
        for line in WORD_CORPUS.lines() {
            let Some((text, expected)) = parse_corpus_line(line) else {
                continue;
            };
            cases += 1;
            let got: Vec<TextOffset> = Segmenter::new(&text).word_boundaries().collect();
            assert_eq!(
                got,
                expected,
                "word boundaries diverge for {:?}",
                text.chars().map(|c| c as u32).collect::<Vec<_>>()
            );
        }
        assert!(cases > 1800, "corpus should carry the full sample set");
    }

    #[test]
    fn incremental_from_any_checkpoint_equals_full_pass() {
        // The incremental contract: for every corpus sample and every confirmed
        // boundary in it, resuming segmentation at that checkpoint yields exactly
        // the tail of the full pass. This proves the checkpoint is a sound resume
        // point rather than a fixed-character-window heuristic.
        for line in GRAPHEME_CORPUS.lines() {
            let Some((text, _)) = parse_corpus_line(line) else {
                continue;
            };
            let seg = Segmenter::new(&text);
            let full: Vec<TextOffset> = seg.grapheme_boundaries().collect();
            for &checkpoint in &full {
                let incremental: Vec<TextOffset> = seg
                    .grapheme_boundaries_from(GraphemeCheckpoint(checkpoint))
                    .collect();
                let tail: Vec<TextOffset> =
                    full.iter().copied().filter(|&b| b >= checkpoint).collect();
                assert_eq!(
                    incremental, tail,
                    "resuming at {checkpoint:?} must equal the full-pass tail for {text:?}"
                );
            }
        }
    }

    #[test]
    fn checkpoint_snaps_back_to_a_cluster_boundary() {
        // "e" + ("a" + combining acute U+0301): a single cluster of 3 bytes after
        // the "e". A checkpoint requested mid-cluster snaps back to the cluster's
        // start, never into its interior.
        let text = "e\u{0061}\u{0301}";
        let seg = Segmenter::new(text);
        // Boundaries are 0, 1 (after "e"), 4 (after the combined cluster).
        let boundaries: Vec<TextOffset> = seg.grapheme_boundaries().collect();
        assert_eq!(
            boundaries,
            vec![TextOffset(0), TextOffset(1), TextOffset(4)]
        );
        // An offset of 2 sits inside the second cluster; snap back to 1.
        assert_eq!(
            seg.checkpoint_at_or_before(TextOffset(2)),
            GraphemeCheckpoint(TextOffset(1))
        );
        // An offset exactly on a boundary keeps that boundary.
        assert_eq!(
            seg.checkpoint_at_or_before(TextOffset(4)),
            GraphemeCheckpoint(TextOffset(4))
        );
    }

    #[test]
    fn caret_navigation_never_enters_a_cluster_interior() {
        // 👩‍💻 (woman technologist) is a single ZWJ emoji cluster; forward and
        // backward navigation jump it whole, never stopping inside.
        let text = "A\u{1F469}\u{200D}\u{1F4BB}B";
        let seg = Segmenter::new(text);
        let a_end = TextOffset(1);
        let emoji_end = TextOffset("A\u{1F469}\u{200D}\u{1F4BB}".len());

        // Forward from just after "A" skips the whole emoji cluster.
        assert_eq!(seg.next_grapheme(a_end), emoji_end);
        // From inside the emoji cluster's bytes, forward still lands on its end.
        assert_eq!(seg.next_grapheme(TextOffset(3)), emoji_end);
        // Backward from the emoji's end lands on "A"'s end, not inside it.
        assert_eq!(seg.prev_grapheme(emoji_end), a_end);
    }

    #[test]
    fn empty_text_has_a_single_zero_boundary() {
        let seg = Segmenter::new("");
        assert_eq!(
            seg.grapheme_boundaries().collect::<Vec<_>>(),
            vec![TextOffset(0)]
        );
        assert_eq!(
            seg.word_boundaries().collect::<Vec<_>>(),
            vec![TextOffset(0)]
        );
        assert_eq!(seg.next_grapheme(TextOffset(0)), TextOffset(0));
        assert_eq!(seg.prev_grapheme(TextOffset(0)), TextOffset(0));
    }
}
