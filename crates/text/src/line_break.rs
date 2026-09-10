//! Line breaking (UAX#14): the mandatory and permitted break opportunities in a
//! paragraph, the base the width-driven line filling chooses from.
//!
//! This module produces the *candidate set*, not the final line layout. The
//! width fit, CJK line-start/line-end prohibition ("kinsoku"), and hyphenation
//! layer on top in [`crate::paragraph`] and [`crate::line_break_tailoring`];
//! shaping-safe boundary handling (never split an unsafe-to-break cluster) is
//! [`crate::shaping`]'s concern. UAX#14 is the base, not full typography.
//!
//! # Why wrap ICU4X rather than a pair table
//!
//! The UAX#14 pair table is proven, standards-heavy Unicode data, so this module
//! *wraps* a provider rather than reimplementing the rules (the Ownership
//! Ladder: prefer a proven algorithm for a Unicode primitive). It wraps
//! `icu_segmenter` specifically — not a smaller pure pair-table crate — for one
//! world-ready reason: the no-space scripts (Thai, Lao, Khmer, and CJK) have no
//! useful break opportunities in a pair table at all; they need dictionary or
//! LSTM segmentation. [`LineSegmenter::new_auto`] carries that segmentation, so
//! the candidate set is correct for those scripts from the base up, and the
//! locale-aware provider seam UAX#14 calls for is the provider itself rather
//! than a bolt-on.
//!
//! `icu_segmenter` gives break *positions*; it does not classify them as
//! mandatory or allowed on its stable surface. UAX#14 rule LB4/LB5 make a break
//! mandatory exactly when it follows a hard line break (BK/CR/LF/NL), and rule
//! LB3 makes the end of text a mandatory break. This module derives the class
//! from the `LineBreak` code-point property of the character ending each break,
//! which is deterministic regardless of which internal path (rule table, LSTM,
//! dictionary) produced the position — this is the classification ICU's own
//! documentation recommends.

use icu_properties::CodePointMapData;
use icu_properties::props::LineBreak;
use icu_segmenter::{LineSegmenter, LineSegmenterBorrowed};
// `LineSegmenter::new_auto` returns the borrowed, `'static` form backed by baked
// data; `LineSegmenter` itself is only the constructor namespace here.

use crate::line_break_tailoring::LineBreakTailoring;
use crate::text_position::TextOffset;

/// The class of a line-break opportunity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakOpportunity {
    /// A mandatory break: the line *must* end here (a hard newline, or the end
    /// of the paragraph). The width fit has no choice about it.
    Mandatory,
    /// A permitted break the width fit *may* choose when a line would otherwise
    /// overflow.
    Allowed,
}

/// The break-opportunity analyzer over paragraph text.
///
/// It owns the UAX#14 provider and the `LineBreak` property map, both of which
/// are built once and reused across every paragraph — construction touches
/// baked Unicode data and is not a per-frame cost. Text is borrowed per query,
/// so one analyzer serves an entire document.
pub struct LineBreaker {
    segmenter: LineSegmenterBorrowed<'static>,
    line_break: CodePointMapData<LineBreak>,
}

impl std::fmt::Debug for LineBreaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The provider and property map are opaque baked data; naming the type
        // is the useful part.
        f.debug_struct("LineBreaker").finish_non_exhaustive()
    }
}

impl Default for LineBreaker {
    fn default() -> Self {
        Self::new()
    }
}

impl LineBreaker {
    /// A world-ready analyzer with the default policy: the auto provider selects
    /// dictionary/LSTM segmentation for no-space scripts and the pair table
    /// elsewhere, with no locale tailoring and the strictest level.
    ///
    /// This is the base candidate set. Strictness levels and CJK tailoring are
    /// chosen through [`LineBreakTailoring`] and applied by
    /// [`Self::with_tailoring`]; the paragraph line-break policy must stay stable
    /// within one paragraph rather than varying per line, so the tailoring is
    /// resolved once and this analyzer holds it for the paragraph's whole extent.
    pub fn new() -> Self {
        Self::with_tailoring(LineBreakTailoring::default())
    }

    /// An analyzer for a resolved paragraph policy: the locale bucket,
    /// strictness, and word-break folded into the provider's tailoring. The
    /// candidate set and its mandatory/allowed classification are otherwise the
    /// same as [`Self::new`] — tailoring only adjusts which permitted breaks the
    /// provider offers, never the mandatory hard breaks.
    pub fn with_tailoring(tailoring: LineBreakTailoring) -> Self {
        Self {
            segmenter: LineSegmenter::new_auto(tailoring.to_icu_options()),
            line_break: CodePointMapData::<LineBreak>::new().static_to_owned(),
        }
    }

    /// Every break opportunity in `text`, in order, each with its class.
    ///
    /// Offsets are byte offsets into `text`, as [`TextOffset`]. The leading `0`
    /// (the start of the text, never a break) is not emitted; the trailing
    /// `text.len()` always is, classified [`BreakOpportunity::Mandatory`] per
    /// rule LB3. Empty text yields nothing.
    pub fn break_opportunities<'a>(
        &'a self,
        text: &'a str,
    ) -> impl Iterator<Item = (TextOffset, BreakOpportunity)> + 'a {
        let map = self.line_break.as_borrowed();
        self.segmenter
            .segment_str(text)
            // The provider emits a leading 0 for the text start; it is not a
            // break opportunity, so drop it.
            .filter(|&offset| offset != 0)
            .map(move |offset| {
                // A break at `offset` is mandatory when the character ending
                // there is a hard line break (UAX#14 LB4/LB5), or when it is the
                // end of the text (LB3).
                let ends_hard = text[..offset]
                    .chars()
                    .next_back()
                    .is_some_and(|c| is_mandatory_break_after(map.get(c)));
                let class = if ends_hard || offset == text.len() {
                    BreakOpportunity::Mandatory
                } else {
                    BreakOpportunity::Allowed
                };
                (TextOffset(offset), class)
            })
    }

    /// The first break opportunity strictly after `from`, with its class, or
    /// `None` if none remains (i.e. `from` is at or past the last break).
    ///
    /// `from` need not itself be a break offset. This is the forward-scan a
    /// width fit uses to find the next candidate after the current line start.
    pub fn next_break(
        &self,
        text: &str,
        from: TextOffset,
    ) -> Option<(TextOffset, BreakOpportunity)> {
        self.break_opportunities(text).find(|&(at, _)| at > from)
    }
}

/// Whether a break *following* a character of this `LineBreak` class is a
/// mandatory break under UAX#14 (rules LB4 and LB5).
///
/// `MandatoryBreak` (BK), `CarriageReturn` (CR), `LineFeed` (LF), and
/// `NextLine` (NL) force a break after them. A lone CR followed by LF does not
/// break between the two (LB5 keeps CR × LF), and the break after the LF is
/// caught here by the LF class, so CRLF is handled without a special case.
fn is_mandatory_break_after(class: LineBreak) -> bool {
    matches!(
        class,
        LineBreak::MandatoryBreak
            | LineBreak::CarriageReturn
            | LineBreak::LineFeed
            | LineBreak::NextLine
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse one UAX#14 `LineBreakTest.txt` line into its source text and the
    /// set of break offsets it asserts. The format mirrors the UAX#29 corpora:
    /// space-separated tokens where `÷` (U+00F7) marks a break opportunity, `×`
    /// (U+00D7) a non-break, and every other token is a hex code point. A
    /// leading `÷` at offset 0 and a trailing `÷` at the end are structural, not
    /// break opportunities we emit, so offset 0 is dropped when comparing.
    fn parse_corpus_line(line: &str) -> Option<(String, Vec<usize>)> {
        let data = line.split('#').next().unwrap_or("").trim();
        if data.is_empty() {
            return None;
        }
        let mut text = String::new();
        let mut breaks = Vec::new();
        for token in data.split_whitespace() {
            match token {
                "\u{00F7}" => breaks.push(text.len()),
                "\u{00D7}" => {}
                hex => {
                    let cp = u32::from_str_radix(hex, 16).ok()?;
                    text.push(char::from_u32(cp)?);
                }
            }
        }
        Some((text, breaks))
    }

    const LINE_BREAK_CORPUS: &str = include_str!("../tests/fixtures/LineBreakTest.txt");

    /// The reference example from the UAX#14 literature: `"a b \nc"` breaks
    /// after the first space (allowed), after the line feed (mandatory), and at
    /// the end (mandatory). The leading text start is not emitted.
    #[test]
    fn reference_example_classifies_mandatory_and_allowed() {
        let breaker = LineBreaker::new();
        let got: Vec<(TextOffset, BreakOpportunity)> =
            breaker.break_opportunities("a b \nc").collect();
        assert_eq!(
            got,
            vec![
                (TextOffset(2), BreakOpportunity::Allowed),
                (TextOffset(5), BreakOpportunity::Mandatory),
                (TextOffset(6), BreakOpportunity::Mandatory),
            ]
        );
    }

    /// A CRLF is one hard break, not two: no break lands between CR and LF, and
    /// the break after the pair is mandatory.
    #[test]
    fn crlf_is_a_single_mandatory_break() {
        let breaker = LineBreaker::new();
        let text = "ab\r\ncd";
        let got: Vec<(TextOffset, BreakOpportunity)> = breaker.break_opportunities(text).collect();
        // Break after "\r\n" (offset 4) is mandatory; end (offset 6) mandatory.
        // Crucially there is no break at offset 3 (between CR and LF).
        assert_eq!(
            got,
            vec![
                (TextOffset(4), BreakOpportunity::Mandatory),
                (TextOffset(6), BreakOpportunity::Mandatory),
            ]
        );
    }

    /// The end of any non-empty text is always a mandatory break (LB3), even
    /// with no hard newline in the text.
    #[test]
    fn end_of_text_is_always_mandatory() {
        let breaker = LineBreaker::new();
        let got: Vec<(TextOffset, BreakOpportunity)> =
            breaker.break_opportunities("word").collect();
        assert_eq!(got, vec![(TextOffset(4), BreakOpportunity::Mandatory)]);
    }

    /// Empty text has no break opportunities at all.
    #[test]
    fn empty_text_has_no_breaks() {
        let breaker = LineBreaker::new();
        assert_eq!(breaker.break_opportunities("").count(), 0);
    }

    /// A no-space script must still break: Thai has no spaces between words, so a
    /// pair table would offer only the end-of-text break. The auto provider's
    /// dictionary/LSTM segmentation finds interior word boundaries — this is the
    /// reason the heavier provider was chosen, so prove it holds.
    #[test]
    fn no_space_script_breaks_via_dictionary_segmentation() {
        let breaker = LineBreaker::new();
        // "ภาษาไทยง่ายนิดเดียว" — a run of Thai with no spaces.
        let text = "ภาษาไทยง่ายนิดเดียว";
        let breaks: Vec<(TextOffset, BreakOpportunity)> =
            breaker.break_opportunities(text).collect();
        // At minimum the end-of-text mandatory break, plus at least one interior
        // allowed break that only dictionary segmentation can find.
        let interior_allowed = breaks
            .iter()
            .filter(|(at, class)| *class == BreakOpportunity::Allowed && at.0 < text.len())
            .count();
        assert!(
            interior_allowed >= 1,
            "dictionary segmentation should find interior breaks in spaceless Thai, got {breaks:?}"
        );
        assert_eq!(
            breaks.last().map(|&(_, c)| c),
            Some(BreakOpportunity::Mandatory)
        );
    }

    /// `next_break` scans forward from an arbitrary offset to the next candidate.
    #[test]
    fn next_break_scans_forward_from_any_offset() {
        let breaker = LineBreaker::new();
        let text = "one two three";
        // From the very start, the first break is after "one " (offset 4).
        assert_eq!(
            breaker.next_break(text, TextOffset(0)),
            Some((TextOffset(4), BreakOpportunity::Allowed))
        );
        // From inside "two", the next break is after "two " (offset 8).
        assert_eq!(
            breaker.next_break(text, TextOffset(5)),
            Some((TextOffset(8), BreakOpportunity::Allowed))
        );
        // Past the last break there is nothing further.
        assert_eq!(breaker.next_break(text, TextOffset(text.len())), None);
    }

    /// Conformance against the official Unicode 15.1.0 `LineBreakTest.txt`.
    ///
    /// Two documented facts shape what "conformance" means here, and the test
    /// asserts the exact invariants they imply rather than a blanket equality:
    ///
    /// 1. The corpus is *not* the pure default algorithm. Its own header states
    ///    it applies "the tailoring of numbers described in Example 7 of Section
    ///    8.2 of UAX #14" — extra break opportunities around digits, currency,
    ///    percent, and brackets (e.g. `}%`, `,0`, `%(`, `a.2`). Our provider
    ///    runs the *un-tailored* default, so on those sequences the corpus
    ///    asserts breaks the provider does not: the provider's breaks are a
    ///    subset of the corpus's, never a superset.
    /// 2. For no-space scripts the auto provider runs dictionary/LSTM
    ///    segmentation, which *adds* interior breaks the pair table has no way
    ///    to find. There the direction flips: the provider may exceed the
    ///    corpus inside a complex-script run.
    ///
    /// So the invariants that must hold for every sample are:
    /// - **Mandatory classification is exact**: every break the provider marks
    ///   [`BreakOpportunity::Mandatory`] is a break the corpus asserts. Hard
    ///   line breaks are not subject to either tailoring, so this is absolute.
    /// - **No invented breaks outside dictionary scripts**: every provider break
    ///   is either asserted by the corpus or lands inside a complex-script run
    ///   (a dictionary interior break). The provider never breaks where the
    ///   un-tailored default forbids it.
    /// - **The bulk matches exactly**: only the ~34 number-tailoring/dictionary
    ///   samples may differ, so an exact-match floor guards against a silent
    ///   regression that would let the two drift apart wholesale.
    #[test]
    fn line_breaks_match_uax14_corpus() {
        let breaker = LineBreaker::new();
        let mut cases = 0;
        let mut exact = 0;
        for line in LINE_BREAK_CORPUS.lines() {
            let Some((text, expected)) = parse_corpus_line(line) else {
                continue;
            };
            if text.is_empty() {
                continue;
            }
            cases += 1;
            // Corpus breaks minus the structural leading 0.
            let expected: std::collections::HashSet<usize> =
                expected.into_iter().filter(|&b| b != 0).collect();
            let got: Vec<(usize, BreakOpportunity)> = breaker
                .break_opportunities(&text)
                .map(|(at, class)| (at.0, class))
                .collect();

            let got_offsets: std::collections::HashSet<usize> =
                got.iter().map(|&(at, _)| at).collect();
            if got_offsets == expected {
                exact += 1;
            }

            let cps = || text.chars().map(|c| c as u32).collect::<Vec<_>>();
            for &(at, class) in &got {
                // A mandatory break is never subject to number tailoring or
                // dictionary segmentation; the corpus must assert it exactly.
                if class == BreakOpportunity::Mandatory {
                    assert!(
                        expected.contains(&at),
                        "provider invented a mandatory break at {at} for {:?}",
                        cps()
                    );
                    continue;
                }
                // An allowed break the corpus does not assert is only legitimate
                // when it is a dictionary interior break: the break must sit
                // between two characters of a dictionary-segmented script.
                if !expected.contains(&at) {
                    assert!(
                        is_dictionary_interior(&text, at),
                        "provider invented an allowed break at {at} for {:?}",
                        cps()
                    );
                }
            }
        }
        assert!(
            cases > 4000,
            "corpus should carry the full sample set, got {cases}"
        );
        assert!(
            exact >= 10_000,
            "the vast majority of samples must match exactly; only number-tailoring \
             and dictionary samples may differ, got {exact} exact of {cases}"
        );
    }

    /// Whether a break at byte offset `at` sits between two characters of a
    /// dictionary-segmented, no-space script — the only place the auto provider
    /// may legitimately add an interior break the un-tailored pair table (and
    /// thus the corpus) does not.
    fn is_dictionary_interior(text: &str, at: usize) -> bool {
        let before = text[..at].chars().next_back();
        let after = text[at..].chars().next();
        matches!((before, after), (Some(b), Some(a)) if is_dictionary_segmented(b) && is_dictionary_segmented(a))
    }

    /// Characters the auto provider segments with a dictionary/LSTM rather than
    /// the pair table, where interior breaks legitimately diverge from the
    /// pair-table-only corpus expectation.
    fn is_dictionary_segmented(c: char) -> bool {
        let cp = c as u32;
        matches!(cp,
            0x0E00..=0x0E7F   // Thai
            | 0x0E80..=0x0EFF // Lao
            | 0x1780..=0x17FF // Khmer
            | 0x1000..=0x109F // Myanmar
            | 0x3040..=0x30FF // Hiragana/Katakana
            | 0x3400..=0x9FFF // CJK ideographs
            | 0xF900..=0xFAFF // CJK compat ideographs
        )
    }
}

#[cfg(test)]
mod tailoring_tests {
    use super::*;
    use crate::line_break_tailoring::{LineBreakStrictness, LineBreakTailoring, WordBreak};

    /// A CJK sample carrying the characters that make strictness observable: a
    /// small kana (ぁ), a prolonged sound mark (ー), CJK ideographs and CJK
    /// punctuation, plus a trailing Latin run.
    const CJK_SAMPLE: &str = "テストぁーテスト、漢字。 abc";

    fn break_offsets(tailoring: LineBreakTailoring, text: &str) -> Vec<usize> {
        LineBreaker::with_tailoring(tailoring)
            .break_opportunities(text)
            .map(|(at, _)| at.0)
            .collect()
    }

    /// The four locales the specification names map onto the provider's two
    /// buckets: `ja`/`zh-Hans`/`zh-Hant` flip the CJK bit, `ko` does not. The
    /// three CJK locales are indistinguishable at the provider (all `ja_zh`), so
    /// their break sets are identical; the API still lets a caller name each.
    #[test]
    fn locale_selects_cjk_bucket_ja_zh_but_not_ko() {
        for cjk_locale in ["ja", "zh-Hans", "zh-Hant"] {
            let t = LineBreakTailoring::for_locale(cjk_locale);
            assert!(
                t.to_icu_options().content_locale.is_some(),
                "{cjk_locale} should select the CJK bucket"
            );
        }
        assert!(
            LineBreakTailoring::for_locale("ko")
                .to_icu_options()
                .content_locale
                .is_none(),
            "ko has inter-word spaces and takes the non-CJK path"
        );
        assert!(
            LineBreakTailoring::for_locale("en")
                .to_icu_options()
                .content_locale
                .is_none()
        );
        // The three CJK locales collapse to the same provider table, so their
        // break sets coincide.
        let ja = break_offsets(LineBreakTailoring::for_locale("ja"), CJK_SAMPLE);
        let hans = break_offsets(LineBreakTailoring::for_locale("zh-Hans"), CJK_SAMPLE);
        let hant = break_offsets(LineBreakTailoring::for_locale("zh-Hant"), CJK_SAMPLE);
        assert_eq!(ja, hans);
        assert_eq!(ja, hant);
        assert!(!ja.is_empty());
    }

    /// Loosening strictness only ever *adds* break opportunities: the strict set
    /// is a subset of the loose set. On this sample the extra breaks sit exactly
    /// around the small kana (ぁ) and the prolonged sound mark (ー) — positions
    /// Strict forbids and Loose permits.
    #[test]
    fn loose_is_a_superset_of_strict() {
        let base = LineBreakTailoring::for_locale("ja");
        let strict: std::collections::HashSet<usize> = break_offsets(
            base.with_strictness(LineBreakStrictness::Strict),
            CJK_SAMPLE,
        )
        .into_iter()
        .collect();
        let loose: std::collections::HashSet<usize> =
            break_offsets(base.with_strictness(LineBreakStrictness::Loose), CJK_SAMPLE)
                .into_iter()
                .collect();
        assert!(
            strict.is_subset(&loose),
            "strict {strict:?} must be a subset of loose {loose:?}"
        );
        // The small kana and prolonged mark positions are the extra breaks.
        let extra: std::collections::BTreeSet<usize> = loose.difference(&strict).copied().collect();
        assert_eq!(
            extra,
            std::collections::BTreeSet::from([9, 12]),
            "loose adds breaks around the small kana and prolonged mark"
        );
    }

    /// The default policy for a non-CJK locale at strict level is the base
    /// candidate set: it must match `LineBreaker::new` offset for offset, so the
    /// tailoring layer does not regress the default path.
    #[test]
    fn strict_non_cjk_matches_base_candidate_set() {
        let text = "one two three\nfour";
        let base: Vec<usize> = LineBreaker::new()
            .break_opportunities(text)
            .map(|(at, _)| at.0)
            .collect();
        let tailored = break_offsets(
            LineBreakTailoring::for_locale("en").with_strictness(LineBreakStrictness::Strict),
            text,
        );
        assert_eq!(base, tailored);
    }

    /// A resolved policy is stable within the analyzer: repeated queries on the
    /// same text return identical break sets, so a paragraph's policy does not
    /// drift line to line.
    #[test]
    fn policy_is_stable_within_instance() {
        let breaker = LineBreaker::with_tailoring(
            LineBreakTailoring::for_locale("ja").with_strictness(LineBreakStrictness::Normal),
        );
        let first: Vec<_> = breaker.break_opportunities(CJK_SAMPLE).collect();
        let second: Vec<_> = breaker.break_opportunities(CJK_SAMPLE).collect();
        assert_eq!(first, second);
    }

    /// The no-space-script dictionary/LSTM seam survives locale tailoring: a
    /// Thai run still gains interior breaks under a Thai locale, under a CJK
    /// locale, and under the default policy. Tailoring adjusts CJK punctuation
    /// rules; it never turns off complex-script segmentation.
    #[test]
    fn no_space_script_breaks_under_any_locale() {
        let thai = "ภาษาไทยง่ายนิดเดียว";
        for tailoring in [
            LineBreakTailoring::for_locale("th"),
            LineBreakTailoring::for_locale("ja"),
            LineBreakTailoring::default(),
        ] {
            let breaker = LineBreaker::with_tailoring(tailoring);
            let interior_allowed = breaker
                .break_opportunities(thai)
                .filter(|(at, class)| *class == BreakOpportunity::Allowed && at.0 < thai.len())
                .count();
            assert!(
                interior_allowed >= 1,
                "dictionary segmentation must survive tailoring {tailoring:?}, got none"
            );
        }
    }

    /// `word-break` is observable both ways: `keep-all` suppresses the interior
    /// breaks between continuous CJK ideographs, and `break-all` adds interior
    /// breaks inside a Latin word.
    #[test]
    fn word_break_keep_all_and_break_all_are_observable() {
        let ideographs = "漢字漢字漢字";
        let interior = |wb: WordBreak| {
            break_offsets(
                LineBreakTailoring::for_locale("ja").with_word_break(wb),
                ideographs,
            )
            .into_iter()
            .filter(|&o| o < ideographs.len())
            .count()
        };
        assert_eq!(interior(WordBreak::Normal), 5);
        assert_eq!(interior(WordBreak::KeepAll), 0);

        let latin = "hello";
        let latin_interior = |wb: WordBreak| {
            break_offsets(
                LineBreakTailoring::for_locale("en").with_word_break(wb),
                latin,
            )
            .into_iter()
            .filter(|&o| o < latin.len())
            .count()
        };
        assert_eq!(latin_interior(WordBreak::Normal), 0);
        assert_eq!(latin_interior(WordBreak::BreakAll), 4);
    }
}
