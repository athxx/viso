//! A row of the editor: one line, its text on the cell grid, what lies
//! behind and over it, and its piece of the gutter.

use std::ops::Range;
use std::rc::Rc;

use viso_ui::{
    AbsoluteStyle, BoxStyle, BuildCx, Content, DirtyClass, FontSlant, FontWeight, LeafStyle,
    Length, NodeId, PathCmd, Point, Rgba, Size, SpanStyle, Stroke, StructureCx, TextAlign,
    TextSpan, Vec2,
};

use super::{Metrics, Shared, text_request};
use crate::decoration::{DecorationStyle, Severity};
use crate::highlight::{Token, TokenKind};
use crate::layout::{self, InlaySpan, ItemKind, RowSpan};
use crate::session::{InlaySource, LineInlays, Session};
use crate::theme::Theme;

/// One drawn text segment.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Seg {
    pub x: f32,
    pub y: f32,
    pub text: String,
    pub color: Rgba,
    pub spans: Vec<TextSpan>,
    /// The width of the cells a grapheme drawn alone is centered in; `None`
    /// for a run, as wide as it is shaped.
    pub cells_w: Option<f32>,
}

/// One drawn rectangle, or a wavy line as wide.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Mark {
    pub x: f32,
    pub y: f32,
    /// `None` fills the row across.
    pub w: Option<f32>,
    pub h: f32,
    pub color: Rgba,
    pub radius: f32,
    /// A squiggle along its bottom rather than a filled box.
    pub squiggle: bool,
}

impl Mark {
    fn rect(x: f32, y: f32, w: f32, h: f32, color: Rgba) -> Mark {
        Mark {
            x,
            y,
            w: Some(w),
            h,
            color,
            radius: 0.0,
            squiggle: false,
        }
    }
}

/// What a row's text was drawn from: equal again, it is drawn as it is.
#[derive(Debug, Clone, PartialEq, Default)]
struct TextKey {
    text: String,
    tokens: Vec<Token>,
    inlays: Vec<(usize, String)>,
    preedit: Option<(usize, String)>,
    folded: bool,
    rows: usize,
    cell: f32,
    grid_wide: bool,
    text_x: f32,
    font_size: f32,
}

/// Where an inlay or a fold's placeholder sits in a row: its row, cell,
/// cells and what it is.
#[derive(Debug, Clone, Copy, PartialEq)]
struct InlayBox {
    row: usize,
    col: u32,
    cells: u32,
    source: InlaySource,
}

/// The fold marker drawn in a row's gutter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct FoldMark {
    pub node: NodeId,
    pub folded: bool,
    pub x: f32,
}

/// A row the editor built.
#[derive(Debug)]
pub(super) struct RowView {
    /// The box as tall as the line's rows.
    pub row: NodeId,
    marks: NodeId,
    text: NodeId,
    over: NodeId,
    gutter: NodeId,
    /// The line it shows, by id, and where it was last.
    pub line_id: u32,
    pub line: usize,
    rows: usize,
    key: TextKey,
    inlay_boxes: Vec<InlayBox>,
    pub segs: Vec<(NodeId, Seg)>,
    pub mark_nodes: Vec<(NodeId, Mark)>,
    pub over_nodes: Vec<(NodeId, Mark)>,
    /// The gutter's width and where it is moved to.
    gutter_w: f32,
    pub gutter_x: f32,
    pub numbers: Vec<(NodeId, Seg)>,
    pub gutter_marks: Vec<(NodeId, Mark)>,
    pub fold: Option<FoldMark>,
}

/// Builds the row for list row `index` under the host the list gave it.
pub(super) fn build_row(shared: &Rc<Shared>, index: usize, cx: &mut BuildCx<'_>) {
    let (line, line_id, rows) = {
        let s = shared.session.borrow();
        let line = s.line_of_row(index);
        (line, s.doc().line_id(line), s.row_count(line))
    };
    let m = shared.metrics.get();
    let lh = m.line_height;
    let fill = AbsoluteStyle {
        size: Size::fill(),
        ..AbsoluteStyle::default()
    };
    let (mut marks, mut text, mut over, mut gutter) = (None, None, None, None);
    let row = cx.absolute(
        AbsoluteStyle {
            size: Size {
                width: Length::fill(),
                height: Length::Fixed(rows as f32 * lh),
            },
            ..AbsoluteStyle::default()
        },
        |cx| {
            marks = Some(cx.absolute(fill, |_| {}).id());
            text = Some(cx.absolute(fill, |_| {}).id());
            over = Some(cx.absolute(fill, |_| {}).id());
            // Last, so the text scrolled under it hides.
            gutter = Some(
                cx.absolute(
                    AbsoluteStyle {
                        size: Size::fixed(m.gutter, rows as f32 * lh),
                        style: BoxStyle::solid(shared.theme.gutter),
                        ..AbsoluteStyle::default()
                    },
                    |_| {},
                )
                .id(),
            );
        },
    );
    let (Some(marks), Some(text), Some(over), Some(gutter)) = (marks, text, over, gutter) else {
        return;
    };
    let mut view = RowView {
        row: row.id(),
        marks,
        text,
        over,
        gutter,
        line_id,
        line,
        rows,
        key: TextKey::default(),
        inlay_boxes: Vec::new(),
        segs: Vec::new(),
        mark_nodes: Vec::new(),
        over_nodes: Vec::new(),
        gutter_w: m.gutter,
        gutter_x: 0.0,
        numbers: Vec::new(),
        gutter_marks: Vec::new(),
        fold: None,
    };
    cx.structure(|s| {
        s.store.set_hittable(gutter, false);
        pin_gutter(s, &mut view, shared.across.get());
        let mut session = shared.session.borrow_mut();
        draw_row(shared, &mut session, s, &mut view, line);
    });
    let mut rows = shared.rows.borrow_mut();
    rows.retain(|r| r.row != view.row);
    rows.push(view);
}

/// Moves every row's gutter to `offset`, how far the list is scrolled
/// sideways.
pub(super) fn pin_gutters(shared: &Shared, s: &mut StructureCx<'_>, offset: f32) {
    shared.across.set(offset);
    for view in shared.rows.borrow_mut().iter_mut() {
        pin_gutter(s, view, offset);
    }
}

fn pin_gutter(s: &mut StructureCx<'_>, view: &mut RowView, offset: f32) {
    if view.gutter_x != offset {
        view.gutter_x = offset;
        s.store
            .set_translate(view.gutter, Vec2 { x: offset, y: 0.0 });
    }
}

/// Draws line `line` into `view`, touching only what changed.
pub(super) fn draw_row(
    shared: &Shared,
    session: &mut Session,
    s: &mut StructureCx<'_>,
    view: &mut RowView,
    line: usize,
) {
    let m = shared.metrics.get();
    let lh = m.line_height;
    let text = session.doc().line(line).into_owned();
    let inl = session.line_inlays(line);
    let rows = layout::rows(&text, &inl.spans, session.rules());
    view.line = line;
    view.line_id = session.doc().line_id(line);
    if view.rows != rows.len() {
        view.rows = rows.len();
        s.store.set_fixed_size(
            view.row,
            Size {
                width: Length::fill(),
                height: Length::Fixed(rows.len() as f32 * lh),
            },
        );
        view.gutter_w = -1.0;
    }
    if view.gutter_w != m.gutter {
        view.gutter_w = m.gutter;
        s.store
            .set_fixed_size(view.gutter, Size::fixed(m.gutter, rows.len() as f32 * lh));
    }
    draw_text(shared, session, s, view, line, text, &inl, &rows);
    draw_gutter(shared, session, s, view, line, rows.len());
    // Backgrounds and what goes over the text.
    let mut marks = Vec::new();
    let mut over = Vec::new();
    row_marks(
        shared,
        session,
        line,
        &rows,
        &inl.spans,
        &view.inlay_boxes,
        &mut marks,
        &mut over,
    );
    sync_marks(s, view.marks, &mut view.mark_nodes, marks);
    sync_marks(s, view.over, &mut view.over_nodes, over);
}

/// Draws the text of line `line`, when it, its tokens, its inlays or the
/// measures changed.
#[allow(clippy::too_many_arguments)]
fn draw_text(
    shared: &Shared,
    session: &mut Session,
    s: &mut StructureCx<'_>,
    view: &mut RowView,
    line: usize,
    text: String,
    inl: &LineInlays,
    rows: &[RowSpan],
) {
    let m = shared.metrics.get();
    let theme = &shared.theme;
    let lh = m.line_height;
    let tab = session.rules().tab;
    let start = session.doc().line_start(line);
    let mut tokens = Vec::new();
    session.tokens(line, &mut tokens);
    let preedit = session
        .preedit()
        .filter(|p| session.doc().line_of(p.at) == line)
        .map(|p| (p.at - start, p.text.clone()));
    let key = TextKey {
        text,
        tokens,
        inlays: session
            .line_inlay_list(line)
            .iter()
            .map(|i| (i.at - start, i.text.clone()))
            .collect(),
        preedit,
        folded: session.folds().at(line).is_some(),
        rows: rows.len(),
        cell: m.cell,
        grid_wide: m.grid_wide,
        text_x: m.text_x(),
        font_size: m.font_size,
    };
    if key == view.key {
        return;
    }
    let mut segs = Vec::new();
    let mut boxes = Vec::new();
    // The session's inlays on this line, in the order the spans hold them.
    let mut inlay_texts = key.inlays.iter().map(|(_, t)| t);
    let mut texts: Vec<Option<&String>> = Vec::with_capacity(inl.sources.len());
    for source in &inl.sources {
        texts.push(match source {
            InlaySource::Inlay(_) => inlay_texts.next(),
            _ => None,
        });
    }
    for (r, span) in rows.iter().enumerate() {
        let y = r as f32 * lh;
        let mut run: Option<(usize, u32)> = None;
        let flush = |run: &mut Option<(usize, u32)>, end: usize, segs: &mut Vec<Seg>| {
            if let Some((a, col)) = run.take()
                && end > a
            {
                segs.push(Seg {
                    x: m.x(col),
                    y,
                    text: key.text[a..end].to_owned(),
                    color: theme.text,
                    spans: token_spans(theme, &key.tokens, a..end),
                    cells_w: None,
                });
            }
        };
        for mut item in layout::items(&key.text, *span, &inl.spans, tab) {
            // Past ASCII, a face that draws in a fallback's widths would
            // move the rest of the run off the grid.
            if item.kind == ItemKind::Grid
                && !m.grid_wide
                && !key.text.as_bytes()[item.start].is_ascii()
            {
                item.kind = ItemKind::Glyph;
            }
            match item.kind {
                ItemKind::Grid => {
                    if run.is_none() {
                        run = Some((item.start, item.col));
                    }
                }
                ItemKind::Tab => flush(&mut run, item.start, &mut segs),
                ItemKind::Glyph | ItemKind::Control => {
                    flush(&mut run, item.start, &mut segs);
                    let g = &key.text[item.start..item.end];
                    let (shown, color) = if item.kind == ItemKind::Control {
                        (control_picture(g), theme.line_number)
                    } else {
                        let kind = crate::highlight::kind_at(&key.tokens, item.start);
                        (g.to_owned(), theme.token(kind).color)
                    };
                    segs.push(Seg {
                        x: m.x(item.col),
                        y,
                        text: shown,
                        color,
                        spans: Vec::new(),
                        cells_w: Some(item.cells as f32 * m.cell),
                    });
                }
                ItemKind::Inlay(k) => {
                    flush(&mut run, item.start, &mut segs);
                    let source = inl.sources[k];
                    boxes.push(InlayBox {
                        row: r,
                        col: item.col,
                        cells: item.cells,
                        source,
                    });
                    // Centered in its cells, whatever widths its glyphs come in.
                    let (text, color) = match source {
                        InlaySource::Inlay(_) => {
                            (texts[k].cloned().unwrap_or_default(), theme.inlay_text)
                        }
                        InlaySource::Fold => ("\u{22ef}".to_owned(), theme.fold_text),
                        InlaySource::Preedit => {
                            if let Some((_, p)) = &key.preedit {
                                composition_segs(&m, theme, p, item.col, y, tab, &mut segs);
                            }
                            continue;
                        }
                    };
                    segs.push(Seg {
                        x: m.x(item.col),
                        y,
                        text,
                        color,
                        spans: Vec::new(),
                        cells_w: Some(item.cells as f32 * m.cell),
                    });
                }
            }
        }
        flush(&mut run, span.end, &mut segs);
    }
    sync_segs(shared, s, view.text, &mut view.segs, segs);
    view.inlay_boxes = boxes;
    view.key = key;
}

/// The segments of composition `text` from cell `col`, underlined: a run of
/// what keeps to the grid, and each other grapheme centered in its cells,
/// so the composition's caret, counted in cells, sits between its glyphs.
fn composition_segs(
    m: &Metrics,
    theme: &Theme,
    text: &str,
    col: u32,
    y: f32,
    tab: u32,
    segs: &mut Vec<Seg>,
) {
    let underlined = |len| {
        vec![TextSpan {
            range: 0..len,
            style: SpanStyle {
                underline: true,
                ..SpanStyle::default()
            },
        }]
    };
    let span = RowSpan {
        start: 0,
        end: text.len(),
        indent: col,
        cells: 0,
    };
    let mut run: Option<(usize, u32)> = None;
    let flush = |run: &mut Option<(usize, u32)>, end: usize, segs: &mut Vec<Seg>| {
        if let Some((a, at)) = run.take()
            && end > a
        {
            segs.push(Seg {
                x: m.x(at),
                y,
                text: text[a..end].to_owned(),
                color: theme.text,
                spans: underlined(end - a),
                cells_w: None,
            });
        }
    };
    for item in layout::items(text, span, &[], tab) {
        let ascii = text.as_bytes()[item.start].is_ascii();
        match item.kind {
            ItemKind::Grid if ascii || m.grid_wide => {
                if run.is_none() {
                    run = Some((item.start, item.col));
                }
            }
            ItemKind::Tab => flush(&mut run, item.start, segs),
            _ => {
                flush(&mut run, item.start, segs);
                let g = &text[item.start..item.end];
                segs.push(Seg {
                    x: m.x(item.col),
                    y,
                    text: g.to_owned(),
                    color: theme.text,
                    spans: underlined(g.len()),
                    cells_w: Some(item.cells as f32 * m.cell),
                });
            }
        }
    }
    flush(&mut run, text.len(), segs);
}

/// Draws line `line`'s piece of the gutter: its number, its fold marker,
/// its worst diagnostic.
fn draw_gutter(
    shared: &Shared,
    session: &Session,
    s: &mut StructureCx<'_>,
    view: &mut RowView,
    line: usize,
    rows: usize,
) {
    let m = shared.metrics.get();
    let theme = &shared.theme;
    let lh = m.line_height;
    if m.gutter <= 0.0 {
        sync_segs(shared, s, view.gutter, &mut view.numbers, Vec::new());
        sync_marks(s, view.gutter, &mut view.gutter_marks, Vec::new());
        set_fold_mark(shared, s, view, None);
        return;
    }
    let doc = session.doc();
    let fold = session.folds().at(line);
    let last = fold.map_or(line, |f| f.end);
    let current = session.selections().list().iter().any(|sel| {
        let l = doc.line_of(sel.head);
        line <= l && l <= last
    });
    let mut numbers = Vec::new();
    if shared.line_numbers {
        let n = (line + 1).to_string();
        numbers.push(Seg {
            x: m.number_right() - n.len() as f32 * m.cell,
            y: 0.0,
            text: n,
            color: if current {
                theme.line_number_current
            } else {
                theme.line_number
            },
            spans: Vec::new(),
            cells_w: None,
        });
    }
    sync_segs(shared, s, view.gutter, &mut view.numbers, numbers);
    // The most serious diagnostic over the line, or a mark of the caller's.
    let range = doc.line_start(line)..doc.line_end(last);
    let mut color = None;
    let mut worst = None;
    for d in session.decorations().in_range(range.clone()) {
        match d.style {
            DecorationStyle::Diagnostic(sev) if sev > Severity::Hint => {
                if worst.is_none_or(|w| sev > w) {
                    worst = Some(sev);
                    color = Some(theme.severity(sev));
                }
            }
            DecorationStyle::Custom {
                gutter: Some(c), ..
            } if worst.is_none() && d.range.start >= range.start => color = Some(c),
            _ => {}
        }
    }
    let marks = color
        .map(|c| Mark {
            x: (m.cell * 0.5 - 1.5).max(0.0),
            y: 2.0,
            w: Some(3.0),
            h: rows as f32 * lh - 4.0,
            color: c,
            radius: 1.5,
            squiggle: false,
        })
        .into_iter()
        .collect();
    sync_marks(s, view.gutter, &mut view.gutter_marks, marks);
    let state = if fold.is_some() {
        Some(true)
    } else if crate::fold::folds_by_indent(doc, line, session.rules().tab) {
        Some(false)
    } else {
        None
    };
    set_fold_mark(shared, s, view, state);
}

/// The fold marker's size, in dp.
fn fold_size(m: &Metrics) -> f32 {
    (m.cell * 1.2).min(m.line_height * 0.6).max(4.0)
}

/// Shows the fold marker `state` says: folded, foldable or none.
fn set_fold_mark(
    shared: &Shared,
    s: &mut StructureCx<'_>,
    view: &mut RowView,
    state: Option<bool>,
) {
    let m = shared.metrics.get();
    let size = fold_size(&m);
    let x = m.fold_x() + (m.cell * 1.75 - size) / 2.0;
    match (view.fold, state) {
        (None, None) => {}
        (Some(f), Some(folded)) if f.folded == folded => {
            if f.x != x {
                let y = (m.line_height - size) / 2.0;
                s.store.set_translate(f.node, Vec2 { x, y });
                view.fold = Some(FoldMark { x, ..f });
            }
        }
        (old, state) => {
            if let Some(f) = old {
                let mut freed = Vec::new();
                s.store.free_tree(f.node, s.effects, &mut freed);
            }
            view.fold = state.map(|folded| {
                let mut cx = BuildCx::with_parent(s.store, s.states, s.bindings, view.gutter);
                let leaf = cx.leaf(LeafStyle {
                    size: Size::fixed(size, size),
                    style: BoxStyle::NONE,
                });
                // A chevron: pointing right when folded, down when open.
                let (a, b, c) = if folded {
                    ((0.35, 0.15), (0.7, 0.5), (0.35, 0.85))
                } else {
                    ((0.15, 0.35), (0.5, 0.7), (0.85, 0.35))
                };
                let p = |(x, y): (f32, f32)| Point {
                    x: x * size,
                    y: y * size,
                };
                cx.path(
                    leaf,
                    vec![
                        PathCmd::MoveTo(p(a)),
                        PathCmd::LineTo(p(b)),
                        PathCmd::LineTo(p(c)),
                    ],
                    None,
                    Some(Stroke::new(1.5, shared.theme.fold_mark)),
                    Vec2 { x: size, y: size },
                );
                let node = leaf.id();
                s.store.set_hittable(node, false);
                s.store.set_translate(
                    node,
                    Vec2 {
                        x,
                        y: (m.line_height - size) / 2.0,
                    },
                );
                FoldMark { node, folded, x }
            });
            // A node a hook adds is laid out once its parent is.
            s.store.mark_dirty(view.gutter, DirtyClass::LAYOUT);
        }
    }
}

/// The symbol a control character draws as.
fn control_picture(g: &str) -> String {
    let c = g.chars().next().unwrap_or(' ') as u32;
    let pic = match c {
        0..=0x1f => char::from_u32(0x2400 + c),
        0x7f => Some('\u{2421}'),
        _ => None,
    };
    pic.unwrap_or('\u{fffd}').to_string()
}

/// The token spans of bytes `range` of a line, in its own offsets: plain
/// text left to the text's own color.
fn token_spans(theme: &Theme, tokens: &[Token], range: Range<usize>) -> Vec<TextSpan> {
    let mut out = Vec::new();
    for (r, kind) in crate::highlight::spans(tokens, range.end) {
        if kind == TokenKind::Text || r.end <= range.start {
            continue;
        }
        if r.start >= range.end {
            break;
        }
        let look = theme.token(kind);
        let (a, b) = (r.start.max(range.start), r.end.min(range.end));
        if b <= a {
            continue;
        }
        out.push(TextSpan {
            range: a - range.start..b - range.start,
            style: SpanStyle {
                color: Some(look.color),
                slant: look.italic.then_some(FontSlant::Italic),
                weight: look.bold.then_some(FontWeight::BOLD),
                ..SpanStyle::default()
            },
        });
    }
    out
}

/// A line's rows, to find where its bytes are drawn.
struct Geometry<'a> {
    text: &'a str,
    /// Where the line starts in the document.
    start: usize,
    rows: &'a [RowSpan],
    inlays: &'a [InlaySpan],
    tab: u32,
}

impl Geometry<'_> {
    /// The cells document bytes `a..b` cover on each row of the line, as
    /// `(row, from, to)`; `to` takes a cell more when the range goes on
    /// past the line's break with `break_cell`. An empty range covers the
    /// cell after it.
    fn cover(&self, a: usize, b: usize, break_cell: bool, mut f: impl FnMut(usize, u32, u32)) {
        let len = self.text.len();
        let past = b > self.start + len;
        let a = a.saturating_sub(self.start).min(len);
        let b = b.saturating_sub(self.start).min(len);
        let last = self.rows.len() - 1;
        let col = |at: usize, r: usize| {
            layout::col_of(self.text, self.rows[r], self.inlays, at, self.tab)
        };
        if a == b && !past {
            let r = layout::row_of(self.rows, a);
            let c = col(a, r);
            f(r, c, c + 1);
            return;
        }
        for (r, span) in self.rows.iter().enumerate() {
            let (sa, sb) = (span.start, span.end);
            let out = b < sa || (b == sa && r > 0 && !past) || a > sb || (a == sb && r < last);
            if out {
                continue;
            }
            let x0 = col(a.max(sa), r);
            let mut x1 = if b > sb || (past && r == last) {
                span.cells
            } else {
                col(b, r)
            };
            if past && r == last && break_cell {
                x1 += 1;
            }
            if x1 > x0 {
                f(r, x0, x1);
            }
        }
    }
}

/// The rectangles of line `line`: its backgrounds into `marks`, its carets
/// and underlines into `over`.
#[allow(clippy::too_many_arguments)]
fn row_marks(
    shared: &Shared,
    session: &Session,
    line: usize,
    rows: &[RowSpan],
    inlays: &[InlaySpan],
    boxes: &[InlayBox],
    marks: &mut Vec<Mark>,
    over: &mut Vec<Mark>,
) {
    let m = shared.metrics.get();
    let theme = &shared.theme;
    let lh = m.line_height;
    let tab = session.rules().tab;
    let doc = session.doc();
    let range = doc.line_range(line);
    let text = doc.line(line);
    let geo = Geometry {
        text: &text,
        start: range.start,
        rows,
        inlays,
        tab,
    };
    let focused = shared.focused.get();
    let sel = session.selections();
    let last_line = session.folds().at(line).map_or(line, |f| f.end);
    let line_end = doc.line_end(last_line);
    // What reaches into the line, its folded lines with it.
    let reach = range.start..line_end;
    let fill = |marks: &mut Vec<Mark>, r: &Range<usize>, break_cell: bool, color: Rgba| {
        geo.cover(r.start, r.end, break_cell, |row, x0, x1| {
            marks.push(Mark::rect(
                m.x(x0),
                row as f32 * lh,
                (x1 - x0) as f32 * m.cell,
                lh,
                color,
            ));
        });
    };
    // The current line, behind all.
    let current = sel.list().iter().any(|s| {
        let l = doc.line_of(s.head);
        s.is_empty() && l >= line && l <= last_line
    });
    if current && focused {
        marks.push(Mark {
            x: 0.0,
            y: 0.0,
            w: None,
            h: rows.len() as f32 * lh,
            color: theme.current_line,
            radius: 0.0,
            squiggle: false,
        });
    }
    // Decorations: their backgrounds behind, their lines over.
    for d in session.decorations().in_range(reach.clone()) {
        let (background, underline, squiggle) = match d.style {
            DecorationStyle::Diagnostic(sev) => {
                (None, Some(theme.severity(sev)), sev > Severity::Hint)
            }
            DecorationStyle::Highlight => (Some(theme.highlight), None, false),
            DecorationStyle::WriteHighlight => (Some(theme.write_highlight), None, false),
            DecorationStyle::Custom {
                background,
                underline,
                squiggle,
                whole_line,
                ..
            } => {
                if whole_line && let Some(bg) = background {
                    marks.push(Mark {
                        x: 0.0,
                        y: 0.0,
                        w: None,
                        h: rows.len() as f32 * lh,
                        color: bg,
                        radius: 0.0,
                        squiggle: false,
                    });
                    (None, underline, squiggle)
                } else {
                    (background, underline, squiggle)
                }
            }
        };
        if let Some(bg) = background
            && !d.range.is_empty()
        {
            fill(marks, &d.range, false, bg);
        }
        if let Some(color) = underline {
            geo.cover(d.range.start, d.range.end, false, |row, x0, x1| {
                let w = (x1 - x0) as f32 * m.cell;
                over.push(if squiggle {
                    Mark {
                        x: m.x(x0),
                        y: row as f32 * lh + lh - 3.5,
                        w: Some(w),
                        h: 3.0,
                        color,
                        radius: 0.0,
                        squiggle: true,
                    }
                } else {
                    Mark::rect(m.x(x0), row as f32 * lh + lh - 1.5, w, 1.0, color)
                });
            });
        }
    }
    // Matches of the search, the current one brighter.
    if let Some(search) = session.search() {
        let current = session.current_match();
        let first = search.matches().partition_point(|m| m.end <= reach.start);
        for (k, r) in search.in_range(reach.clone()).iter().enumerate() {
            let color = if current == Some(first + k) {
                theme.find_current
            } else {
                theme.find_match
            };
            fill(marks, r, false, color);
        }
    }
    // Selections.
    let color = if focused {
        theme.selection
    } else {
        theme.selection_unfocused
    };
    for s in sel.list() {
        if s.is_empty() || s.end() < range.start || s.start() > line_end {
            continue;
        }
        fill(marks, &s.range(), true, color);
    }
    // The bracket at the caret and its pair.
    if focused && let Some((a, b)) = shared.brackets.borrow().as_ref() {
        for r in [a, b] {
            if r.start >= range.start && r.end <= range.end {
                fill(marks, r, false, theme.bracket);
            }
        }
    }
    // Behind inlays and a fold's placeholder.
    for b in boxes {
        let (color, inset) = match b.source {
            InlaySource::Inlay(_) => (theme.inlay_background, m.cell * 0.25),
            InlaySource::Fold => (theme.fold_background, m.cell * 0.5),
            InlaySource::Preedit => continue,
        };
        marks.push(Mark {
            x: m.x(b.col) + inset,
            y: b.row as f32 * lh + 2.0,
            w: Some((b.cells as f32 * m.cell - inset * 2.0).max(1.0)),
            h: lh - 4.0,
            color,
            radius: 3.0,
            squiggle: false,
        });
    }
    // Carets.
    if focused && shared.blink_on.get() {
        for s in sel.list() {
            if s.head < range.start || s.head > range.end {
                continue;
            }
            let at = s.head - range.start;
            let r = layout::row_of(rows, at);
            let mut c = layout::col_of(&text, rows[r], inlays, at, tab);
            // Inside a composition, at its own caret.
            if let Some(p) = session.preedit()
                && p.at == s.head
            {
                c += crate::text::cells(&p.text[..p.caret], 0, tab);
            }
            over.push(Mark::rect(
                m.x(c) - 1.0,
                r as f32 * lh,
                2.0,
                lh,
                theme.caret,
            ));
        }
    }
}

/// Makes the text nodes under `parent` draw `want`, reusing the ones there.
fn sync_segs(
    shared: &Shared,
    s: &mut StructureCx<'_>,
    parent: NodeId,
    have: &mut Vec<(NodeId, Seg)>,
    want: Vec<Seg>,
) {
    let lh = shared.metrics.get().line_height;
    let size = |seg: &Seg| Size {
        width: seg.cells_w.map_or(Length::MaxContent, Length::Fixed),
        height: Length::Fixed(lh),
    };
    let request = |seg: &Seg| {
        let mut r = text_request(shared, seg.text.clone(), seg.color, seg.spans.clone());
        if seg.cells_w.is_some() {
            r.style.align = TextAlign::Center;
        }
        r
    };
    let mut added = false;
    let mut i = 0;
    for seg in want {
        if i < have.len() {
            let (node, old) = &mut have[i];
            if old.cells_w != seg.cells_w {
                s.store.set_fixed_size(*node, size(&seg));
            }
            if old.text != seg.text
                || old.spans != seg.spans
                || old.color != seg.color
                || old.cells_w.is_some() != seg.cells_w.is_some()
            {
                s.store.set_text(*node, request(&seg));
            }
            if old.x != seg.x || old.y != seg.y {
                s.store.set_translate(*node, Vec2 { x: seg.x, y: seg.y });
            }
            *old = seg;
        } else {
            let mut cx = BuildCx::with_parent(s.store, s.states, s.bindings, parent);
            let leaf = cx.leaf(LeafStyle {
                size: size(&seg),
                style: BoxStyle::NONE,
            });
            cx.text_request(leaf, request(&seg));
            let node = leaf.id();
            s.store.set_hittable(node, false);
            s.store.set_translate(node, Vec2 { x: seg.x, y: seg.y });
            have.push((node, seg));
            added = true;
        }
        i += 1;
    }
    if added {
        s.store.mark_dirty(parent, DirtyClass::LAYOUT);
    }
    let mut freed = Vec::new();
    for (node, _) in have.drain(i..) {
        s.store.free_tree(node, s.effects, &mut freed);
    }
}

/// A wavy line `w` wide, 3 tall.
fn squiggle(w: f32, color: Rgba) -> Content {
    let mut cmds = vec![PathCmd::MoveTo(Point { x: 0.0, y: 2.5 })];
    let mut x = 0.0;
    let mut up = true;
    while x < w {
        x = (x + 2.0).min(w);
        cmds.push(PathCmd::LineTo(Point {
            x,
            y: if up { 0.5 } else { 2.5 },
        }));
        up = !up;
    }
    Content::Path {
        cmds,
        fill: None,
        stroke: Some(Stroke::new(1.0, color)),
        natural: Vec2 { x: w, y: 3.0 },
    }
}

/// Makes the marks under `parent` draw `want`, reusing the ones there.
fn sync_marks(
    s: &mut StructureCx<'_>,
    parent: NodeId,
    have: &mut Vec<(NodeId, Mark)>,
    want: Vec<Mark>,
) {
    let size = |m: &Mark| Size {
        width: m.w.map_or(Length::fill(), Length::Fixed),
        height: Length::Fixed(m.h),
    };
    let mut freed = Vec::new();
    let mut added = false;
    let mut i = 0;
    for mark in want {
        if i < have.len() && have[i].1.squiggle == mark.squiggle {
            let (node, old) = &mut have[i];
            if old.w != mark.w || old.h != mark.h {
                s.store.set_fixed_size(*node, size(&mark));
            }
            if old.x != mark.x || old.y != mark.y {
                s.store.set_translate(
                    *node,
                    Vec2 {
                        x: mark.x,
                        y: mark.y,
                    },
                );
            }
            if mark.squiggle {
                if old.w != mark.w || old.color != mark.color {
                    s.store
                        .set_content_payload(*node, squiggle(mark.w.unwrap_or(0.0), mark.color));
                }
            } else if old.color != mark.color {
                s.store.set_fill(*node, mark.color);
            }
            *old = mark;
        } else {
            let mut cx = BuildCx::with_parent(s.store, s.states, s.bindings, parent);
            let leaf = cx.leaf(LeafStyle {
                size: size(&mark),
                style: if mark.squiggle {
                    BoxStyle::NONE
                } else {
                    BoxStyle {
                        fill: mark.color,
                        radius: mark.radius,
                        ..BoxStyle::NONE
                    }
                },
            });
            let node = leaf.id();
            if mark.squiggle {
                s.store
                    .set_content_payload(node, squiggle(mark.w.unwrap_or(0.0), mark.color));
            }
            s.store.set_hittable(node, false);
            s.store.set_translate(
                node,
                Vec2 {
                    x: mark.x,
                    y: mark.y,
                },
            );
            if i < have.len() {
                // A mark of the other kind was here.
                let (old, _) = std::mem::replace(&mut have[i], (node, mark));
                s.store.free_tree(old, s.effects, &mut freed);
            } else {
                have.push((node, mark));
            }
            added = true;
        }
        i += 1;
    }
    if added {
        // A node a hook adds is laid out once its parent is.
        s.store.mark_dirty(parent, DirtyClass::LAYOUT);
    }
    for (node, _) in have.drain(i..) {
        s.store.free_tree(node, s.effects, &mut freed);
    }
}
