//! Android's font configuration (`/system/etc/fonts.xml`) and the order a
//! query tries its faces in.
//!
//! This is the resolver for devices older than `AFontMatcher` (API 29), kept
//! free of I/O so it runs on the host: [`FontConfig::parse`] reads the
//! document, [`FontConfig::candidates`] ranks its faces for a query, and the
//! caller keeps the first candidate whose `cmap` covers the sample.
//!
//! Families come in two kinds. A named family (`sans-serif`, `serif`, …) is
//! what a role asks for, possibly through an `<alias>`. Unnamed families form
//! the fallback chain in document order, each tagged with the languages and
//! scripts it serves (`und-Arab`, `zh-Hans`, `und-Zsye` for emoji). A query
//! tries its role's family, then the chain families whose language matches
//! the query's region or the sample's script, then the rest of the chain.

use viso_text::FontRole;
use viso_text::fallback::FontFallback;

use super::sample::CjkRegion;

/// One `<font>` entry: a file in the fonts directory and its style.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontFile {
    pub file: String,
    pub index: u32,
    pub weight: u16,
    pub italic: bool,
    /// A fallback font that only serves the named family (`serif`).
    pub fallback_for: Option<String>,
    pub postscript_name: Option<String>,
}

/// One `<family>`: named, or a member of the fallback chain.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Family {
    pub name: Option<String>,
    /// Space-separated BCP-47 tags from the `lang` attribute.
    pub langs: Vec<String>,
    /// `compact` or `elegant`, when the chain carries both designs of a script.
    pub variant: Option<String>,
    pub fonts: Vec<FontFile>,
}

/// An `<alias>`: a family name resolved to another family, optionally at a
/// fixed weight (`sans-serif-medium` → `sans-serif` at 500).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alias {
    pub name: String,
    pub to: String,
    pub weight: Option<u16>,
}

/// The parsed configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FontConfig {
    pub families: Vec<Family>,
    pub aliases: Vec<Alias>,
}

/// The query as the configuration sees it.
#[derive(Clone, Copy, Debug)]
pub struct Want<'a> {
    pub role: FontRole,
    pub region: Option<CjkRegion>,
    pub weight: u16,
    pub italic: bool,
    pub sample: &'a str,
}

/// Maximum candidates a query yields: the head of the ranking is what
/// matters, and each candidate may cost a file open to check coverage.
const MAX_CANDIDATES: usize = 48;

impl FontConfig {
    /// Parse a `fonts.xml` document. Unknown elements and attributes are
    /// skipped, so vendor extensions do not break the read.
    pub fn parse(xml: &str) -> Self {
        let mut config = Self::default();
        let mut family: Option<Family> = None;
        let mut font: Option<FontFile> = None;
        for token in Tokens::new(xml) {
            match token {
                Token::Open {
                    name,
                    attrs,
                    closed,
                } => match name {
                    "family" => {
                        let mut opened = Family {
                            name: attr(&attrs, "name").map(str::to_owned),
                            langs: attr(&attrs, "lang")
                                .map(|langs| langs.split_whitespace().map(str::to_owned).collect())
                                .unwrap_or_default(),
                            variant: attr(&attrs, "variant").map(str::to_owned),
                            fonts: Vec::new(),
                        };
                        if closed {
                            config.push_family(std::mem::take(&mut opened));
                        } else {
                            family = Some(opened);
                        }
                    }
                    "font" if family.is_some() => {
                        let opened = FontFile {
                            file: String::new(),
                            index: attr(&attrs, "index")
                                .and_then(|n| n.parse().ok())
                                .unwrap_or(0),
                            weight: attr(&attrs, "weight")
                                .and_then(|n| n.parse().ok())
                                .unwrap_or(400),
                            italic: attr(&attrs, "style") == Some("italic"),
                            fallback_for: attr(&attrs, "fallbackFor").map(str::to_owned),
                            postscript_name: attr(&attrs, "postScriptName").map(str::to_owned),
                        };
                        if !closed {
                            font = Some(opened);
                        }
                    }
                    "alias" => {
                        if let (Some(name), Some(to)) = (attr(&attrs, "name"), attr(&attrs, "to")) {
                            config.aliases.push(Alias {
                                name: name.to_owned(),
                                to: to.to_owned(),
                                weight: attr(&attrs, "weight").and_then(|n| n.parse().ok()),
                            });
                        }
                    }
                    _ => {}
                },
                Token::Text(text) => {
                    if let Some(font) = &mut font {
                        font.file.push_str(text.trim());
                    }
                }
                Token::Close(name) => match name {
                    "font" => {
                        if let (Some(opened), Some(family)) = (font.take(), family.as_mut())
                            && !opened.file.is_empty()
                        {
                            family.fonts.push(opened);
                        }
                    }
                    "family" => {
                        if let Some(closed) = family.take() {
                            config.push_family(closed);
                        }
                    }
                    _ => {}
                },
            }
        }
        config
    }

    fn push_family(&mut self, family: Family) {
        if !family.fonts.is_empty() {
            self.families.push(family);
        }
    }

    /// The faces to try for `want`, best first, without duplicates.
    pub fn candidates(&self, want: &Want) -> Vec<&FontFile> {
        let named = role_family(want.role);
        let (named, alias_weight) = self.resolve_alias(named);
        let weight = alias_weight.unwrap_or(want.weight);
        let script = FontFallback::run_script(want.sample).short_name();
        let wanted_langs: Vec<&str> = match want.role {
            FontRole::Emoji => vec!["und-Zsye"],
            _ => want.region.map(region_langs).unwrap_or_default().to_vec(),
        };

        // Family indices in the order they are tried.
        let mut ordered: Vec<usize> = Vec::new();
        let mut push = |at: usize| {
            if !ordered.contains(&at) {
                ordered.push(at);
            }
        };
        let is_named = |family: &Family| family.name.as_deref() == Some(named);
        let families = || self.families.iter().enumerate();
        // The role's own family, unless the query needs an emoji or CJK face.
        if want.role != FontRole::Emoji && want.role != FontRole::Cjk {
            families()
                .filter(|(_, f)| is_named(f))
                .for_each(|(at, _)| push(at));
        }
        // Chain families serving the requested region, emoji, or the sample's
        // script, the regular design before the compact one.
        for compact in [false, true] {
            for (at, family) in
                families().filter(|(_, f)| f.name.is_none() && is_compact(f) == compact)
            {
                let serves_lang = family
                    .langs
                    .iter()
                    .any(|tag| wanted_langs.iter().any(|want| lang_matches(tag, want)));
                let serves_script = script != "Zyyy"
                    && family
                        .langs
                        .iter()
                        .any(|tag| tag_script(tag) == Some(script));
                if serves_lang || serves_script {
                    push(at);
                }
            }
        }
        // Then the role's family if it was held back, and the rest of the
        // chain in document order.
        families()
            .filter(|(_, f)| is_named(f))
            .for_each(|(at, _)| push(at));
        families()
            .filter(|(_, f)| f.name.is_none())
            .for_each(|(at, _)| push(at));

        let mut faces = Vec::new();
        for at in ordered.into_iter().take(MAX_CANDIDATES) {
            // A font marked as the fallback for the named family takes its
            // family's slot for that query and is skipped by every other.
            let best = self.families[at]
                .fonts
                .iter()
                .filter(|font| {
                    font.fallback_for
                        .as_deref()
                        .is_none_or(|target| target == named)
                })
                .min_by_key(|font| {
                    (
                        font.fallback_for.is_none(),
                        style_distance(font, weight, want.italic),
                    )
                });
            faces.extend(best);
        }
        faces
    }

    /// Follow `name` through the aliases to a declared family, returning the
    /// family and the alias's fixed weight, if any.
    fn resolve_alias<'a>(&'a self, name: &'a str) -> (&'a str, Option<u16>) {
        let mut current = name;
        let mut weight = None;
        for _ in 0..self.aliases.len() {
            if self
                .families
                .iter()
                .any(|f| f.name.as_deref() == Some(current))
            {
                break;
            }
            let Some(alias) = self.aliases.iter().find(|a| a.name == current) else {
                break;
            };
            current = &alias.to;
            weight = weight.or(alias.weight);
        }
        (current, weight)
    }
}

/// The family a role asks for by name.
fn role_family(role: FontRole) -> &'static str {
    match role {
        FontRole::Ui | FontRole::Cjk | FontRole::Emoji => "sans-serif",
        FontRole::Serif => "serif",
        FontRole::Mono => "monospace",
    }
}

fn region_langs(region: CjkRegion) -> &'static [&'static str] {
    match region {
        CjkRegion::Japanese => &["ja", "und-Jpan"],
        CjkRegion::Korean => &["ko", "und-Kore"],
        CjkRegion::TraditionalChinese => &["zh-Hant", "und-Hant"],
        CjkRegion::SimplifiedChinese => &["zh-Hans", "und-Hans"],
    }
}

fn is_compact(family: &Family) -> bool {
    family.variant.as_deref() == Some("compact")
}

/// Whether a family's `tag` serves the wanted tag: equal, or the wanted tag
/// is a prefix at a subtag boundary (`zh-Hans` serves `zh-Hans-CN`'s query).
fn lang_matches(tag: &str, want: &str) -> bool {
    tag.eq_ignore_ascii_case(want)
        || (tag.len() > want.len()
            && tag.as_bytes()[want.len()] == b'-'
            && tag[..want.len()].eq_ignore_ascii_case(want))
}

/// The ISO 15924 script subtag of a BCP-47 tag (`und-Arab` → `Arab`).
fn tag_script(tag: &str) -> Option<&str> {
    tag.split('-')
        .skip(1)
        .find(|sub| sub.len() == 4 && sub.as_bytes()[0].is_ascii_uppercase())
}

/// How far a font's style is from the wanted one; slant outranks weight.
fn style_distance(font: &FontFile, weight: u16, italic: bool) -> u32 {
    let slant = if font.italic == italic { 0 } else { 10_000 };
    slant + u32::from(font.weight.abs_diff(weight))
}

fn attr<'a>(attrs: &[(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, value)| *value)
}

/// A token of the XML subset `fonts.xml` uses: elements, attributes and text.
/// Declarations, comments and processing instructions are skipped.
enum Token<'a> {
    Open {
        name: &'a str,
        attrs: Vec<(&'a str, &'a str)>,
        closed: bool,
    },
    Close(&'a str),
    Text(&'a str),
}

/// Markup skipped whole, by opening and closing delimiter. A comment is
/// matched before the general `<!` declaration it also starts with.
const SKIPPED: [(&str, &str); 3] = [("<!--", "-->"), ("<?", "?>"), ("<!", ">")];

struct Tokens<'a> {
    rest: &'a str,
}

impl<'a> Tokens<'a> {
    fn new(xml: &'a str) -> Self {
        Self { rest: xml }
    }
}

impl<'a> Iterator for Tokens<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Token<'a>> {
        loop {
            if self.rest.is_empty() {
                return None;
            }
            if !self.rest.starts_with('<') {
                let end = self.rest.find('<').unwrap_or(self.rest.len());
                let (text, rest) = self.rest.split_at(end);
                self.rest = rest;
                return Some(Token::Text(text));
            }
            if let Some((open, close)) =
                SKIPPED.iter().find(|(open, _)| self.rest.starts_with(open))
            {
                let body = &self.rest[open.len()..];
                self.rest = body.find(close).map_or("", |at| &body[at + close.len()..]);
                continue;
            }
            let end = tag_end(self.rest)?;
            let tag = &self.rest[1..end];
            self.rest = &self.rest[end + 1..];
            if let Some(name) = tag.strip_prefix('/') {
                return Some(Token::Close(name.trim()));
            }
            let (tag, closed) = match tag.strip_suffix('/') {
                Some(tag) => (tag, true),
                None => (tag, false),
            };
            let name_end = tag
                .find(|c: char| c.is_ascii_whitespace())
                .unwrap_or(tag.len());
            return Some(Token::Open {
                name: &tag[..name_end],
                attrs: attributes(&tag[name_end..]),
                closed,
            });
        }
    }
}

/// The index of the `>` closing the tag that starts `text`, skipping any `>`
/// inside a quoted attribute value.
fn tag_end(text: &str) -> Option<usize> {
    let mut quote = None;
    for (at, byte) in text.bytes().enumerate() {
        match (quote, byte) {
            (None, b'"' | b'\'') => quote = Some(byte),
            (Some(open), _) if open == byte => quote = None,
            (None, b'>') => return Some(at),
            _ => {}
        }
    }
    None
}

fn attributes(mut text: &str) -> Vec<(&str, &str)> {
    let mut attrs = Vec::new();
    loop {
        text = text.trim_start();
        let Some(eq) = text.find('=') else {
            return attrs;
        };
        let name = text[..eq].trim();
        let value = text[eq + 1..].trim_start();
        let Some(quote) = value.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            return attrs;
        };
        let Some(close) = value[1..].find(quote) else {
            return attrs;
        };
        attrs.push((name, &value[1..1 + close]));
        text = &value[close + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FONTS_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<!-- A trimmed fonts.xml in the shape Android ships. -->
<familyset version="23">
    <family name="sans-serif">
        <font weight="400" style="normal">Roboto-Regular.ttf</font>
        <font weight="700" style="normal">Roboto-Bold.ttf</font>
        <font weight="400" style="italic">Roboto-Italic.ttf</font>
    </family>
    <alias name="sans-serif-medium" to="sans-serif" weight="500" />
    <family name="serif">
        <font weight="400" style="normal" postScriptName="NotoSerif">NotoSerif-Regular.ttf</font>
    </family>
    <family name="droid-sans-mono">
        <font weight="400" style="normal">DroidSansMono.ttf</font>
    </family>
    <alias name="monospace" to="droid-sans-mono" />
    <family lang="und-Arab" variant="elegant">
        <font weight="400" style="normal">NotoNaskhArabic-Regular.ttf</font>
    </family>
    <family lang="und-Arab" variant="compact">
        <font weight="400" style="normal">NotoNaskhArabicUI-Regular.ttf</font>
    </family>
    <family lang="und-Zsye">
        <font weight="400" style="normal">NotoColorEmoji.ttf</font>
    </family>
    <family lang="zh-Hans">
        <font weight="400" style="normal" index="2">NotoSansCJK-Regular.ttc
            <axis tag="wght" stylevalue="400" />
        </font>
        <font weight="400" style="normal" index="2" fallbackFor="serif">NotoSerifCJK-Regular.ttc</font>
    </family>
    <family lang="zh-Hant zh-Bopo">
        <font weight="400" style="normal" index="3">NotoSansCJK-Regular.ttc</font>
    </family>
    <family lang="ja">
        <font weight="400" style="normal" index="0">NotoSansCJK-Regular.ttc</font>
    </family>
    <family lang="ko">
        <font weight="400" style="normal" index="1">NotoSansCJK-Regular.ttc</font>
    </family>
</familyset>
"#;

    fn want(role: FontRole, region: Option<CjkRegion>, sample: &str) -> Want<'_> {
        Want {
            role,
            region,
            weight: 400,
            italic: false,
            sample,
        }
    }

    fn files(config: &FontConfig, want: &Want) -> Vec<(String, u32)> {
        config
            .candidates(want)
            .into_iter()
            .map(|font| (font.file.clone(), font.index))
            .collect()
    }

    #[test]
    fn parses_families_fonts_and_aliases() {
        let config = FontConfig::parse(FONTS_XML);
        assert_eq!(config.families.len(), 10);
        assert_eq!(config.aliases.len(), 2);
        let cjk = &config.families[6];
        assert_eq!(cjk.langs, vec!["zh-Hans"]);
        assert_eq!(cjk.fonts[0].file, "NotoSansCJK-Regular.ttc");
        assert_eq!(cjk.fonts[0].index, 2);
        assert_eq!(cjk.fonts[1].fallback_for.as_deref(), Some("serif"));
        assert_eq!(
            config.families[1].fonts[0].postscript_name.as_deref(),
            Some("NotoSerif")
        );
        assert_eq!(config.families[7].langs, vec!["zh-Hant", "zh-Bopo"]);
    }

    #[test]
    fn ui_query_starts_at_its_family_in_the_nearest_style() {
        let config = FontConfig::parse(FONTS_XML);
        let mut bold = want(FontRole::Ui, None, "a");
        bold.weight = 650;
        assert_eq!(files(&config, &bold)[0].0, "Roboto-Bold.ttf");
        let mut italic = want(FontRole::Ui, None, "a");
        italic.italic = true;
        italic.weight = 700;
        assert_eq!(files(&config, &italic)[0].0, "Roboto-Italic.ttf");
    }

    #[test]
    fn aliases_resolve_to_their_family() {
        let config = FontConfig::parse(FONTS_XML);
        assert_eq!(
            files(&config, &want(FontRole::Mono, None, "a"))[0].0,
            "DroidSansMono.ttf"
        );
    }

    #[test]
    fn a_script_sample_moves_its_chain_family_forward() {
        let config = FontConfig::parse(FONTS_XML);
        let faces = files(&config, &want(FontRole::Ui, None, "مرحبا"));
        assert_eq!(faces[0].0, "Roboto-Regular.ttf");
        assert_eq!(faces[1].0, "NotoNaskhArabic-Regular.ttf");
        assert_eq!(faces[2].0, "NotoNaskhArabicUI-Regular.ttf");
    }

    #[test]
    fn emoji_and_cjk_regions_lead_their_queries() {
        let config = FontConfig::parse(FONTS_XML);
        assert_eq!(
            files(&config, &want(FontRole::Emoji, None, "😀"))[0].0,
            "NotoColorEmoji.ttf"
        );
        let ja = files(
            &config,
            &want(FontRole::Cjk, Some(CjkRegion::Japanese), "漢"),
        );
        assert_eq!(ja[0], ("NotoSansCJK-Regular.ttc".into(), 0));
        let tw = files(
            &config,
            &want(FontRole::Cjk, Some(CjkRegion::TraditionalChinese), "漢"),
        );
        assert_eq!(tw[0], ("NotoSansCJK-Regular.ttc".into(), 3));
    }

    #[test]
    fn serif_fallbacks_only_serve_serif() {
        let config = FontConfig::parse(FONTS_XML);
        let serif = files(&config, &want(FontRole::Serif, None, "漢"));
        assert!(serif.contains(&("NotoSerifCJK-Regular.ttc".into(), 2)));
        let sans = files(&config, &want(FontRole::Ui, None, "漢"));
        assert!(
            !sans
                .iter()
                .any(|(file, _)| file == "NotoSerifCJK-Regular.ttc")
        );
    }

    #[test]
    fn a_query_tries_its_family_and_the_whole_chain_once() {
        let config = FontConfig::parse(FONTS_XML);
        let faces = files(&config, &want(FontRole::Ui, None, "a"));
        let chain = config.families.iter().filter(|f| f.name.is_none()).count();
        assert_eq!(faces.len(), 1 + chain);
        assert_eq!(faces[0].0, "Roboto-Regular.ttf");
    }

    #[test]
    fn malformed_input_degrades_to_what_parsed() {
        let config =
            FontConfig::parse("<familyset><family name=\"x\"><font>A.ttf</font></family><family");
        assert_eq!(config.families.len(), 1);
        assert!(FontConfig::parse("").families.is_empty());
    }
}
