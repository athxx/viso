//! HTML into blocks: a forgiving reader of a safe subset — paragraphs,
//! headings, block quotes, lists, preformatted code, tables, rules, line
//! breaks, links, pictures and the inline styles — folded into the same
//! blocks a Markdown document builds.
//!
//! It runs no script and loads nothing: `script`, `style` and the other
//! elements that carry no text show nothing, a link that would run script
//! is plain text, and a picture's pixels come only from the document's
//! resolver. Tags it does not know show their text. Block tags close the
//! paragraph and the inline styles open before them, a list item closes the
//! item before it, and whatever is open at the end closes there, as a
//! browser's reader does.

use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, CowStr, Event, HeadingLevel, LinkType, Tag, TagEnd};

use super::DocumentStyle;
use super::blocks::{Fold, Parsed};

/// Parse `src`, which starts at byte `base` of the document.
pub(super) fn parse(src: &str, base: usize, look: &DocumentStyle) -> Parsed {
    let mut reader = Reader {
        fold: Fold::new(look, base),
        stack: Vec::new(),
        pre: 0,
        fresh: false,
        space: true,
        rows: 0,
        last: 0,
    };
    let mut at = 0;
    while at < src.len() {
        at = reader.token(src, at);
    }
    reader.close_to(0);
    Parsed {
        blocks: reader.fold.finish(),
        defs: String::new(),
    }
}

/// What an open element is to the reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Paragraph,
    /// A paragraph the reader opened for text no paragraph held.
    Implicit,
    Heading(u8),
    Quote,
    List {
        ordered: bool,
    },
    Item,
    Pre,
    Table,
    Head,
    Row {
        head: bool,
    },
    Cell,
    Link,
    Style,
    /// A block that only bounds the paragraphs in it.
    Section,
    /// An element shown as its content, a link that would run script, say.
    Plain,
}

impl Kind {
    /// Whether text goes straight into it.
    fn holds_text(self) -> bool {
        matches!(
            self,
            Kind::Paragraph
                | Kind::Implicit
                | Kind::Heading(_)
                | Kind::Item
                | Kind::Pre
                | Kind::Cell
                | Kind::Link
                | Kind::Style
                | Kind::Plain
        )
    }

    /// Whether a block's start closes it.
    fn inline_or_paragraph(self) -> bool {
        matches!(
            self,
            Kind::Paragraph
                | Kind::Implicit
                | Kind::Heading(_)
                | Kind::Link
                | Kind::Style
                | Kind::Plain
        )
    }
}

struct Element {
    name: Box<str>,
    kind: Kind,
}

struct Reader<'a> {
    fold: Fold<'a>,
    stack: Vec<Element>,
    /// How deep in `pre`, and whether nothing is read in it yet: its first
    /// line break is not its text.
    pre: u32,
    fresh: bool,
    /// The text last read ends in a space, or a block boundary is behind it:
    /// the next text's leading space collapses into it.
    space: bool,
    /// The rows of the table being read so far.
    rows: u32,
    /// Where the last token read ends.
    last: usize,
}

/// Elements whose content is not text to show.
const HIDDEN: &[&str] = &[
    "script", "style", "head", "title", "template", "noscript", "iframe", "object", "svg", "math",
    "canvas", "video", "audio", "select", "textarea", "button", "form",
];

/// Elements that only bound paragraphs.
const SECTIONS: &[&str] = &[
    "html",
    "body",
    "div",
    "section",
    "article",
    "main",
    "header",
    "footer",
    "nav",
    "aside",
    "figure",
    "figcaption",
    "details",
    "summary",
    "address",
    "center",
    "dl",
    "dt",
    "dd",
];

const VOID: &[&str] = &[
    "br", "hr", "img", "input", "meta", "link", "area", "base", "col", "embed", "param", "source",
    "track", "wbr",
];

impl Reader<'_> {
    /// Read the token at byte `at` of `src` and return where the next one
    /// starts.
    fn token(&mut self, src: &str, at: usize) -> usize {
        let rest = &src[at..];
        if !rest.starts_with('<') {
            let end = rest.find('<').map_or(src.len(), |i| at + i);
            self.text(&src[at..end], at..end);
            return end;
        }
        if let Some(comment) = rest.strip_prefix("<!--") {
            return comment.find("-->").map_or(src.len(), |i| at + 4 + i + 3);
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            return rest.find('>').map_or(src.len(), |i| at + i + 1);
        }
        let closing = rest.starts_with("</");
        let name_at = at + if closing { 2 } else { 1 };
        let name_len = src[name_at..]
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(src.len() - name_at);
        let starts_alpha = src[name_at..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic());
        if name_len == 0 || !starts_alpha {
            self.text("<", at..at + 1);
            return at + 1;
        }
        let name = src[name_at..name_at + name_len].to_ascii_lowercase();
        let Some(end) = tag_end(src, name_at + name_len) else {
            // A tag the source does not finish yet: nothing to show.
            return src.len();
        };
        let range = at..end;
        let attrs = &src[name_at + name_len..end - 1];
        if closing {
            self.end_tag(&name, range);
        } else if HIDDEN.contains(&name.as_str()) {
            let close = format!("</{name}");
            let skip = find_ignoring_case(&src[end..], &close).map_or(src.len(), |i| end + i);
            self.last = end;
            return tag_end(src, skip + close.len()).unwrap_or(src.len());
        } else {
            self.start_tag(&name, attrs, range);
        }
        end
    }

    fn emit(&mut self, event: Event<'_>, range: Range<usize>) {
        self.last = range.end;
        self.fold.event(event, range);
    }

    /// The innermost open element's kind, `None` at the top.
    fn context(&self) -> Option<Kind> {
        self.stack.last().map(|e| e.kind)
    }

    fn push(&mut self, name: &str, kind: Kind) {
        self.stack.push(Element {
            name: name.into(),
            kind,
        });
    }

    /// Make room for text or an inline element at `at`: a paragraph opens
    /// where none would hold it. Whether there is room.
    fn inline_room(&mut self, at: usize) -> bool {
        match self.context() {
            Some(kind) if kind.holds_text() => true,
            None | Some(Kind::Quote | Kind::Section) => {
                self.emit(Event::Start(Tag::Paragraph), at..at);
                self.push("", Kind::Implicit);
                true
            }
            Some(_) => false,
        }
    }

    fn text(&mut self, raw: &str, range: Range<usize>) {
        let mut text = decode(raw);
        if self.pre > 0 {
            if std::mem::take(&mut self.fresh) && text.starts_with('\n') {
                text.remove(0);
            }
            if self.inline_room(range.start) {
                self.emit(Event::Text(CowStr::from(text)), range);
            }
            return;
        }
        let mut collapsed = String::with_capacity(text.len());
        for c in text.chars() {
            if c.is_whitespace() && c != '\u{a0}' {
                if !self.space {
                    collapsed.push(' ');
                    self.space = true;
                }
            } else {
                collapsed.push(c);
                self.space = false;
            }
        }
        if collapsed.is_empty() || collapsed == " " && !self.context().is_some_and(Kind::holds_text)
        {
            return;
        }
        if self.inline_room(range.start) {
            self.emit(Event::Text(CowStr::from(collapsed)), range);
        }
    }

    /// The paragraphs and inline styles open since the innermost container
    /// close, as a block's start closes them.
    fn close_inline(&mut self) {
        let keep = self
            .stack
            .iter()
            .rposition(|e| !e.kind.inline_or_paragraph())
            .map_or(0, |i| i + 1);
        self.close_to(keep);
    }

    /// Close every open element from `depth` up, where the last token read
    /// ends.
    fn close_to(&mut self, depth: usize) {
        let at = self.last;
        while self.stack.len() > depth {
            let element = self.stack.pop().expect("deeper than depth");
            self.close(element, at);
        }
    }

    fn close(&mut self, element: Element, at: usize) {
        let end = match element.kind {
            Kind::Paragraph | Kind::Implicit => TagEnd::Paragraph,
            Kind::Heading(level) => TagEnd::Heading(heading_level(level)),
            Kind::Quote => TagEnd::BlockQuote(None),
            Kind::List { ordered } => TagEnd::List(ordered),
            Kind::Item => TagEnd::Item,
            Kind::Pre => {
                self.pre -= 1;
                TagEnd::CodeBlock
            }
            Kind::Table => TagEnd::Table,
            Kind::Row { head: true } => TagEnd::TableHead,
            Kind::Row { head: false } => TagEnd::TableRow,
            Kind::Cell => TagEnd::TableCell,
            Kind::Link => TagEnd::Link,
            Kind::Style => {
                self.fold.style_tag(&element.name, true);
                return;
            }
            // A head's row ends it.
            Kind::Head | Kind::Section | Kind::Plain => return,
        };
        self.emit(Event::End(end), at..at);
    }

    /// The depth of the innermost open element of `kind` above the
    /// innermost of `bound`, if any.
    fn open_above(
        &self,
        kind: impl Fn(Kind) -> bool,
        bound: impl Fn(Kind) -> bool,
    ) -> Option<usize> {
        for (depth, e) in self.stack.iter().enumerate().rev() {
            if kind(e.kind) {
                return Some(depth);
            }
            if bound(e.kind) {
                return None;
            }
        }
        None
    }

    fn in_cell(&self) -> bool {
        self.stack
            .iter()
            .any(|e| matches!(e.kind, Kind::Cell | Kind::Heading(_) | Kind::Pre))
    }

    fn start_tag(&mut self, name: &str, attrs: &str, range: Range<usize>) {
        let at = range.start;
        if self.pre > 0 && name != "br" {
            return;
        }
        let block = match name {
            "p" | "blockquote" | "ul" | "ol" | "li" | "pre" | "table" | "hr" => true,
            _ => heading(name).is_some() || SECTIONS.contains(&name),
        };
        if block && self.in_cell() {
            // A cell, a heading or code holds text only: its blocks break
            // its lines.
            if matches!(name, "p" | "li" | "div" | "hr") && !self.space {
                self.emit(Event::HardBreak, range);
            }
            return;
        }
        if block {
            self.close_inline();
            self.space = true;
        }
        match name {
            "p" => {
                self.emit(Event::Start(Tag::Paragraph), range);
                self.push(name, Kind::Paragraph);
            }
            "blockquote" => {
                self.emit(Event::Start(Tag::BlockQuote(None)), range);
                self.push(name, Kind::Quote);
            }
            "ul" | "ol" => {
                let start = (name == "ol").then(|| {
                    attr(attrs, "start")
                        .and_then(|s| s.trim().parse().ok())
                        .unwrap_or(1)
                });
                // A list directly in a list is an item's.
                if matches!(self.context(), Some(Kind::List { .. })) {
                    self.emit(Event::Start(Tag::Item), at..at);
                    self.push("", Kind::Item);
                }
                self.emit(Event::Start(Tag::List(start)), range);
                self.push(
                    name,
                    Kind::List {
                        ordered: start.is_some(),
                    },
                );
            }
            "li" => {
                if let Some(depth) =
                    self.open_above(|k| k == Kind::Item, |k| matches!(k, Kind::List { .. }))
                {
                    self.close_to(depth);
                }
                if !matches!(self.context(), Some(Kind::List { .. })) {
                    self.emit(Event::Start(Tag::List(None)), at..at);
                    self.push("", Kind::List { ordered: false });
                }
                self.emit(Event::Start(Tag::Item), range);
                self.push(name, Kind::Item);
            }
            "pre" => {
                self.emit(Event::Start(Tag::CodeBlock(CodeBlockKind::Indented)), range);
                self.push(name, Kind::Pre);
                self.pre += 1;
                self.fresh = true;
            }
            "table" => {
                self.emit(Event::Start(Tag::Table(Vec::new())), range);
                self.push(name, Kind::Table);
                self.rows = 0;
            }
            "hr" => self.emit(Event::Rule, range),
            _ if SECTIONS.contains(&name) => self.push(name, Kind::Section),
            _ if heading(name).is_some() => {
                let level = heading(name).expect("a heading");
                self.emit(
                    Event::Start(Tag::Heading {
                        level: heading_level(level),
                        id: None,
                        classes: Vec::new(),
                        attrs: Vec::new(),
                    }),
                    range,
                );
                self.push(name, Kind::Heading(level));
            }
            _ => self.inline_tag(name, attrs, range),
        }
        if block {
            self.space = true;
        }
    }

    fn inline_tag(&mut self, name: &str, attrs: &str, range: Range<usize>) {
        match name {
            "thead" | "tbody" | "tfoot" | "tr" | "th" | "td" => {
                self.table_tag(name, range);
                return;
            }
            "br" => {
                if self.inline_room(range.start) {
                    self.emit(Event::HardBreak, range);
                    self.space = true;
                }
                return;
            }
            _ => {}
        }
        let known = matches!(name, "a" | "img") || is_style(name);
        if !known || VOID.contains(&name) && name != "img" {
            return;
        }
        if !self.inline_room(range.start) {
            return;
        }
        match name {
            "img" => {
                let url = attr(attrs, "src").unwrap_or_default();
                let alt = attr(attrs, "alt").unwrap_or_default();
                self.emit(
                    Event::Start(Tag::Image {
                        link_type: LinkType::Inline,
                        dest_url: CowStr::from(url),
                        title: CowStr::from(""),
                        id: CowStr::from(""),
                    }),
                    range.clone(),
                );
                if !alt.is_empty() {
                    self.emit(Event::Text(CowStr::from(alt)), range.clone());
                }
                self.emit(Event::End(TagEnd::Image), range);
                self.space = false;
            }
            "a" => match attr(attrs, "href").filter(|h| safe_link(h)) {
                Some(href) => {
                    self.emit(
                        Event::Start(Tag::Link {
                            link_type: LinkType::Inline,
                            dest_url: CowStr::from(href),
                            title: CowStr::from(""),
                            id: CowStr::from(""),
                        }),
                        range,
                    );
                    self.push(name, Kind::Link);
                }
                None => self.push(name, Kind::Plain),
            },
            _ => {
                self.fold.style_tag(name, false);
                self.push(name, Kind::Style);
            }
        }
    }

    fn table_tag(&mut self, name: &str, range: Range<usize>) {
        let at = range.start;
        let Some(table) = self.open_above(|k| k == Kind::Table, |_| false) else {
            return;
        };
        let row = self.open_above(|k| matches!(k, Kind::Row { .. }), |k| k == Kind::Table);
        match name {
            "thead" | "tbody" | "tfoot" => {
                self.close_to(table + 1);
                if name == "thead" && self.rows == 0 {
                    self.push(name, Kind::Head);
                } else {
                    self.push(name, Kind::Section);
                }
            }
            "tr" => {
                if let Some(row) = row {
                    self.close_to(row);
                }
                let in_head = matches!(self.context(), Some(Kind::Head));
                if in_head && self.rows > 0 {
                    return;
                }
                self.emit(Event::Start(Tag::TableRow), range);
                self.push(name, Kind::Row { head: in_head });
                self.rows += 1;
            }
            _ => {
                let row = match row {
                    Some(row) => row,
                    None => {
                        // A cell outside a row starts one.
                        self.emit(Event::Start(Tag::TableRow), at..at);
                        self.push("tr", Kind::Row { head: false });
                        self.rows += 1;
                        self.stack.len() - 1
                    }
                };
                self.close_to(row + 1);
                let first = self.rows == 1;
                if name == "th"
                    && first
                    && let Some(Element {
                        kind: Kind::Row { head },
                        ..
                    }) = self.stack.last_mut()
                {
                    *head = true;
                }
                self.emit(Event::Start(Tag::TableCell), range);
                self.push(name, Kind::Cell);
            }
        }
    }

    fn end_tag(&mut self, name: &str, range: Range<usize>) {
        if self.pre > 0 && name != "pre" {
            return;
        }
        let Some(depth) = self.stack.iter().rposition(|e| &*e.name == name) else {
            if matches!(name, "p" | "li" | "div") && self.in_cell() {
                return;
            }
            if heading(name).is_some() || matches!(name, "p" | "br") {
                self.space = true;
            }
            return;
        };
        // What the element held closes where its content ends, and the
        // element at its own end tag.
        self.close_to(depth + 1);
        let element = self.stack.pop().expect("found above");
        let inline = matches!(element.kind, Kind::Link | Kind::Style | Kind::Plain);
        self.close(element, range.end);
        self.last = range.end;
        if !inline {
            self.space = true;
        }
    }
}

fn is_style(name: &str) -> bool {
    matches!(
        name,
        "b" | "strong"
            | "i"
            | "em"
            | "cite"
            | "var"
            | "s"
            | "del"
            | "strike"
            | "u"
            | "ins"
            | "sup"
            | "sub"
            | "code"
            | "kbd"
            | "samp"
            | "tt"
            | "mark"
    )
}

fn heading(name: &str) -> Option<u8> {
    match name.as_bytes() {
        [b'h', d @ b'1'..=b'6'] => Some(d - b'0'),
        _ => None,
    }
}

fn heading_level(level: u8) -> HeadingLevel {
    HeadingLevel::try_from(usize::from(level)).unwrap_or(HeadingLevel::H6)
}

/// A link that leads somewhere rather than running script.
fn safe_link(href: &str) -> bool {
    let scheme: String = href
        .trim()
        .chars()
        .take_while(|&c| c != ':' && c != '/' && c != '?' && c != '#')
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect::<String>()
        .to_ascii_lowercase();
    let has_scheme =
        href.trim().len() > scheme.len() && href.trim()[scheme.len()..].starts_with(':');
    !has_scheme || !matches!(scheme.as_str(), "javascript" | "vbscript" | "data" | "file")
}

/// Where the tag whose name ends at `from` ends: past its `>`, quoted
/// values skipped. `None` for a tag the source does not finish.
fn tag_end(src: &str, from: usize) -> Option<usize> {
    let mut quote = None;
    for (i, c) in src[from..].char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '>') => return Some(from + i + 1),
            _ => {}
        }
    }
    None
}

fn find_ignoring_case(haystack: &str, needle: &str) -> Option<usize> {
    let (h, n) = (haystack.as_bytes(), needle.as_bytes());
    (0..h.len().saturating_sub(n.len() - 1)).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

/// The value of attribute `name` among `attrs`, entities decoded.
fn attr(attrs: &str, name: &str) -> Option<String> {
    let bytes = attrs.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        let start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && !matches!(bytes[i], b'=' | b'/')
        {
            i += 1;
        }
        let key = &attrs[start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = "";
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && matches!(bytes[i], b'"' | b'\'') {
                let q = bytes[i];
                let from = i + 1;
                let to = attrs[from..]
                    .find(q as char)
                    .map_or(attrs.len(), |j| from + j);
                value = &attrs[from..to];
                i = (to + 1).min(attrs.len());
            } else {
                let from = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                value = &attrs[from..i];
            }
        }
        if key.eq_ignore_ascii_case(name) {
            return Some(decode(value));
        }
        if i == start {
            i += 1;
        }
    }
    None
}

/// `text` with its character references read.
fn decode(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let end = rest[1..]
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '#')
            .map_or(rest.len(), |i| i + 1);
        let name = &rest[1..end];
        let decoded = if let Some(number) = name.strip_prefix('#') {
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok(),
                None => number.parse().ok(),
            };
            code.map(|c| char::from_u32(c).unwrap_or('\u{fffd}'))
        } else {
            named(name)
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end..];
                rest = rest.strip_prefix(';').unwrap_or(rest);
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn named(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "hellip" => '…',
        "mdash" => '—',
        "ndash" => '–',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "bull" => '•',
        "middot" => '·',
        "times" => '×',
        "divide" => '÷',
        "laquo" => '«',
        "raquo" => '»',
        "deg" => '°',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "cent" => '¢',
        "sect" => '§',
        "para" => '¶',
        "plusmn" => '±',
        "larr" => '←',
        "rarr" => '→',
        "uarr" => '↑',
        "darr" => '↓',
        "shy" => '\u{ad}',
        "zwj" => '\u{200d}',
        "zwnj" => '\u{200c}',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::blocks::{Block, Column, Inline, Item, Picture};

    fn blocks(src: &str) -> Vec<Block> {
        parse(src, 0, &DocumentStyle::default())
            .blocks
            .into_iter()
            .map(|t| t.block)
            .collect()
    }

    fn plain(text: &str) -> Inline {
        Inline {
            text: text.into(),
            spans: Vec::new(),
        }
    }

    #[test]
    fn blocks_read_with_their_text_collapsed_and_their_styles_spans() {
        let read = blocks(
            "<h2>Title</h2>\n<p>Some   <b>bold</b>\n and <a href=\"/x\">a link</a>.</p><hr>",
        );
        assert_eq!(read.len(), 3);
        assert_eq!(
            read[0],
            Block::Heading {
                level: 2,
                text: plain("Title")
            }
        );
        let Block::Paragraph(text) = &read[1] else {
            panic!("a paragraph: {read:?}");
        };
        assert_eq!(text.text, "Some bold and a link.");
        assert_eq!(&text.text[text.spans[0].range.clone()], "bold");
        assert_eq!(text.spans[1].style.link.as_deref(), Some("/x"));
        assert_eq!(read[2], Block::Rule);
    }

    #[test]
    fn each_top_level_block_keeps_the_source_it_came_from() {
        let src = "<p>one</p>\n<ul><li>a<li>b</ul>loose <i>text</i><pre>x</pre>";
        let parsed = parse(src, 0, &DocumentStyle::default());
        let ranges: Vec<&str> = parsed
            .blocks
            .iter()
            .map(|t| &src[t.range.clone()])
            .collect();
        assert_eq!(
            ranges,
            [
                "<p>one</p>",
                "<ul><li>a<li>b</ul>",
                "loose <i>text</i>",
                "<pre>x</pre>"
            ]
        );
    }

    #[test]
    fn lists_close_their_items_and_nest() {
        let read = blocks("<ol start=3><li>one<li>two<ul><li>deep</ul></ol>");
        let [
            Block::List {
                start: Some(3),
                items,
                ..
            },
        ] = &read[..]
        else {
            panic!("an ordered list: {read:?}");
        };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].blocks, [Block::Paragraph(plain("one"))]);
        let Block::List {
            start: None,
            items: deep,
            ..
        } = &items[1].blocks[1]
        else {
            panic!("a nested list: {:?}", items[1]);
        };
        assert_eq!(
            deep[..],
            [Item {
                task: None,
                blocks: vec![Block::Paragraph(plain("deep"))]
            }]
        );
    }

    #[test]
    fn code_keeps_its_spaces_and_tables_their_head() {
        assert_eq!(
            blocks("<pre><code>fn main() {\n    x &lt; 1\n}</code></pre>"),
            [Block::Code("fn main() {\n    x < 1\n}".into())]
        );
        let read =
            blocks("<table><tr><th>a</th><th>b</th></tr><tr><td>1</td><td>2<td>3</tr></table>");
        assert_eq!(
            read,
            [Block::Table {
                columns: vec![Column::Start; 3],
                head: vec![plain("a"), plain("b")],
                rows: vec![vec![plain("1"), plain("2"), plain("3")]],
            }]
        );
    }

    #[test]
    fn nothing_runs_or_loads_and_unknown_tags_show_their_text() {
        let read = blocks(
            "<script>alert(1)</script><style>p{}</style>\
             <p><a href=\"javascript:alert(1)\">bad</a> <span>kept</span> \
             <img src=\"cat.png\" alt=\"cat\" onerror=\"x\"></p>",
        );
        let [Block::Paragraph(text)] = &read[..] else {
            panic!("one paragraph: {read:?}");
        };
        assert_eq!(text.text, "bad kept cat");
        assert!(text.spans.iter().all(|s| s.style.link.is_none()));
        assert_eq!(
            blocks("<img src='cat.png' alt='a &amp; b'>"),
            [Block::Picture(Picture {
                url: "cat.png".into(),
                alt: "a & b".into(),
                link: None,
            })]
        );
    }

    #[test]
    fn an_unfinished_source_reads_as_far_as_it_goes() {
        assert_eq!(
            blocks("<p>Hello <b>wor"),
            [Block::Paragraph(Inline {
                text: "Hello wor".into(),
                spans: vec![viso_ui::TextSpan {
                    range: 6..9,
                    style: viso_ui::SpanStyle::default().bold(),
                }],
            })]
        );
        assert_eq!(blocks("<p>Hi</p><blockqu"), [Block::Paragraph(plain("Hi"))]);
        assert_eq!(decode("&lt;&#65;&#x42;&bogus;&amp"), "<AB&bogus;&");
    }
}
