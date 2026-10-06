//! ICU MessageFormat, the syntax of a catalog's messages: literal text,
//! `{name}`, `{name, number}`, `{name, plural, ..}`, `{name, selectordinal,
//! ..}` and `{name, select, ..}`, with `#` inside a plural case and
//! apostrophe quoting (`''` is an apostrophe; `'` before `{`, `}`, `#` or `|`
//! starts quoted text up to the next lone apostrophe).

use viso_behavior::i18n::Category;

/// One piece of a message.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Part {
    Text(String),
    /// `{name}`: the argument as text.
    Arg(String),
    /// `{name, number}`.
    Number(String),
    /// `#`: the number of the plural case it is in.
    Pound,
    Plural {
        name: String,
        ordinal: bool,
        offset: f64,
        cases: Vec<(PluralKey, Vec<Part>)>,
    },
    Select {
        name: String,
        cases: Vec<(Option<String>, Vec<Part>)>,
    },
}

/// What a plural case matches.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum PluralKey {
    Exact(f64),
    Category(Category),
    Other,
}

/// Parses `text` as a message.
///
/// # Errors
///
/// What is malformed, with the byte offset it was found at.
pub(crate) fn parse(text: &str) -> Result<Vec<Part>, (usize, String)> {
    let mut parser = Parser { text, at: 0 };
    let parts = parser.message(false, 0)?;
    if parser.at < text.len() {
        return Err((parser.at, "an unmatched `}`".into()));
    }
    Ok(parts)
}

struct Parser<'t> {
    text: &'t str,
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.text[self.at..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.at += c.len_utf8();
        Some(c)
    }

    fn fail<T>(&self, message: impl Into<String>) -> Result<T, (usize, String)> {
        Err((self.at, message.into()))
    }

    fn skip_space(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }

    /// Text and arguments up to a `}` closing the enclosing case (left
    /// unread) or the end; `#` is the case's number inside a plural.
    fn message(&mut self, in_plural: bool, depth: u32) -> Result<Vec<Part>, (usize, String)> {
        if depth > 16 {
            return self.fail("choices nest too deep");
        }
        let mut parts = Vec::new();
        let mut text = String::new();
        let flush = |text: &mut String, parts: &mut Vec<Part>| {
            if !text.is_empty() {
                parts.push(Part::Text(std::mem::take(text)));
            }
        };
        while let Some(c) = self.peek() {
            match c {
                '}' => break,
                '{' => {
                    flush(&mut text, &mut parts);
                    parts.push(self.argument(depth)?);
                }
                '#' if in_plural => {
                    self.bump();
                    flush(&mut text, &mut parts);
                    parts.push(Part::Pound);
                }
                '\'' => {
                    self.bump();
                    match self.peek() {
                        Some('\'') => {
                            self.bump();
                            text.push('\'');
                        }
                        Some('{' | '}' | '|') => self.quoted(&mut text),
                        Some('#') if in_plural => self.quoted(&mut text),
                        _ => text.push('\''),
                    }
                }
                _ => {
                    self.bump();
                    text.push(c);
                }
            }
        }
        flush(&mut text, &mut parts);
        Ok(parts)
    }

    /// Quoted text after its opening apostrophe, up to the closing one; `''`
    /// inside is an apostrophe.
    fn quoted(&mut self, text: &mut String) {
        while let Some(c) = self.bump() {
            if c == '\'' {
                if self.peek() == Some('\'') {
                    self.bump();
                    text.push('\'');
                } else {
                    return;
                }
            } else {
                text.push(c);
            }
        }
    }

    fn name(&mut self) -> Result<String, (usize, String)> {
        self.skip_space();
        let start = self.at;
        while self.peek().is_some_and(|c| c.is_alphanumeric() || c == '_') {
            self.bump();
        }
        let name = &self.text[start..self.at];
        if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
            return Err((start, "an argument is named, as `{count}`".into()));
        }
        self.skip_space();
        Ok(name.to_owned())
    }

    fn word(&mut self) -> String {
        self.skip_space();
        let start = self.at;
        while self.peek().is_some_and(|c| c.is_alphanumeric() || c == '_') {
            self.bump();
        }
        let word = self.text[start..self.at].to_owned();
        self.skip_space();
        word
    }

    fn expect(&mut self, want: char) -> Result<(), (usize, String)> {
        self.skip_space();
        if self.peek() == Some(want) {
            self.bump();
            Ok(())
        } else {
            self.fail(format!("expected `{want}`"))
        }
    }

    /// `{name ..}`, at its `{`.
    fn argument(&mut self, depth: u32) -> Result<Part, (usize, String)> {
        self.bump();
        let name = self.name()?;
        match self.bump() {
            Some('}') => return Ok(Part::Arg(name)),
            Some(',') => {}
            _ => return self.fail("expected `,` or `}` after the argument name"),
        }
        let at = self.at;
        let kind = self.word();
        match kind.as_str() {
            "number" => {
                if self.peek() == Some(',') {
                    self.bump();
                    let style = self.word();
                    if style != "integer" {
                        return Err((
                            at,
                            format!(
                                "the number style `{style}` is not supported; use `{{{name}, number}}`"
                            ),
                        ));
                    }
                }
                self.expect('}')?;
                Ok(Part::Number(name))
            }
            "plural" | "selectordinal" => {
                self.expect(',')?;
                self.plural(name, kind == "selectordinal", depth)
            }
            "select" => {
                self.expect(',')?;
                self.select(name, depth)
            }
            "" => self.fail("expected an argument type"),
            other => Err((
                at,
                format!(
                    "the argument type `{other}` is not supported: use `number`, `plural`, \
                     `selectordinal` or `select`"
                ),
            )),
        }
    }

    fn case_body(&mut self, in_plural: bool, depth: u32) -> Result<Vec<Part>, (usize, String)> {
        self.expect('{')?;
        let body = self.message(in_plural, depth + 1)?;
        self.expect('}')?;
        self.skip_space();
        Ok(body)
    }

    fn plural(&mut self, name: String, ordinal: bool, depth: u32) -> Result<Part, (usize, String)> {
        self.skip_space();
        let mut offset = 0.0;
        if self.text[self.at..].starts_with("offset:") {
            self.at += "offset:".len();
            self.skip_space();
            offset = self.number()?;
        }
        let mut cases: Vec<(PluralKey, Vec<Part>)> = Vec::new();
        loop {
            self.skip_space();
            let at = self.at;
            let key = match self.peek() {
                Some('}') => break,
                Some('=') => {
                    self.bump();
                    PluralKey::Exact(self.number()?)
                }
                _ => {
                    let word = self.word();
                    if word == "other" {
                        PluralKey::Other
                    } else if let Some(c) = Category::named(&word) {
                        PluralKey::Category(c)
                    } else {
                        return Err((
                            at,
                            format!(
                                "`{word}` is no plural case: use `=n`, `zero`, `one`, `two`, \
                                 `few`, `many` or `other`"
                            ),
                        ));
                    }
                }
            };
            if cases.iter().any(|(k, _)| *k == key) {
                return Err((at, "a case is given twice".into()));
            }
            let body = self.case_body(true, depth)?;
            cases.push((key, body));
        }
        if !cases.iter().any(|(k, _)| *k == PluralKey::Other) {
            return self.fail("a plural needs an `other` case");
        }
        self.bump();
        Ok(Part::Plural {
            name,
            ordinal,
            offset,
            cases,
        })
    }

    fn select(&mut self, name: String, depth: u32) -> Result<Part, (usize, String)> {
        let mut cases: Vec<(Option<String>, Vec<Part>)> = Vec::new();
        loop {
            self.skip_space();
            if self.peek() == Some('}') {
                break;
            }
            let at = self.at;
            let word = self.word();
            if word.is_empty() {
                return self.fail("expected a select case");
            }
            let key = (word != "other").then_some(word);
            if cases.iter().any(|(k, _)| *k == key) {
                return Err((at, "a case is given twice".into()));
            }
            let body = self.case_body(false, depth)?;
            cases.push((key, body));
        }
        if !cases.iter().any(|(k, _)| k.is_none()) {
            return self.fail("a select needs an `other` case");
        }
        self.bump();
        Ok(Part::Select { name, cases })
    }

    fn number(&mut self) -> Result<f64, (usize, String)> {
        let start = self.at;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_digit() || c == '.' || c == '-')
        {
            self.bump();
        }
        self.text[start..self.at]
            .parse()
            .map_err(|_| (start, "expected a number".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_arguments_and_quoting() {
        assert_eq!(
            parse("Hi {name}, it''s '{x}' #").unwrap(),
            [
                Part::Text("Hi ".into()),
                Part::Arg("name".into()),
                Part::Text(", it's {x} #".into()),
            ]
        );
        assert_eq!(parse("{n, number}").unwrap(), [Part::Number("n".into())]);
    }

    #[test]
    fn plurals_and_selects() {
        let parts =
            parse("{count, plural, offset:1 =0 {none} one {# item} other {# items}}").unwrap();
        let [Part::Plural { cases, offset, .. }] = &parts[..] else {
            panic!("{parts:?}")
        };
        assert_eq!(*offset, 1.0);
        assert_eq!(cases.len(), 3);
        assert_eq!(cases[1].1, [Part::Pound, Part::Text(" item".into())]);
        let parts = parse("{g, select, female {she} other {they}}").unwrap();
        assert!(matches!(&parts[..], [Part::Select { cases, .. }] if cases.len() == 2));
    }

    #[test]
    fn malformed_messages_say_where() {
        assert!(
            parse("{count, plural, one {x}}")
                .unwrap_err()
                .1
                .contains("other")
        );
        assert!(
            parse("{count, date}")
                .unwrap_err()
                .1
                .contains("not supported")
        );
        assert!(parse("{0}").unwrap_err().1.contains("named"));
        assert!(parse("a } b").unwrap_err().1.contains("unmatched"));
        assert!(
            parse("{n, plural, lots {x} other {y}}")
                .unwrap_err()
                .1
                .contains("no plural case")
        );
    }
}
