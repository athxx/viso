//! Bidirectional text (UAX#9): resolve per-character embedding levels for a
//! paragraph, split logical direction runs for shaping, and reorder a *line*
//! from logical to visual order at display time.
//!
//! The UAX#9 algorithm — explicit embeddings and overrides, the isolate
//! initiators [`LRI`/`RLI`/`FSI`/`PDI`], paired-bracket resolution, neutral and
//! number handling — is standards-heavy, conformance-tested Unicode data, so
//! this module *wraps* `unicode-bidi` rather than reimplementing the rules (the
//! Ownership Ladder: prefer a proven algorithm for a Unicode primitive). What
//! this module owns is the typing — levels and offsets are typed
//! ([`BidiLevel`], [`TextOffset`]), base direction is an explicit enum rather
//! than a UI-locale guess — and the contract with the layers on either side.
//!
//! [`LRI`/`RLI`/`FSI`/`PDI`]: BaseDirection
//!
//! # Base direction is explicit, never guessed from the UI locale
//!
//! Each paragraph resolves under one [`BaseDirection`]: [`LeftToRight`],
//! [`RightToLeft`], or [`Auto`]. `Auto` is UAX#9 first-strong (rules P2/P3) —
//! the first strong directional character decides — *not* "the UI locale is
//! Arabic, so force RTL". Paragraph separators split the text into paragraphs,
//! each resolving its own base direction independently.
//!
//! [`LeftToRight`]: BaseDirection::LeftToRight
//! [`RightToLeft`]: BaseDirection::RightToLeft
//! [`Auto`]: BaseDirection::Auto
//!
//! # The boundary with shaping, and with line layout
//!
//! Shaping consumes *logical-order* direction runs: [`Self::direction_runs`]
//! yields runs keyed to source [`TextOffset`]s with their resolved level and
//! direction, and the source is never reversed before shaping — an RTL run is
//! handed to the shaper as-is with [`Direction::RightToLeft`], and the shaper
//! (not this layer) produces glyphs in visual order. Control characters
//! (embeddings, overrides, isolates) participate in resolution and stay in the
//! source; they are never deleted, so logical offsets remain stable for
//! copy/paste, IME, and the cluster mapping.
//!
//! Visual reordering is a *line* concern, applied only after line formation:
//! [`Self::visual_order`] reorders one line's characters (UAX#9 rule L2) on
//! demand. This module never eagerly reorders the whole paragraph, because a
//! paragraph spans many lines and each line reorders independently.

use unicode_bidi::{BidiInfo as UnicodeBidiInfo, Level, ParagraphInfo};

use crate::shaping::Direction;
use crate::text_position::TextOffset;

/// The base direction a paragraph resolves under.
///
/// This is a deliberate input, not an inference: an editor or layout box states
/// the base direction it wants, and `Auto` defers to the text's own first
/// strong character (UAX#9 P2/P3) rather than to any ambient UI locale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseDirection {
    /// First-strong: the first strong directional character in each paragraph
    /// sets its base level; a paragraph with no strong character is LTR.
    Auto,
    /// Force left-to-right base level for every paragraph.
    LeftToRight,
    /// Force right-to-left base level for every paragraph.
    RightToLeft,
}

impl BaseDirection {
    /// The `default_para_level` this base direction hands to `unicode-bidi`:
    /// `None` requests first-strong auto-detection, `Some(level)` forces it.
    fn default_level(self) -> Option<Level> {
        match self {
            BaseDirection::Auto => None,
            BaseDirection::LeftToRight => Some(Level::ltr()),
            BaseDirection::RightToLeft => Some(Level::rtl()),
        }
    }
}

/// A resolved UAX#9 embedding level. Even levels are left-to-right, odd levels
/// right-to-left; the value is the nesting depth of directional context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BidiLevel(pub u8);

impl BidiLevel {
    /// Whether this level is right-to-left (odd).
    pub fn is_rtl(self) -> bool {
        self.0 % 2 == 1
    }

    /// The run direction this level implies.
    pub fn direction(self) -> Direction {
        if self.is_rtl() {
            Direction::RightToLeft
        } else {
            Direction::LeftToRight
        }
    }
}

/// One paragraph within the resolved text: its byte range and its base level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Paragraph {
    /// The paragraph's byte range in the source, `[start, end)`.
    pub start: TextOffset,
    pub end: TextOffset,
    /// The resolved base embedding level of the paragraph.
    pub base_level: BidiLevel,
}

/// A maximal logical-order run of one embedding level: the unit shaping splits
/// on. The range is a slice of the source in logical order — never reversed —
/// and `direction` tells the shaper which way to shape it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectionRun {
    /// The run's byte range in the source, `[start, end)`, in logical order.
    pub start: TextOffset,
    pub end: TextOffset,
    /// The run's resolved embedding level.
    pub level: BidiLevel,
    /// The direction the run shapes in, derived from `level`.
    pub direction: Direction,
}

/// Resolved bidirectional levels for a run of paragraph source text.
///
/// Holds the per-byte embedding levels and paragraph boundaries `unicode-bidi`
/// resolved, keyed back to typed [`TextOffset`]s. Cheap to query; visual
/// reordering is deferred to [`Self::visual_order`] per line.
#[derive(Debug)]
pub struct BidiInfo {
    /// Owned source, so levels/paragraphs outlive the caller's borrow.
    text: String,
    /// Per-byte resolved embedding levels *before* rule L1, one per byte of
    /// `text`. L1 (resetting separators and trailing whitespace to the paragraph
    /// level) is a per-line rule, applied on demand in line queries.
    levels: Vec<Level>,
    /// Per-byte bidi classes, retained so rule L1 can be applied per line.
    original_classes: Vec<unicode_bidi::BidiClass>,
    /// Paragraph boundaries and base levels, in logical order.
    paragraphs: Vec<Paragraph>,
    /// Whether any character resolved to an RTL level. When false, the text is
    /// pure LTR and both shaping and line reorder can take the trivial path.
    has_rtl: bool,
}

impl BidiInfo {
    /// Resolve embedding levels for `text` under `base`.
    ///
    /// Splits into paragraphs on paragraph separators, resolves each paragraph's
    /// base direction (first-strong for [`BaseDirection::Auto`]) and every
    /// character's level, and retains the result for level and reorder queries.
    pub fn resolve(text: &str, base: BaseDirection) -> Self {
        let info = UnicodeBidiInfo::new(text, base.default_level());
        let has_rtl = info.has_rtl();
        let mut paragraphs: Vec<Paragraph> = info
            .paragraphs
            .iter()
            .map(|p| Paragraph {
                start: TextOffset(p.range.start),
                end: TextOffset(p.range.end),
                base_level: BidiLevel(p.level.number()),
            })
            .collect();
        // Empty text still presents as one empty paragraph: a caret must have a
        // paragraph to sit in. `unicode-bidi` yields none, so synthesize it under
        // the requested base (LTR for auto, per P3).
        if paragraphs.is_empty() {
            let base_level = base.default_level().unwrap_or_else(Level::ltr);
            paragraphs.push(Paragraph {
                start: TextOffset(0),
                end: TextOffset(0),
                base_level: BidiLevel(base_level.number()),
            });
        }
        Self {
            text: text.to_owned(),
            levels: info.levels,
            original_classes: info.original_classes,
            paragraphs,
            has_rtl,
        }
    }

    /// A borrowed `unicode-bidi` view over the owned parts, for calling its
    /// per-line facilities (rule L1 / L2). Cheap: clones only the level and
    /// class vectors the crate's methods need to own.
    fn view(&self) -> UnicodeBidiInfo<'_> {
        UnicodeBidiInfo {
            text: &self.text,
            original_classes: self.original_classes.clone(),
            levels: self.levels.clone(),
            paragraphs: self
                .paragraphs
                .iter()
                .map(|p| ParagraphInfo {
                    range: p.start.0..p.end.0,
                    level: Level::new(p.base_level.0).unwrap_or(Level::ltr()),
                })
                .collect(),
        }
    }

    /// The paragraph containing `[start, end)` — the paragraph a line belongs
    /// to. A line never spans paragraphs, so the paragraph of `start` governs.
    fn paragraph_of(&self, start: TextOffset) -> Option<&Paragraph> {
        self.paragraphs
            .iter()
            .find(|p| start >= p.start && start < p.end)
            .or_else(|| self.paragraphs.last())
    }

    /// The source text these levels were resolved over.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The paragraphs, in logical order, each with its base direction.
    pub fn paragraphs(&self) -> &[Paragraph] {
        &self.paragraphs
    }

    /// Whether any character resolved to a right-to-left level.
    ///
    /// When this is false the whole text is left-to-right and the reorder step
    /// is the identity — the fast path shaping and line layout take.
    pub fn has_rtl(&self) -> bool {
        self.has_rtl
    }

    /// The line levels of `[start, end)` with UAX#9 rule L1 applied — separators
    /// and trailing whitespace reset to the paragraph level. Returns the full
    /// per-byte level vector, adjusted within the line, matching the crate's
    /// `reordered_levels` contract.
    fn line_levels(&self, start: TextOffset, end: TextOffset) -> Vec<Level> {
        let lo = start.0.min(self.levels.len());
        let hi = end.0.min(self.levels.len());
        let view = self.view();
        let para = self
            .paragraph_of(start)
            .map(|p| ParagraphInfo {
                range: p.start.0..p.end.0,
                level: Level::new(p.base_level.0).unwrap_or(Level::ltr()),
            })
            .unwrap_or(ParagraphInfo {
                range: 0..self.text.len(),
                level: Level::ltr(),
            });
        view.reordered_levels(&para, lo..hi)
    }

    /// The resolved embedding level of the byte at `offset`, with rule L1 applied
    /// over the offset's paragraph (treated as a single line, as line layout will
    /// re-apply L1 per line). Falls back to the trailing context level at the end
    /// of the text.
    pub fn level_at(&self, offset: TextOffset) -> BidiLevel {
        let Some(para) = self.paragraph_of(offset) else {
            return BidiLevel(0);
        };
        let (start, end) = (para.start, para.end);
        let levels = self.line_levels(start, end);
        let idx = offset.0.min(levels.len().saturating_sub(1));
        BidiLevel(levels.get(idx).copied().unwrap_or_else(Level::ltr).number())
    }

    /// The maximal logical-order direction runs the shaping stage splits on,
    /// across the whole text.
    ///
    /// A run is a maximal span of equal embedding level; runs are emitted in
    /// *logical* order with their source byte range intact, so the shaper
    /// receives an RTL run unreversed and shapes it into visual order itself.
    pub fn direction_runs(&self) -> Vec<DirectionRun> {
        self.direction_runs_in(TextOffset(0), TextOffset(self.text.len()))
    }

    /// The logical-order direction runs within one byte range (typically a
    /// line): maximal spans of equal embedding level, clipped to `[start, end)`.
    pub fn direction_runs_in(&self, start: TextOffset, end: TextOffset) -> Vec<DirectionRun> {
        let lo = start.0.min(self.levels.len());
        let hi = end.0.min(self.levels.len());
        let mut runs = Vec::new();
        let mut i = lo;
        while i < hi {
            let level = self.levels[i];
            let run_start = i;
            i += 1;
            while i < hi && self.levels[i] == level {
                i += 1;
            }
            let level = BidiLevel(level.number());
            runs.push(DirectionRun {
                start: TextOffset(run_start),
                end: TextOffset(i),
                level,
                direction: level.direction(),
            });
        }
        runs
    }

    /// The visual left-to-right order of the byte offsets in one line, applying
    /// UAX#9 rule L2.
    ///
    /// `line` is a byte range within the resolved text corresponding to one
    /// laid-out line; the result lists each character's logical byte offset in
    /// the order it is drawn left to right. Reordering is per line because a
    /// paragraph's lines each reorder independently — this is never applied to a
    /// whole paragraph eagerly.
    pub fn visual_order(&self, start: TextOffset, end: TextOffset) -> Vec<TextOffset> {
        let lo = start.0.min(self.text.len());
        let hi = end.0.min(self.text.len());
        if lo >= hi {
            return Vec::new();
        }
        // Apply rule L1 to this line (resetting separators and trailing
        // whitespace to the paragraph level), then rule L2 to reorder. The
        // corpus reorder is post-L1, and line layout applies L1 per line — so
        // this is exactly the per-line reorder line formation will invoke.
        let levels = self.line_levels(TextOffset(lo), TextOffset(hi));
        // reorder_visual maps visual position -> level index within the slice;
        // rebase those slice-relative indices back to absolute byte offsets.
        let visual = UnicodeBidiInfo::reorder_visual(&levels[lo..hi]);
        visual.into_iter().map(|rel| TextOffset(lo + rel)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One parsed `BidiCharacterTest.txt` case: the source text, the base
    /// direction input, the expected resolved paragraph level, the expected
    /// per-character levels (`None` for characters removed under rule X9), and
    /// the expected visual order (logical indices of non-removed characters,
    /// left to right).
    struct CharCase {
        text: String,
        base: BaseDirection,
        paragraph_level: u8,
        char_levels: Vec<Option<u8>>,
        visual_order: Vec<usize>,
        /// Byte offset of each character's start, parallel to `char_levels`.
        char_byte_starts: Vec<usize>,
    }

    /// Parse one `BidiCharacterTest.txt` line into a case, or `None` for a
    /// comment or blank line. The five `;`-separated fields are: hex code
    /// points, base-direction input (0=LTR, 1=RTL, 2=auto), resolved paragraph
    /// level, per-character resolved levels (`x` = removed under X9), and the
    /// visual reorder order (logical indices, removed characters skipped).
    fn parse_char_case(line: &str) -> Option<CharCase> {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            return None;
        }
        let mut fields = line.split(';');
        let cps = fields.next()?;
        let base = fields.next()?.trim();
        let para_level = fields.next()?.trim();
        let levels = fields.next()?.trim();
        let order = fields.next()?.trim();

        let mut text = String::new();
        let mut char_byte_starts = Vec::new();
        for hex in cps.split_whitespace() {
            let cp = u32::from_str_radix(hex, 16).ok()?;
            char_byte_starts.push(text.len());
            text.push(char::from_u32(cp)?);
        }
        let base = match base {
            "0" => BaseDirection::LeftToRight,
            "1" => BaseDirection::RightToLeft,
            "2" => BaseDirection::Auto,
            _ => return None,
        };
        let paragraph_level = para_level.parse().ok()?;
        let char_levels = levels
            .split_whitespace()
            .map(|t| {
                if t == "x" {
                    // 'x' = removed under rule X9: present but level-less.
                    Some(None)
                } else {
                    t.parse::<u8>().ok().map(Some)
                }
            })
            .collect::<Option<Vec<Option<u8>>>>()?;
        let visual_order = order
            .split_whitespace()
            .map(|t| t.parse().ok())
            .collect::<Option<Vec<usize>>>()?;
        Some(CharCase {
            text,
            base,
            paragraph_level,
            char_levels,
            visual_order,
            char_byte_starts,
        })
    }

    const CHAR_CORPUS: &str = include_str!("../tests/fixtures/BidiCharacterTest.txt");

    #[test]
    fn resolves_paragraph_level_and_char_levels_against_uax9_corpus() {
        // Every single-paragraph case in the official Unicode 16.0.0
        // BidiCharacterTest must reproduce exactly: the resolved paragraph level
        // and every non-removed character's embedding level. Characters removed
        // under rule X9 (marked 'x') carry no independent level in the corpus, so
        // they are not level-checked here; the reorder test covers them.
        let mut cases = 0;
        for line in CHAR_CORPUS.lines() {
            let Some(case) = parse_char_case(line) else {
                continue;
            };
            cases += 1;
            let info = BidiInfo::resolve(&case.text, case.base);
            // Each corpus case is exactly one paragraph.
            assert_eq!(
                info.paragraphs().len(),
                1,
                "corpus case is a single paragraph: {:?}",
                case.text
            );
            assert_eq!(
                info.paragraphs()[0].base_level.0,
                case.paragraph_level,
                "paragraph level diverges for {:?}",
                case.text
            );
            for (i, expected) in case.char_levels.iter().enumerate() {
                let Some(expected) = expected else {
                    continue;
                };
                let got = info.level_at(TextOffset(case.char_byte_starts[i])).0;
                assert_eq!(
                    got, *expected,
                    "level at char {i} diverges for {:?}",
                    case.text
                );
            }
        }
        assert!(cases > 90_000, "corpus should carry the full sample set");
    }

    #[test]
    fn visual_reorder_matches_uax9_corpus() {
        // Rule L2: the visual left-to-right order of the whole (single-line)
        // paragraph must match the corpus exactly, once removed (X9) characters
        // are dropped — the corpus reorder skips them.
        let mut cases = 0;
        for line in CHAR_CORPUS.lines() {
            let Some(case) = parse_char_case(line) else {
                continue;
            };
            cases += 1;
            let info = BidiInfo::resolve(&case.text, case.base);
            // Map the crate's byte-offset visual order to character indices,
            // dropping characters the corpus removed under X9.
            let removed: Vec<bool> = case.char_levels.iter().map(|l| l.is_none()).collect();
            let byte_to_char: std::collections::HashMap<usize, usize> = case
                .char_byte_starts
                .iter()
                .enumerate()
                .map(|(ci, &b)| (b, ci))
                .collect();
            let got: Vec<usize> = info
                .visual_order(TextOffset(0), TextOffset(case.text.len()))
                .into_iter()
                .filter_map(|off| byte_to_char.get(&off.0).copied())
                .filter(|&ci| !removed[ci])
                .collect();
            assert_eq!(
                got, case.visual_order,
                "visual order diverges for {:?}",
                case.text
            );
        }
        assert!(cases > 90_000, "corpus should carry the full sample set");
    }

    #[test]
    fn auto_base_direction_is_first_strong_not_locale() {
        // Auto = UAX#9 first-strong. Leading Latin -> LTR base even with RTL
        // content after it; leading Hebrew -> RTL base. Nothing consults a
        // locale.
        let ltr_first = BidiInfo::resolve("a\u{05D0}", BaseDirection::Auto);
        assert!(!ltr_first.paragraphs()[0].base_level.is_rtl());

        let rtl_first = BidiInfo::resolve("\u{05D0}a", BaseDirection::Auto);
        assert!(rtl_first.paragraphs()[0].base_level.is_rtl());

        // Forcing the base overrides first-strong.
        let forced = BidiInfo::resolve("a\u{05D0}", BaseDirection::RightToLeft);
        assert!(forced.paragraphs()[0].base_level.is_rtl());
    }

    #[test]
    fn direction_runs_keep_logical_order_and_split_on_level() {
        // "abc" + Hebrew "אבג" under LTR base: an even-level LTR run followed by
        // an odd-level RTL run, both in logical (source) order, source unreversed.
        let info = BidiInfo::resolve("abc\u{05D0}\u{05D1}\u{05D2}", BaseDirection::LeftToRight);
        let runs = info.direction_runs();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].start, TextOffset(0));
        assert_eq!(runs[0].direction, Direction::LeftToRight);
        assert!(!runs[0].level.is_rtl());
        assert_eq!(runs[1].direction, Direction::RightToLeft);
        assert!(runs[1].level.is_rtl());
        // Runs are contiguous and cover the text, in logical order.
        assert_eq!(runs[0].end, runs[1].start);
        assert_eq!(runs[1].end, TextOffset(info.text().len()));
    }

    #[test]
    fn control_characters_stay_in_source() {
        // An RLI...PDI isolate wraps LTR text. The control characters are not
        // deleted: logical offsets still span the whole source including them.
        let text = "a\u{2067}b\u{2069}c";
        let info = BidiInfo::resolve(text, BaseDirection::LeftToRight);
        assert_eq!(info.text().len(), text.len());
        // The reorder covers every non-removed offset; the source is intact.
        let order = info.visual_order(TextOffset(0), TextOffset(text.len()));
        assert!(!order.is_empty());
    }

    #[test]
    fn pure_ltr_reorder_is_identity() {
        let info = BidiInfo::resolve("hello world", BaseDirection::LeftToRight);
        assert!(!info.has_rtl());
        let order = info.visual_order(TextOffset(0), TextOffset(info.text().len()));
        let identity: Vec<TextOffset> = (0..info.text().len()).map(TextOffset).collect();
        assert_eq!(order, identity);
    }

    /// A representative code point for each UAX#9 bidi class the class-based
    /// `BidiTest.txt` corpus uses as input. Every choice is pinned to Unicode
    /// 16.0.0 (the version `unicode-bidi 0.3.18` implements) and verified by
    /// [`class_representatives_have_expected_bidi_class`], so a class-string test
    /// case can be materialized into real characters.
    fn class_representative(class: &str) -> Option<char> {
        Some(match class {
            "L" => '\u{0041}',
            "R" => '\u{05D0}',
            "AL" => '\u{0627}',
            "EN" => '\u{0030}',
            "ES" => '\u{002B}',
            "ET" => '\u{0024}',
            "AN" => '\u{0660}',
            "CS" => '\u{002C}',
            "NSM" => '\u{0300}',
            "BN" => '\u{200B}',
            "B" => '\u{2029}',
            "S" => '\u{0009}',
            "WS" => '\u{0020}',
            "ON" => '\u{0021}',
            "LRE" => '\u{202A}',
            "RLE" => '\u{202B}',
            "PDF" => '\u{202C}',
            "LRO" => '\u{202D}',
            "RLO" => '\u{202E}',
            "LRI" => '\u{2066}',
            "RLI" => '\u{2067}',
            "FSI" => '\u{2068}',
            "PDI" => '\u{2069}',
            _ => return None,
        })
    }

    #[test]
    fn class_representatives_have_expected_bidi_class() {
        // The class-based corpus test is only sound if each representative code
        // point actually carries the bidi class it stands for in this Unicode
        // version. Verify every mapping against `unicode-bidi`'s own table.
        use unicode_bidi::BidiClass::*;
        let expected: &[(&str, unicode_bidi::BidiClass)] = &[
            ("L", L),
            ("R", R),
            ("AL", AL),
            ("EN", EN),
            ("ES", ES),
            ("ET", ET),
            ("AN", AN),
            ("CS", CS),
            ("NSM", NSM),
            ("BN", BN),
            ("B", B),
            ("S", S),
            ("WS", WS),
            ("ON", ON),
            ("LRE", LRE),
            ("RLE", RLE),
            ("PDF", PDF),
            ("LRO", LRO),
            ("RLO", RLO),
            ("LRI", LRI),
            ("RLI", RLI),
            ("FSI", FSI),
            ("PDI", PDI),
        ];
        for (class, want) in expected {
            let c = class_representative(class).expect("known class");
            let s = c.to_string();
            let info = UnicodeBidiInfo::new(&s, Some(Level::ltr()));
            assert_eq!(
                info.original_classes[0], *want,
                "representative U+{:04X} for {class} has the wrong bidi class",
                c as u32
            );
        }
    }

    const CLASS_CORPUS: &str = include_str!("../tests/fixtures/BidiTest.txt");

    #[test]
    fn class_based_reorder_matches_uax9_corpus() {
        // The class-based BidiTest corpus: `@Levels`/`@Reorder` directives set the
        // expected per-input-item levels and visual order for the data lines that
        // follow, each of which lists bidi classes plus a bitset of paragraph
        // directions to test (1 = auto, 2 = LTR, 4 = RTL). Materialize each class
        // string into representative characters, resolve under every direction the
        // bitset selects, and check the resolved levels and the rule-L1+L2 reorder
        // exactly. Items with no assigned level (`x`) are skipped in both checks.
        let mut expected_levels: Vec<Option<u8>> = Vec::new();
        let mut expected_order: Vec<usize> = Vec::new();
        let mut cases = 0usize;
        for line in CLASS_CORPUS.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("@Levels:") {
                expected_levels = rest
                    .split_whitespace()
                    .map(|t| if t == "x" { None } else { t.parse().ok() })
                    .collect();
                continue;
            }
            if let Some(rest) = line.strip_prefix("@Reorder:") {
                expected_order = rest
                    .split_whitespace()
                    .filter_map(|t| t.parse().ok())
                    .collect();
                continue;
            }
            if line.starts_with('@') {
                continue;
            }
            let mut fields = line.split(';');
            let classes: Vec<&str> = fields.next().unwrap_or("").split_whitespace().collect();
            let bitset = u32::from_str_radix(fields.next().unwrap_or("").trim(), 16).unwrap_or(0);

            // Build the input string; track each item's byte start and whether it
            // is level-less (`x`) in the expected output.
            let mut text = String::new();
            let mut byte_starts = Vec::with_capacity(classes.len());
            for class in &classes {
                byte_starts.push(text.len());
                text.push(class_representative(class).expect("corpus uses known classes"));
            }
            let removed: Vec<bool> = expected_levels.iter().map(|l| l.is_none()).collect();
            let byte_to_item: std::collections::HashMap<usize, usize> = byte_starts
                .iter()
                .enumerate()
                .map(|(i, &b)| (b, i))
                .collect();

            for (bit, base) in [
                (1, BaseDirection::Auto),
                (2, BaseDirection::LeftToRight),
                (4, BaseDirection::RightToLeft),
            ] {
                if bitset & bit == 0 {
                    continue;
                }
                cases += 1;
                let info = BidiInfo::resolve(&text, base);
                for (i, expected) in expected_levels.iter().enumerate() {
                    let Some(expected) = expected else { continue };
                    let got = info.level_at(TextOffset(byte_starts[i])).0;
                    assert_eq!(
                        got, *expected,
                        "level of item {i} ({}) diverges for {classes:?} base {base:?}",
                        classes[i]
                    );
                }
                let got_order: Vec<usize> = info
                    .visual_order(TextOffset(0), TextOffset(text.len()))
                    .into_iter()
                    .filter_map(|off| byte_to_item.get(&off.0).copied())
                    .filter(|&i| !removed[i])
                    .collect();
                assert_eq!(
                    got_order, expected_order,
                    "reorder diverges for {classes:?} base {base:?}"
                );
            }
        }
        assert!(
            cases > 400_000,
            "class corpus should carry the full sample set"
        );
    }

    #[test]
    fn empty_text_resolves_to_one_empty_paragraph() {
        let info = BidiInfo::resolve("", BaseDirection::Auto);
        assert_eq!(info.paragraphs().len(), 1);
        assert!(info.visual_order(TextOffset(0), TextOffset(0)).is_empty());
        assert!(info.direction_runs().is_empty());
    }
}
