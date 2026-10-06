//! A package's messages: the catalogs under `i18n/`, one TOML file per
//! locale (`i18n/en.toml`, `i18n/zh-Hant.toml`), each a table of keys whose
//! values are ICU MessageFormat strings. Nested tables and dotted keys make
//! the key: `[inbox] unread = ".."` is `inbox.unread`.
//!
//! The source locale (`Viso.toml [i18n] source`) declares every message and
//! its arguments: `{name}` takes text or a number, `{name, number}`,
//! `plural` and `selectordinal` a number, `select` a `String` or a unit-only
//! enum. A translation may leave a message out (it falls back along the
//! CLDR chain to the source) and uses only the arguments the source
//! declares, in kinds the source's satisfy (`E3706`).
//!
//! [`Messages::compile`] numbers the messages and compiles each locale's
//! patterns into the runtime [`Catalog`]; a message a locale does not
//! translate names its fallback's pattern.

mod message;

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::Path;

use icu_locale_core::Locale;
use icu_locale_fallback::LocaleFallbacker;
use viso_behavior::i18n::{CaseKey, Catalog, Entry, MessageInfo, PatternOp, Table};

use message::{Part, PluralKey};

/// The directory of a package's catalogs, beside its `Viso.toml`.
pub const CATALOG_DIR: &str = "i18n";

/// The source locale of a package that names none.
pub const DEFAULT_SOURCE: &str = "en";

/// One catalog file: its locale, a label for diagnostics (its path), and its
/// text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogFile {
    pub locale: String,
    pub path: String,
    pub text: String,
}

/// What kind of value an argument takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ArgKind {
    /// `{name}`: text, or a number shown in the reader's locale.
    Text,
    /// `number`, `plural`, `selectordinal`: a number.
    Number,
    /// `select`: a `String` or a unit-only enum, by variant name.
    Select,
}

impl ArgKind {
    /// Whether a value passed for `self` serves a use as `other`.
    fn serves(self, other: ArgKind) -> bool {
        self == other || other == ArgKind::Text
    }

    fn name(self) -> &'static str {
        match self {
            ArgKind::Text => "text",
            ArgKind::Number => "a number",
            ArgKind::Select => "a `select` value",
        }
    }
}

/// A message of the source locale: its key and arguments by name, in the
/// order a call passes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageSig {
    pub key: String,
    pub args: Vec<(String, ArgKind)>,
}

/// A problem in a catalog file, at a byte range of its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogIssue {
    /// The index of the file in the files compiled.
    pub file: usize,
    pub range: Range<usize>,
    /// Whether it is an error rather than a warning.
    pub error: bool,
    pub message: String,
}

impl CatalogIssue {
    /// The stable diagnostic code.
    pub const CODE: &'static str = "E3706";
}

/// A package's compiled messages.
#[derive(Debug, Clone)]
pub struct Messages {
    source: String,
    sigs: Vec<MessageSig>,
    catalog: Catalog,
    untranslated: Vec<(String, Vec<String>)>,
    issues: Vec<CatalogIssue>,
    fingerprint: u64,
}

impl PartialEq for Messages {
    fn eq(&self, other: &Messages) -> bool {
        self.fingerprint == other.fingerprint
    }
}

impl Eq for Messages {}

impl Messages {
    /// Compiles `files` with `source` the source locale.
    pub fn compile(source: &str, files: &[CatalogFile]) -> Messages {
        let mut issues = Vec::new();
        let source = canonical(source).unwrap_or_else(|| source.to_owned());
        let mut locales: Vec<(String, usize, BTreeMap<String, Parsed>)> = Vec::new();
        for (index, file) in files.iter().enumerate() {
            let whole = 0..file.text.len().min(1);
            let Some(tag) = canonical(&file.locale) else {
                issues.push(CatalogIssue {
                    file: index,
                    range: whole,
                    error: true,
                    message: format!("`{}` is no BCP 47 locale", file.locale),
                });
                continue;
            };
            if locales.iter().any(|(t, _, _)| *t == tag) {
                issues.push(CatalogIssue {
                    file: index,
                    range: whole,
                    error: true,
                    message: format!("a second catalog of `{tag}`"),
                });
                continue;
            }
            let messages = read_file(index, &file.text, &mut issues);
            locales.push((tag, index, messages));
        }
        let source_at = locales.iter().position(|(t, _, _)| *t == source);
        let Some(source_at) = source_at else {
            if !files.is_empty() {
                issues.push(CatalogIssue {
                    file: 0,
                    range: 0..0,
                    error: true,
                    message: format!(
                        "no catalog of the source locale `{source}` (`{CATALOG_DIR}/{source}.toml`)"
                    ),
                });
            }
            return Messages::empty(source, issues, files);
        };
        let source_locale = locales.remove(source_at);
        locales.insert(0, source_locale);

        // A source message that does not parse is reported and left out.
        let unfit: Vec<String> = locales[0]
            .2
            .iter()
            .filter(|(_, p)| !p.fits)
            .map(|(k, _)| k.clone())
            .collect();
        let sigs: Vec<MessageSig> = locales[0]
            .2
            .iter()
            .filter(|(_, p)| p.fits)
            .map(|(key, parsed)| MessageSig {
                key: key.clone(),
                args: parsed.args.iter().map(|(n, k)| (n.clone(), *k)).collect(),
            })
            .collect();
        let ids: BTreeMap<&str, usize> = sigs
            .iter()
            .enumerate()
            .map(|(i, s)| (s.key.as_str(), i))
            .collect();

        // Every translation fits its source message.
        for (tag, file, messages) in &locales[1..] {
            for (key, parsed) in messages {
                let Some(&id) = ids.get(key.as_str()) else {
                    if unfit.contains(key) {
                        continue;
                    }
                    issues.push(CatalogIssue {
                        file: *file,
                        range: parsed.range.clone(),
                        error: false,
                        message: format!(
                            "`{key}` is not a message of the source locale `{source}`, so \
                             nothing shows it"
                        ),
                    });
                    continue;
                };
                for (name, kind) in &parsed.args {
                    let declared = sigs[id].args.iter().find(|(n, _)| n == name);
                    let message = match declared {
                        None => format!(
                            "the `{tag}` `{key}` uses `{name}`, which the source message does \
                             not take"
                        ),
                        Some((_, given)) if !given.serves(*kind) => format!(
                            "the `{tag}` `{key}` uses `{name}` as {}, but the source passes {}",
                            kind.name(),
                            given.name()
                        ),
                        Some(_) => continue,
                    };
                    issues.push(CatalogIssue {
                        file: *file,
                        range: parsed.range.clone(),
                        error: true,
                        message,
                    });
                }
            }
        }

        // Each locale's own patterns, and where the rest fall back to.
        let tags: Vec<&str> = locales.iter().map(|(t, _, _)| t.as_str()).collect();
        let chains: Vec<Vec<usize>> = tags.iter().map(|t| chain(t, &tags)).collect();
        let mut tables: Vec<(String, Table)> = Vec::new();
        let mut untranslated = Vec::new();
        for (at, (tag, _, messages)) in locales.iter().enumerate() {
            let mut table = Table::default();
            let mut missing = Vec::new();
            for sig in &sigs {
                let own = messages.get(&sig.key).filter(|p| {
                    p.fits
                        && p.args.iter().all(|(n, used)| {
                            sig.args
                                .iter()
                                .any(|(m, given)| m == n && given.serves(*used))
                        })
                });
                match own {
                    Some(parsed) => {
                        let start = table.ops.len() as u32;
                        lower(&parsed.parts, &sig.args, &mut table);
                        table.entries.push(Entry {
                            origin: at as u16,
                            start,
                            len: table.ops.len() as u32 - start,
                        });
                    }
                    None => {
                        missing.push(sig.key.clone());
                        // Filled below, once every locale's own patterns
                        // are placed.
                        table.entries.push(Entry {
                            origin: u16::MAX,
                            start: 0,
                            len: 0,
                        });
                    }
                }
            }
            if at > 0 && !missing.is_empty() {
                untranslated.push((tag.clone(), missing));
            }
            tables.push((tag.clone(), table));
        }
        for at in 0..tables.len() {
            for id in 0..sigs.len() {
                if tables[at].1.entries[id].origin != u16::MAX {
                    continue;
                }
                let from = chains[at]
                    .iter()
                    .copied()
                    .find(|&c| tables[c].1.entries[id].origin == c as u16)
                    .unwrap_or(0);
                tables[at].1.entries[id] = tables[from].1.entries[id];
            }
        }
        let infos = sigs
            .iter()
            .map(|s| MessageInfo {
                key: s.key.as_str().into(),
                args: s.args.len() as u16,
            })
            .collect();
        let catalog = Catalog::new(infos, tables).expect("a compiled catalog is well formed");
        Messages {
            fingerprint: fingerprint(&source, files),
            source,
            sigs,
            catalog,
            untranslated,
            issues,
        }
    }

    fn empty(source: String, issues: Vec<CatalogIssue>, files: &[CatalogFile]) -> Messages {
        let catalog = Catalog::new(Vec::new(), vec![(source.clone(), Table::default())])
            .expect("an empty catalog is well formed");
        Messages {
            fingerprint: fingerprint(&source, files),
            source,
            sigs: Vec::new(),
            catalog,
            untranslated: Vec::new(),
            issues,
        }
    }

    /// Reads the catalogs in `dir` (`<locale>.toml`), sorted by locale; none
    /// when it does not exist.
    ///
    /// # Errors
    ///
    /// When a file of it cannot be read.
    pub fn read_dir(dir: &Path) -> std::io::Result<Vec<CatalogFile>> {
        let mut files = Vec::new();
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(files),
            Err(e) => return Err(e),
        };
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "toml")
                && let Some(locale) = path.file_stem().and_then(|s| s.to_str())
            {
                files.push(CatalogFile {
                    locale: locale.to_owned(),
                    path: path.display().to_string(),
                    text: std::fs::read_to_string(&path)?,
                });
            }
        }
        files.sort_by(|a, b| a.locale.cmp(&b.locale));
        Ok(files)
    }

    /// The source locale.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The message `key` and its id.
    pub fn message(&self, key: &str) -> Option<(u32, &MessageSig)> {
        let at = self
            .sigs
            .binary_search_by(|s| s.key.as_str().cmp(key))
            .ok()?;
        Some((at as u32, &self.sigs[at]))
    }

    /// Every message, by id.
    pub fn sigs(&self) -> &[MessageSig] {
        &self.sigs
    }

    /// The compiled catalog a module carries.
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// Each translation's messages it leaves to its fallback, by locale.
    pub fn untranslated(&self) -> &[(String, Vec<String>)] {
        &self.untranslated
    }

    /// What is wrong in the catalog files.
    pub fn issues(&self) -> &[CatalogIssue] {
        &self.issues
    }
}

/// A message as read from its file: its parts, the arguments it uses and
/// how, where its value is, and whether it parsed.
struct Parsed {
    parts: Vec<Part>,
    args: BTreeMap<String, ArgKind>,
    range: Range<usize>,
    fits: bool,
}

/// The canonical form of the BCP 47 tag `tag`.
fn canonical(tag: &str) -> Option<String> {
    tag.parse::<Locale>().ok().map(|l| l.to_string())
}

/// The locales of `tags` that `tag` falls back to, nearest first, ending at
/// the source (index 0).
fn chain(tag: &str, tags: &[&str]) -> Vec<usize> {
    let mut out = Vec::new();
    let Ok(locale) = tag.parse::<Locale>() else {
        return vec![0];
    };
    let fallbacker = LocaleFallbacker::new();
    let mut it = fallbacker
        .for_config(Default::default())
        .fallback_for((&locale).into());
    it.step();
    loop {
        let at = it.get();
        if at.is_unknown() {
            break;
        }
        let name = at.to_string();
        if let Some(found) = tags.iter().position(|t| *t == name)
            && !out.contains(&found)
        {
            out.push(found);
        }
        it.step();
    }
    if !out.contains(&0) {
        out.push(0);
    }
    out
}

fn fingerprint(source: &str, files: &[CatalogFile]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
        }
        h = (h ^ 0xff).wrapping_mul(0x100_0000_01b3);
    };
    eat(source.as_bytes());
    for file in files {
        eat(file.locale.as_bytes());
        eat(file.text.as_bytes());
    }
    h
}

/// The messages of one catalog file, by key.
fn read_file(file: usize, text: &str, issues: &mut Vec<CatalogIssue>) -> BTreeMap<String, Parsed> {
    let mut out = BTreeMap::new();
    let document = match toml_edit::Document::parse(text) {
        Ok(document) => document,
        Err(e) => {
            issues.push(CatalogIssue {
                file,
                range: e.span().unwrap_or(0..0),
                error: true,
                message: format!("the catalog is not valid TOML: {}", e.message()),
            });
            return out;
        }
    };
    let mut prefix = Vec::new();
    read_table(
        file,
        document.as_table().iter_items(),
        &mut prefix,
        &mut out,
        issues,
    );
    out
}

/// What a table or an inline table yields: its keys and items.
enum Node<'d> {
    Item(&'d toml_edit::Item),
    Value(&'d toml_edit::Value),
}

trait Items<'d> {
    fn iter_items(self) -> Vec<(&'d str, Option<Range<usize>>, Node<'d>)>;
}

impl<'d> Items<'d> for &'d toml_edit::Table {
    fn iter_items(self) -> Vec<(&'d str, Option<Range<usize>>, Node<'d>)> {
        self.iter()
            .map(|(k, item)| {
                let span = self.key(k).and_then(|key| key.span());
                (k, span, Node::Item(item))
            })
            .collect()
    }
}

impl<'d> Items<'d> for &'d toml_edit::InlineTable {
    fn iter_items(self) -> Vec<(&'d str, Option<Range<usize>>, Node<'d>)> {
        self.iter()
            .map(|(k, value)| {
                let span = self.key(k).and_then(|key| key.span());
                (k, span, Node::Value(value))
            })
            .collect()
    }
}

fn read_table(
    file: usize,
    items: Vec<(&str, Option<Range<usize>>, Node<'_>)>,
    prefix: &mut Vec<String>,
    out: &mut BTreeMap<String, Parsed>,
    issues: &mut Vec<CatalogIssue>,
) {
    for (name, key_span, node) in items {
        prefix.push(name.to_owned());
        let value = match node {
            Node::Item(toml_edit::Item::Table(table)) => {
                read_table(file, table.iter_items(), prefix, out, issues);
                prefix.pop();
                continue;
            }
            Node::Item(toml_edit::Item::Value(value)) | Node::Value(value) => value,
            Node::Item(_) => {
                issues.push(CatalogIssue {
                    file,
                    range: key_span.unwrap_or(0..0),
                    error: true,
                    message: "a catalog holds messages and tables of them".into(),
                });
                prefix.pop();
                continue;
            }
        };
        if let toml_edit::Value::InlineTable(table) = value {
            read_table(file, table.iter_items(), prefix, out, issues);
            prefix.pop();
            continue;
        }
        let key = prefix.join(".");
        prefix.pop();
        let range = value.span().or(key_span).unwrap_or(0..0);
        let Some(text) = value.as_str() else {
            issues.push(CatalogIssue {
                file,
                range,
                error: true,
                message: format!("the message `{key}` is not a string"),
            });
            continue;
        };
        let (parts, fits) = match message::parse(text) {
            Ok(parts) => (parts, true),
            Err((at, why)) => {
                issues.push(CatalogIssue {
                    file,
                    range: range.clone(),
                    error: true,
                    message: format!("the message `{key}` is malformed at byte {at}: {why}"),
                });
                (Vec::new(), false)
            }
        };
        let mut args = BTreeMap::new();
        let mut fits = fits;
        if let Err(name) = collect_args(&parts, &mut args) {
            issues.push(CatalogIssue {
                file,
                range: range.clone(),
                error: true,
                message: format!(
                    "the message `{key}` uses `{name}` both as a number and as a `select` value"
                ),
            });
            fits = false;
        }
        out.insert(
            key,
            Parsed {
                parts,
                args,
                range,
                fits,
            },
        );
    }
}

/// Gathers the arguments `parts` use and as what; a number serves text, and
/// a `select` value text. Fails with an argument used both as a number and
/// as a `select` value.
fn collect_args(parts: &[Part], args: &mut BTreeMap<String, ArgKind>) -> Result<(), String> {
    let put = |name: &String, kind: ArgKind, args: &mut BTreeMap<String, ArgKind>| {
        let now = args.entry(name.clone()).or_insert(kind);
        match (*now, kind) {
            (a, b) if a == b => Ok(()),
            (ArgKind::Text, k) => {
                *now = k;
                Ok(())
            }
            (_, ArgKind::Text) => Ok(()),
            _ => Err(name.clone()),
        }
    };
    for part in parts {
        match part {
            Part::Text(_) | Part::Pound => {}
            Part::Arg(name) => put(name, ArgKind::Text, args)?,
            Part::Number(name) => put(name, ArgKind::Number, args)?,
            Part::Plural { name, cases, .. } => {
                put(name, ArgKind::Number, args)?;
                for (_, body) in cases {
                    collect_args(body, args)?;
                }
            }
            Part::Select { name, cases } => {
                put(name, ArgKind::Select, args)?;
                for (_, body) in cases {
                    collect_args(body, args)?;
                }
            }
        }
    }
    Ok(())
}

/// Appends the ops of `parts` to `table`, arguments numbered by `order`.
fn lower(parts: &[Part], order: &[(String, ArgKind)], table: &mut Table) {
    let arg = |name: &str| {
        order
            .iter()
            .position(|(n, _)| n == name)
            .expect("a fitting message uses only its source's arguments") as u16
    };
    for part in parts {
        match part {
            Part::Text(text) => {
                let start = table.text.len() as u32;
                table.text.push_str(text);
                table.ops.push(PatternOp::Text {
                    start,
                    len: text.len() as u32,
                });
            }
            Part::Arg(name) => table.ops.push(PatternOp::Arg(arg(name))),
            Part::Number(name) => table.ops.push(PatternOp::Number(arg(name))),
            Part::Pound => table.ops.push(PatternOp::Pound),
            Part::Plural {
                name,
                ordinal,
                offset,
                cases,
            } => {
                table.ops.push(PatternOp::Plural {
                    arg: arg(name),
                    ordinal: *ordinal,
                    offset: *offset,
                    cases: cases.len() as u16,
                });
                for (key, body) in cases {
                    let key = match key {
                        PluralKey::Exact(n) => CaseKey::Exact(*n),
                        PluralKey::Category(c) => CaseKey::Category(*c),
                        PluralKey::Other => CaseKey::Other,
                    };
                    case(table, key, body, order);
                }
            }
            Part::Select { name, cases } => {
                table.ops.push(PatternOp::Select {
                    arg: arg(name),
                    cases: cases.len() as u16,
                });
                for (key, body) in cases {
                    let key = match key {
                        Some(text) => {
                            let start = table.text.len() as u32;
                            table.text.push_str(text);
                            CaseKey::Text {
                                start,
                                len: text.len() as u32,
                            }
                        }
                        None => CaseKey::Other,
                    };
                    case(table, key, body, order);
                }
            }
        }
    }
}

fn case(table: &mut Table, key: CaseKey, body: &[Part], order: &[(String, ArgKind)]) {
    let header = table.ops.len();
    table.ops.push(PatternOp::Case { key, len: 0 });
    lower(body, order, table);
    let len = (table.ops.len() - header - 1) as u32;
    table.ops[header] = PatternOp::Case { key, len };
}
