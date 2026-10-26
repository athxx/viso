//! A small XML reader and writer for the files packages carry for their
//! platform: elements, attributes and text, each element with the bytes it
//! spans. Comments, processing instructions and the document type are
//! skipped; entities are the five predefined ones and character references.

use std::fmt::Write as _;
use std::ops::Range;

/// An element.
#[derive(Debug, Clone)]
pub(crate) struct Element {
    pub(crate) name: String,
    /// Each attribute, in order, its value decoded.
    pub(crate) attrs: Vec<(String, String)>,
    pub(crate) children: Vec<Node>,
    /// From the `<` of its start tag to past the `>` of its end tag.
    pub(crate) span: Range<usize>,
    /// Where its end tag starts; the end of `span` for an empty element.
    pub(crate) content_end: usize,
}

/// What an element holds.
#[derive(Debug, Clone)]
pub(crate) enum Node {
    Element(Element),
    /// Text, decoded.
    Text(String),
}

/// Why a text is not XML: what, and the byte it was found at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Error {
    pub(crate) at: usize,
    pub(crate) message: String,
}

impl Element {
    /// The value of the attribute `name`.
    pub(crate) fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// The elements it holds.
    pub(crate) fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|node| match node {
            Node::Element(element) => Some(element),
            Node::Text(_) => None,
        })
    }

    /// It with `from` replaced by `to` in every attribute value and text it
    /// holds.
    pub(crate) fn replace(&mut self, from: &str, to: &str) {
        for (_, value) in &mut self.attrs {
            if value.contains(from) {
                *value = value.replace(from, to);
            }
        }
        for child in &mut self.children {
            match child {
                Node::Element(element) => element.replace(from, to),
                Node::Text(text) if text.contains(from) => *text = text.replace(from, to),
                Node::Text(_) => {}
            }
        }
    }

    /// Its text, the text nodes it holds joined.
    pub(crate) fn text(&self) -> String {
        self.children
            .iter()
            .filter_map(|node| match node {
                Node::Text(text) => Some(text.as_str()),
                Node::Element(_) => None,
            })
            .collect()
    }

    /// The first text it holds that is not whitespace: what a holder of
    /// elements alone must not have.
    pub(crate) fn stray_text(&self) -> Option<&str> {
        self.children.iter().find_map(|node| match node {
            Node::Text(text) if !text.trim().is_empty() => Some(text.trim()),
            _ => None,
        })
    }

    /// Whether it says the same as `other`: the same name, the same
    /// attributes in any order, and the same children, text compared
    /// without its surrounding whitespace.
    pub(crate) fn same(&self, other: &Element) -> bool {
        fn sorted(e: &Element) -> Vec<&(String, String)> {
            let mut attrs: Vec<&(String, String)> = e.attrs.iter().collect();
            attrs.sort();
            attrs
        }
        let content = |e: &Element| -> Vec<Node> {
            e.children
                .iter()
                .filter(|n| !matches!(n, Node::Text(t) if t.trim().is_empty()))
                .cloned()
                .collect()
        };
        self.name == other.name && sorted(self) == sorted(other) && {
            let (a, b) = (content(self), content(other));
            a.len() == b.len()
                && a.iter().zip(&b).all(|pair| match pair {
                    (Node::Element(a), Node::Element(b)) => a.same(b),
                    (Node::Text(a), Node::Text(b)) => a.trim() == b.trim(),
                    _ => false,
                })
        }
    }

    /// It written at `indent` (each level four spaces deeper), without its
    /// whitespace-only text.
    pub(crate) fn write(&self, indent: usize, out: &mut String) {
        let pad = " ".repeat(indent);
        let _ = write!(out, "{pad}<{}", self.name);
        for (name, value) in &self.attrs {
            let _ = write!(out, " {name}=\"{}\"", escape(value, true));
        }
        let children: Vec<&Node> = self
            .children
            .iter()
            .filter(|n| !matches!(n, Node::Text(t) if t.trim().is_empty()))
            .collect();
        if children.is_empty() {
            out.push_str(" />");
            return;
        }
        if let [Node::Text(text)] = children[..] {
            let _ = write!(out, ">{}</{}>", escape(text.trim(), false), self.name);
            return;
        }
        out.push('>');
        for child in children {
            out.push('\n');
            match child {
                Node::Element(element) => element.write(indent + 4, out),
                Node::Text(text) => {
                    let _ = write!(out, "{pad}    {}", escape(text.trim(), false));
                }
            }
        }
        let _ = write!(out, "\n{pad}</{}>", self.name);
    }
}

/// `text` with what XML reserves escaped; quotes too in an attribute.
pub(crate) fn escape(text: &str, attribute: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// The root element of the document `text`.
pub(crate) fn parse(text: &str) -> Result<Element, Error> {
    let mut reader = Reader { text, at: 0 };
    reader.misc()?;
    if !reader.rest().starts_with('<') {
        return Err(reader.error("no element"));
    }
    let root = reader.element()?;
    reader.misc()?;
    if reader.at < text.len() {
        return Err(reader.error("more after the root element"));
    }
    Ok(root)
}

struct Reader<'a> {
    text: &'a str,
    at: usize,
}

impl Reader<'_> {
    fn rest(&self) -> &str {
        &self.text[self.at..]
    }

    fn error(&self, message: impl Into<String>) -> Error {
        Error {
            at: self.at,
            message: message.into(),
        }
    }

    fn skip_space(&mut self) {
        let rest = self.rest();
        self.at += rest.len() - rest.trim_start().len();
    }

    /// Past `end`, which must come.
    fn skip_past(&mut self, end: &str, what: &str) -> Result<(), Error> {
        match self.rest().find(end) {
            Some(at) => {
                self.at += at + end.len();
                Ok(())
            }
            None => Err(self.error(format!("{what} does not end"))),
        }
    }

    /// Past the whitespace, comments, processing instructions and document
    /// type around the root element.
    fn misc(&mut self) -> Result<(), Error> {
        loop {
            self.skip_space();
            let rest = self.rest();
            if rest.starts_with("<!--") {
                self.skip_past("-->", "a comment")?;
            } else if rest.starts_with("<?") {
                self.skip_past("?>", "a processing instruction")?;
            } else if rest.starts_with("<!DOCTYPE") {
                let end = match (rest.find('['), rest.find('>')) {
                    (Some(open), Some(close)) if open < close => "]>",
                    _ => ">",
                };
                self.skip_past(end, "the document type")?;
            } else {
                return Ok(());
            }
        }
    }

    /// A name: up to whitespace, `/`, `>` or `=`.
    fn name(&mut self) -> Result<String, Error> {
        let rest = self.rest();
        let len = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '/' | '>' | '=' | '<'))
            .unwrap_or(rest.len());
        if len == 0 {
            return Err(self.error("a name is missing"));
        }
        let name = rest[..len].to_owned();
        self.at += len;
        Ok(name)
    }

    /// The element starting here, at its `<`.
    fn element(&mut self) -> Result<Element, Error> {
        let start = self.at;
        self.at += 1;
        let name = self.name()?;
        let mut attrs = Vec::new();
        loop {
            self.skip_space();
            let rest = self.rest();
            if rest.starts_with("/>") {
                self.at += 2;
                return Ok(Element {
                    name,
                    attrs,
                    children: Vec::new(),
                    span: start..self.at,
                    content_end: self.at,
                });
            }
            if rest.starts_with('>') {
                self.at += 1;
                break;
            }
            if rest.is_empty() {
                return Err(self.error(format!("`<{name}` does not end")));
            }
            let attr = self.name()?;
            self.skip_space();
            if !self.rest().starts_with('=') {
                return Err(self.error(format!("`{attr}` has no value")));
            }
            self.at += 1;
            self.skip_space();
            let quote = match self.rest().chars().next() {
                Some(q @ ('"' | '\'')) => q,
                _ => return Err(self.error(format!("the value of `{attr}` is not quoted"))),
            };
            self.at += 1;
            let len = self
                .rest()
                .find(quote)
                .ok_or_else(|| self.error(format!("the value of `{attr}` does not end")))?;
            let value = decode(&self.text[self.at..self.at + len], self.at)?;
            self.at += len + 1;
            if attrs.iter().any(|(n, _)| *n == attr) {
                return Err(self.error(format!("`{attr}` is given twice")));
            }
            attrs.push((attr, value));
        }
        let mut children = Vec::new();
        loop {
            let rest = self.rest();
            if rest.starts_with("</") {
                let content_end = self.at;
                self.at += 2;
                let end = self.name()?;
                if end != name {
                    return Err(Error {
                        at: content_end,
                        message: format!("`<{name}>` is closed by `</{end}>`"),
                    });
                }
                self.skip_space();
                if !self.rest().starts_with('>') {
                    return Err(self.error(format!("`</{name}` does not end")));
                }
                self.at += 1;
                return Ok(Element {
                    name,
                    attrs,
                    children,
                    span: start..self.at,
                    content_end,
                });
            } else if rest.starts_with("<!--") {
                self.skip_past("-->", "a comment")?;
            } else if rest.starts_with("<![CDATA[") {
                self.at += "<![CDATA[".len();
                let len = self
                    .rest()
                    .find("]]>")
                    .ok_or_else(|| self.error("a CDATA section does not end"))?;
                children.push(Node::Text(self.rest()[..len].to_owned()));
                self.at += len + 3;
            } else if rest.starts_with("<?") {
                self.skip_past("?>", "a processing instruction")?;
            } else if rest.starts_with('<') {
                children.push(Node::Element(self.element()?));
            } else if rest.is_empty() {
                return Err(Error {
                    at: start,
                    message: format!("`<{name}>` is not closed"),
                });
            } else {
                let len = rest.find('<').unwrap_or(rest.len());
                let text = decode(&rest[..len], self.at)?;
                self.at += len;
                children.push(Node::Text(text));
            }
        }
    }
}

/// `raw`, found at `at`, with its entities and character references
/// replaced.
fn decode(raw: &str, at: usize) -> Result<String, Error> {
    if !raw.contains('&') {
        return Ok(raw.to_owned());
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let here = at + (raw.len() - rest.len()) + amp;
        let bad = || Error {
            at: here,
            message: "an `&` that starts no entity (write `&amp;`)".to_owned(),
        };
        let end = rest[amp..].find(';').ok_or_else(bad)?;
        let entity = &rest[amp + 1..amp + end];
        let c = match entity {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let code = if let Some(hex) = entity.strip_prefix("#x") {
                    u32::from_str_radix(hex, 16).ok()
                } else if let Some(dec) = entity.strip_prefix('#') {
                    dec.parse().ok()
                } else {
                    None
                };
                code.and_then(char::from_u32).ok_or_else(bad)?
            }
        };
        out.push(c);
        rest = &rest[amp + end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_reads_as_its_elements() {
        let text = "<?xml version=\"1.0\"?>\n<!-- a note -->\n\
                    <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"x.dtd\">\n\
                    <a x=\"1 &amp; 2\" y='&#x41;'>\n  <b/><!-- c -->\n  <c>t &lt; u<![CDATA[<v>]]></c>\n</a>\n";
        let root = parse(text).unwrap();
        assert_eq!(root.name, "a");
        assert_eq!(root.attr("x"), Some("1 & 2"));
        assert_eq!(root.attr("y"), Some("A"));
        let children: Vec<&Element> = root.elements().collect();
        assert_eq!(children.len(), 2);
        assert_eq!(&text[children[0].span.clone()], "<b/>");
        assert_eq!(children[1].text(), "t < u<v>");
        assert_eq!(&text[root.content_end..root.span.end], "</a>");
    }

    #[test]
    fn what_is_not_xml_says_where() {
        let fails = |text: &str| parse(text).unwrap_err();
        assert_eq!(fails("<a><b></a>").message, "`<b>` is closed by `</a>`");
        assert_eq!(fails("<a><b></a>").at, 6);
        assert!(fails("<a x=1/>").message.contains("not quoted"));
        assert!(fails("<a x=\"1\" x=\"2\"/>").message.contains("twice"));
        assert!(fails("<a>&nbsp;</a>").message.contains("`&`"));
        assert!(fails("<a/><b/>").message.contains("more after"));
        assert!(fails("<a>").message.contains("not closed"));
    }

    #[test]
    fn sameness_ignores_attribute_order_and_layout() {
        let a = parse("<s n=\"1\" m=\"2\">\n  <i a=\"x\"/>\n</s>").unwrap();
        let b = parse("<s m=\"2\" n=\"1\"><i a=\"x\" /></s>").unwrap();
        let c = parse("<s m=\"2\" n=\"1\"><i a=\"y\" /></s>").unwrap();
        assert!(a.same(&b));
        assert!(!a.same(&c));
        let mut out = String::new();
        a.write(4, &mut out);
        assert_eq!(
            out,
            "    <s n=\"1\" m=\"2\">\n        <i a=\"x\" />\n    </s>"
        );
    }
}
