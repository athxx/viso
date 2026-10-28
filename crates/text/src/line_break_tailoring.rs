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
//! # Locales
//!
//! The provider keys its tailoring on one `ja_zh` bit: `ja` and every `zh`
//! select its CJK tables, everything else the general ones. On top of that
//! Viso keeps one prohibition table per [`LineBreakLocale`], each written from
//! that writing system's own layout requirements:
//!
//! - `zh-Hans` and `zh-Hant` (CLReq): curly quotation marks open and close —
//!   `“` and `‘` never end a line, `”` and `’` never start one, and a line may
//!   break before an opening mark and after a closing one where an ideograph
//!   meets it. The interpunct `·` that joins a transliterated name never starts
//!   a line; Simplified typesetting also keeps it from ending one, so a name is
//!   never split at its dot.
//! - `ja` (JLReq): the same quotation-mark direction; small kana, the
//!   prolonged sound mark and the iteration marks follow the strictness level.
//! - `ko` (KLReq): Hangul breaks at spaces and between syllables on the general
//!   tables, and a closing quotation mark keeps the particle written after it
//!   (`“안녕”이라고`), so its quotes stay unresolved.
//!
//! The quotation-mark breaks apply at every level but [`LineBreakStrictness::Anywhere`]
//! and never under [`WordBreak::KeepAll`]; the prohibitions apply at `Strict`
//! and `Normal`, as `Loose` lets centered punctuation start a line.

use icu_locale_core::LanguageIdentifier;
use icu_locale_core::subtags::{Language, language, region, script};
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

/// The content locale whose line-breaking rules a paragraph follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LineBreakLocale {
    /// Any other locale, or none: the general rules.
    #[default]
    Other,
    /// Simplified Chinese.
    ZhHans,
    /// Traditional Chinese.
    ZhHant,
    /// Japanese.
    Ja,
    /// Korean.
    Ko,
}

impl LineBreakLocale {
    /// The locale a BCP-47 tag (`_` accepted for `-`) breaks its lines by. A
    /// `zh` tag is Traditional with the `Hant` script, or with no script in
    /// Taiwan, Hong Kong or Macau, and Simplified otherwise. A tag that does not
    /// parse is [`Self::Other`].
    pub fn of(tag: &str) -> Self {
        const JA: Language = language!("ja");
        const ZH: Language = language!("zh");
        const KO: Language = language!("ko");
        let parsed = if tag.contains('_') {
            LanguageIdentifier::try_from_str(&tag.replace('_', "-"))
        } else {
            LanguageIdentifier::try_from_str(tag)
        };
        let Ok(id) = parsed else {
            return Self::Other;
        };
        match id.language {
            JA => Self::Ja,
            KO => Self::Ko,
            ZH => {
                let traditional = match id.script {
                    Some(script) => script == script!("Hant"),
                    None => id.region.is_some_and(|r| {
                        r == region!("TW") || r == region!("HK") || r == region!("MO")
                    }),
                };
                if traditional {
                    Self::ZhHant
                } else {
                    Self::ZhHans
                }
            }
            _ => Self::Other,
        }
    }

    /// Whether the provider's CJK tables apply: `ja` and both Chinese.
    fn provider_cjk(self) -> bool {
        matches!(self, Self::ZhHans | Self::ZhHant | Self::Ja)
    }
}

/// The prohibitions and added breaks a resolved policy applies over the
/// provider's breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Kinsoku {
    /// Characters a line never starts with.
    pub no_start: &'static [char],
    /// Characters a line never ends with.
    pub no_end: &'static [char],
    /// Whether curly quotation marks open (`“` `‘`) and close (`”` `’`): a
    /// break is added before an opening mark and after a closing one where it
    /// meets an ideograph or CJK punctuation.
    pub quotes: bool,
}

impl Kinsoku {
    const NONE: Self = Self {
        no_start: &[],
        no_end: &[],
        quotes: false,
    };
}

/// A resolved paragraph line-break policy: locale, strictness, and word-break.
/// Immutable within a paragraph — resolve it once, then construct a
/// [`crate::line_break::LineBreaker`] from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LineBreakTailoring {
    /// CSS `line-break` strictness.
    pub strictness: LineBreakStrictness,
    /// CSS `word-break` option.
    pub word_break: WordBreak,
    /// The content locale whose rules apply.
    pub locale: LineBreakLocale,
}

impl LineBreakTailoring {
    /// The default Latin policy with no content locale: strictness `Strict`,
    /// word-break `Normal`, the general rules — exactly the base candidate set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Tailor for a BCP-47 locale tag, resolved by [`LineBreakLocale::of`].
    pub fn for_locale(locale: &str) -> Self {
        Self {
            locale: LineBreakLocale::of(locale),
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

    /// The prohibitions and added breaks this policy applies over the
    /// provider's.
    pub(crate) fn kinsoku(self) -> Kinsoku {
        /// The interpunct joining a transliterated name.
        const INTERPUNCT: &[char] = &['\u{00B7}'];
        if self.strictness == LineBreakStrictness::Anywhere {
            return Kinsoku::NONE;
        }
        let quotes = self.word_break != WordBreak::KeepAll;
        let prohibit = matches!(
            self.strictness,
            LineBreakStrictness::Strict | LineBreakStrictness::Normal
        );
        let (no_start, no_end): (&[char], &[char]) = match self.locale {
            LineBreakLocale::ZhHans if prohibit => (INTERPUNCT, INTERPUNCT),
            LineBreakLocale::ZhHant if prohibit => (INTERPUNCT, &[]),
            _ => (&[], &[]),
        };
        match self.locale {
            LineBreakLocale::ZhHans | LineBreakLocale::ZhHant | LineBreakLocale::Ja => Kinsoku {
                no_start,
                no_end,
                quotes,
            },
            LineBreakLocale::Ko | LineBreakLocale::Other => Kinsoku::NONE,
        }
    }

    /// Fold this policy into the provider's options. `content_locale` is set to
    /// a `'static` `ja` identifier for `ja` and both Chinese — the provider only
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
        options.content_locale = self.locale.provider_cjk().then_some(CJK_CONTENT_LOCALE);
        options
    }
}

/// The `ja` identifier for the CJK bucket. Any `ja`/`zh` identifier flips
/// the provider's `ja_zh` bit identically, so one constant serves every CJK
/// policy.
const CJK_CONTENT_LOCALE: &LanguageIdentifier = &icu_locale_core::langid!("ja");
