//! Blocks into nodes: each block one node, its text a label, its
//! containers flex boxes, its table a grid and its code a scrolling box.

use std::rc::Rc;

use viso_ui::{
    Align, Axis, BoxStyle, BuildCx, CheckMark, CheckShape, Component, EventCx, FlexStyle,
    FontFamily, FontSize, FontWeight, GridStyle, Handle, Inset, Justify, Key, LeafStyle, Length,
    LengthTerms, NodeId, PointerButtons, PointerPhase, Rgba, Role, ScrollStyle, SemanticState,
    Semantics, Size, SpanStyle, TextAlign, TextRequest, TextSpan, TextStyle, TrackSizing,
};

use super::parse::{Alert, Block, Column, Inline, Item, Picture, Task};
use super::{MarkdownStyle, Pictures};
use crate::{image_asset, label};

/// What a task box runs when it is toggled.
pub(super) type Toggle = Rc<dyn Fn(&mut EventCx<'_>, NodeId)>;

/// What a document's blocks build with.
pub(super) struct Env<'a> {
    pub look: &'a MarkdownStyle,
    pub selectable: bool,
    pub pictures: Option<&'a Pictures>,
    /// What a task box runs when toggled; `None` leaves the boxes read-only.
    pub toggle: Option<&'a Toggle>,
    /// The task boxes built, in source order.
    pub tasks: Vec<NodeId>,
    /// The ink of the text being built.
    pub color: Rgba,
    /// How deep in lists.
    pub depth: u8,
}

/// Each heading level's size, of the body's.
const HEADING_SCALE: [f32; 6] = [2.0, 1.5, 1.25, 1.0, 0.875, 0.85];

const fn rgb(r: u8, g: u8, b: u8) -> Rgba {
    Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
}

impl Alert {
    fn title(self) -> &'static str {
        match self {
            Alert::Note => "Note",
            Alert::Tip => "Tip",
            Alert::Important => "Important",
            Alert::Warning => "Warning",
            Alert::Caution => "Caution",
        }
    }

    fn color(self) -> Rgba {
        match self {
            Alert::Note => rgb(9, 105, 218),
            Alert::Tip => rgb(26, 127, 55),
            Alert::Important => rgb(130, 80, 223),
            Alert::Warning => rgb(154, 103, 0),
            Alert::Caution => rgb(207, 34, 46),
        }
    }
}

const FILL_FIT: Size = Size {
    width: Length::Fill { weight: 1.0 },
    height: Length::Fit,
};

const FIT: Size = Size {
    width: Length::Fit,
    height: Length::Fit,
};

impl Env<'_> {
    fn em(&self, em: f32) -> f32 {
        self.look.font_size * em
    }

    fn column(&self, gap: f32) -> FlexStyle {
        FlexStyle {
            axis: Axis::Column,
            gap,
            align: Align::Stretch,
            size: FILL_FIT,
            ..FlexStyle::default()
        }
    }

    fn blocks(&mut self, cx: &mut BuildCx<'_>, blocks: &[Block], gap: f32) -> Handle {
        let style = self.column(gap);
        cx.flex(style, |cx| {
            for block in blocks {
                self.block(cx, block);
            }
        })
    }

    /// A run of text `size` big, wrapping to its box.
    fn text(&self, cx: &mut BuildCx<'_>, text: &Inline, size: f32) -> Handle {
        let mut text = label(text.text.clone())
            .with_spans(text.spans.clone())
            .font_size(size)
            .color(self.color)
            .line_height(size * self.look.line_height)
            .size(FILL_FIT)
            .wrap();
        if self.selectable {
            text = text.selectable();
        }
        text.build_node(cx)
    }

    /// Build `block` as one node and return it.
    pub(super) fn block(&mut self, cx: &mut BuildCx<'_>, block: &Block) -> NodeId {
        match block {
            Block::Paragraph(text) => self.text(cx, text, self.look.font_size).id(),
            Block::Heading { level, text } => self.heading(cx, *level, text),
            Block::Quote { alert, blocks } => self.quote(cx, *alert, blocks),
            Block::List {
                start,
                tight,
                items,
            } => self.list(cx, *start, *tight, items),
            Block::Code(code) => self.code(cx, code),
            Block::Table {
                columns,
                head,
                rows,
            } => self.table(cx, columns, head, rows),
            Block::Rule => cx
                .leaf(LeafStyle {
                    size: Size {
                        width: Length::Fill { weight: 1.0 },
                        height: Length::Fixed(2.0),
                    },
                    style: BoxStyle::solid(self.look.rule),
                })
                .id(),
            Block::Picture(picture) => self.picture(cx, picture),
            Block::Footnote {
                label: name,
                blocks,
            } => {
                let gap = self.em(0.5);
                let row = FlexStyle {
                    axis: Axis::Row,
                    gap,
                    size: FILL_FIT,
                    ..FlexStyle::default()
                };
                let look = self.look;
                let block_gap = self.em(look.block_gap);
                cx.flex(row, |cx| {
                    label(format!("{name}."))
                        .font_size(look.font_size * 0.85)
                        .color(look.muted)
                        .line_height(look.font_size * look.line_height)
                        .build(cx);
                    self.blocks(cx, blocks, block_gap);
                })
                .id()
            }
        }
    }

    fn heading(&mut self, cx: &mut BuildCx<'_>, level: u8, text: &Inline) -> NodeId {
        let size = self.look.font_size * HEADING_SCALE[usize::from(level.clamp(1, 6)) - 1];
        let semantics = Semantics::role(Role::Heading)
            .with_label(text.text.clone())
            .with_heading_level(level);
        let build = |cx: &mut BuildCx<'_>, env: &Self| {
            let mut heading = label(text.text.clone())
                .with_spans(text.spans.clone())
                .font_size(size)
                .weight(FontWeight::BOLD)
                .color(env.color)
                .line_height(size * 1.25)
                .size(FILL_FIT)
                .wrap();
            if env.selectable {
                heading = heading.selectable();
            }
            let node = heading.build_node(cx);
            cx.semantics(node, semantics.clone());
            node
        };
        if level > 2 {
            return build(cx, self).id();
        }
        // The top two levels are ruled off beneath.
        let style = self.column(self.em(0.3));
        let rule = self.look.rule;
        cx.flex(style, |cx| {
            build(cx, self);
            cx.leaf(LeafStyle {
                size: Size {
                    width: Length::Fill { weight: 1.0 },
                    height: Length::Fixed(1.0),
                },
                style: BoxStyle::solid(rule),
            });
        })
        .id()
    }

    fn quote(&mut self, cx: &mut BuildCx<'_>, alert: Option<Alert>, blocks: &[Block]) -> NodeId {
        let bar = alert.map_or(self.look.rule, Alert::color);
        let row = FlexStyle {
            axis: Axis::Row,
            gap: self.em(0.75),
            align: Align::Stretch,
            size: FILL_FIT,
            ..FlexStyle::default()
        };
        let width = self.em(0.25);
        let gap = self.em(self.look.block_gap);
        let quoted = if alert.is_some() {
            self.color
        } else {
            self.look.muted
        };
        let ink = std::mem::replace(&mut self.color, quoted);
        let node = cx.flex(row, |cx| {
            cx.leaf(LeafStyle {
                size: Size {
                    width: Length::Fixed(width),
                    height: Length::Fit,
                },
                style: BoxStyle::solid(bar).with_radius(width * 0.5),
            });
            let style = self.column(gap);
            cx.flex(style, |cx| {
                if let Some(alert) = alert {
                    label(alert.title())
                        .font_size(self.look.font_size)
                        .weight(FontWeight::BOLD)
                        .color(alert.color())
                        .line_height(self.em(self.look.line_height))
                        .build(cx);
                }
                for block in blocks {
                    self.block(cx, block);
                }
            });
        });
        self.color = ink;
        node.id()
    }

    fn list(
        &mut self,
        cx: &mut BuildCx<'_>,
        start: Option<u64>,
        tight: bool,
        items: &[Item],
    ) -> NodeId {
        let gap = if tight {
            self.em(0.25)
        } else {
            self.em(self.look.block_gap)
        };
        let marker = match start {
            Some(first) => {
                let last = first + items.len().saturating_sub(1) as u64;
                let digits = last.checked_ilog10().unwrap_or(0) + 1;
                self.em(0.6 * digits as f32 + 0.5)
            }
            None => self.em(1.25),
        };
        let style = self.column(gap);
        self.depth += 1;
        let list = cx.flex(style, |cx| {
            for (at, item) in items.iter().enumerate() {
                let number = start.map(|first| first + at as u64);
                self.item(cx, item, number, marker, gap);
            }
        });
        self.depth -= 1;
        cx.semantics(list, Semantics::role(Role::List));
        list.id()
    }

    fn item(
        &mut self,
        cx: &mut BuildCx<'_>,
        item: &Item,
        number: Option<u64>,
        marker: f32,
        gap: f32,
    ) {
        let row = FlexStyle {
            axis: Axis::Row,
            gap: self.em(0.375),
            size: FILL_FIT,
            ..FlexStyle::default()
        };
        let node = cx.flex(row, |cx| {
            match item.task {
                Some(task) => {
                    let name = match item.blocks.first() {
                        Some(Block::Paragraph(text)) => text.text.as_str(),
                        _ => "",
                    };
                    self.task(cx, task, name);
                }
                None => {
                    let mark = match number {
                        Some(n) => format!("{n}."),
                        None => ["\u{2022}", "\u{25e6}", "\u{25aa}"]
                            [usize::from(self.depth.saturating_sub(1)).min(2)]
                        .to_owned(),
                    };
                    label(mark)
                        .font_size(self.look.font_size)
                        .color(self.color)
                        .line_height(self.em(self.look.line_height))
                        .align(TextAlign::End)
                        .size(Size {
                            width: Length::Fixed(marker),
                            height: Length::Fit,
                        })
                        .build(cx);
                }
            }
            self.blocks(cx, &item.blocks, gap);
        });
        cx.semantics(node, Semantics::role(Role::ListItem));
    }

    /// A task's box: a check of no caption, its first line's height.
    fn task(&mut self, cx: &mut BuildCx<'_>, task: Task, name: &str) {
        let node = cx.leaf(LeafStyle {
            size: FIT,
            style: BoxStyle::NONE,
        });
        cx.text_request(
            node,
            TextRequest {
                text: String::new(),
                font_size: FontSize::Fixed(self.look.font_size),
                color: self.color,
                soft_wrap: false,
                locale: None,
                style: TextStyle {
                    line_height: Some(LengthTerms::em(self.look.line_height)),
                    ..TextStyle::default()
                },
                spans: Vec::new(),
            },
        );
        let id = node.id();
        let interactive = self.toggle.is_some();
        cx.structure(|s| {
            s.store.set_check_mark(
                id,
                Some(CheckMark {
                    shape: CheckShape::Box,
                    checked: task.checked,
                }),
            );
            if !interactive {
                s.store.set_enabled(id, false);
            }
        });
        cx.semantics(node, Semantics::role(Role::CheckBox).with_label(name));
        cx.semantic_state(node, SemanticState::checked(task.checked));
        if let Some(toggle) = self.toggle {
            cx.focusable(node, true);
            let on_pointer = Rc::clone(toggle);
            cx.on_pointer(node, move |ev| {
                let Some(p) = ev.pointer() else { return };
                if p.phase == PointerPhase::Up && p.buttons.contains(PointerButtons::PRIMARY) {
                    on_pointer(ev, id);
                }
            });
            let on_key = Rc::clone(toggle);
            cx.on_key(node, move |ev| {
                if let Some(k) = ev.key()
                    && k.pressed
                    && !k.repeat
                    && matches!(k.key, Key::Enter | Key::Space)
                {
                    on_key(ev, id);
                }
            });
        }
        self.tasks.push(id);
    }

    fn code(&mut self, cx: &mut BuildCx<'_>, code: &str) -> NodeId {
        let size = self.look.font_size * self.look.code_scale;
        let style = FlexStyle {
            padding: Inset::all(self.em(0.85)),
            style: BoxStyle::solid(self.look.code_background).with_radius(6.0),
            ..self.column(0.0)
        };
        let selectable = self.selectable;
        let ink = self.color;
        let line = size * self.look.line_height;
        let node = cx.flex(style, |cx| {
            cx.scroll(
                ScrollStyle {
                    axis: Axis::Row,
                    size: FILL_FIT,
                    style: BoxStyle::NONE,
                },
                |cx| {
                    let mut code = label(code.to_owned())
                        .font_size(size)
                        .color(ink)
                        .family(FontFamily::Monospace)
                        .line_height(line);
                    if selectable {
                        code = code.selectable();
                    }
                    code.build(cx);
                },
            );
        });
        node.id()
    }

    fn table(
        &mut self,
        cx: &mut BuildCx<'_>,
        columns: &[Column],
        head: &[Inline],
        rows: &[Vec<Inline>],
    ) -> NodeId {
        let look = self.look;
        let grid = GridStyle {
            columns: vec![TrackSizing::Auto; columns.len().max(1)],
            auto_rows: TrackSizing::Auto,
            column_gap: 1.0,
            row_gap: 1.0,
            padding: Inset::all(1.0),
            size: FIT,
            style: BoxStyle::solid(look.rule).with_radius(2.0),
            ..GridStyle::default()
        };
        let padding = Inset {
            left: self.em(0.75),
            right: self.em(0.75),
            top: self.em(0.4),
            bottom: self.em(0.4),
        };
        let selectable = self.selectable;
        let ink = self.color;
        let cell = |cx: &mut BuildCx<'_>, text: &Inline, at: usize, header: bool| {
            let justify = match columns.get(at).copied().unwrap_or(Column::Start) {
                Column::Start => Justify::Start,
                Column::Center => Justify::Center,
                Column::End => Justify::End,
            };
            let fill = if header {
                look.table_header
            } else {
                look.table_cell
            };
            let style = FlexStyle {
                axis: Axis::Row,
                padding,
                justify,
                size: Size::fill(),
                style: BoxStyle::solid(fill),
                ..FlexStyle::default()
            };
            cx.flex(style, |cx| {
                let mut text = label(text.text.clone())
                    .with_spans(text.spans.clone())
                    .font_size(look.font_size)
                    .color(ink)
                    .line_height(look.font_size * look.line_height);
                if header {
                    text = text.weight(FontWeight::BOLD);
                }
                if selectable {
                    text = text.selectable();
                }
                text.build(cx);
            });
        };
        let node = cx.scroll(
            ScrollStyle {
                axis: Axis::Row,
                size: FILL_FIT,
                style: BoxStyle::NONE,
            },
            |cx| {
                cx.grid(grid, |cx| {
                    for (at, text) in head.iter().enumerate() {
                        cell(cx, text, at, true);
                    }
                    for row in rows {
                        for at in 0..columns.len() {
                            let blank = Inline::default();
                            cell(cx, row.get(at).unwrap_or(&blank), at, false);
                        }
                    }
                });
            },
        );
        node.id()
    }

    fn picture(&mut self, cx: &mut BuildCx<'_>, picture: &Picture) -> NodeId {
        let asset = self.pictures.and_then(|resolve| resolve(&picture.url));
        let Some(asset) = asset else {
            let alt = if picture.alt.is_empty() {
                "image"
            } else {
                &picture.alt
            };
            let ink = self.look.muted;
            let text = Inline {
                text: alt.to_owned(),
                spans: vec![TextSpan {
                    range: 0..alt.len(),
                    style: SpanStyle::default().italic(),
                }],
            };
            let ink = std::mem::replace(&mut self.color, ink);
            let node = self.text(cx, &text, self.look.font_size);
            self.color = ink;
            return node.id();
        };
        let alt = picture.alt.clone();
        cx.flex(self.column(0.0), |cx| {
            image_asset(asset).label(alt).build(cx);
        })
        .id()
    }
}
