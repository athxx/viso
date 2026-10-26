//! A line laid out on the grid: the rows it wraps to, where each grapheme
//! and inlay sits in cells, and the cell under a point.
//!
//! Everything here is worked out from the text and the cell counts alone, a
//! line at a time, so it is known the moment the text changes and costs
//! nothing for the lines not on screen. A row starts at a grapheme boundary;
//! one wrapped from the row before is indented as far as its line is, unless
//! that leaves too little room.

use unicode_segmentation::UnicodeSegmentation;

use crate::text::{grapheme_cells, tab_cells};

/// How lines are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutRules {
    /// The cells between tab stops.
    pub tab: u32,
    /// The cells a row holds before it wraps; `None` not to wrap.
    pub wrap: Option<u32>,
}

impl Default for LayoutRules {
    fn default() -> Self {
        LayoutRules { tab: 4, wrap: None }
    }
}

/// Text drawn between a line's characters that is not in it: an inlay hint,
/// a folded range's placeholder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InlaySpan {
    /// The byte of the line it sits before; the line's length for its end.
    pub at: usize,
    /// The cells it takes.
    pub cells: u32,
}

/// One row of a line: bytes `start..end`, drawn from cell `indent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowSpan {
    pub start: usize,
    pub end: usize,
    /// The cells left blank before it: a wrapped row's indent.
    pub indent: u32,
    /// The cells it takes, its indent with them.
    pub cells: u32,
}

/// What sits in a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    /// Graphemes the monospace face draws a cell each: printable ASCII,
    /// and the Latin, Greek and Cyrillic letters and box drawing it is
    /// made with.
    Grid,
    /// One grapheme drawn alone at its cell: wide, or of a script the
    /// face may lack.
    Glyph,
    /// A tab, to the next stop.
    Tab,
    /// A control character, drawn as its symbol.
    Control,
    /// Inlay `n` of the line.
    Inlay(usize),
}

/// One grapheme, or inlay, of a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    pub kind: ItemKind,
    /// Its bytes in the line; empty for an inlay.
    pub start: usize,
    pub end: usize,
    /// The cell it starts at.
    pub col: u32,
    /// The cells it takes.
    pub cells: u32,
}

/// Whether every byte of `text` is printable ASCII or a tab: a grapheme a
/// byte.
#[inline]
pub fn is_simple(text: &str) -> bool {
    text.bytes()
        .all(|b| (0x20..0x7f).contains(&b) || b == b'\t')
}

/// Whether the face draws `c` a cell wide on the grid, in a run with its
/// neighbors.
#[inline]
fn on_grid(c: char) -> bool {
    let c = c as u32;
    (0x20..0x7f).contains(&c) || (0xa0..0x0530).contains(&c) || (0x2500..0x25a0).contains(&c)
}

/// The items of `text`'s bytes `start..end`, from cell `col`, with the
/// `inlays` (sorted, in line bytes) that sit among them. An inlay at `end`
/// comes only when `end` is the line's end.
pub fn items<'a>(
    text: &'a str,
    span: RowSpan,
    inlays: &'a [InlaySpan],
    tab: u32,
) -> impl Iterator<Item = Item> + 'a {
    let simple = is_simple(&text[span.start..span.end]);
    let first = inlays.partition_point(|i| i.at < span.start);
    Items {
        text,
        at: span.start,
        end: span.end,
        col: span.indent,
        tab,
        simple,
        inlays,
        inlay: first,
        line_end: text.len(),
    }
}

struct Items<'a> {
    text: &'a str,
    at: usize,
    end: usize,
    col: u32,
    tab: u32,
    simple: bool,
    inlays: &'a [InlaySpan],
    inlay: usize,
    line_end: usize,
}

impl Iterator for Items<'_> {
    type Item = Item;

    fn next(&mut self) -> Option<Item> {
        if let Some(i) = self.inlays.get(self.inlay)
            && i.at == self.at
            && (self.at < self.end || self.end == self.line_end)
        {
            let item = Item {
                kind: ItemKind::Inlay(self.inlay),
                start: self.at,
                end: self.at,
                col: self.col,
                cells: i.cells,
            };
            self.inlay += 1;
            self.col += i.cells;
            return Some(item);
        }
        if self.at >= self.end {
            return None;
        }
        let start = self.at;
        let (end, kind, cells) = if self.simple {
            let b = self.text.as_bytes()[start];
            if b == b'\t' {
                (start + 1, ItemKind::Tab, tab_cells(self.col, self.tab))
            } else {
                (start + 1, ItemKind::Grid, 1)
            }
        } else {
            let g = self.text[start..self.end]
                .graphemes(true)
                .next()
                .unwrap_or(&self.text[start..self.end]);
            let c = g.chars().next().unwrap_or(' ');
            if g == "\t" {
                (start + 1, ItemKind::Tab, tab_cells(self.col, self.tab))
            } else if c.is_control() {
                (start + g.len(), ItemKind::Control, 1)
            } else {
                let cells = grapheme_cells(g);
                let kind = if cells == 1 && g.chars().all(on_grid_or_mark) {
                    ItemKind::Grid
                } else {
                    ItemKind::Glyph
                };
                (start + g.len(), kind, cells)
            }
        };
        self.at = end;
        let item = Item {
            kind,
            start,
            end,
            col: self.col,
            cells,
        };
        self.col += cells;
        Some(item)
    }
}

/// Whether `c` stays on the grid in a run: a grid character, or a mark
/// combining with one.
fn on_grid_or_mark(c: char) -> bool {
    on_grid(c) || (0x0300..0x0370).contains(&(c as u32))
}

/// The rows `text` lays out to, with `inlays` among its bytes, under
/// `rules`: one unless it wraps.
pub fn rows(text: &str, inlays: &[InlaySpan], rules: LayoutRules) -> Vec<RowSpan> {
    let mut out = Vec::with_capacity(1);
    wrap(text, inlays, rules, |row| out.push(row));
    out
}

/// The number of rows `text` lays out to.
pub fn row_count(text: &str, inlays: &[InlaySpan], rules: LayoutRules) -> usize {
    if rules.wrap.is_none() {
        return 1;
    }
    let mut n = 0;
    wrap(text, inlays, rules, |_| n += 1);
    n
}

/// The cells `text` takes on one row.
pub fn line_cells(text: &str, inlays: &[InlaySpan], tab: u32) -> u32 {
    if inlays.is_empty() && !text.contains('\t') && text.is_ascii() {
        return text.len() as u32;
    }
    let span = RowSpan {
        start: 0,
        end: text.len(),
        indent: 0,
        cells: 0,
    };
    items(text, span, inlays, tab)
        .last()
        .map_or(0, |i| i.col + i.cells)
}

/// Lays `text` out in rows and hands each to `row`.
fn wrap(text: &str, inlays: &[InlaySpan], rules: LayoutRules, mut row: impl FnMut(RowSpan)) {
    let whole = RowSpan {
        start: 0,
        end: text.len(),
        indent: 0,
        cells: 0,
    };
    let Some(width) = rules.wrap.map(|w| w.max(8)) else {
        row(RowSpan {
            cells: line_cells(text, inlays, rules.tab),
            ..whole
        });
        return;
    };
    if inlays.is_empty() && text.len() as u32 <= width && is_simple(text) && !text.contains('\t') {
        row(RowSpan {
            cells: text.len() as u32,
            ..whole
        });
        return;
    }
    // A wrapped row starts as far in as its line, unless that leaves less
    // than half the width.
    let indent = crate::text::indent_cells(text, rules.tab).unwrap_or(0);
    let indent = if indent * 2 > width { 0 } else { indent };
    let mut start = 0;
    let mut first = true;
    loop {
        let from = if first { 0 } else { indent };
        let span = RowSpan {
            start,
            end: text.len(),
            indent: from,
            cells: 0,
        };
        // The last place a row may end at: before a word after a space, or
        // after punctuation.
        let mut brk: Option<(usize, u32)> = None;
        let mut prev_space = false;
        let mut prev_punct = false;
        let mut cut: Option<(usize, u32)> = None;
        let mut col = from;
        let mut any = false;
        for item in items(text, span, inlays, rules.tab) {
            let space = matches!(item.kind, ItemKind::Tab) || text[item.start..item.end] == *" ";
            if any && !space && (prev_space || prev_punct) {
                brk = Some((item.start, item.col));
            }
            if any && !space && item.col + item.cells > width {
                cut = Some(brk.unwrap_or((item.start, item.col)));
                break;
            }
            // Spaces may run past the edge: they draw nothing.
            any = true;
            col = item.col + item.cells;
            prev_space = space;
            prev_punct = !space
                && item.end > item.start
                && text[item.start..item.end]
                    .chars()
                    .next()
                    .is_some_and(|c| crate::text::char_class(c) == crate::text::CharClass::Punct);
        }
        match cut {
            Some((end, cells)) if end > start => {
                row(RowSpan {
                    start,
                    end,
                    indent: from,
                    cells,
                });
                start = end;
                first = false;
            }
            _ => {
                row(RowSpan {
                    start,
                    end: text.len(),
                    indent: from,
                    cells: col,
                });
                return;
            }
        }
    }
}

/// The row of `rows` byte `at` is drawn on: a byte where one row ends and
/// the next starts is on the next.
pub fn row_of(rows: &[RowSpan], at: usize) -> usize {
    rows.partition_point(|r| r.end <= at && r.end != r.start)
        .min(rows.len() - 1)
}

/// The cell a caret before byte `at` of row `span` stands at. A caret
/// before an inlay stands before it.
pub fn col_of(text: &str, span: RowSpan, inlays: &[InlaySpan], at: usize, tab: u32) -> u32 {
    if at <= span.start {
        return span.indent;
    }
    let mut col = span.indent;
    for item in items(text, span, inlays, tab) {
        if matches!(item.kind, ItemKind::Inlay(_)) {
            if item.start >= at {
                return item.col;
            }
        } else if item.start >= at {
            return item.col;
        }
        col = item.col + item.cells;
    }
    col
}

/// The grapheme boundary of row `span` nearest cell `x` (fractional, from
/// the row's start): the caret a click there places. A click in an inlay
/// lands at its byte.
pub fn byte_at(text: &str, span: RowSpan, inlays: &[InlaySpan], x: f32, tab: u32) -> usize {
    for item in items(text, span, inlays, tab) {
        let mid = item.col as f32 + item.cells as f32 / 2.0;
        if matches!(item.kind, ItemKind::Inlay(_)) {
            if x < (item.col + item.cells) as f32 {
                return item.start;
            }
            continue;
        }
        if x < mid {
            return item.start;
        }
    }
    span.end
}

/// The grapheme boundary of row `span` at or before cell `goal`: where a
/// caret moving up or down onto the row lands.
pub fn byte_at_col(text: &str, span: RowSpan, inlays: &[InlaySpan], goal: u32, tab: u32) -> usize {
    let mut last = span.start;
    for item in items(text, span, inlays, tab) {
        if matches!(item.kind, ItemKind::Inlay(_)) {
            continue;
        }
        if goal < item.col + item.cells {
            // In a wide grapheme or a tab: the nearer side of it.
            return if goal - item.col.min(goal) < item.cells.div_ceil(2) {
                item.start
            } else {
                item.end
            };
        }
        last = item.end;
    }
    // Past the row's end: its end, or, on a row wrapped from, before its
    // last grapheme so the caret stays on it.
    if span.end < text.len() && last == span.end {
        return crate::text::prev_grapheme(text, span.end).max(span.start);
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(text: &str, width: u32) -> Vec<(usize, usize, u32)> {
        rows(
            text,
            &[],
            LayoutRules {
                tab: 4,
                wrap: Some(width),
            },
        )
        .iter()
        .map(|r| (r.start, r.end, r.indent))
        .collect()
    }

    #[test]
    fn rows_wrap_before_words_and_keep_the_indent() {
        assert_eq!(spans("short", 20), [(0, 5, 0)]);
        // "    let value = compute(first, second);" at 20 cells.
        let line = "    let value = compute(first, second);";
        let rows = spans(line, 20);
        assert_eq!(rows[0], (0, 16, 0));
        assert_eq!(&line[rows[0].0..rows[0].1], "    let value = ");
        assert_eq!(&line[rows[1].0..rows[1].1], "compute(first, ");
        assert_eq!(rows[1].2, 4);
        assert_eq!(&line[rows[2].0..rows[2].1], "second);");
        // A word longer than a row breaks where it must.
        let rows = spans("abcdefghijklmnopqrstuvwxyz", 10);
        assert_eq!(rows, [(0, 10, 0), (10, 20, 0), (20, 26, 0)]);
        assert_eq!(
            row_count(
                "abcdefghijklmnopqrstuvwxyz",
                &[],
                LayoutRules {
                    tab: 4,
                    wrap: Some(10)
                }
            ),
            3
        );
    }

    #[test]
    fn wide_glyphs_take_two_cells_and_split_from_runs() {
        let text = "a日b\tc";
        let span = RowSpan {
            start: 0,
            end: text.len(),
            indent: 0,
            cells: 0,
        };
        let items: Vec<_> = items(text, span, &[], 4)
            .map(|i| (i.kind, i.col, i.cells))
            .collect();
        assert_eq!(
            items,
            [
                (ItemKind::Grid, 0, 1),
                (ItemKind::Glyph, 1, 2),
                (ItemKind::Grid, 3, 1),
                (ItemKind::Tab, 4, 4),
                (ItemKind::Grid, 8, 1)
            ]
        );
        assert_eq!(col_of(text, span, &[], 4, 4), 3);
        assert_eq!(byte_at(text, span, &[], 2.2, 4), 4);
        assert_eq!(byte_at(text, span, &[], 1.9, 4), 1);
        assert_eq!(byte_at(text, span, &[], 5.5, 4), 5);
        assert_eq!(byte_at(text, span, &[], 6.5, 4), 6);
        assert_eq!(line_cells(text, &[], 4), 9);
    }

    #[test]
    fn inlays_take_cells_and_a_caret_stands_before_them() {
        let text = "let x = 1;";
        let inlays = [InlaySpan { at: 5, cells: 5 }];
        let span = rows(text, &inlays, LayoutRules::default())[0];
        assert_eq!(span.cells, 15);
        assert_eq!(col_of(text, span, &inlays, 5, 4), 5);
        assert_eq!(col_of(text, span, &inlays, 6, 4), 11);
        // A click inside the inlay lands at its byte.
        assert_eq!(byte_at(text, span, &inlays, 7.0, 4), 5);
        assert_eq!(byte_at(text, span, &inlays, 11.2, 4), 6);
        // At the line's end too.
        let inlays = [InlaySpan { at: 10, cells: 3 }];
        let span = rows(text, &inlays, LayoutRules::default())[0];
        assert_eq!(span.cells, 13);
    }

    #[test]
    fn a_goal_cell_lands_on_the_grapheme_there() {
        let text = "ab日cd";
        let span = rows(text, &[], LayoutRules::default())[0];
        assert_eq!(byte_at_col(text, span, &[], 0, 4), 0);
        assert_eq!(byte_at_col(text, span, &[], 2, 4), 2);
        assert_eq!(byte_at_col(text, span, &[], 3, 4), 5);
        assert_eq!(byte_at_col(text, span, &[], 40, 4), text.len());
        let rows = rows(
            "abcdefghijklmnop",
            &[],
            LayoutRules {
                tab: 4,
                wrap: Some(8),
            },
        );
        assert_eq!(row_of(&rows, 7), 0);
        assert_eq!(row_of(&rows, 8), 1);
        assert_eq!(row_of(&rows, 16), 1);
        // Past a wrapped row's end the caret stays on it.
        assert_eq!(byte_at_col("abcdefghijklmnop", rows[0], &[], 20, 4), 7);
    }
}
