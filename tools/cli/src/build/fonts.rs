//! The text an embedded build ships, and what its text runtime is cut to by
//! it: the static text `packaged_fonts!()` subsets the app's fonts to
//! — every string literal of the package's `.vs`
//! files, of each `ui!` and `component!` in its Rust sources, and the text of
//! every locale of its message catalogs — and whether the Unicode data for
//! line breaking Thai, Lao, Khmer and Myanmar stays in.
//!
//! The text is a superset of what the app can show: a literal that names an
//! asset or a theme counts as text too. Each string is written once, in byte
//! order, so the same sources give the same file, and the file is rewritten
//! only when that text changes, so the build that reads it is not invalidated
//! by a run that changed nothing.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use viso_dsl::i18n::Messages;
use viso_dsl::package::{SOURCE_ROOT, collect_sources};
use viso_dsl::syntax::{SyntaxKind, tokenize};
use viso_project::{DeviceTarget, Layout, Project};
use viso_text::line_break::{is_complex_script, is_complex_script_locale};

/// The variable naming the static text file.
pub(crate) const FONT_TEXT_ENV: &str = "VISO_FONT_TEXT";

/// The facade feature that keeps the line-break models of Thai, Lao, Khmer
/// and Myanmar.
pub(crate) const COMPLEX_LINE_BREAK: &str = "viso/complex-line-break";

/// The text a build of a project ships.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Shipped {
    /// Every static string, one per line.
    pub(crate) text: String,
    /// The locales of its message catalogs, the source locale first; the
    /// source locale alone when it has none.
    pub(crate) locales: Vec<String>,
}

impl Shipped {
    /// The static text and catalog locales of `project`.
    ///
    /// # Errors
    ///
    /// A source or a catalog that cannot be read.
    pub(crate) fn of(project: &Project) -> Result<Self, String> {
        static_text(project)
    }

    /// Whether `device`'s build keeps line breaking between the words of
    /// Thai, Lao, Khmer and Myanmar: a locale it declares — its `locales`,
    /// else the catalogs' — is written in one, or the text it ships or its
    /// `fonts.charset` holds a character of one. Static text in those
    /// scripts keeps the models whatever the locales say, so text the build
    /// shows never loses its breaks.
    pub(crate) fn complex_line_break(&self, device: &DeviceTarget) -> bool {
        let declared = match device.locales.is_empty() {
            true => self.locales.iter().any(|l| is_complex_script_locale(l)),
            false => device
                .locales
                .iter()
                .any(|l| is_complex_script_locale(&l.value)),
        };
        declared
            || self.text.chars().any(is_complex_script)
            || device.charset.iter().any(|entry| {
                charset_range(&entry.value)
                    .is_some_and(|range| range.filter_map(char::from_u32).any(is_complex_script))
            })
    }
}

/// The code points of a `U+XXXX` or `U+XXXX-YYYY` charset entry; `None`
/// for a named set, none of which holds a complex script.
fn charset_range(entry: &str) -> Option<std::ops::RangeInclusive<u32>> {
    let entry = entry.trim();
    let code = entry
        .strip_prefix("U+")
        .or_else(|| entry.strip_prefix("u+"))?;
    let (low, high) = code.split_once('-').unwrap_or((code, code));
    let parse = |hex: &str| u32::from_str_radix(hex, 16).ok();
    Some(parse(low)?..=parse(high)?)
}

/// Writes `shipped`'s static text for the device target `name` under the
/// generated sources of `layout`, and returns the file's absolute path.
///
/// # Errors
///
/// The file that cannot be written.
pub(crate) fn write(shipped: &Shipped, layout: &Layout, name: &str) -> Result<PathBuf, String> {
    let text = &shipped.text;
    let dir = layout.generated().join("fonts");
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create `{}`: {e}", dir.display()))?;
    let path = dir.join(format!("{name}.txt"));
    if fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
        fs::write(&path, text).map_err(|e| format!("cannot write `{}`: {e}", path.display()))?;
    }
    Ok(path.canonicalize().unwrap_or(path))
}

/// Every static string of `project`, one per line, and its catalogs'
/// locales.
fn static_text(project: &Project) -> Result<Shipped, String> {
    let root = &project.root;
    let mut strings = BTreeSet::new();
    let unreadable =
        |path: &Path, error: std::io::Error| format!("cannot read `{}`: {error}", path.display());

    let mut views = Vec::new();
    let mut failed = Vec::new();
    collect_sources(&root.join(SOURCE_ROOT), &mut views, &mut failed);
    if let Some((path, error)) = failed.into_iter().next() {
        return Err(unreadable(&path, error));
    }
    for path in views {
        let source = fs::read_to_string(&path).map_err(|e| unreadable(&path, e))?;
        strings.extend(view_strings(&source));
    }

    let mut rust = Vec::new();
    rust_sources(&root.join(SOURCE_ROOT), &mut rust);
    for path in rust {
        let source = fs::read_to_string(&path).map_err(|e| unreadable(&path, e))?;
        strings.extend(macro_strings(&source));
    }

    let catalogs = root.join(viso_dsl::i18n::CATALOG_DIR);
    let files = Messages::read_dir(&catalogs).map_err(|e| unreadable(&catalogs, e))?;
    let source = project
        .manifest
        .i18n
        .source
        .as_ref()
        .map_or(viso_dsl::i18n::DEFAULT_SOURCE, |source| {
            source.value.as_str()
        });
    let mut locales = vec![source.to_owned()];
    if !files.is_empty() {
        let catalog = Messages::compile(source, &files);
        let catalog = catalog.catalog();
        locales = catalog.locales().map(str::to_owned).collect();
        for index in 0..catalog.locales().count() {
            if let Ok(table) = catalog.table(index) {
                strings.insert(table.text);
            }
        }
    }

    strings.remove("");
    let mut text = String::new();
    for string in strings {
        text.push_str(&string);
        text.push('\n');
    }
    Ok(Shipped { text, locales })
}

/// Every `.rs` file below `dir`, in path order, hidden entries left out.
fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for path in entries {
        if path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        {
            continue;
        }
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// The value of every string literal of the `.vs` text `source`.
fn view_strings(source: &str) -> Vec<String> {
    tokenize(source)
        .into_iter()
        .filter(|token| token.error.is_none())
        .filter_map(|token| {
            let text = &source[std::ops::Range::<usize>::from(token.range)];
            match token.kind {
                SyntaxKind::StringLiteral => unescape(text.strip_prefix('"')?.strip_suffix('"')?),
                SyntaxKind::RawStringLiteral => raw_body(text).map(str::to_owned),
                _ => None,
            }
        })
        .collect()
}

/// The value of every string literal in the body of each `ui!` and
/// `component!` of the Rust text `source`.
fn macro_strings(source: &str) -> Vec<String> {
    let tokens = rust_tokens(source);
    let mut strings = Vec::new();
    let mut at = 0;
    while at < tokens.len() {
        let invoked = matches!(tokens[at], Rust::Ident("ui" | "component"))
            && matches!(tokens.get(at + 1), Some(Rust::Punct('!')))
            && matches!(tokens.get(at + 2), Some(Rust::Open(_)));
        if !invoked {
            at += 1;
            continue;
        }
        let mut depth = 0usize;
        at += 2;
        while let Some(token) = tokens.get(at) {
            match token {
                Rust::Open(_) => depth += 1,
                Rust::Close(_) => depth -= 1,
                Rust::Str(value) => strings.push(value.clone()),
                _ => {}
            }
            at += 1;
            if depth == 0 {
                break;
            }
        }
    }
    strings
}

/// A Rust token, as far as finding a macro's body and its strings needs.
#[derive(Debug, PartialEq)]
enum Rust<'s> {
    Ident(&'s str),
    Punct(char),
    Open(char),
    Close(char),
    /// A string literal's value.
    Str(String),
}

/// The tokens of the Rust text `source`, comments, whitespace, numbers,
/// characters and lifetimes left out; a byte string's value is not text.
fn rust_tokens(source: &str) -> Vec<Rust<'_>> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let rest = &source[at..];
        let c = rest.chars().next().unwrap_or(' ');
        if rest.starts_with("//") {
            at += rest.find('\n').unwrap_or(rest.len());
        } else if rest.starts_with("/*") {
            at += block_comment(rest);
        } else if let Some((len, value)) = string_literal(rest) {
            if let Some(value) = value {
                tokens.push(Rust::Str(value));
            }
            at += len;
        } else if c == '\'' {
            at += char_or_lifetime(rest);
        } else if c.is_alphabetic() || c == '_' {
            let len = rest
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            tokens.push(Rust::Ident(&rest[..len]));
            at += len;
        } else if c.is_ascii_digit() {
            at += rest
                .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
                .unwrap_or(rest.len());
        } else {
            match c {
                '(' | '[' | '{' => tokens.push(Rust::Open(c)),
                ')' | ']' | '}' => tokens.push(Rust::Close(c)),
                c if c.is_whitespace() => {}
                c => tokens.push(Rust::Punct(c)),
            }
            at += c.len_utf8();
        }
    }
    tokens
}

/// The length of the block comment `rest` starts with, nested ones inside.
fn block_comment(rest: &str) -> usize {
    let mut depth = 0;
    let mut at = 0;
    while at < rest.len() {
        if rest[at..].starts_with("/*") {
            depth += 1;
            at += 2;
        } else if rest[at..].starts_with("*/") {
            depth -= 1;
            at += 2;
            if depth == 0 {
                return at;
            }
        } else {
            at += rest[at..].chars().next().map_or(1, char::len_utf8);
        }
    }
    rest.len()
}

/// The string literal `rest` starts with, if it does: its length, and its
/// value unless it is a byte or C string.
fn string_literal(rest: &str) -> Option<(usize, Option<String>)> {
    let (prefix, text) = ["br", "cr", "b", "c", "r", ""].iter().find_map(|prefix| {
        let text = rest.strip_prefix(prefix)?;
        (text.starts_with('"') || (prefix.ends_with('r') && text.starts_with('#')))
            .then_some((*prefix, text))
    })?;
    let text_value = !prefix.starts_with('b') && !prefix.starts_with('c');
    if prefix.ends_with('r') {
        let hashes = text.len() - text.trim_start_matches('#').len();
        let open = &text[hashes..];
        if !open.starts_with('"') {
            return None;
        }
        let close = format!("\"{}", "#".repeat(hashes));
        let end = open[1..].find(&close)?;
        let len = prefix.len() + hashes + 1 + end + close.len();
        return Some((len, text_value.then(|| open[1..1 + end].to_owned())));
    }
    let mut escaped = false;
    for (offset, c) in text[1..].char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            let body = &text[1..1 + offset];
            let len = prefix.len() + offset + 2;
            return Some((len, if text_value { unescape(body) } else { None }));
        }
    }
    Some((rest.len(), None))
}

/// The length of the character literal or lifetime `rest` starts with.
fn char_or_lifetime(rest: &str) -> usize {
    let body = &rest[1..];
    let mut chars = body.char_indices();
    match chars.next() {
        Some((_, '\\')) => body[1..]
            .find('\'')
            .map_or(rest.len(), |end| 1 + 1 + end + 1),
        Some((_, c)) => match body[c.len_utf8()..].starts_with('\'') {
            true => 1 + c.len_utf8() + 1,
            false => 1,
        },
        None => 1,
    }
}

/// The body of the raw string literal `text`: `r"…"` or `r#"…"#`.
fn raw_body(text: &str) -> Option<&str> {
    let text = text.strip_prefix('r')?;
    let hashes = text.len() - text.trim_start_matches('#').len();
    text[hashes..]
        .strip_prefix('"')?
        .strip_suffix(&"#".repeat(hashes))?
        .strip_suffix('"')
}

/// The value a string literal's `body` spells, `None` when an escape is
/// malformed. A `\` before a line break skips it and the whitespace after.
fn unescape(body: &str) -> Option<String> {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let escaped = match chars.next()? {
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            '0' => '\0',
            '\\' => '\\',
            '"' => '"',
            '\'' => '\'',
            '\n' => {
                while chars.next_if(|c| c.is_whitespace()).is_some() {}
                continue;
            }
            'x' => {
                let digits: String = chars.by_ref().take(2).collect();
                char::from_u32(u32::from_str_radix(&digits, 16).ok()?)?
            }
            'u' => {
                if chars.next()? != '{' {
                    return None;
                }
                let digits: String = chars.by_ref().take_while(|c| *c != '}').collect();
                let digits = digits.replace('_', "");
                if digits.is_empty() || digits.len() > 6 {
                    return None;
                }
                char::from_u32(u32::from_str_radix(&digits, 16).ok()?)?
            }
            _ => return None,
        };
        out.push(escaped);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_view_gives_its_string_literals() {
        let source = "component App {\n    view {\n        Text(\"Grüße\\n\")\n        \
                      Text(r#\"say \"hi\"\"#)\n        // \"not text\"\n    }\n}\n";
        assert_eq!(view_strings(source), ["Grüße\n", "say \"hi\""]);
    }

    #[test]
    fn a_macro_gives_the_strings_of_its_body_alone() {
        let source = r##"
            // ui! { Text("in a comment") }
            fn main() {
                let other = "not in a macro";
                let tree = viso::ui! { Column { Text("温度") Text(r#"a "b""#) } };
                let c = 'x';
                fn f<'a>(s: &'a str) -> &'a str { s }
                let bytes = b"bytes";
                component! {
                    component Card { view { Text("卡\u{7247}") } }
                }
                println!("{}", "after");
            }
        "##;
        assert_eq!(macro_strings(source), ["温度", "a \"b\"", "卡片"]);
    }

    fn panel(toml: &str) -> DeviceTarget {
        let text = format!("[target.panel]\nprofile = \"embedded-cpu\"\n{toml}");
        match viso_project::Manifest::parse("Viso.toml", &text) {
            viso_project::ParseOutcome::Parsed { manifest, .. } => {
                manifest.targets.device("panel").unwrap().clone()
            }
            viso_project::ParseOutcome::Failed(diags) => panic!("{diags:?}"),
        }
    }

    fn shipped(text: &str, locales: &[&str]) -> Shipped {
        Shipped {
            text: text.to_owned(),
            locales: locales.iter().map(|&l| l.to_owned()).collect(),
        }
    }

    #[test]
    fn the_complex_line_break_models_follow_the_locales_and_the_text() {
        let latin = shipped("Hello\n", &["en", "de"]);
        assert!(!latin.complex_line_break(&panel("")));
        assert!(shipped("Hello\n", &["en", "th"]).complex_line_break(&panel("")));
        assert!(
            !shipped("Hello\n", &["en", "th"]).complex_line_break(&panel("locales = [\"en\"]")),
            "the target's locales replace the catalogs'"
        );
        assert!(latin.complex_line_break(&panel("locales = [\"en\", \"km\"]")));
        assert!(
            shipped("สวัสดี\n", &["en"]).complex_line_break(&panel("locales = [\"en\"]")),
            "shipped Thai text keeps its breaks"
        );
        assert!(latin.complex_line_break(&panel("fonts = { charset = [\"U+0E00-0E7F\"] }")));
        assert!(!latin.complex_line_break(&panel(
            "fonts = { charset = [\"latin\", \"gb2312\", \"U+4E00\"] }"
        )));
    }

    #[test]
    fn escapes_spell_their_characters() {
        assert_eq!(unescape(r"a\tb\x41\u{1F600}").as_deref(), Some("a\tbA😀"));
        assert_eq!(unescape("one \\\n     two").as_deref(), Some("one two"));
        assert_eq!(unescape(r"\q"), None);
        assert_eq!(raw_body("r##\"x\"#y\"##"), Some("x\"#y"));
    }
}
