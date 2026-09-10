//! Paragraph line-break policy: locale tailoring (CJK forbidden-position rules,
//! "kinsoku") and the CSS `line-break` / `word-break` axes.
//!
//! The base candidate set lives in [`crate::line_break`]; this module decides
//! *which tailoring* the UAX#14 provider applies on top of it — the locale
//! bucket, the strictness level, and the word-break option. A policy is chosen
//! once per paragraph and held stable for its whole extent (the line-break
//! policy must not vary line to line), so [`LineBreakTailoring`] is a small
//! immutable value the paragraph resolves and hands to
//! [`crate::line_break::LineBreaker::with_tailoring`].
//!
//! # What the provider can and cannot distinguish
//!
//! ICU4X keys its tailoring on `(ja_zh, strictness, word_option)`, and its
//! locale input collapses to a single `ja_zh` bit: only `ja` and `zh` (every
//! `zh` subtag, so both `zh-Hans` and `zh-Hant`) select the CJK tables;
//! everything else, `ko` included, uses the non-CJK path. This is not a gap to
//! paper over: Korean has inter-word spaces and breaks correctly under the
//! default algorithm plus strictness, so the same table is the right one. The
//! public API therefore accepts the four locales the specification names and
//! maps them honestly — `ja`/`zh`/`zh-*` to the CJK bucket, `ko` and the rest
//! to the Latin bucket — rather than pretending to four distinct tables.

use std::sync::OnceLock;

use icu_locale_core::LanguageIdentifier;
use icu_locale_core::subtags::{Language, language};
use icu_segmenter::options::{
    LineBreakOptions, LineBreakStrictness as IcuStrictness, LineBreakWordOption as IcuWordOption,
};

/// CSS `line-break` strictness, aligned with CSS Text 3 and the four levels the
/// provider distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineBreakStrictness {
    /// The most permissive: breaks are allowed around small kana, iteration
    /// marks, hyphen-like characters, and similar positions a stricter level
    /// would forbid.
    Loose,
    /// The common level: the default CJK behavior for most content.
    Normal,
    /// The strictest, and the base candidate set: the fullest set of
    /// forbidden positions. This is the default.
    #[default]
    Strict,
    /// Break at any grapheme-cluster boundary, approximating CSS
    /// `line-break: anywhere`. The provider degrades to grapheme segmentation.
    Anywhere,
}

/// CSS `word-break`: whether break opportunities are added *within* letters and
/// runs beyond what UAX#14 and strictness give.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WordBreak {
    /// The default: no extra breaks inside letters or words (UAX#14 plus
    /// strictness only).
    #[default]
    Normal,
    /// A break is allowed between any two characters, including inside Latin
    /// words (`break-all`).
    BreakAll,
    /// Runs of CJK characters are kept together, suppressing the interior
    /// breaks they would otherwise permit (`keep-all`).
    KeepAll,
}

/// A resolved paragraph line-break policy: locale bucket, strictness, and
/// word-break. Immutable within a paragraph — resolve it once, then construct a
/// [`crate::line_break::LineBreaker`] from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LineBreakTailoring {
    /// CSS `line-break` strictness.
    pub strictness: LineBreakStrictness,
    /// CSS `word-break` option.
    pub word_break: WordBreak,
    /// Whether the content locale is in the CJK bucket (`ja`/`zh`/`zh-*`). See
    /// the module docs for why `ko` is deliberately not in this bucket.
    cjk: bool,
}

impl LineBreakTailoring {
    /// The default Latin policy with no content locale: strictness `Strict`,
    /// word-break `Normal`, non-CJK — exactly the base candidate set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Tailor for a BCP-47 locale string. `ja` and `zh` (including `zh-Hans`
    /// and `zh-Hant`) select the CJK tables; `ko` and any unrecognized locale
    /// take the non-CJK path, where inter-word spaces plus strictness already
    /// break correctly. A locale that fails to parse is treated as non-CJK.
    pub fn for_locale(locale: &str) -> Self {
        let cjk = LanguageIdentifier::try_from_str(locale)
            .map(|id| is_cjk_language(id.language))
            .unwrap_or(false);
        Self {
            cjk,
            ..Self::default()
        }
    }

    /// The same policy with a different strictness.
    pub fn with_strictness(self, strictness: LineBreakStrictness) -> Self {
        Self { strictness, ..self }
    }

    /// The same policy with a different word-break option.
    pub fn with_word_break(self, word_break: WordBreak) -> Self {
        Self { word_break, ..self }
    }

    /// Fold this policy into the provider's options. `content_locale` is set to
    /// a `'static` `ja` identifier only in the CJK bucket — the provider only
    /// reads whether the language is `ja`/`zh`, so one shared identifier is
    /// enough to flip its `ja_zh` bit.
    pub(crate) fn to_icu_options(self) -> LineBreakOptions<'static> {
        let mut options = LineBreakOptions::default();
        options.strictness = Some(match self.strictness {
            LineBreakStrictness::Loose => IcuStrictness::Loose,
            LineBreakStrictness::Normal => IcuStrictness::Normal,
            LineBreakStrictness::Strict => IcuStrictness::Strict,
            LineBreakStrictness::Anywhere => IcuStrictness::Anywhere,
        });
        options.word_option = Some(match self.word_break {
            WordBreak::Normal => IcuWordOption::Normal,
            WordBreak::BreakAll => IcuWordOption::BreakAll,
            WordBreak::KeepAll => IcuWordOption::KeepAll,
        });
        options.content_locale = self.cjk.then(cjk_content_locale);
        options
    }
}

/// Whether a language subtag is in the CJK bucket. Matches the provider's own
/// test (`language ∈ {ja, zh}`), so the public locale choice and the internal
/// table selection never disagree.
fn is_cjk_language(language: Language) -> bool {
    const JA: Language = language!("ja");
    const ZH: Language = language!("zh");
    matches!(language, JA | ZH)
}

/// A shared `'static` `ja` identifier for the CJK bucket. Any `ja`/`zh`
/// identifier flips the provider's `ja_zh` bit identically, so one owned value
/// serves every CJK policy without leaking per-locale allocations.
fn cjk_content_locale() -> &'static LanguageIdentifier {
    static JA: OnceLock<LanguageIdentifier> = OnceLock::new();
    JA.get_or_init(|| LanguageIdentifier::from(language!("ja")))
}
