//! System-font resolution: turn a shaped run's uncovered characters into
//! on-demand fallback faces, fetched from the platform.
//!
//! viso-text is a pure algorithm layer — it never links a platform font API.
//! Instead it defines the [`SystemFontProvider`] seam: the facade supplies a
//! platform implementation (CoreText on macOS) that, given a role and a sample
//! string, hands back sfnt bytes for a face that covers it. This module owns the
//! *policy* around that seam, which is platform-independent and testable with a
//! mock provider:
//!
//!   - scan a shaped run for `.notdef` (id 0) glyphs the loaded chain could not
//!     render, and group them by the Unicode script of their source character
//!     (emoji are surfaced separately — they are `Script::Common` but want the
//!     color-emoji face, not a language font);
//!   - for each missing script/emoji not already attempted, query the provider
//!     with a representative sample, register the returned face, and append it to
//!     the fallback chain so a reshape resolves the run;
//!   - a **negative cache** ([`SystemFallback`]) records which (script) / emoji
//!     queries were already made — resolved or not — so a run full of a script
//!     the platform cannot cover does not re-query the OS every frame.
//!
//! This mirrors the fallback model the [`FontStore`](crate::FontStore) chain was
//! built for: `has_char` orders candidates, the shaper decides coverage, and the
//! provider extends the chain only when the shaper reports a genuine gap.

use std::collections::HashSet;

use unicode_script::{Script, UnicodeScript};

use crate::font::FontStore;
use crate::shape::ShapedGlyph;

/// The role a system-font query wants filled. The provider maps each to a
/// platform default (the UI font, a CJK-covering font, the color-emoji font);
/// per-glyph fallback additionally passes the exact uncovered `sample`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FontRole {
    /// The platform's default UI text font.
    Ui,
    /// A font covering CJK / a specific language's script.
    Cjk,
    /// The color-emoji font.
    Emoji,
}

/// A request for a system font, resolved by a [`SystemFontProvider`].
///
/// The provider asks the OS for a font matching `role` that can render `sample`
/// (the exact uncovered characters for per-glyph fallback, or a representative
/// character for a role). `lang` is an optional BCP-47-ish hint (`"zh"`, `"ja"`,
/// `"ko"`) that biases CJK resolution to the right regional face.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SystemFontQuery {
    pub role: FontRole,
    pub sample: String,
    pub lang: String,
}

/// The bytes of a resolved system font, plus the face index within them.
///
/// System CJK fonts are often TrueType Collections where the covering face is
/// not index 0; `index` is fed straight to the parser so the right subface loads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemFontResult {
    pub bytes: Vec<u8>,
    pub index: u32,
}

/// The platform seam: resolve a [`SystemFontQuery`] to font bytes, or `None` if
/// the platform has nothing covering it. Cold path — a `dyn` object queried only
/// when the shaper reports an uncovered script, so virtual dispatch is fine here
/// (see AGENTS.md section 7.2 / 42).
pub trait SystemFontProvider {
    fn load(&self, query: &SystemFontQuery) -> Option<SystemFontResult>;
}

/// Negative cache for system-font resolution: which scripts / emoji have already
/// been queried, so an uncoverable run does not re-ask the OS every frame.
///
/// Scripts are keyed alone (no weight/size — this layer resolves coverage, not
/// styling); emoji are a single flag since every emoji resolves to the one
/// color-emoji face. A query is marked attempted whether or not it resolved: a
/// script the platform genuinely cannot cover must not be retried forever.
#[derive(Debug, Default)]
pub struct SystemFallback {
    attempted_scripts: HashSet<Script>,
    attempted_emoji: bool,
}

impl SystemFallback {
    pub fn new() -> Self {
        Self::default()
    }

    /// Scan `shaped` for uncovered glyphs, resolve a system face for each missing
    /// script / emoji not already attempted via `provider`, and append each
    /// resolved face to `store`'s fallback chain. Returns `true` if the chain
    /// grew — the caller should reshape the run so the new faces resolve it.
    ///
    /// Every attempted script/emoji is marked in the negative cache regardless of
    /// outcome, so a subsequent run of the same uncovered script is a no-op.
    pub fn resolve_missing(
        &mut self,
        store: &mut FontStore,
        provider: &dyn SystemFontProvider,
        text: &str,
        shaped: &[ShapedGlyph],
    ) -> bool {
        let (scripts, missing_emoji) = collect_missing(text, shaped);
        let mut grew = false;

        for script in scripts {
            if !self.attempted_scripts.insert(script) {
                continue; // already tried this script — resolved or not.
            }
            let query = SystemFontQuery {
                role: FontRole::Cjk,
                sample: script_sample(script).to_string(),
                lang: script_lang(script).to_string(),
            };
            if let Some(result) = provider.load(&query)
                && let Some(id) = store.load(result.bytes, result.index)
            {
                store.push_fallback(id);
                grew = true;
            }
        }

        if missing_emoji && !self.attempted_emoji {
            self.attempted_emoji = true;
            let query = SystemFontQuery {
                role: FontRole::Emoji,
                sample: EMOJI_SAMPLE.to_string(),
                lang: String::new(),
            };
            if let Some(result) = provider.load(&query)
                && let Some(id) = store.load(result.bytes, result.index)
            {
                // A system emoji face arrives with its color strikes stripped
                // (~180 MB, dropped at load), so the table set cannot reveal it as
                // color; flag it so the glyph path routes it to the platform
                // color rasterizer instead of the SDF outline path.
                store.mark_color_emoji(id);
                store.push_fallback(id);
                grew = true;
            }
        }

        grew
    }
}

/// A representative emoji used to coax the platform into resolving its color
/// emoji face when a run has an uncovered emoji.
const EMOJI_SAMPLE: &str = "\u{1f600}"; // 😀

/// Scan shaped glyphs for `.notdef` (id 0) and return the de-duplicated scripts
/// of the characters that fell back, plus whether any of them were emoji.
///
/// `Common` / `Inherited` / `Unknown` are dropped from the script list — they
/// are whitespace, combining marks, and shared punctuation, which say nothing
/// about which language font to fetch. Emoji are also `Common` but are reported
/// separately via the `bool`, because they want the color-emoji face.
fn collect_missing(text: &str, shaped: &[ShapedGlyph]) -> (Vec<Script>, bool) {
    let mut scripts: Vec<Script> = Vec::new();
    let mut missing_emoji = false;
    for g in shaped {
        if g.id != 0 {
            continue;
        }
        let Some(rest) = text.get(g.cluster as usize..) else {
            continue;
        };
        let Some(ch) = rest.chars().next() else {
            continue;
        };
        if is_emoji(ch) {
            missing_emoji = true;
            continue;
        }
        let script = ch.script();
        if matches!(script, Script::Common | Script::Inherited | Script::Unknown) {
            continue;
        }
        if !scripts.contains(&script) {
            scripts.push(script);
        }
    }
    (scripts, missing_emoji)
}

/// Whether `ch` should be rendered by the color-emoji font. A conservative
/// superset of the emoji blocks: it only has to be right for characters that
/// already fell back to `.notdef`, and the OS does real coverage matching when we
/// then fetch the emoji font.
fn is_emoji(ch: char) -> bool {
    matches!(ch as u32,
        0x1F300..=0x1FAFF   // symbols & pictographs, emoticons, transport, extended-A
        | 0x1F000..=0x1F0FF // mahjong / dominoes / playing cards
        | 0x2600..=0x27BF   // misc symbols + dingbats
        | 0x2B00..=0x2BFF   // misc symbols and arrows (stars etc.)
        | 0x1F1E6..=0x1F1FF // regional indicator symbols (flags)
        | 0xFE00..=0xFE0F   // variation selectors (VS16 emoji presentation)
        | 0x2190..=0x21FF   // arrows (some emoji-presented)
        | 0x2300..=0x23FF   // misc technical (⌚ ⏰ etc.)
    )
}

/// A representative character the resolved face for `script` must cover. Used as
/// the provider's sample string when the per-glyph text is not carried through;
/// covers the scripts the multilingual scope targets and defaults to a Han
/// ideograph for the many CJK scripts.
fn script_sample(script: Script) -> &'static str {
    match script {
        Script::Han => "\u{4e2d}",        // 中
        Script::Hiragana => "\u{3042}",   // あ
        Script::Katakana => "\u{30a2}",   // ア
        Script::Hangul => "\u{ac00}",     // 가
        Script::Thai => "\u{0e01}",       // ก
        Script::Hebrew => "\u{05d0}",     // א
        Script::Arabic => "\u{0627}",     // ا
        Script::Devanagari => "\u{0905}", // अ
        _ => "\u{4e2d}",
    }
}

/// The BCP-47-ish language hint that biases CJK resolution for `script`. Empty
/// for scripts where no regional disambiguation is needed.
fn script_lang(script: Script) -> &'static str {
    match script {
        Script::Hiragana | Script::Katakana => "ja",
        Script::Hangul => "ko",
        Script::Han => "zh",
        _ => "",
    }
}
