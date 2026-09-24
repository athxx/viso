//! A small, strict-enough XML parser for SVG documents.
//!
//! Produces a flat node arena ([`XmlDoc`]) with namespace-resolved element and
//! attribute names. Supports elements, attributes (both quote styles), text,
//! CDATA, comments, processing instructions, the predefined and numeric
//! character references, and internal-subset `<!ENTITY name "value">`
//! declarations (Illustrator exports rely on them for `xmlns="&ns_svg;"`).
//! External entities and DTD validation are intentionally not supported.

use std::collections::HashMap;

pub(crate) const SVG_NS: &str = "http://www.w3.org/2000/svg";
pub(crate) const XLINK_NS: &str = "http://www.w3.org/1999/xlink";
pub(crate) const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

/// Index of a node in [`XmlDoc::nodes`].
pub(crate) type NodeId = usize;

#[derive(Debug, Clone)]
pub(crate) enum XmlKind {
    /// An element. `name` is the local name; `svg` is set when it lives in the
    /// SVG namespace (or no namespace at all — lenient, like browsers opening a
    /// bare `<svg>` file without `xmlns`).
    Element {
        name: String,
        svg: bool,
        attrs: Vec<(String, String)>,
    },
    Text(String),
}

#[derive(Debug, Clone)]
pub(crate) struct XmlNode {
    pub kind: XmlKind,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
}

#[derive(Debug, Clone)]
pub(crate) struct XmlDoc {
    pub nodes: Vec<XmlNode>,
    pub root: NodeId,
}

/// A parse failure with the byte offset where it was detected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlError {
    pub pos: usize,
    pub msg: &'static str,
}

impl std::fmt::Display for XmlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at byte {}", self.msg, self.pos)
    }
}

impl XmlDoc {
    pub(crate) fn element_name(&self, id: NodeId) -> Option<&str> {
        match &self.nodes[id].kind {
            XmlKind::Element {
                name, svg: true, ..
            } => Some(name),
            _ => None,
        }
    }

    pub(crate) fn attrs(&self, id: NodeId) -> &[(String, String)] {
        match &self.nodes[id].kind {
            XmlKind::Element { attrs, .. } => attrs,
            _ => &[],
        }
    }

    pub(crate) fn attr(&self, id: NodeId, name: &str) -> Option<&str> {
        self.attrs(id)
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Element children in document order (text nodes skipped).
    pub(crate) fn element_children(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes[id]
            .children
            .iter()
            .copied()
            .filter(|&c| matches!(self.nodes[c].kind, XmlKind::Element { .. }))
    }

    /// Concatenated text content of all descendant text nodes.
    pub(crate) fn text_content(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.collect_text(id, &mut out);
        out
    }

    fn collect_text(&self, id: NodeId, out: &mut String) {
        for &c in &self.nodes[id].children {
            match &self.nodes[c].kind {
                XmlKind::Text(t) => out.push_str(t),
                XmlKind::Element { .. } => self.collect_text(c, out),
            }
        }
    }
}

/// A raw (not yet namespace-resolved) start tag.
struct RawTag {
    qname: String,
    attrs: Vec<(String, String)>,
}

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
    entities: HashMap<String, String>,
}

/// Maximum nesting depth; deeper documents are rejected rather than risking
/// stack exhaustion further down the pipeline.
const MAX_DEPTH: usize = 1024;

/// Parse an XML document.
/// (node id, qname, namespace scope) of an element still open while parsing.
type OpenElement = (NodeId, String, Vec<(String, String)>);

pub(crate) fn parse(text: &str) -> Result<XmlDoc, XmlError> {
    let mut p = Parser {
        s: text.as_bytes(),
        pos: 0,
        entities: HashMap::new(),
    };
    if p.s.starts_with(b"\xEF\xBB\xBF") {
        p.pos = 3;
    }

    let mut nodes: Vec<XmlNode> = Vec::new();
    let mut stack: Vec<OpenElement> = Vec::new();
    let mut root: Option<NodeId> = None;

    loop {
        if p.pos >= p.s.len() {
            break;
        }
        if p.s[p.pos] == b'<' {
            if p.starts_with("<!--") {
                p.skip_until("-->")?;
            } else if p.starts_with("<?") {
                p.skip_until("?>")?;
            } else if p.starts_with("<![CDATA[") {
                p.pos += 9;
                let start = p.pos;
                let end = p.find("]]>").ok_or(p.err("unterminated CDATA"))?;
                let text = String::from_utf8_lossy(&p.s[start..end]).into_owned();
                p.pos = end + 3;
                if let Some((parent, _, _)) = stack.last() {
                    push_text(&mut nodes, *parent, text);
                }
            } else if p.starts_with("<!DOCTYPE") {
                p.parse_doctype()?;
            } else if p.starts_with("</") {
                p.pos += 2;
                let name = p.parse_name()?;
                p.skip_ws();
                p.expect(b'>')?;
                match stack.pop() {
                    Some((_, open, _)) if open == name => {}
                    _ => return Err(p.err("mismatched closing tag")),
                }
                if stack.is_empty() {
                    // Only trailing misc (comments/PIs/whitespace) may follow.
                    p.skip_trailing()?;
                    break;
                }
            } else {
                p.pos += 1;
                let (tag, self_closing) = p.parse_tag()?;
                if stack.is_empty() && root.is_some() {
                    return Err(p.err("multiple root elements"));
                }
                if stack.len() >= MAX_DEPTH {
                    return Err(p.err("document nested too deeply"));
                }
                // Namespace scope: inherit the parent's, then apply this tag's
                // `xmlns`/`xmlns:*` declarations.
                let mut scope = stack.last().map(|s| s.2.clone()).unwrap_or_default();
                for (k, v) in &tag.attrs {
                    if k == "xmlns" {
                        set_ns(&mut scope, "", v);
                    } else if let Some(prefix) = k.strip_prefix("xmlns:") {
                        set_ns(&mut scope, prefix, v);
                    }
                }
                let element = resolve_element(&tag, &scope);
                let id = nodes.len();
                let parent = stack.last().map(|s| s.0);
                nodes.push(XmlNode {
                    kind: element,
                    parent,
                    children: Vec::new(),
                });
                match parent {
                    Some(par) => nodes[par].children.push(id),
                    None => root = Some(id),
                }
                if self_closing {
                    if stack.is_empty() {
                        p.skip_trailing()?;
                        break;
                    }
                } else {
                    stack.push((id, tag.qname, scope));
                }
            }
        } else {
            let start = p.pos;
            while p.pos < p.s.len() && p.s[p.pos] != b'<' {
                p.pos += 1;
            }
            let raw = String::from_utf8_lossy(&p.s[start..p.pos]).into_owned();
            match stack.last() {
                Some((parent, _, _)) => {
                    let text = p.decode_entities(&raw, start)?;
                    push_text(&mut nodes, *parent, text);
                }
                None if raw.trim().is_empty() => {}
                None => {
                    return Err(XmlError {
                        pos: start,
                        msg: "text outside the root element",
                    });
                }
            }
        }
    }

    if !stack.is_empty() {
        return Err(p.err("unclosed element"));
    }
    let root = root.ok_or(p.err("no root element"))?;
    Ok(XmlDoc { nodes, root })
}

fn push_text(nodes: &mut Vec<XmlNode>, parent: NodeId, text: String) {
    // Merge adjacent text runs (e.g. text + CDATA) into one node.
    if let Some(&last) = nodes[parent].children.last()
        && let XmlKind::Text(t) = &mut nodes[last].kind
    {
        t.push_str(&text);
        return;
    }
    let id = nodes.len();
    nodes.push(XmlNode {
        kind: XmlKind::Text(text),
        parent: Some(parent),
        children: Vec::new(),
    });
    nodes[parent].children.push(id);
}

fn set_ns(scope: &mut Vec<(String, String)>, prefix: &str, uri: &str) {
    if let Some(e) = scope.iter_mut().find(|(p, _)| p == prefix) {
        e.1 = uri.to_string();
    } else {
        scope.push((prefix.to_string(), uri.to_string()));
    }
}

fn lookup_ns<'a>(scope: &'a [(String, String)], prefix: &str) -> Option<&'a str> {
    scope
        .iter()
        .rev()
        .find(|(p, _)| p == prefix)
        .map(|(_, u)| u.as_str())
}

/// Resolve a raw tag's element and attribute names against the namespace scope.
///
/// Attributes keep their local name when unprefixed; `xlink:*` becomes
/// `xlink:<local>` (normalized to `href` for `xlink:href`), `xml:space` stays
/// `xml:space`, and attributes in any foreign namespace are dropped (Inkscape's
/// `inkscape:*`, `sodipodi:*` and friends never affect rendering).
fn resolve_element(tag: &RawTag, scope: &[(String, String)]) -> XmlKind {
    let (prefix, local) = split_qname(&tag.qname);
    let ns = lookup_ns(scope, prefix);
    let svg = match ns {
        Some(uri) => uri == SVG_NS,
        None => prefix.is_empty(),
    };

    let mut attrs: Vec<(String, String)> = Vec::with_capacity(tag.attrs.len());
    for (k, v) in &tag.attrs {
        if k == "xmlns" || k.starts_with("xmlns:") {
            continue;
        }
        let (ap, al) = split_qname(k);
        let name = if ap.is_empty() {
            al.to_string()
        } else {
            match lookup_ns(scope, ap) {
                Some(XLINK_NS) => {
                    if al == "href" {
                        // SVG 2 `href` wins over `xlink:href` when both exist.
                        if attrs.iter().any(|(n, _)| n == "href") {
                            continue;
                        }
                        "href".to_string()
                    } else {
                        format!("xlink:{al}")
                    }
                }
                Some(XML_NS) => format!("xml:{al}"),
                None if ap == "xml" => format!("xml:{al}"),
                _ => continue,
            }
        };
        if name == "href"
            && let Some(e) = attrs.iter_mut().find(|(n, _)| n == "href")
        {
            e.1 = v.clone();
            continue;
        }
        attrs.push((name, v.clone()));
    }
    XmlKind::Element {
        name: local.to_string(),
        svg,
        attrs,
    }
}

fn split_qname(q: &str) -> (&str, &str) {
    match q.find(':') {
        Some(i) => (&q[..i], &q[i + 1..]),
        None => ("", q),
    }
}

impl Parser<'_> {
    fn err(&self, msg: &'static str) -> XmlError {
        XmlError { pos: self.pos, msg }
    }

    fn starts_with(&self, lit: &str) -> bool {
        self.s[self.pos..].starts_with(lit.as_bytes())
    }

    fn find(&self, lit: &str) -> Option<usize> {
        let needle = lit.as_bytes();
        self.s[self.pos..]
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|i| self.pos + i)
    }

    fn skip_until(&mut self, lit: &str) -> Result<(), XmlError> {
        let end = self.find(lit).ok_or(self.err("unterminated markup"))?;
        self.pos = end + lit.len();
        Ok(())
    }

    fn skip_ws(&mut self) {
        while self.pos < self.s.len() && matches!(self.s[self.pos], b' ' | b'\t' | b'\n' | b'\r') {
            self.pos += 1;
        }
    }

    fn expect(&mut self, b: u8) -> Result<(), XmlError> {
        if self.s.get(self.pos) == Some(&b) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.err("unexpected character"))
        }
    }

    /// After the root element closes: allow whitespace, comments and PIs only.
    fn skip_trailing(&mut self) -> Result<(), XmlError> {
        loop {
            self.skip_ws();
            if self.pos >= self.s.len() {
                return Ok(());
            }
            if self.starts_with("<!--") {
                self.skip_until("-->")?;
            } else if self.starts_with("<?") {
                self.skip_until("?>")?;
            } else {
                // Browsers reject trailing content; being lenient here costs
                // nothing and keeps concatenated/garbage-tailed files usable.
                self.pos = self.s.len();
                return Ok(());
            }
        }
    }

    fn parse_name(&mut self) -> Result<String, XmlError> {
        let start = self.pos;
        while self.pos < self.s.len() {
            let c = self.s[self.pos];
            let ok =
                c.is_ascii_alphanumeric() || matches!(c, b'_' | b':' | b'-' | b'.') || c >= 0x80;
            if !ok {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start {
            return Err(self.err("expected a name"));
        }
        Ok(String::from_utf8_lossy(&self.s[start..self.pos]).into_owned())
    }

    /// Parse a start tag after the `<`. Returns the tag and whether it was
    /// self-closing.
    fn parse_tag(&mut self) -> Result<(RawTag, bool), XmlError> {
        let qname = self.parse_name()?;
        let mut attrs = Vec::new();
        loop {
            self.skip_ws();
            match self.s.get(self.pos) {
                Some(b'>') => {
                    self.pos += 1;
                    return Ok((RawTag { qname, attrs }, false));
                }
                Some(b'/') => {
                    self.pos += 1;
                    self.expect(b'>')?;
                    return Ok((RawTag { qname, attrs }, true));
                }
                Some(_) => {
                    let name = self.parse_name()?;
                    self.skip_ws();
                    self.expect(b'=')?;
                    self.skip_ws();
                    let quote = match self.s.get(self.pos) {
                        Some(&q @ (b'"' | b'\'')) => q,
                        _ => return Err(self.err("expected a quoted attribute value")),
                    };
                    self.pos += 1;
                    let start = self.pos;
                    while self.pos < self.s.len() && self.s[self.pos] != quote {
                        self.pos += 1;
                    }
                    if self.pos >= self.s.len() {
                        return Err(self.err("unterminated attribute value"));
                    }
                    let raw = String::from_utf8_lossy(&self.s[start..self.pos]).into_owned();
                    self.pos += 1;
                    let value = self.decode_entities(&raw, start)?;
                    // Attribute-value normalization: literal whitespace → space.
                    let value = value.replace(['\t', '\n', '\r'], " ");
                    if !attrs.iter().any(|(k, _): &(String, String)| *k == name) {
                        attrs.push((name, value));
                    }
                }
                None => return Err(self.err("unterminated start tag")),
            }
        }
    }

    /// `<!DOCTYPE ... [ internal subset ]>` — records `<!ENTITY>` declarations,
    /// skips everything else.
    fn parse_doctype(&mut self) -> Result<(), XmlError> {
        self.pos += "<!DOCTYPE".len();
        while self.pos < self.s.len() {
            match self.s[self.pos] {
                b'>' => {
                    self.pos += 1;
                    return Ok(());
                }
                b'[' => {
                    self.pos += 1;
                    self.parse_internal_subset()?;
                }
                b'"' | b'\'' => {
                    let q = self.s[self.pos];
                    self.pos += 1;
                    while self.pos < self.s.len() && self.s[self.pos] != q {
                        self.pos += 1;
                    }
                    self.pos += 1;
                }
                _ => self.pos += 1,
            }
        }
        Err(self.err("unterminated DOCTYPE"))
    }

    fn parse_internal_subset(&mut self) -> Result<(), XmlError> {
        loop {
            self.skip_ws();
            if self.pos >= self.s.len() {
                return Err(self.err("unterminated DOCTYPE internal subset"));
            }
            if self.s[self.pos] == b']' {
                self.pos += 1;
                return Ok(());
            }
            if self.starts_with("<!--") {
                self.skip_until("-->")?;
            } else if self.starts_with("<!ENTITY") {
                self.pos += "<!ENTITY".len();
                self.skip_ws();
                let parameter = self.s.get(self.pos) == Some(&b'%');
                if parameter {
                    self.pos += 1;
                    self.skip_ws();
                }
                let name = self.parse_name()?;
                self.skip_ws();
                let value = match self.s.get(self.pos) {
                    Some(&q @ (b'"' | b'\'')) => {
                        self.pos += 1;
                        let start = self.pos;
                        while self.pos < self.s.len() && self.s[self.pos] != q {
                            self.pos += 1;
                        }
                        let v = String::from_utf8_lossy(&self.s[start..self.pos]).into_owned();
                        self.pos += 1;
                        Some(v)
                    }
                    // External (SYSTEM/PUBLIC) entities are not fetched.
                    _ => None,
                };
                self.skip_until(">")?;
                if let Some(v) = value
                    && !parameter
                {
                    self.entities.entry(name).or_insert(v);
                }
            } else if self.starts_with("<!") || self.starts_with("<?") {
                // ELEMENT / ATTLIST / NOTATION / PI declarations: skip, honoring
                // quoted strings that may contain '>'.
                self.pos += 2;
                while self.pos < self.s.len() && self.s[self.pos] != b'>' {
                    if matches!(self.s[self.pos], b'"' | b'\'') {
                        let q = self.s[self.pos];
                        self.pos += 1;
                        while self.pos < self.s.len() && self.s[self.pos] != q {
                            self.pos += 1;
                        }
                    }
                    self.pos += 1;
                }
                self.pos += 1;
            } else {
                // Parameter-entity references (`%name;`) and stray bytes.
                self.pos += 1;
            }
        }
    }

    /// Expand character and entity references in `raw` (which started at byte
    /// `at`, for error positions). Custom entities may reference others; the
    /// expansion depth is bounded to defeat "billion laughs" documents.
    fn decode_entities(&self, raw: &str, at: usize) -> Result<String, XmlError> {
        if !raw.contains('&') {
            return Ok(raw.to_string());
        }
        let mut out = String::with_capacity(raw.len());
        self.expand_into(raw, at, &mut out, 0)?;
        Ok(out)
    }

    fn expand_into(
        &self,
        raw: &str,
        at: usize,
        out: &mut String,
        depth: u32,
    ) -> Result<(), XmlError> {
        const MAX_EXPANDED: usize = 8 << 20;
        let mut rest = raw;
        while let Some(i) = rest.find('&') {
            out.push_str(&rest[..i]);
            rest = &rest[i + 1..];
            let Some(end) = rest.find(';') else {
                // A bare '&' — not well-formed, but keep it literally.
                out.push('&');
                continue;
            };
            let name = &rest[..end];
            rest = &rest[end + 1..];
            match name {
                "lt" => out.push('<'),
                "gt" => out.push('>'),
                "amp" => out.push('&'),
                "quot" => out.push('"'),
                "apos" => out.push('\''),
                _ if name.starts_with("#x") || name.starts_with("#X") => {
                    let c = u32::from_str_radix(&name[2..], 16)
                        .ok()
                        .and_then(char::from_u32)
                        .ok_or(XmlError {
                            pos: at,
                            msg: "invalid character reference",
                        })?;
                    out.push(c);
                }
                _ if name.starts_with('#') => {
                    let c = name[1..]
                        .parse::<u32>()
                        .ok()
                        .and_then(char::from_u32)
                        .ok_or(XmlError {
                            pos: at,
                            msg: "invalid character reference",
                        })?;
                    out.push(c);
                }
                _ => match self.entities.get(name) {
                    Some(v) if depth < 8 => self.expand_into(v, at, out, depth + 1)?,
                    Some(_) => {
                        return Err(XmlError {
                            pos: at,
                            msg: "entity expansion too deep",
                        });
                    }
                    None => {
                        out.push('&');
                        out.push_str(name);
                        out.push(';');
                    }
                },
            }
            if out.len() > MAX_EXPANDED {
                return Err(XmlError {
                    pos: at,
                    msg: "entity expansion too large",
                });
            }
        }
        out.push_str(rest);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_namespaces_entities_and_cdata() {
        let src = r##"<?xml version="1.0"?>
<!DOCTYPE svg [ <!ENTITY ns_svg "http://www.w3.org/2000/svg"> ]>
<!-- c -->
<svg xmlns="&ns_svg;" xmlns:x="http://www.w3.org/1999/xlink" xmlns:ink="urn:ink">
  <use x:href="#a" ink:label="z" title="a &amp; b &#x41;"/>
  <style><![CDATA[ rect { fill: red } ]]></style>
  <ink:thing/>
</svg>"##;
        let doc = parse(src).unwrap();
        assert_eq!(doc.element_name(doc.root), Some("svg"));
        let kids: Vec<_> = doc.element_children(doc.root).collect();
        assert_eq!(kids.len(), 3);
        assert_eq!(doc.attr(kids[0], "href"), Some("#a"));
        assert_eq!(doc.attr(kids[0], "label"), None);
        assert_eq!(doc.attr(kids[0], "title"), Some("a & b A"));
        assert!(doc.text_content(kids[1]).contains("fill: red"));
        assert_eq!(doc.element_name(kids[2]), None, "foreign element");
    }

    #[test]
    fn rejects_mismatched_tags() {
        assert!(parse("<svg><g></svg>").is_err());
        assert!(parse("not xml").is_err());
    }
}
