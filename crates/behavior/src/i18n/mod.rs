//! Translated messages (`viso::i18n`): a package's message catalog in its
//! compiled form, and the `MessageKey` value and `tr` function a view names.
//!
//! The compiler numbers a package's messages and compiles each locale's
//! patterns into a flat [`Table`] of [`PatternOp`]s; a [`Catalog`] holds
//! them with the module. A message a locale does not translate names the
//! pattern of the locale it falls back to, so a lookup is one index. At run
//! time a `Translate` instruction formats a message for the reader's
//! `env.locale` with the call's arguments in the message's argument order. A
//! key written in the call is its id; a `MessageKey` value is its key, which
//! survives a reload that renumbers the messages and is found by binary
//! search. No argument name is looked up. A locale's table decodes on its
//! first use.

mod translate;

use std::fmt;
use std::rc::Rc;

use viso_ende::{DecodeError, Decoder, Encoder};

use crate::native::{NativeError, NativeLibrary, NativeType};
use crate::wire::{malformed, read_list, write_list};

pub(crate) use translate::Translator;

/// The path of the `MessageKey` type.
pub const MESSAGE_KEY: &str = "viso::i18n::MessageKey";

/// The path of the `tr` function.
pub const TR: &str = "viso::i18n::tr";

/// A package's compiled messages: each message's key and argument count, by
/// id in key order, and each locale's table, the source locale first.
#[derive(Debug, Clone, PartialEq)]
pub struct Catalog {
    messages: Box<[MessageInfo]>,
    locales: Box<[LocaleTable]>,
}

/// A message: its key, for diagnostics and tools, and how many arguments a
/// call passes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageInfo {
    pub key: Box<str>,
    pub args: u16,
}

/// A locale's table as the module stores it: its BCP 47 tag and its encoded
/// [`Table`], decoded on first use.
#[derive(Debug, Clone, PartialEq)]
struct LocaleTable {
    tag: Box<str>,
    bytes: Rc<[u8]>,
}

/// One locale's patterns: the literal text they show, their ops, and where
/// each message's pattern lies.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Table {
    /// The literal text every [`PatternOp::Text`] slices.
    pub text: String,
    /// The patterns, each a run of ops.
    pub ops: Vec<PatternOp>,
    /// Each message's pattern, by id.
    pub entries: Vec<Entry>,
}

/// Where a message's pattern lies: the ops `start..start + len` of the table
/// of locale `origin`, this one or the one it falls back to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub origin: u16,
    pub start: u32,
    pub len: u32,
}

/// One step of a pattern. A `Plural` or `Select` is followed by its `cases`
/// [`PatternOp::Case`]s, each followed by its body.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PatternOp {
    /// The text `start..start + len` of the table's text.
    Text { start: u32, len: u32 },
    /// Argument `arg` as text: a `String` as is, a number in the reader's
    /// locale.
    Arg(u16),
    /// Argument `arg`, a number, in the reader's locale.
    Number(u16),
    /// The number the enclosing `plural` chose by, less its offset.
    Pound,
    /// A choice by the plural category of argument `arg` less `offset`:
    /// cardinal, or ordinal (`selectordinal`).
    Plural {
        arg: u16,
        ordinal: bool,
        offset: f64,
        cases: u16,
    },
    /// A choice by argument `arg`, a `String`.
    Select { arg: u16, cases: u16 },
    /// A case of the choice before it, whose body is the next `len` ops.
    Case { key: CaseKey, len: u32 },
}

/// What a case of a choice matches.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CaseKey {
    /// `=n`: the number itself, before the offset.
    Exact(f64),
    /// A plural category.
    Category(Category),
    /// A `select` value: the text `start..start + len` of the table's text.
    Text { start: u32, len: u32 },
    /// `other`, which every choice has.
    Other,
}

/// A CLDR plural category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Zero,
    One,
    Two,
    Few,
    Many,
}

impl Category {
    /// The category spelled `name`, `None` for `other` or another word.
    pub fn named(name: &str) -> Option<Category> {
        Some(match name {
            "zero" => Category::Zero,
            "one" => Category::One,
            "two" => Category::Two,
            "few" => Category::Few,
            "many" => Category::Many,
            _ => return None,
        })
    }

    fn tag(self) -> u8 {
        self as u8
    }

    fn from_tag(tag: u8) -> Option<Category> {
        [
            Category::Zero,
            Category::One,
            Category::Two,
            Category::Few,
            Category::Many,
        ]
        .get(usize::from(tag))
        .copied()
    }
}

/// Why a catalog is not well formed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogError(pub String);

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CatalogError {}

impl Catalog {
    /// A catalog of `messages` and each locale's table, the source locale
    /// first: every table has an entry per message naming a pattern within
    /// the table of a locale of the catalog, and every op stays within its
    /// pattern and its arguments.
    ///
    /// # Errors
    ///
    /// The first entry or op that does not.
    pub fn new(
        messages: Vec<MessageInfo>,
        locales: Vec<(String, Table)>,
    ) -> Result<Catalog, CatalogError> {
        if locales.is_empty() {
            return Err(CatalogError("a catalog has a source locale".into()));
        }
        if let Some(pair) = messages.windows(2).find(|w| w[0].key >= w[1].key) {
            return Err(CatalogError(format!(
                "the messages are not in key order at `{}`",
                pair[1].key
            )));
        }
        for (own, (tag, table)) in locales.iter().enumerate() {
            check_table(table, own as u16, &messages, &locales, tag)?;
        }
        Ok(Catalog {
            messages: messages.into(),
            locales: locales
                .into_iter()
                .map(|(tag, table)| {
                    let mut enc = Encoder::new();
                    write_table(&mut enc, &table);
                    LocaleTable {
                        tag: tag.into(),
                        bytes: enc.into_bytes().into(),
                    }
                })
                .collect(),
        })
    }

    /// The messages, by id.
    pub fn messages(&self) -> &[MessageInfo] {
        &self.messages
    }

    /// The id of the message `key`.
    pub fn position(&self, key: &str) -> Option<usize> {
        self.messages.binary_search_by(|m| (*m.key).cmp(key)).ok()
    }

    /// The locale tags, the source locale first.
    pub fn locales(&self) -> impl Iterator<Item = &str> {
        self.locales.iter().map(|l| &*l.tag)
    }

    /// The table of locale `index`, decoded.
    ///
    /// # Errors
    ///
    /// When its bytes are malformed or it does not fit the catalog.
    fn table(&self, index: usize) -> Result<Table, CatalogError> {
        let locale = &self.locales[index];
        let mut dec = Decoder::new(&locale.bytes);
        let table = read_table(&mut dec)
            .and_then(|t| dec.finish().map(|()| t))
            .map_err(|e| CatalogError(format!("the `{}` table is malformed: {e:?}", locale.tag)))?;
        check_entries(&table, &self.messages, self.locales.len(), &locale.tag)?;
        check_ops(&table, index as u16, &self.messages, &locale.tag)?;
        Ok(table)
    }

    pub(crate) fn encode(&self, enc: &mut Encoder) {
        write_list(enc, &self.messages, |enc, m| {
            enc.write_str(&m.key);
            enc.write_u16(m.args);
        });
        write_list(enc, &self.locales, |enc, l| {
            enc.write_str(&l.tag);
            enc.write_bytes(&l.bytes);
        });
    }

    pub(crate) fn decode(dec: &mut Decoder<'_>) -> Result<Catalog, DecodeError> {
        let messages = read_list(dec, |dec| {
            Ok(MessageInfo {
                key: dec.read_str()?.into(),
                args: dec.read_u16()?,
            })
        })?;
        let locales = read_list(dec, |dec| {
            Ok(LocaleTable {
                tag: dec.read_str()?.into(),
                bytes: dec.read_bytes()?.into(),
            })
        })?;
        if locales.is_empty()
            || locales.len() > usize::from(u16::MAX)
            || messages.windows(2).any(|w| w[0].key >= w[1].key)
        {
            return Err(malformed(dec));
        }
        Ok(Catalog {
            messages: messages.into(),
            locales: locales.into(),
        })
    }
}

fn check_table(
    table: &Table,
    own: u16,
    messages: &[MessageInfo],
    locales: &[(String, Table)],
    tag: &str,
) -> Result<(), CatalogError> {
    check_entries(table, messages, locales.len(), tag)?;
    for (id, entry) in table.entries.iter().enumerate() {
        let origin = &locales[usize::from(entry.origin)].1;
        let end = entry.start as usize + entry.len as usize;
        if end > origin.ops.len() {
            return Err(CatalogError(format!(
                "message {id} of `{tag}` lies past its pattern table"
            )));
        }
    }
    check_ops(table, own, messages, tag)
}

fn check_entries(
    table: &Table,
    messages: &[MessageInfo],
    locales: usize,
    tag: &str,
) -> Result<(), CatalogError> {
    if table.entries.len() != messages.len() {
        return Err(CatalogError(format!(
            "the `{tag}` table has {} entries for {} messages",
            table.entries.len(),
            messages.len()
        )));
    }
    if let Some(id) = table
        .entries
        .iter()
        .position(|e| usize::from(e.origin) >= locales)
    {
        return Err(CatalogError(format!(
            "message {id} of `{tag}` names no locale of the catalog"
        )));
    }
    Ok(())
}

/// Checks every pattern table `own` holds: each op within its pattern, each
/// argument within its message's, each text within the table's text.
fn check_ops(
    table: &Table,
    own: u16,
    messages: &[MessageInfo],
    tag: &str,
) -> Result<(), CatalogError> {
    let bad = |what: &str| CatalogError(format!("the `{tag}` table has {what}"));
    let text = |start: u32, len: u32| {
        let end = start as usize + len as usize;
        end <= table.text.len()
            && table.text.is_char_boundary(start as usize)
            && table.text.is_char_boundary(end)
    };
    let owned = table
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.origin == own);
    for (id, entry) in owned {
        let start = entry.start as usize;
        let end = start + entry.len as usize;
        let Some(ops) = table.ops.get(start..end) else {
            return Err(bad("a pattern past its ops"));
        };
        check_pattern(ops, messages[id].args, &text).map_err(bad)?;
    }
    Ok(())
}

/// Checks one pattern's structure: every choice is followed by its cases,
/// each case's body lies within the pattern, a `plural` matches numbers and
/// categories and a `select` texts, and every choice has an `other` case.
fn check_pattern(
    ops: &[PatternOp],
    args: u16,
    text: &dyn Fn(u32, u32) -> bool,
) -> Result<(), &'static str> {
    let mut i = 0;
    while i < ops.len() {
        match ops[i] {
            PatternOp::Text { start, len } if !text(start, len) => {
                return Err("text outside its text");
            }
            PatternOp::Text { .. } | PatternOp::Pound => {}
            PatternOp::Arg(a) | PatternOp::Number(a) if a >= args => {
                return Err("an argument the message does not take");
            }
            PatternOp::Arg(_) | PatternOp::Number(_) => {}
            PatternOp::Plural { arg, cases, .. } | PatternOp::Select { arg, cases } => {
                if arg >= args {
                    return Err("a choice of an argument the message does not take");
                }
                let plural = matches!(ops[i], PatternOp::Plural { .. });
                let mut other = false;
                i += 1;
                for _ in 0..cases {
                    let Some(&PatternOp::Case { key, len }) = ops.get(i) else {
                        return Err("a choice missing a case");
                    };
                    let body = ops
                        .get(i + 1..i + 1 + len as usize)
                        .ok_or("a case past its pattern")?;
                    match key {
                        CaseKey::Other => other = true,
                        CaseKey::Exact(_) | CaseKey::Category(_) if plural => {}
                        CaseKey::Text { start, len } if !plural && text(start, len) => {}
                        _ => return Err("a case key its choice does not match by"),
                    }
                    check_pattern(body, args, text)?;
                    i += 1 + len as usize;
                }
                if !other {
                    return Err("a choice without an `other` case");
                }
                continue;
            }
            PatternOp::Case { .. } => return Err("a case outside a choice"),
        }
        i += 1;
    }
    Ok(())
}

fn write_table(enc: &mut Encoder, table: &Table) {
    enc.write_str(&table.text);
    write_list(enc, &table.ops, |enc, op| match *op {
        PatternOp::Text { start, len } => {
            enc.write_u8(0);
            enc.write_varint(u64::from(start));
            enc.write_varint(u64::from(len));
        }
        PatternOp::Arg(a) => {
            enc.write_u8(1);
            enc.write_u16(a);
        }
        PatternOp::Number(a) => {
            enc.write_u8(2);
            enc.write_u16(a);
        }
        PatternOp::Pound => enc.write_u8(3),
        PatternOp::Plural {
            arg,
            ordinal,
            offset,
            cases,
        } => {
            enc.write_u8(4);
            enc.write_u16(arg);
            enc.write_bool(ordinal);
            enc.write_f64(offset);
            enc.write_u16(cases);
        }
        PatternOp::Select { arg, cases } => {
            enc.write_u8(5);
            enc.write_u16(arg);
            enc.write_u16(cases);
        }
        PatternOp::Case { key, len } => {
            enc.write_u8(6);
            match key {
                CaseKey::Exact(n) => {
                    enc.write_u8(0);
                    enc.write_f64(n);
                }
                CaseKey::Category(c) => {
                    enc.write_u8(1);
                    enc.write_u8(c.tag());
                }
                CaseKey::Text { start, len } => {
                    enc.write_u8(2);
                    enc.write_varint(u64::from(start));
                    enc.write_varint(u64::from(len));
                }
                CaseKey::Other => enc.write_u8(3),
            }
            enc.write_varint(u64::from(len));
        }
    });
    write_list(enc, &table.entries, |enc, e| {
        enc.write_u16(e.origin);
        enc.write_varint(u64::from(e.start));
        enc.write_varint(u64::from(e.len));
    });
}

fn read_u32(dec: &mut Decoder<'_>) -> Result<u32, DecodeError> {
    let offset = dec.position();
    u32::try_from(dec.read_varint()?).map_err(|_| DecodeError::Malformed { offset })
}

fn read_table(dec: &mut Decoder<'_>) -> Result<Table, DecodeError> {
    let text = dec.read_str()?.to_owned();
    let ops = read_list(dec, |dec| {
        Ok(match dec.read_u8()? {
            0 => PatternOp::Text {
                start: read_u32(dec)?,
                len: read_u32(dec)?,
            },
            1 => PatternOp::Arg(dec.read_u16()?),
            2 => PatternOp::Number(dec.read_u16()?),
            3 => PatternOp::Pound,
            4 => PatternOp::Plural {
                arg: dec.read_u16()?,
                ordinal: dec.read_bool()?,
                offset: dec.read_f64()?,
                cases: dec.read_u16()?,
            },
            5 => PatternOp::Select {
                arg: dec.read_u16()?,
                cases: dec.read_u16()?,
            },
            6 => {
                let key = match dec.read_u8()? {
                    0 => CaseKey::Exact(dec.read_f64()?),
                    1 => CaseKey::Category(
                        Category::from_tag(dec.read_u8()?).ok_or_else(|| malformed(dec))?,
                    ),
                    2 => CaseKey::Text {
                        start: read_u32(dec)?,
                        len: read_u32(dec)?,
                    },
                    3 => CaseKey::Other,
                    _ => return Err(malformed(dec)),
                };
                PatternOp::Case {
                    key,
                    len: read_u32(dec)?,
                }
            }
            _ => return Err(malformed(dec)),
        })
    })?;
    let entries = read_list(dec, |dec| {
        Ok(Entry {
            origin: dec.read_u16()?,
            start: read_u32(dec)?,
            len: read_u32(dec)?,
        })
    })?;
    Ok(Table { text, ops, entries })
}

/// `tr` is compiled into a `Translate` instruction; its native entry only
/// names it for imports.
static I18N_FUNCTIONS: [crate::native::NativeFunction; 1] =
    [crate::native!(fn "tr" |_cx| -> String {
        Err(NativeError::new("`tr` is compiled, never called"))
    })];

/// `viso::i18n`: `tr` and `MessageKey`.
pub(crate) static I18N: NativeLibrary = NativeLibrary {
    path: "viso::i18n",
    version: 1,
    functions: &I18N_FUNCTIONS,
    types: &[NativeType::value("MessageKey", &[])],
    traits: &[],
    derives: &[],
    widgets: &[],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;

    fn info(key: &str, args: u16) -> MessageInfo {
        MessageInfo {
            key: key.into(),
            args,
        }
    }

    /// `a` is "n is {n, plural, one {one} other {#}}", `b` "Hi".
    fn source() -> Table {
        let text = "n is oneHi".to_owned();
        let ops = vec![
            PatternOp::Text { start: 0, len: 5 },
            PatternOp::Plural {
                arg: 0,
                ordinal: false,
                offset: 0.0,
                cases: 2,
            },
            PatternOp::Case {
                key: CaseKey::Category(Category::One),
                len: 1,
            },
            PatternOp::Text { start: 5, len: 3 },
            PatternOp::Case {
                key: CaseKey::Other,
                len: 1,
            },
            PatternOp::Pound,
            PatternOp::Text { start: 8, len: 2 },
        ];
        let entries = vec![
            Entry {
                origin: 0,
                start: 0,
                len: 6,
            },
            Entry {
                origin: 0,
                start: 6,
                len: 1,
            },
        ];
        Table { text, ops, entries }
    }

    fn catalog() -> Catalog {
        let fr = Table {
            entries: vec![
                Entry {
                    origin: 0,
                    start: 0,
                    len: 6,
                },
                Entry {
                    origin: 1,
                    start: 0,
                    len: 1,
                },
            ],
            text: "Salut".into(),
            ops: vec![PatternOp::Text { start: 0, len: 5 }],
        };
        Catalog::new(
            vec![info("a", 1), info("b", 0)],
            vec![("en".into(), source()), ("fr".into(), fr)],
        )
        .expect("well formed")
    }

    fn locale(tag: &str) -> Value {
        Value::Agg(Rc::new(crate::Aggregate {
            tag: 0,
            fields: Box::new([Value::str(tag)]),
        }))
    }

    #[test]
    fn a_catalog_round_trips_and_decodes_its_tables_lazily() {
        let catalog = catalog();
        let mut enc = Encoder::new();
        catalog.encode(&mut enc);
        let bytes = enc.into_bytes();
        let decoded = Catalog::decode(&mut Decoder::new(&bytes)).expect("decodes");
        assert_eq!(decoded, catalog);
        assert_eq!(decoded.table(1).expect("valid").text, "Salut");
        assert_eq!(decoded.position("b"), Some(1));
        assert_eq!(decoded.position("c"), None);
    }

    #[test]
    fn a_message_formats_for_its_reader_and_falls_back_by_entry() {
        let catalog = catalog();
        let mut t = Translator::default();
        let show = |t: &mut Translator, message: Value, tag: &str, args: &[Value]| match t
            .translate(&catalog, &message, &locale(tag), args)
        {
            Ok(Value::Str(text)) => text.to_string(),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            show(&mut t, Value::Int(0), "en", &[Value::Int(1)]),
            "n is one"
        );
        assert_eq!(
            show(&mut t, Value::Int(0), "en", &[Value::Int(1200)]),
            "n is 1,200"
        );
        assert_eq!(show(&mut t, Value::Int(1), "fr-CA", &[]), "Salut");
        assert_eq!(
            show(&mut t, Value::Int(0), "fr-CA", &[Value::Int(1200)]),
            "n is 1\u{a0}200",
            "the source's pattern with the reader's digits"
        );
        assert_eq!(
            show(&mut t, Value::str("b"), "und", &[]),
            "Hi",
            "a key finds its id"
        );
        assert_eq!(
            show(&mut t, Value::str("zz"), "und", &[]),
            "zz",
            "a lost key shows itself"
        );
        assert!(
            t.translate(&catalog, &Value::Int(0), &locale("en"), &[])
                .is_err()
        );
        assert!(
            t.translate(&catalog, &Value::Int(7), &locale("en"), &[])
                .is_err()
        );
    }

    #[test]
    fn a_malformed_catalog_is_refused() {
        let unsorted = Catalog::new(
            vec![info("b", 1), info("a", 0)],
            vec![("en".into(), source())],
        );
        assert!(unsorted.unwrap_err().0.contains("key order"));
        let mut no_other = source();
        no_other.ops[4] = PatternOp::Case {
            key: CaseKey::Exact(2.0),
            len: 1,
        };
        let refused = Catalog::new(
            vec![info("a", 1), info("b", 0)],
            vec![("en".into(), no_other)],
        );
        assert!(refused.unwrap_err().0.contains("`other`"));
        let refused = Catalog::new(
            vec![info("a", 0), info("b", 0)],
            vec![("en".into(), source())],
        );
        assert!(refused.unwrap_err().0.contains("does not take"));
        let mut past = source();
        past.entries[1].len = 9;
        let refused = Catalog::new(vec![info("a", 1), info("b", 0)], vec![("en".into(), past)]);
        assert!(refused.is_err());
    }
}
