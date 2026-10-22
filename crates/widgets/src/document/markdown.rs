//! Markdown into blocks: a CommonMark parse with the GitHub extensions
//! (tables, task lists, strikethrough, footnotes, alerts).

use pulldown_cmark::{Options, Parser};

use super::DocumentStyle;
use super::blocks::{Fold, Parsed};

const OPTIONS: Options = Options::ENABLE_TABLES
    .union(Options::ENABLE_STRIKETHROUGH)
    .union(Options::ENABLE_TASKLISTS)
    .union(Options::ENABLE_FOOTNOTES)
    .union(Options::ENABLE_GFM);

/// Parse `src`, which starts at byte `base` of the document. The link
/// reference definitions reach every block, so the parse names them.
pub(super) fn parse(src: &str, base: usize, look: &DocumentStyle) -> Parsed {
    let mut events = Parser::new_ext(src, OPTIONS).into_offset_iter();
    let mut fold = Fold::new(look, base);
    for (event, range) in events.by_ref() {
        fold.event(event, range);
    }
    let mut defs: Vec<&str> = events
        .reference_definitions()
        .iter()
        .map(|(_, def)| &src[def.span.clone()])
        .collect();
    defs.sort_unstable();
    Parsed {
        blocks: fold.finish(),
        defs: defs.join("\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::blocks::{Alert, Block, Column, Inline, Picture};
    use viso_ui::SpanStyle;

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
    fn each_top_level_block_keeps_the_source_it_came_from() {
        let src = "# Title\n\nSome *words*.\n\n- a\n- b\n\n---\n";
        let parsed = parse(src, 0, &DocumentStyle::default());
        let ranges: Vec<&str> = parsed
            .blocks
            .iter()
            .map(|t| src[t.range.clone()].trim_end())
            .collect();
        assert_eq!(ranges, ["# Title", "Some *words*.", "- a\n- b", "---"]);
        assert_eq!(
            parse("x", 10, &DocumentStyle::default()).blocks[0].range,
            10..11
        );
    }

    #[test]
    fn inline_styles_become_spans_over_one_text() {
        let look = DocumentStyle::default();
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
        let look = DocumentStyle::default();
        let a = parse("[x]\n\n[x]: /a\n", 0, &look);
        let b = parse("[x]\n\n[x]: /b\n", 0, &look);
        assert_ne!(a.defs, b.defs);
        assert!(parse("plain", 0, &look).defs.is_empty());
    }
}
