//! Query hints for the resolvers that pick a face by family and coverage
//! rather than through a platform cascade.
//!
//! Fallback always asks for [`FontRole::Ui`] with the uncovered run as the
//! sample and a locale as the language, so the role a family table needs —
//! emoji, or a CJK face in the right regional style — is recovered from the
//! sample and language here, identically on every such target.

use viso_text::fallback::FontFallback;
use viso_text::{FontRole, SystemFontQuery};

use crate::text_worker::is_emoji;

/// Distinct sample scalars a coverage-driven query carries: enough to steer a
/// match towards a face for the whole run, bounded so a long run never builds
/// a large query.
pub const SAMPLE_SCALARS: usize = 64;

/// The regional style of a CJK face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CjkRegion {
    Japanese,
    Korean,
    TraditionalChinese,
    SimplifiedChinese,
}

/// The role `query` actually needs: an emoji or CJK sample promotes a UI or
/// CJK query to the face family that can draw it.
pub fn effective_role(query: &SystemFontQuery) -> FontRole {
    let first = query.sample.chars().next();
    if query.role == FontRole::Emoji || first.is_some_and(is_emoji) {
        return FontRole::Emoji;
    }
    if query.role == FontRole::Cjk || FontFallback::is_cjk(FontFallback::run_script(&query.sample))
    {
        return FontRole::Cjk;
    }
    query.role
}

/// The region a CJK query should resolve in: an explicit language wins, then
/// the sample's own scalars (kana is Japanese, hangul Korean), then Simplified
/// Chinese, the most widely installed default.
pub fn cjk_region(lang: &str, sample: &str) -> CjkRegion {
    if let Some(region) = region_of_lang(lang) {
        return region;
    }
    for ch in sample.chars() {
        match ch as u32 {
            0x3040..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F => return CjkRegion::Japanese,
            0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF => return CjkRegion::Korean,
            _ => {}
        }
    }
    CjkRegion::SimplifiedChinese
}

fn region_of_lang(lang: &str) -> Option<CjkRegion> {
    let lower = lang.to_ascii_lowercase().replace('_', "-");
    let mut subtags = lower.split('-');
    match subtags.next()? {
        "ja" => Some(CjkRegion::Japanese),
        "ko" => Some(CjkRegion::Korean),
        "zh" | "yue" => {
            let traditional = subtags.any(|tag| matches!(tag, "hant" | "tw" | "hk" | "mo"));
            Some(if traditional {
                CjkRegion::TraditionalChinese
            } else {
                CjkRegion::SimplifiedChinese
            })
        }
        _ => None,
    }
}

/// The first [`SAMPLE_SCALARS`] distinct scalars of `sample`, in order.
pub fn distinct_scalars(sample: &str) -> Vec<char> {
    let mut scalars = Vec::new();
    for ch in sample.chars() {
        if scalars.len() == SAMPLE_SCALARS {
            break;
        }
        if !scalars.contains(&ch) {
            scalars.push(ch);
        }
    }
    scalars
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_text::{FontSlant, FontWeight, FontWidth};

    fn query(role: FontRole, lang: &str, sample: &str) -> SystemFontQuery {
        SystemFontQuery {
            role,
            weight: FontWeight::REGULAR,
            width: FontWidth::NORMAL,
            slant: FontSlant::Normal,
            lang: lang.into(),
            sample: sample.into(),
        }
    }

    #[test]
    fn samples_promote_the_ui_role() {
        assert_eq!(
            effective_role(&query(FontRole::Ui, "", "😀 hi")),
            FontRole::Emoji
        );
        assert_eq!(
            effective_role(&query(FontRole::Ui, "", "漢字")),
            FontRole::Cjk
        );
        assert_eq!(
            effective_role(&query(FontRole::Ui, "", " かな")),
            FontRole::Cjk
        );
        assert_eq!(
            effective_role(&query(FontRole::Ui, "", "abc")),
            FontRole::Ui
        );
        assert_eq!(
            effective_role(&query(FontRole::Mono, "", "")),
            FontRole::Mono
        );
    }

    #[test]
    fn language_decides_the_region_before_the_sample() {
        assert_eq!(cjk_region("ja-JP", "漢"), CjkRegion::Japanese);
        assert_eq!(cjk_region("zh-Hant", "漢"), CjkRegion::TraditionalChinese);
        assert_eq!(cjk_region("zh_TW", "漢"), CjkRegion::TraditionalChinese);
        assert_eq!(cjk_region("zh-CN", "かな"), CjkRegion::SimplifiedChinese);
        assert_eq!(cjk_region("en", "漢かな"), CjkRegion::Japanese);
        assert_eq!(cjk_region("", "한국"), CjkRegion::Korean);
        assert_eq!(cjk_region("", "汉字"), CjkRegion::SimplifiedChinese);
    }

    #[test]
    fn distinct_scalars_keep_order_and_bound() {
        assert_eq!(distinct_scalars("abcab"), vec!['a', 'b', 'c']);
        let long: String = (0..200u32)
            .filter_map(|n| char::from_u32(0x4E00 + n))
            .collect();
        assert_eq!(distinct_scalars(&long).len(), SAMPLE_SCALARS);
    }
}
