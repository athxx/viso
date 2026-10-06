//! Formatting a message for a reader: the locale tag a reader reads resolves
//! once to the catalog locale it falls back to and the number formatter of
//! its own locale; a locale's table decodes on first use, with the plural
//! rules of its language; a message of plain text is one shared string.

use std::fmt::Write as _;
use std::rc::Rc;

use fixed_decimal::{Decimal, FloatPrecision};
use icu_decimal::DecimalFormatter;
use icu_locale_core::Locale;
use icu_locale_fallback::LocaleFallbacker;
use icu_plurals::{PluralCategory, PluralRules};

use super::{CaseKey, Catalog, Category, PatternOp, Table};
use crate::Value;

/// The readers a VM remembers the resolution of; a view shows a handful of
/// locales at most.
const READERS: usize = 8;

/// A VM's translation state: the tables it decoded and the readers it
/// resolved.
#[derive(Default)]
pub(crate) struct Translator {
    tables: Vec<Option<Decoded>>,
    readers: Vec<Reader>,
    scratch: String,
}

impl std::fmt::Debug for Translator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Translator")
            .field(
                "tables",
                &self.tables.iter().filter(|t| t.is_some()).count(),
            )
            .field("readers", &self.readers.len())
            .finish()
    }
}

struct Decoded {
    table: Table,
    /// The value of each message whose pattern here is plain text, made on
    /// first use.
    plain: Vec<Option<Value>>,
    cardinal: PluralRules,
    ordinal: PluralRules,
}

struct Reader {
    tag: Rc<String>,
    /// The catalog locale it reads.
    table: usize,
    numbers: DecimalFormatter,
}

impl Translator {
    /// Message `message` of `catalog`, its id or a `MessageKey`'s key, for
    /// the reader whose `env.locale` is `locale`, with `args` in the
    /// message's argument order. A key the catalog does not have shows as
    /// itself.
    ///
    /// # Errors
    ///
    /// What does not fit: an id or arguments the catalog does not have, a
    /// malformed table, an argument of the wrong kind.
    pub(crate) fn translate(
        &mut self,
        catalog: &Catalog,
        message: &Value,
        locale: &Value,
        args: &[Value],
    ) -> Result<Value, String> {
        let id = match message {
            Value::Int(n) => usize::try_from(*n)
                .ok()
                .filter(|&id| id < catalog.messages.len())
                .ok_or_else(|| format!("message {n} is not in the catalog"))?,
            Value::Str(key) => match catalog.position(key) {
                Some(id) => id,
                None => return Ok(message.clone()),
            },
            _ => return Err("a message is named by its id or key".into()),
        };
        let info = &catalog.messages[id];
        if args.len() != usize::from(info.args) {
            return Err(format!(
                "`{}` takes {} arguments, not {}",
                info.key,
                info.args,
                args.len()
            ));
        }
        if self.tables.len() != catalog.locales.len() {
            self.tables = (0..catalog.locales.len()).map(|_| None).collect();
        }
        let reader = self.reader(catalog, locale);
        let read = self.readers[reader].table;
        self.decode(catalog, read)?;
        let entry = self.tables[read].as_ref().map(|t| t.table.entries[id]);
        let entry = entry.ok_or("the table did not decode")?;
        let origin = usize::from(entry.origin);
        self.decode(catalog, origin)?;
        let Some(decoded) = self.tables[origin].as_mut() else {
            return Err("the table did not decode".into());
        };
        let start = entry.start as usize;
        let ops = &decoded.table.ops[start..start + entry.len as usize];
        if let Some(value) = &decoded.plain[id] {
            return Ok(value.clone());
        }
        let plain = match ops {
            [] => Some(""),
            [PatternOp::Text { start, len }] => {
                let (start, len) = (*start as usize, *len as usize);
                Some(&decoded.table.text[start..start + len])
            }
            _ => None,
        };
        if let Some(text) = plain {
            let value = Value::str(text);
            decoded.plain[id] = Some(value.clone());
            return Ok(value);
        }
        let cx = Cx {
            text: &decoded.table.text,
            numbers: &self.readers[reader].numbers,
            cardinal: &decoded.cardinal,
            ordinal: &decoded.ordinal,
        };
        self.scratch.clear();
        cx.write(&mut self.scratch, ops, args, None)?;
        Ok(Value::Str(Rc::new(self.scratch.clone())))
    }

    /// The index of the reader of `locale`, resolving it the first time.
    fn reader(&mut self, catalog: &Catalog, locale: &Value) -> usize {
        let tag = match locale {
            Value::Agg(agg) => match agg.fields.first() {
                Some(Value::Str(tag)) => Some(tag),
                _ => None,
            },
            _ => None,
        };
        let found = self.readers.iter().position(|r| match tag {
            Some(tag) => Rc::ptr_eq(&r.tag, tag) || r.tag == *tag,
            None => r.tag.is_empty(),
        });
        if let Some(found) = found {
            return found;
        }
        let tag = tag.cloned().unwrap_or_default();
        let parsed = tag.parse::<Locale>().unwrap_or(Locale::UNKNOWN);
        let table = resolve(catalog, &parsed);
        let numbers = DecimalFormatter::try_new((&parsed).into(), Default::default())
            .or_else(|_| DecimalFormatter::try_new(Default::default(), Default::default()))
            .expect("the compiled data formats the root locale");
        if self.readers.len() == READERS {
            self.readers.remove(0);
        }
        self.readers.push(Reader {
            tag,
            table,
            numbers,
        });
        self.readers.len() - 1
    }

    /// Decodes the table of locale `index` unless it is.
    fn decode(&mut self, catalog: &Catalog, index: usize) -> Result<(), String> {
        if self.tables[index].is_some() {
            return Ok(());
        }
        let table = catalog.table(index).map_err(|e| e.0)?;
        let locale = catalog.locales[index]
            .tag
            .parse::<Locale>()
            .unwrap_or(Locale::UNKNOWN);
        let rules = |ordinal: bool| {
            let made = if ordinal {
                PluralRules::try_new_ordinal((&locale).into())
            } else {
                PluralRules::try_new_cardinal((&locale).into())
            };
            made.or_else(|_| {
                if ordinal {
                    PluralRules::try_new_ordinal(Default::default())
                } else {
                    PluralRules::try_new_cardinal(Default::default())
                }
            })
            .map_err(|e| format!("no plural rules for `{locale}`: {e}"))
        };
        self.tables[index] = Some(Decoded {
            plain: vec![None; table.entries.len()],
            table,
            cardinal: rules(false)?,
            ordinal: rules(true)?,
        });
        Ok(())
    }
}

/// The catalog locale `locale` reads: the first along its CLDR fallback chain
/// the catalog has, else the source locale.
fn resolve(catalog: &Catalog, locale: &Locale) -> usize {
    let fallbacker = LocaleFallbacker::new();
    let mut chain = fallbacker
        .for_config(Default::default())
        .fallback_for(locale.into());
    loop {
        let at = chain.get();
        if at.is_unknown() {
            return 0;
        }
        let tag = at.to_string();
        if let Some(found) = catalog.locales.iter().position(|l| *l.tag == *tag) {
            return found;
        }
        chain.step();
    }
}

/// What formatting a pattern reads.
struct Cx<'a> {
    text: &'a str,
    numbers: &'a DecimalFormatter,
    cardinal: &'a PluralRules,
    ordinal: &'a PluralRules,
}

impl Cx<'_> {
    fn write(
        &self,
        out: &mut String,
        ops: &[PatternOp],
        args: &[Value],
        pound: Option<&Decimal>,
    ) -> Result<(), String> {
        let mut i = 0;
        while i < ops.len() {
            match ops[i] {
                PatternOp::Text { start, len } => {
                    out.push_str(&self.text[start as usize..(start + len) as usize]);
                }
                PatternOp::Arg(a) => match &args[usize::from(a)] {
                    Value::Str(text) => out.push_str(text),
                    other => self.number(out, &decimal(other)?),
                },
                PatternOp::Number(a) => self.number(out, &decimal(&args[usize::from(a)])?),
                PatternOp::Pound => match pound {
                    Some(n) => self.number(out, n),
                    None => out.push('#'),
                },
                PatternOp::Plural {
                    arg,
                    ordinal,
                    offset,
                    cases,
                } => {
                    let value = &args[usize::from(arg)];
                    let exact = match value {
                        Value::Int(n) => *n as f64,
                        Value::Float(x) => *x,
                        _ => return Err("a `plural` argument is a number".into()),
                    };
                    let shown = if offset == 0.0 {
                        decimal(value)?
                    } else {
                        decimal(&Value::Float(exact - offset))?
                    };
                    let rules = if ordinal { self.ordinal } else { self.cardinal };
                    let category = category(rules.category_for(&shown));
                    let (body, next) = choose(ops, i, cases, |key| match key {
                        CaseKey::Exact(n) => (n == exact).then_some(0),
                        CaseKey::Category(c) => (Some(c) == category).then_some(1),
                        _ => None,
                    });
                    self.write(out, body, args, Some(&shown))?;
                    i = next;
                    continue;
                }
                PatternOp::Select { arg, cases } => {
                    let Value::Str(chosen) = &args[usize::from(arg)] else {
                        return Err("a `select` argument is text".into());
                    };
                    let (body, next) = choose(ops, i, cases, |key| match key {
                        CaseKey::Text { start, len } => {
                            (self.text[start as usize..(start + len) as usize] == **chosen)
                                .then_some(0)
                        }
                        _ => None,
                    });
                    self.write(out, body, args, pound)?;
                    i = next;
                    continue;
                }
                PatternOp::Case { .. } => return Err("a case outside a choice".into()),
            }
            i += 1;
        }
        Ok(())
    }

    fn number(&self, out: &mut String, n: &Decimal) {
        let _ = write!(out, "{}", self.numbers.format(n));
    }
}

/// The body of the case the choice at `at` with `cases` cases picks and the
/// op after the choice: the case `rank` ranks lowest (exact before
/// category), else `other`.
fn choose(
    ops: &[PatternOp],
    at: usize,
    cases: u16,
    rank: impl Fn(CaseKey) -> Option<u8>,
) -> (&[PatternOp], usize) {
    let mut i = at + 1;
    let mut best: Option<(u8, usize, usize)> = None;
    let mut other = (i, i);
    for _ in 0..cases {
        let PatternOp::Case { key, len } = ops[i] else {
            break;
        };
        let body = (i + 1, i + 1 + len as usize);
        if key == CaseKey::Other {
            other = body;
        } else if let Some(r) = rank(key)
            && best.is_none_or(|(b, _, _)| r < b)
        {
            best = Some((r, body.0, body.1));
        }
        i = body.1;
    }
    let (start, end) = best.map_or(other, |(_, s, e)| (s, e));
    (&ops[start..end], i)
}

fn category(c: PluralCategory) -> Option<Category> {
    match c {
        PluralCategory::Zero => Some(Category::Zero),
        PluralCategory::One => Some(Category::One),
        PluralCategory::Two => Some(Category::Two),
        PluralCategory::Few => Some(Category::Few),
        PluralCategory::Many => Some(Category::Many),
        PluralCategory::Other => None,
    }
}

fn decimal(value: &Value) -> Result<Decimal, String> {
    match value {
        Value::Int(n) => Ok(Decimal::from(*n)),
        Value::Float(x) => Decimal::try_from_f64(*x, FloatPrecision::RoundTrip)
            .map_err(|_| format!("{x} is not a finite number")),
        _ => Err("a number argument is a number".into()),
    }
}
