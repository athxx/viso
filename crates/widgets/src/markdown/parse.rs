//! Markdown source into blocks: a CommonMark parse with the GitHub
//! extensions, folded into the block tree the document builds its nodes
//! from, each top-level block with the source it came from.

use std::ops::Range;
use std::sync::Arc;

use pulldown_cmark::{Alignment, BlockQuoteKind, Event, Options, Parser, Tag, TagEnd};
use viso_ui::{Rgba, SpanStyle, TextSpan};

use super::MarkdownStyle;

/// A run of text and the stretches of it drawn their own way.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Inline {
    pub text: String,
    pub spans: Vec<TextSpan>,
}

impl Inline {
    fn push(&mut self, piece: &str, style: &SpanStyle) {
        if piece.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(piece);
        let end = self.text.len();
        if *style == SpanStyle::default() {
            return;
        }
        if let Some(last) = self.spans.last_mut()
            && last.range.end == start
            && last.style == *style
        {
            last.range.end = end;
            return;
        }
        self.spans.push(TextSpan {
            range: start..end,
            style: style.clone(),
        });
    }

    /// Without its trailing whitespace, which a hard break at the end of a
    /// paragraph leaves.
    fn trimmed(mut self) -> Self {
        let end = self.text.trim_end().len();
        if end < self.text.len() {
            self.text.truncate(end);
            self.spans.retain_mut(|s| {
                s.range.end = s.range.end.min(end);
                s.range.start < s.range.end
            });
        }
        self
    }
}

/// A GitHub alert: a block quote that opens with `[!NOTE]` and its kin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Alert {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

/// A task list item's box: whether it is checked, and where its `[ ]` is in
/// the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Task {
    pub checked: bool,
    pub at: usize,
}

#[derive(Debug, PartialEq)]
pub(super) struct Item {
    pub task: Option<Task>,
    pub blocks: Vec<Block>,
}

/// A picture: where it is, its text alternative, and where it leads when a
/// link holds it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Picture {
    pub url: String,
    pub alt: String,
    pub link: Option<Arc<str>>,
}

/// How a table's column sits in its cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Column {
    Start,
    Center,
    End,
}

#[derive(Debug, PartialEq)]
pub(super) enum Block {
    Paragraph(Inline),
    Heading {
        level: u8,
        text: Inline,
    },
    Quote {
        alert: Option<Alert>,
        blocks: Vec<Block>,
    },
    List {
        /// The first item's number, for an ordered list.
        start: Option<u64>,
        /// No blank line separates its items.
        tight: bool,
        items: Vec<Item>,
    },
    Code(String),
    Table {
        columns: Vec<Column>,
        head: Vec<Inline>,
        rows: Vec<Vec<Inline>>,
    },
    Rule,
    Picture(Picture),
    Footnote {
        label: String,
        blocks: Vec<Block>,
    },
}

impl Block {
    /// Each task box in the block, in source order.
    pub(super) fn tasks(&self, out: &mut Vec<Task>) {
        match self {
            Block::Quote { blocks, .. } | Block::Footnote { blocks, .. } => {
                blocks.iter().for_each(|b| b.tasks(out));
            }
            Block::List { items, .. } => {
                for item in items {
                    out.extend(item.task);
                    item.blocks.iter().for_each(|b| b.tasks(out));
                }
            }
            _ => {}
        }
    }
}

/// A top-level block and the bytes of the source it came from.
#[derive(Debug)]
pub(super) struct Top {
    pub block: Block,
    pub range: Range<usize>,
}

/// A parse: the top-level blocks, and the link reference definitions, which
/// reach every block, as one text that changes when any of them does.
pub(super) struct Parsed {
    pub blocks: Vec<Top>,
    pub defs: String,
}

const OPTIONS: Options = Options::ENABLE_TABLES
    .union(Options::ENABLE_STRIKETHROUGH)
    .union(Options::ENABLE_TASKLISTS)
    .union(Options::ENABLE_FOOTNOTES)
    .union(Options::ENABLE_GFM);

/// Parse `src`, which starts at byte `base` of the document.
pub(super) fn parse(src: &str, base: usize, look: &MarkdownStyle) -> Parsed {
    let mut events = Parser::new_ext(src, OPTIONS).into_offset_iter();
    let mut fold = Fold {
        look,
        base,
        tops: Vec::new(),
        top: 0..0,
        frames: Vec::new(),
        open: None,
        marks: Marks::default(),
        picture: None,
        skip: 0,
    };
    for (event, range) in events.by_ref() {
        fold.event(event, range);
    }
    fold.close_implicit();
    let mut defs: Vec<&str> = events
        .reference_definitions()
        .iter()
        .map(|(_, def)| &src[def.span.clone()])
        .collect();
    defs.sort_unstable();
    Parsed {
        blocks: fold.tops,
        defs: defs.join("\n"),
    }
}

/// What a block being folded holds so far.
enum Frame {
    Quote {
        alert: Option<Alert>,
        blocks: Vec<Block>,
    },
    List {
        start: Option<u64>,
        tight: bool,
        items: Vec<Item>,
    },
    Item {
        task: Option<Task>,
        blocks: Vec<Block>,
    },
    Footnote {
        label: String,
        blocks: Vec<Block>,
    },
    Table {
        columns: Vec<Column>,
        head: Vec<Inline>,
        rows: Vec<Vec<Inline>>,
        row: Vec<Inline>,
    },
}

/// What the open run of text becomes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Run {
    Paragraph,
    /// A tight list item's text, which no paragraph wraps.
    Implicit,
    Heading(u8),
    Cell,
    Code,
}

struct Open {
    run: Run,
    text: Inline,
    /// The pictures in it, and whether it holds anything else: a paragraph
    /// of pictures alone shows them.
    pictures: Vec<Picture>,
    words: bool,
}

/// The inline styles open where the text is.
#[derive(Default)]
struct Marks {
    emphasis: u16,
    strong: u16,
    strike: u16,
    underline: u16,
    superscript: u16,
    subscript: u16,
    code: u16,
    mark: u16,
    links: Vec<Arc<str>>,
}

/// The fill behind `<mark>`ed text.
const MARK: Rgba = Rgba {
    r: 1.0,
    g: 0.87,
    b: 0.35,
    a: 0.55,
};

impl Marks {
    fn style(&self, look: &MarkdownStyle) -> SpanStyle {
        let mut style = SpanStyle::default();
        if self.strong > 0 {
            style = style.bold();
        }
        if self.emphasis > 0 {
            style = style.italic();
        }
        if self.strike > 0 {
            style = style.strikethrough();
        }
        if self.underline > 0 {
            style = style.underline();
        }
        if self.superscript > 0 {
            style = style.superscript();
        } else if self.subscript > 0 {
            style = style.subscript();
        }
        if self.code > 0 {
            style = style
                .monospace()
                .scale(look.code_scale)
                .background(look.code_background);
        }
        if self.mark > 0 {
            style = style.background(MARK);
        }
        if let Some(link) = self.links.last() {
            style = style.link(Arc::clone(link)).color(look.link);
        }
        style
    }
}

struct Fold<'a> {
    look: &'a MarkdownStyle,
    base: usize,
    tops: Vec<Top>,
    /// The source of the top-level block being folded.
    top: Range<usize>,
    frames: Vec<Frame>,
    open: Option<Open>,
    marks: Marks,
    /// The picture whose text alternative is being read.
    picture: Option<Picture>,
    /// How deep in an HTML or metadata block, whose content shows nothing.
    skip: u32,
}

impl Fold<'_> {
    fn event(&mut self, event: Event<'_>, range: Range<usize>) {
        if self.skip > 0 {
            match event {
                Event::Start(Tag::HtmlBlock | Tag::MetadataBlock(_)) => self.skip += 1,
                Event::End(TagEnd::HtmlBlock | TagEnd::MetadataBlock(_)) => self.skip -= 1,
                _ => {}
            }
            return;
        }
        let at_top = self.frames.is_empty() && self.open.is_none();
        if at_top && matches!(event, Event::Start(_) | Event::Rule) {
            self.top = self.base + range.start..self.base + range.end;
        }
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => match &mut self.picture {
                Some(picture) => picture.alt.push_str(&text),
                None => self.text(&text),
            },
            Event::Code(code) => {
                self.marks.code += 1;
                self.text(&code);
                self.marks.code -= 1;
            }
            Event::InlineMath(math) | Event::DisplayMath(math) => {
                self.marks.code += 1;
                self.text(&math);
                self.marks.code -= 1;
            }
            Event::InlineHtml(html) => self.html(&html),
            Event::Html(_) => {}
            Event::FootnoteReference(label) => {
                self.marks.superscript += 1;
                self.text(&label);
                self.marks.superscript -= 1;
            }
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => self.text("\n"),
            Event::Rule => {
                self.close_implicit();
                self.emit(Block::Rule);
            }
            Event::TaskListMarker(checked) => {
                if let Some(Frame::Item { task, .. }) = self.frames.last_mut() {
                    *task = Some(Task {
                        checked,
                        at: self.base + range.start,
                    });
                }
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                self.close_implicit();
                let n = self.frames.len();
                if n >= 2
                    && matches!(self.frames[n - 1], Frame::Item { .. })
                    && let Frame::List { tight, .. } = &mut self.frames[n - 2]
                {
                    *tight = false;
                }
                self.open(Run::Paragraph);
            }
            Tag::Heading { level, .. } => {
                self.close_implicit();
                self.open(Run::Heading(level as u8));
            }
            Tag::CodeBlock(_) => {
                self.close_implicit();
                self.open(Run::Code);
            }
            Tag::BlockQuote(kind) => {
                self.close_implicit();
                self.frames.push(Frame::Quote {
                    alert: kind.map(alert),
                    blocks: Vec::new(),
                });
            }
            Tag::List(start) => {
                self.close_implicit();
                self.frames.push(Frame::List {
                    start,
                    tight: true,
                    items: Vec::new(),
                });
            }
            Tag::Item => self.frames.push(Frame::Item {
                task: None,
                blocks: Vec::new(),
            }),
            Tag::FootnoteDefinition(label) => {
                self.close_implicit();
                self.frames.push(Frame::Footnote {
                    label: label.into_string(),
                    blocks: Vec::new(),
                });
            }
            Tag::Table(columns) => {
                self.close_implicit();
                self.frames.push(Frame::Table {
                    columns: columns.into_iter().map(column).collect(),
                    head: Vec::new(),
                    rows: Vec::new(),
                    row: Vec::new(),
                });
            }
            Tag::TableCell => self.open(Run::Cell),
            Tag::Emphasis => self.marks.emphasis += 1,
            Tag::Strong => self.marks.strong += 1,
            Tag::Strikethrough => self.marks.strike += 1,
            Tag::Superscript => self.marks.superscript += 1,
            Tag::Subscript => self.marks.subscript += 1,
            Tag::Link { dest_url, .. } => self.marks.links.push(Arc::from(&*dest_url)),
            Tag::Image { dest_url, .. } => {
                self.picture = Some(Picture {
                    url: dest_url.into_string(),
                    alt: String::new(),
                    link: self.marks.links.last().cloned(),
                });
            }
            Tag::HtmlBlock | Tag::MetadataBlock(_) => self.skip += 1,
            Tag::TableHead
            | Tag::TableRow
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::CodeBlock => self.close(),
            TagEnd::TableCell => {
                let text = self.open.take().map(|o| o.text).unwrap_or_default();
                if let Some(Frame::Table { row, .. }) = self.frames.last_mut() {
                    row.push(text);
                }
            }
            TagEnd::TableHead => {
                if let Some(Frame::Table { head, row, .. }) = self.frames.last_mut() {
                    *head = std::mem::take(row);
                }
            }
            TagEnd::TableRow => {
                if let Some(Frame::Table { rows, row, .. }) = self.frames.last_mut() {
                    rows.push(std::mem::take(row));
                }
            }
            TagEnd::Item => {
                self.close_implicit();
                if let Some(Frame::Item { task, blocks }) = self.frames.pop()
                    && let Some(Frame::List { items, .. }) = self.frames.last_mut()
                {
                    items.push(Item { task, blocks });
                }
            }
            TagEnd::BlockQuote(_)
            | TagEnd::List(_)
            | TagEnd::FootnoteDefinition
            | TagEnd::Table => {
                self.close_implicit();
                let block = match self.frames.pop() {
                    Some(Frame::Quote { alert, blocks }) => Block::Quote { alert, blocks },
                    Some(Frame::List {
                        start,
                        tight,
                        items,
                    }) => Block::List {
                        start,
                        tight,
                        items,
                    },
                    Some(Frame::Footnote { label, blocks }) => Block::Footnote { label, blocks },
                    Some(Frame::Table {
                        columns,
                        head,
                        rows,
                        ..
                    }) => Block::Table {
                        columns,
                        head,
                        rows,
                    },
                    Some(Frame::Item { .. }) | None => return,
                };
                self.emit(block);
            }
            TagEnd::Emphasis => self.marks.emphasis -= 1,
            TagEnd::Strong => self.marks.strong -= 1,
            TagEnd::Strikethrough => self.marks.strike -= 1,
            TagEnd::Superscript => self.marks.superscript -= 1,
            TagEnd::Subscript => self.marks.subscript -= 1,
            TagEnd::Link => {
                self.marks.links.pop();
            }
            TagEnd::Image => {
                if let Some(picture) = self.picture.take() {
                    self.marks.emphasis += 1;
                    let alt = if picture.alt.is_empty() {
                        "image"
                    } else {
                        &picture.alt
                    };
                    let style = self.marks.style(self.look);
                    self.marks.emphasis -= 1;
                    let open = self.opened();
                    open.text.push(alt, &style);
                    open.pictures.push(picture);
                }
            }
            TagEnd::HtmlBlock
            | TagEnd::MetadataBlock(_)
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition => {}
        }
    }

    fn open(&mut self, run: Run) {
        self.open = Some(Open {
            run,
            text: Inline::default(),
            pictures: Vec::new(),
            words: false,
        });
    }

    /// The open run, a tight item's text opening when none is.
    fn opened(&mut self) -> &mut Open {
        if self.open.is_none() {
            self.open(Run::Implicit);
        }
        self.open.as_mut().expect("opened above")
    }

    fn text(&mut self, text: &str) {
        let style = self.marks.style(self.look);
        let open = self.opened();
        if open.run == Run::Code {
            open.text.text.push_str(text);
            return;
        }
        open.words |= !text.trim().is_empty();
        open.text.push(text, &style);
    }

    /// An inline tag: the line break, and the styles the GitHub sanitizer
    /// keeps. Any other tag shows nothing; the text between shows as text.
    fn html(&mut self, html: &str) {
        let tag = html.trim();
        let Some(tag) = tag.strip_prefix('<') else {
            return;
        };
        let (closing, tag) = match tag.strip_prefix('/') {
            Some(tag) => (true, tag),
            None => (false, tag),
        };
        let name: String = tag
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect();
        if name == "br" {
            self.text("\n");
            return;
        }
        if tag.trim_end_matches('>').ends_with('/') {
            return;
        }
        let mark = match name.as_str() {
            "b" | "strong" => &mut self.marks.strong,
            "i" | "em" | "cite" | "var" => &mut self.marks.emphasis,
            "s" | "del" | "strike" => &mut self.marks.strike,
            "u" | "ins" => &mut self.marks.underline,
            "sup" => &mut self.marks.superscript,
            "sub" => &mut self.marks.subscript,
            "code" | "kbd" | "samp" | "tt" => &mut self.marks.code,
            "mark" => &mut self.marks.mark,
            _ => return,
        };
        *mark = if closing {
            mark.saturating_sub(1)
        } else {
            *mark + 1
        };
    }

    /// Close the open run into the block it is.
    fn close(&mut self) {
        let Some(open) = self.open.take() else {
            return;
        };
        match open.run {
            Run::Paragraph | Run::Implicit => {
                if !open.words && !open.pictures.is_empty() {
                    for picture in open.pictures {
                        self.emit(Block::Picture(picture));
                    }
                } else {
                    self.emit(Block::Paragraph(open.text.trimmed()));
                }
            }
            Run::Heading(level) => self.emit(Block::Heading {
                level,
                text: open.text.trimmed(),
            }),
            Run::Code => {
                let mut code = open.text.text;
                if code.ends_with('\n') {
                    code.pop();
                }
                self.emit(Block::Code(code));
            }
            Run::Cell => {}
        }
    }

    fn close_implicit(&mut self) {
        if self.open.as_ref().is_some_and(|o| o.run == Run::Implicit) {
            self.close();
        }
    }

    fn emit(&mut self, block: Block) {
        match self.frames.last_mut() {
            None => self.tops.push(Top {
                block,
                range: self.top.clone(),
            }),
            Some(
                Frame::Quote { blocks, .. }
                | Frame::Item { blocks, .. }
                | Frame::Footnote { blocks, .. },
            ) => blocks.push(block),
            Some(Frame::List { .. } | Frame::Table { .. }) => {}
        }
    }
}

fn alert(kind: BlockQuoteKind) -> Alert {
    match kind {
        BlockQuoteKind::Note => Alert::Note,
        BlockQuoteKind::Tip => Alert::Tip,
        BlockQuoteKind::Important => Alert::Important,
        BlockQuoteKind::Warning => Alert::Warning,
        BlockQuoteKind::Caution => Alert::Caution,
    }
}

fn column(align: Alignment) -> Column {
    match align {
        Alignment::None | Alignment::Left => Column::Start,
        Alignment::Center => Column::Center,
        Alignment::Right => Column::End,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks(src: &str) -> Vec<Block> {
        parse(src, 0, &MarkdownStyle::default())
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
    fn each_top_level_block_keeps_the_source_it_came_from() {
        let src = "# Title\n\nSome *words*.\n\n- a\n- b\n\n---\n";
        let parsed = parse(src, 0, &MarkdownStyle::default());
        let ranges: Vec<&str> = parsed
            .blocks
            .iter()
            .map(|t| src[t.range.clone()].trim_end())
            .collect();
        assert_eq!(ranges, ["# Title", "Some *words*.", "- a\n- b", "---"]);
        assert_eq!(
            parse("x", 10, &MarkdownStyle::default()).blocks[0].range,
            10..11
        );
    }

    #[test]
    fn inline_styles_become_spans_over_one_text() {
        let look = MarkdownStyle::default();
        let [Block::Paragraph(text)] = &blocks("a **b** *c* ~~d~~ `e` [f](/f)<br>g")[..] else {
            panic!("one paragraph");
        };
        assert_eq!(text.text, "a b c d e f\ng");
        let styled: Vec<(&str, &SpanStyle)> = text
            .spans
            .iter()
            .map(|s| (&text.text[s.range.clone()], &s.style))
            .collect();
        assert_eq!(styled.len(), 5);
        assert_eq!(styled[0], ("b", &SpanStyle::default().bold()));
        assert_eq!(styled[1], ("c", &SpanStyle::default().italic()));
        assert_eq!(styled[2], ("d", &SpanStyle::default().strikethrough()));
        assert_eq!(styled[3].0, "e");
        assert_eq!(styled[3].1.background, Some(look.code_background));
        assert_eq!(styled[4].0, "f");
        assert_eq!(styled[4].1.link.as_deref(), Some("/f"));
    }

    #[test]
    fn lists_nest_and_know_their_tasks_numbers_and_tightness() {
        let list = blocks("3. one\n4. two\n   - [x] done\n   - [ ] todo\n");
        let [
            Block::List {
                start: Some(3),
                tight: true,
                items,
            },
        ] = &list[..]
        else {
            panic!("an ordered tight list: {list:?}");
        };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].blocks, [Block::Paragraph(plain("one"))]);
        let Block::List { start: None, .. } = &items[1].blocks[1] else {
            panic!("a nested bullet list");
        };
        let mut tasks = Vec::new();
        list[0].tasks(&mut tasks);
        let src = "3. one\n4. two\n   - [x] done\n   - [ ] todo\n";
        let marks: Vec<(bool, &str)> = tasks
            .iter()
            .map(|t| (t.checked, &src[t.at..t.at + 3]))
            .collect();
        assert_eq!(marks, [(true, "[x]"), (false, "[ ]")]);
        let [Block::List { tight: false, .. }] = &blocks("- a\n\n- b\n")[..] else {
            panic!("a loose list");
        };
    }

    #[test]
    fn code_tables_quotes_rules_and_pictures_fold_to_their_blocks() {
        assert_eq!(
            blocks("```rust\nfn main() {}\n```\n"),
            [Block::Code("fn main() {}".into())]
        );
        let table = blocks("| a | b |\n|:-:|--:|\n| 1 | 2 |\n");
        assert_eq!(
            table,
            [Block::Table {
                columns: vec![Column::Center, Column::End],
                head: vec![plain("a"), plain("b")],
                rows: vec![vec![plain("1"), plain("2")]],
            }]
        );
        assert_eq!(
            blocks("> [!WARNING]\n> careful\n"),
            [Block::Quote {
                alert: Some(Alert::Warning),
                blocks: vec![Block::Paragraph(plain("careful"))],
            }]
        );
        assert_eq!(
            blocks("![a cat](cat.png)"),
            [Block::Picture(Picture {
                url: "cat.png".into(),
                alt: "a cat".into(),
                link: None,
            })]
        );
        let [Block::Paragraph(text)] = &blocks("see ![a cat](cat.png) here")[..] else {
            panic!("a picture among words stays in its text");
        };
        assert_eq!(text.text, "see a cat here");
        assert_eq!(blocks("<div>\nhidden\n</div>\n\n***"), [Block::Rule]);
    }

    #[test]
    fn reference_definitions_change_the_parse_s_defs() {
        let look = MarkdownStyle::default();
        let a = parse("[x]\n\n[x]: /a\n", 0, &look);
        let b = parse("[x]\n\n[x]: /b\n", 0, &look);
        assert_ne!(a.defs, b.defs);
        assert!(parse("plain", 0, &look).defs.is_empty());
    }
}
