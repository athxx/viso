//! A compact CSS engine for `<style>` sheets and `style=""` attributes.
//!
//! Selectors: type, universal, `#id`, `.class`, attribute selectors
//! (`[a]`, `=`, `~=`, `|=`, `^=`, `$=`, `*=`), `:first-child`, `:last-child`,
//! `:only-child`, `:root`, `:not(<compound>)`, and the descendant/child/
//! adjacent/general-sibling combinators. Unsupported pseudo-classes make a
//! selector never match rather than over-match. `@media`/`@supports` blocks are
//! entered (their rules apply, as for a screen renderer); other at-rules are
//! skipped.

use crate::xml::{NodeId, XmlDoc};

/// One `name: value` declaration.
#[derive(Debug, Clone)]
pub(crate) struct Declaration {
    pub name: String,
    pub value: String,
    pub important: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct Rule {
    pub selector: Selector,
    pub specificity: (u32, u32, u32),
    /// Source order across all sheets, for cascade tie-breaking.
    pub order: usize,
    pub decls: std::rc::Rc<Vec<Declaration>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Combinator {
    Descendant,
    Child,
    Adjacent,
    Sibling,
}

#[derive(Debug, Clone)]
enum AttrOp {
    Exists,
    Eq(String),
    Word(String),
    DashPrefix(String),
    Prefix(String),
    Suffix(String),
    Substring(String),
}

#[derive(Debug, Clone)]
enum Simple {
    Type(String),
    Id(String),
    Class(String),
    Attr(String, AttrOp),
    FirstChild,
    LastChild,
    OnlyChild,
    Root,
    Not(Box<Compound>),
    /// A pseudo-class we do not implement (`:hover`, …): never matches.
    Never,
}

#[derive(Debug, Clone, Default)]
struct Compound {
    parts: Vec<Simple>,
}

/// A complex selector stored right-to-left: `parts[0]` is the subject compound,
/// each following entry is reached from the previous one via its combinator.
#[derive(Debug, Clone)]
pub(crate) struct Selector {
    subject: Compound,
    chain: Vec<(Combinator, Compound)>,
}

/// Parse a stylesheet, appending rules to `out`. `order` is the running source
/// order counter shared across sheets.
pub(crate) fn parse_stylesheet(text: &str, out: &mut Vec<Rule>, order: &mut usize) {
    let text = strip_comments(text);
    let mut rest = text.as_str();
    parse_rules(&mut rest, out, order, 0);
}

fn parse_rules(rest: &mut &str, out: &mut Vec<Rule>, order: &mut usize, depth: u32) {
    loop {
        *rest = rest.trim_start();
        if rest.is_empty() {
            return;
        }
        if let Some(r) = rest.strip_prefix('}') {
            *rest = r;
            if depth > 0 {
                return;
            }
            continue;
        }
        if rest.starts_with("<!--") || rest.starts_with("-->") {
            *rest = &rest[if rest.starts_with("<!--") { 4 } else { 3 }..];
            continue;
        }
        if rest.starts_with('@') {
            let brace = rest.find('{');
            let semi = rest.find(';');
            match (brace, semi) {
                (Some(b), s) if s.is_none_or(|s| b < s) => {
                    let keyword = rest[1..b]
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    *rest = &rest[b + 1..];
                    if (keyword == "media" || keyword == "supports") && depth < 8 {
                        parse_rules(rest, out, order, depth + 1);
                    } else {
                        skip_block(rest);
                    }
                }
                (_, Some(s)) => *rest = &rest[s + 1..],
                _ => return,
            }
            continue;
        }
        let Some(b) = rest.find('{') else { return };
        let prelude = rest[..b].trim().to_string();
        *rest = &rest[b + 1..];
        let end = rest.find('}').unwrap_or(rest.len());
        let body = &rest[..end];
        *rest = if end < rest.len() {
            &rest[end + 1..]
        } else {
            ""
        };
        let decls = std::rc::Rc::new(parse_declarations(body));
        if decls.is_empty() {
            continue;
        }
        for sel in prelude.split(',') {
            if let Some((selector, specificity)) = parse_selector(sel.trim()) {
                out.push(Rule {
                    selector,
                    specificity,
                    order: *order,
                    decls: decls.clone(),
                });
                *order += 1;
            }
        }
    }
}

/// Skip to the matching close brace (the open brace is already consumed).
fn skip_block(rest: &mut &str) {
    let mut depth = 1;
    for (i, c) in rest.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    *rest = &rest[i + 1..];
                    return;
                }
            }
            _ => {}
        }
    }
    *rest = "";
}

fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("/*") {
        out.push_str(&rest[..i]);
        match rest[i + 2..].find("*/") {
            Some(j) => rest = &rest[i + 2 + j + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Parse a declaration block body (`a: b; c: d !important`).
pub(crate) fn parse_declarations(body: &str) -> Vec<Declaration> {
    let body = if body.contains("/*") {
        strip_comments(body)
    } else {
        body.to_string()
    };
    let mut out = Vec::new();
    for decl in split_top_level(&body, ';') {
        let Some(colon) = decl.find(':') else {
            continue;
        };
        let name = decl[..colon].trim().to_ascii_lowercase();
        let mut value = decl[colon + 1..].trim();
        if name.is_empty() || value.is_empty() {
            continue;
        }
        let mut important = false;
        if let Some(i) = value.rfind('!')
            && value[i + 1..].trim().eq_ignore_ascii_case("important")
        {
            important = true;
            value = value[..i].trim_end();
        }
        out.push(Declaration {
            name,
            value: value.to_string(),
            important,
        });
    }
    out
}

/// Split on `sep` outside parentheses and quotes (`url(data:...;base64,...)`
/// must not be split on its `;`).
fn split_top_level(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => quote = Some(c),
                '(' => depth += 1,
                ')' => depth -= 1,
                _ if c == sep && depth <= 0 => {
                    out.push(&s[start..i]);
                    start = i + c.len_utf8();
                }
                _ => {}
            },
        }
    }
    out.push(&s[start..]);
    out
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_' || !c.is_ascii()
}

fn parse_selector(s: &str) -> Option<(Selector, (u32, u32, u32))> {
    if s.is_empty() {
        return None;
    }
    let mut compounds: Vec<Compound> = Vec::new();
    let mut combinators: Vec<Combinator> = Vec::new();
    let mut chars = s.char_indices().peekable();
    let mut spec = (0u32, 0u32, 0u32);
    let bytes = s;

    let mut current = Compound::default();
    let mut pending: Option<Combinator> = None;
    let mut saw_ws = false;

    while let Some(&(i, c)) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            saw_ws = true;
            continue;
        }
        if matches!(c, '>' | '+' | '~') {
            chars.next();
            if current.parts.is_empty() && compounds.is_empty() {
                return None;
            }
            pending = Some(match c {
                '>' => Combinator::Child,
                '+' => Combinator::Adjacent,
                _ => Combinator::Sibling,
            });
            saw_ws = false;
            continue;
        }
        if (saw_ws || pending.is_some()) && !current.parts.is_empty() {
            compounds.push(std::mem::take(&mut current));
            combinators.push(pending.take().unwrap_or(Combinator::Descendant));
        }
        saw_ws = false;
        // One simple selector.
        let (simple, used) = parse_simple(&bytes[i..], &mut spec)?;
        current.parts.push(simple);
        let end = i + used;
        while let Some(&(j, _)) = chars.peek() {
            if j < end {
                chars.next();
            } else {
                break;
            }
        }
    }
    if current.parts.is_empty() || pending.is_some() {
        return None;
    }
    compounds.push(current);

    let subject = compounds.pop()?;
    let mut chain = Vec::new();
    while let Some(c) = compounds.pop() {
        chain.push((combinators.pop()?, c));
    }
    Some((Selector { subject, chain }, spec))
}

/// Parse one simple selector at the start of `s`; returns it and the bytes used.
fn parse_simple(s: &str, spec: &mut (u32, u32, u32)) -> Option<(Simple, usize)> {
    let first = s.chars().next()?;
    let ident_len = |t: &str| {
        t.char_indices()
            .find(|&(_, c)| !is_ident_char(c))
            .map_or(t.len(), |(i, _)| i)
    };
    match first {
        '*' => Some((Simple::Type("*".into()), 1)),
        '#' => {
            let n = ident_len(&s[1..]);
            if n == 0 {
                return None;
            }
            spec.0 += 1;
            Some((Simple::Id(s[1..1 + n].to_string()), 1 + n))
        }
        '.' => {
            let n = ident_len(&s[1..]);
            if n == 0 {
                return None;
            }
            spec.1 += 1;
            Some((Simple::Class(s[1..1 + n].to_string()), 1 + n))
        }
        '[' => {
            let close = s.find(']')?;
            let inner = &s[1..close];
            spec.1 += 1;
            let ops = ["~=", "|=", "^=", "$=", "*=", "="];
            for op in ops {
                if let Some(at) = inner.find(op) {
                    let name = inner[..at].trim().to_string();
                    let raw = inner[at + op.len()..].trim();
                    // Strip an optional case-sensitivity flag, then quotes.
                    let raw = raw
                        .strip_suffix(" i")
                        .or_else(|| raw.strip_suffix(" s"))
                        .unwrap_or(raw)
                        .trim();
                    let val = raw.trim_matches(|c| c == '"' || c == '\'').to_string();
                    let op = match op {
                        "~=" => AttrOp::Word(val),
                        "|=" => AttrOp::DashPrefix(val),
                        "^=" => AttrOp::Prefix(val),
                        "$=" => AttrOp::Suffix(val),
                        "*=" => AttrOp::Substring(val),
                        _ => AttrOp::Eq(val),
                    };
                    return Some((Simple::Attr(name, op), close + 1));
                }
            }
            Some((
                Simple::Attr(inner.trim().to_string(), AttrOp::Exists),
                close + 1,
            ))
        }
        ':' => {
            let body = s.trim_start_matches(':');
            let colons = s.len() - body.len();
            let n = ident_len(body);
            let name = body[..n].to_ascii_lowercase();
            let mut used = colons + n;
            if name == "not" && body[n..].starts_with('(') {
                let close = body[n..].find(')')? + n;
                let inner = &body[n + 1..close];
                used = colons + close + 1;
                let mut dummy = (0, 0, 0);
                let mut compound = Compound::default();
                let mut rest = inner.trim();
                while !rest.is_empty() {
                    let (simple, u) = parse_simple(rest, &mut dummy)?;
                    compound.parts.push(simple);
                    rest = rest[u..].trim_start();
                }
                // :not() takes the specificity of its argument.
                spec.0 += dummy.0;
                spec.1 += dummy.1;
                spec.2 += dummy.2;
                return Some((Simple::Not(Box::new(compound)), used));
            }
            if body[n..].starts_with('(') {
                let close = body[n..].find(')')? + n;
                used = colons + close + 1;
            }
            let simple = if colons == 2 {
                // Pseudo-elements never match an element.
                Simple::Never
            } else {
                match name.as_str() {
                    "first-child" => Simple::FirstChild,
                    "last-child" => Simple::LastChild,
                    "only-child" => Simple::OnlyChild,
                    "root" => Simple::Root,
                    _ => Simple::Never,
                }
            };
            spec.1 += 1;
            Some((simple, used))
        }
        c if is_ident_char(c) => {
            let n = ident_len(s);
            spec.2 += 1;
            // Namespace-prefixed type selectors (`svg|rect`) keep the local name.
            let name = &s[..n];
            if s[n..].starts_with('|') {
                let m = ident_len(&s[n + 1..]);
                spec.2 += 0;
                return Some((Simple::Type(s[n + 1..n + 1 + m].to_string()), n + 1 + m));
            }
            Some((Simple::Type(name.to_string()), n))
        }
        _ => None,
    }
}

impl Selector {
    pub(crate) fn matches(&self, doc: &XmlDoc, node: NodeId) -> bool {
        if !compound_matches(&self.subject, doc, node) {
            return false;
        }
        chain_matches(&self.chain, doc, node)
    }
}

fn chain_matches(chain: &[(Combinator, Compound)], doc: &XmlDoc, node: NodeId) -> bool {
    let Some(((comb, compound), rest)) = chain.split_first() else {
        return true;
    };
    match comb {
        Combinator::Child => match parent_element(doc, node) {
            Some(p) => compound_matches(compound, doc, p) && chain_matches(rest, doc, p),
            None => false,
        },
        Combinator::Descendant => {
            let mut cur = parent_element(doc, node);
            while let Some(p) = cur {
                if compound_matches(compound, doc, p) && chain_matches(rest, doc, p) {
                    return true;
                }
                cur = parent_element(doc, p);
            }
            false
        }
        Combinator::Adjacent => match prev_sibling(doc, node) {
            Some(s) => compound_matches(compound, doc, s) && chain_matches(rest, doc, s),
            None => false,
        },
        Combinator::Sibling => {
            let mut cur = prev_sibling(doc, node);
            while let Some(s) = cur {
                if compound_matches(compound, doc, s) && chain_matches(rest, doc, s) {
                    return true;
                }
                cur = prev_sibling(doc, s);
            }
            false
        }
    }
}

fn parent_element(doc: &XmlDoc, node: NodeId) -> Option<NodeId> {
    doc.nodes[node].parent
}

fn element_siblings(doc: &XmlDoc, node: NodeId) -> Vec<NodeId> {
    match doc.nodes[node].parent {
        Some(p) => doc.element_children(p).collect(),
        None => vec![node],
    }
}

fn prev_sibling(doc: &XmlDoc, node: NodeId) -> Option<NodeId> {
    let sibs = element_siblings(doc, node);
    let i = sibs.iter().position(|&s| s == node)?;
    (i > 0).then(|| sibs[i - 1])
}

fn compound_matches(c: &Compound, doc: &XmlDoc, node: NodeId) -> bool {
    c.parts.iter().all(|s| simple_matches(s, doc, node))
}

fn simple_matches(s: &Simple, doc: &XmlDoc, node: NodeId) -> bool {
    match s {
        Simple::Type(t) => t == "*" || doc.element_name(node) == Some(t.as_str()),
        Simple::Id(id) => doc.attr(node, "id") == Some(id.as_str()),
        Simple::Class(class) => doc
            .attr(node, "class")
            .is_some_and(|v| v.split_ascii_whitespace().any(|w| w == class)),
        Simple::Attr(name, op) => {
            let Some(v) = doc.attr(node, name) else {
                return false;
            };
            match op {
                AttrOp::Exists => true,
                AttrOp::Eq(x) => v == x,
                AttrOp::Word(x) => v.split_ascii_whitespace().any(|w| w == x),
                AttrOp::DashPrefix(x) => v == x || v.starts_with(&format!("{x}-")),
                AttrOp::Prefix(x) => !x.is_empty() && v.starts_with(x.as_str()),
                AttrOp::Suffix(x) => !x.is_empty() && v.ends_with(x.as_str()),
                AttrOp::Substring(x) => !x.is_empty() && v.contains(x.as_str()),
            }
        }
        Simple::FirstChild => element_siblings(doc, node).first() == Some(&node),
        Simple::LastChild => element_siblings(doc, node).last() == Some(&node),
        Simple::OnlyChild => element_siblings(doc, node).len() == 1,
        Simple::Root => doc.nodes[node].parent.is_none(),
        Simple::Not(inner) => !compound_matches(inner, doc, node),
        Simple::Never => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml;

    fn first_match(css: &str, doc: &XmlDoc) -> Vec<NodeId> {
        let mut rules = Vec::new();
        let mut order = 0;
        let sheet = if css.contains('{') {
            css.to_string()
        } else {
            format!("{css} {{ fill: red }}")
        };
        parse_stylesheet(&sheet, &mut rules, &mut order);
        (0..doc.nodes.len())
            .filter(|&n| {
                doc.element_name(n).is_some() && rules.iter().any(|r| r.selector.matches(doc, n))
            })
            .collect()
    }

    #[test]
    fn selectors_match_expected_nodes() {
        let doc = xml::parse(r#"<svg xmlns="http://www.w3.org/2000/svg"><g class="a b"><rect id="r"/><circle/></g><rect/></svg>"#).unwrap();
        // Nodes: 0 svg, 1 g, 2 rect#r, 3 circle, 4 rect
        assert_eq!(first_match("g > rect", &doc), vec![2]);
        assert_eq!(first_match(".b rect", &doc), vec![2]);
        assert_eq!(first_match("#r + circle", &doc), vec![3]);
        assert_eq!(first_match("rect:not(#r)", &doc), vec![4]);
        assert_eq!(first_match("svg > rect:last-child", &doc), vec![4]);
        assert_eq!(first_match("rect:hover", &doc), Vec::<NodeId>::new());
        assert_eq!(
            first_match("@media screen { circle { fill: red } }", &doc),
            vec![3]
        );
    }

    #[test]
    fn declarations_parse_important_and_urls() {
        let d = parse_declarations("fill: url(data:a;b) ; stroke:red !important;;");
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].value, "url(data:a;b)");
        assert!(d[1].important && d[1].value == "red");
    }
}
