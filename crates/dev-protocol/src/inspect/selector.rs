//! The selectors nodes are found by.
//!
//! ```text
//! selector   = complex { "," complex }
//! complex    = compound { [ ">" ] compound }      descendant, or child with `>`
//! compound   = ( type | "*" ) { part } | part { part }
//! part       = "#" name | "." style | "[" attribute [ "=" value ] "]"
//! type       = name { "::" name }
//! value      = name | number | "…"
//! ```
//!
//! A type matches a node of that widget type or the root node of that
//! component, `module::Component` naming one by its module; `#name` the node
//! `node name: Type { … }` names; `.Style` a node the style is applied to.
//! An attribute reads the node's semantics or state: `[role=button]`,
//! `[label="Save"]`; `[checked]` alone holds of a checked node, `[label]` of
//! one with a label.

use std::fmt;

use super::tree::{Inspected, Inspection};
use viso_schema::view::source_name;

/// A parsed selector: nodes matching any of its alternatives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selector {
    alternatives: Vec<Complex>,
}

/// Compounds left to right, each but the first joined to the one before.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Complex {
    first: Compound,
    rest: Vec<(Combinator, Compound)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Combinator {
    Descendant,
    Child,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Compound {
    /// `None` for `*` or none written.
    ty: Option<String>,
    names: Vec<String>,
    styles: Vec<String>,
    attributes: Vec<(Attribute, Option<String>)>,
}

/// What an attribute selector reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribute {
    Role,
    Label,
    Hint,
    Value,
    Checked,
    Selected,
    Expanded,
    Focused,
    Focusable,
    Shown,
    Kind,
}

impl Attribute {
    /// Every attribute, by the name a selector spells it with.
    pub const ALL: [(&'static str, Attribute); 11] = [
        ("role", Attribute::Role),
        ("label", Attribute::Label),
        ("hint", Attribute::Hint),
        ("value", Attribute::Value),
        ("checked", Attribute::Checked),
        ("selected", Attribute::Selected),
        ("expanded", Attribute::Expanded),
        ("focused", Attribute::Focused),
        ("focusable", Attribute::Focusable),
        ("shown", Attribute::Shown),
        ("kind", Attribute::Kind),
    ];

    /// The name a selector spells it with.
    pub fn name(self) -> &'static str {
        Attribute::ALL
            .iter()
            .find(|(_, a)| *a == self)
            .map_or("", |(name, _)| name)
    }

    /// Its value on `node`, as text, `None` where the node has none.
    pub fn read(self, node: &Inspected<'_>) -> Option<String> {
        let census = node.node;
        let semantics = census.semantics.as_ref();
        let flag = |b: bool| Some(b.to_string());
        match self {
            Attribute::Role => semantics.map(|s| source_name(s.role).to_owned()),
            Attribute::Label => semantics.and_then(|s| s.label.clone()),
            Attribute::Hint => semantics.and_then(|s| s.hint.clone()),
            Attribute::Value => semantics.and_then(|s| {
                s.value
                    .clone()
                    .or_else(|| s.state.value.map(|v| v.to_string()))
            }),
            Attribute::Checked => semantics.and_then(|s| s.state.checked).and_then(flag),
            Attribute::Selected => semantics.and_then(|s| s.state.selected).and_then(flag),
            Attribute::Expanded => semantics.and_then(|s| s.state.expanded).and_then(flag),
            Attribute::Focused => flag(census.focused),
            Attribute::Focusable => flag(census.focusable),
            Attribute::Shown => flag(census.shown),
            Attribute::Kind => Some(census.kind.label().to_owned()),
        }
    }

    /// Whether `[name]` alone holds of `node`.
    fn holds(self, node: &Inspected<'_>) -> bool {
        match self.read(node) {
            Some(value) => value != "false" && !value.is_empty(),
            None => false,
        }
    }
}

/// Why a selector does not parse: the byte it fails at and what is wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectorError {
    pub at: usize,
    pub message: String,
}

impl fmt::Display for SelectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (at byte {})", self.message, self.at)
    }
}

impl std::error::Error for SelectorError {}

impl Selector {
    /// Parses `text`.
    ///
    /// # Errors
    ///
    /// Where and why `text` is not a selector.
    pub fn parse(text: &str) -> Result<Selector, SelectorError> {
        let mut parser = Parser { text, at: 0 };
        let mut alternatives = vec![parser.complex()?];
        loop {
            parser.skip_space();
            match parser.peek() {
                None => break,
                Some(',') => {
                    parser.at += 1;
                    alternatives.push(parser.complex()?);
                }
                Some(c) => return Err(parser.error(format!("unexpected `{c}`"))),
            }
        }
        Ok(Selector { alternatives })
    }

    /// Every node of `inspection` the selector matches, in order.
    pub fn select(&self, inspection: &Inspection<'_>) -> Vec<usize> {
        (0..inspection.nodes.len())
            .filter(|&at| self.matches(inspection, at))
            .collect()
    }

    /// Whether it matches node `at` of `inspection`.
    pub fn matches(&self, inspection: &Inspection<'_>, at: usize) -> bool {
        self.alternatives
            .iter()
            .any(|complex| complex.matches(inspection, complex.rest.len(), at))
    }
}

impl Complex {
    /// The compound `k` (0 the first) by its place in the chain.
    fn compound(&self, k: usize) -> &Compound {
        match k {
            0 => &self.first,
            k => &self.rest[k - 1].1,
        }
    }

    /// Whether compound `k` matches node `at` and the compounds before it
    /// its ancestors as their combinators ask.
    fn matches(&self, inspection: &Inspection<'_>, k: usize, at: usize) -> bool {
        if !self.compound(k).matches(&inspection.nodes[at]) {
            return false;
        }
        if k == 0 {
            return true;
        }
        let mut parent = inspection.nodes[at].parent;
        match self.rest[k - 1].0 {
            Combinator::Child => parent.is_some_and(|p| self.matches(inspection, k - 1, p)),
            Combinator::Descendant => {
                while let Some(p) = parent {
                    if self.matches(inspection, k - 1, p) {
                        return true;
                    }
                    parent = inspection.nodes[p].parent;
                }
                false
            }
        }
    }
}

impl Compound {
    fn matches(&self, node: &Inspected<'_>) -> bool {
        let source = node.source.map(|(_, source)| source);
        if let Some(ty) = &self.ty {
            let Some((file, source)) = node.source else {
                return false;
            };
            let typed = match ty.rsplit_once("::") {
                Some((module, name)) => {
                    file.module.join("::") == module && source.components.iter().any(|c| c == name)
                }
                None => source.type_name == *ty || source.components.iter().any(|c| c == ty),
            };
            if !typed {
                return false;
            }
        }
        if !self.names.is_empty()
            && !self
                .names
                .iter()
                .all(|name| source.and_then(|s| s.name.as_ref()) == Some(name))
        {
            return false;
        }
        if !self
            .styles
            .iter()
            .all(|style| source.is_some_and(|s| s.styles.contains(style)))
        {
            return false;
        }
        self.attributes
            .iter()
            .all(|(attribute, value)| match value {
                None => attribute.holds(node),
                Some(value) => attribute.read(node).as_ref() == Some(value),
            })
    }
}

struct Parser<'a> {
    text: &'a str,
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.text[self.at..].chars().next()
    }

    fn error(&self, message: impl Into<String>) -> SelectorError {
        SelectorError {
            at: self.at,
            message: message.into(),
        }
    }

    fn skip_space(&mut self) -> bool {
        let start = self.at;
        while self.peek().is_some_and(char::is_whitespace) {
            self.at += self.peek().map_or(0, char::len_utf8);
        }
        self.at > start
    }

    fn complex(&mut self) -> Result<Complex, SelectorError> {
        self.skip_space();
        let first = self.compound()?;
        let mut rest = Vec::new();
        loop {
            let spaced = self.skip_space();
            let combinator = match self.peek() {
                Some('>') => {
                    self.at += 1;
                    self.skip_space();
                    Combinator::Child
                }
                Some(',') | None => break,
                Some(_) if spaced => Combinator::Descendant,
                Some(c) => return Err(self.error(format!("unexpected `{c}`"))),
            };
            rest.push((combinator, self.compound()?));
        }
        Ok(Complex { first, rest })
    }

    fn compound(&mut self) -> Result<Compound, SelectorError> {
        let mut compound = Compound::default();
        let mut any = false;
        match self.peek() {
            Some('*') => {
                self.at += 1;
                any = true;
            }
            Some(c) if is_name_start(c) => {
                let mut ty = self.name("a type")?;
                while self.text[self.at..].starts_with("::") {
                    self.at += 2;
                    ty.push_str("::");
                    ty.push_str(&self.name("a name after `::`")?);
                }
                compound.ty = Some(ty);
                any = true;
            }
            _ => {}
        }
        loop {
            match self.peek() {
                Some('#') => {
                    self.at += 1;
                    compound.names.push(self.name("a node name after `#`")?);
                }
                Some('.') => {
                    self.at += 1;
                    compound.styles.push(self.name("a style name after `.`")?);
                }
                Some('[') => {
                    self.at += 1;
                    compound.attributes.push(self.attribute()?);
                }
                _ => break,
            }
            any = true;
        }
        if !any {
            return Err(match self.peek() {
                None => self.error("expected a type, `#name`, `.Style` or `[attribute]`"),
                Some(c) => self.error(format!(
                    "expected a type, `#name`, `.Style` or `[attribute]`, found `{c}`"
                )),
            });
        }
        Ok(compound)
    }

    fn attribute(&mut self) -> Result<(Attribute, Option<String>), SelectorError> {
        self.skip_space();
        let start = self.at;
        let name = self.name("an attribute name")?;
        let Some(attribute) = Attribute::ALL
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, a)| *a)
        else {
            let names: Vec<&str> = Attribute::ALL.iter().map(|(n, _)| *n).collect();
            return Err(SelectorError {
                at: start,
                message: format!("no attribute `{name}`; one of {}", names.join(", ")),
            });
        };
        self.skip_space();
        let value = if self.peek() == Some('=') {
            self.at += 1;
            self.skip_space();
            let value = self.value()?;
            self.skip_space();
            Some(value)
        } else {
            None
        };
        if self.peek() != Some(']') {
            return Err(self.error("expected `]`"));
        }
        self.at += 1;
        Ok((attribute, value))
    }

    fn value(&mut self) -> Result<String, SelectorError> {
        if self.peek() != Some('"') {
            let start = self.at;
            while self
                .peek()
                .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
            {
                self.at += self.peek().map_or(0, char::len_utf8);
            }
            if self.at == start {
                return Err(self.error("expected a value: a name, a number or a quoted string"));
            }
            return Ok(self.text[start..self.at].to_owned());
        }
        let open = self.at;
        self.at += 1;
        let mut value = String::new();
        loop {
            match self.peek() {
                None => {
                    return Err(SelectorError {
                        at: open,
                        message: "unterminated string".into(),
                    });
                }
                Some('"') => {
                    self.at += 1;
                    return Ok(value);
                }
                Some('\\') => {
                    self.at += 1;
                    match self.peek() {
                        Some(c @ ('"' | '\\')) => {
                            value.push(c);
                            self.at += 1;
                        }
                        _ => return Err(self.error("only `\\\"` and `\\\\` escape in a string")),
                    }
                }
                Some(c) => {
                    value.push(c);
                    self.at += c.len_utf8();
                }
            }
        }
    }

    fn name(&mut self, what: &str) -> Result<String, SelectorError> {
        let start = self.at;
        if self.text[self.at..].starts_with("r#") {
            self.at += 2;
        }
        let body = self.at;
        if !self.peek().is_some_and(is_name_start) {
            self.at = start;
            return Err(self.error(format!("expected {what}")));
        }
        while self.peek().is_some_and(|c| c.is_alphanumeric() || c == '_') {
            self.at += self.peek().map_or(0, char::len_utf8);
        }
        Ok(self.text[body..self.at].to_owned())
    }
}

fn is_name_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

impl fmt::Display for Selector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, complex) in self.alternatives.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            complex.first.fmt(f)?;
            for (combinator, compound) in &complex.rest {
                f.write_str(match combinator {
                    Combinator::Descendant => " ",
                    Combinator::Child => " > ",
                })?;
                compound.fmt(f)?;
            }
        }
        Ok(())
    }
}

impl fmt::Display for Compound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bare = self.names.is_empty() && self.styles.is_empty() && self.attributes.is_empty();
        match &self.ty {
            Some(ty) => f.write_str(ty)?,
            None if bare => f.write_str("*")?,
            None => {}
        }
        for name in &self.names {
            write!(f, "#{name}")?;
        }
        for style in &self.styles {
            write!(f, ".{style}")?;
        }
        for (attribute, value) in &self.attributes {
            match value {
                Some(value) => write!(
                    f,
                    "[{}=\"{}\"]",
                    attribute.name(),
                    value.replace('\\', "\\\\").replace('"', "\\\"")
                )?,
                None => write!(f, "[{}]", attribute.name())?,
            }
        }
        Ok(())
    }
}
