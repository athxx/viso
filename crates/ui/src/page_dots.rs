//! Page dots: a row of dots, one per item of a snapping viewport, the one it
//! rests on lit.
//!
//! Page dots are a node's content ([`Content::PageDots`]) linked to a
//! snapping viewport ([`BuildCx::page_dots`]): they count its items — a
//! virtual list's rows, its content's children — and light the item it
//! rests on, heads for or shows nearest, as the viewport moves. Each dot owns
//! a cell a gap wide past it, so the whole row takes taps: a tap glides the
//! viewport to the dot's item. Focused, the arrow keys step one item; an
//! assistive technology reads a slider of pages and steps it the same way.
//! Read right to left, the first dot is the rightmost, as the first item is.

use viso_render::{Border, Primitive, Quad, Rect, Rgba};

use crate::adaptive::LayoutDirection;
use crate::component::{BuildCx, Handle, LeafStyle, NodeStore, StackStyle};
use crate::content::Content;
use crate::dirty::DirtyClass;
use crate::input::{Key, PointerButtons, PointerPhase};
use crate::layout::{Alignment2D, Axis, Length, Sizing, Vec2};
use crate::node::NodeId;
use crate::semantics::{Role, SemanticState, Semantics};
use crate::style::BoxStyle;

/// How page dots look.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageDotsStyle {
    /// The dots' fill.
    pub color: Rgba,
    /// The lit dot's fill.
    pub current_color: Rgba,
    /// A dot's diameter.
    pub dot: f32,
    /// The room between two dots, and above and below them.
    pub gap: f32,
}

impl Default for PageDotsStyle {
    fn default() -> Self {
        PageDotsStyle {
            color: Rgba::new(0.5, 0.5, 0.5, 0.35),
            current_color: Rgba::new(0.5, 0.5, 0.5, 1.0),
            dot: 7.0,
            gap: 9.0,
        }
    }
}

/// What a page dots node draws.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageDots {
    pub style: PageDotsStyle,
    /// How many dots.
    pub count: usize,
    /// The lit dot.
    pub current: usize,
    /// Whether the first dot is the rightmost.
    pub reversed: bool,
}

impl PageDots {
    /// A dot's cell: the dot and a gap past it along the row, a gap above and
    /// below.
    fn cell(&self) -> Vec2 {
        Vec2 {
            x: self.style.dot + self.style.gap,
            y: self.style.dot + self.style.gap * 2.0,
        }
    }

    /// The size the row measures to.
    pub(crate) fn natural(&self) -> Vec2 {
        let cell = self.cell();
        Vec2 {
            x: cell.x * self.count as f32,
            y: cell.y,
        }
    }

    /// The dot whose cell `x` (from the box's left edge) falls in, in a box
    /// `width` wide.
    fn dot_at(&self, x: f32, width: f32) -> Option<usize> {
        if self.count == 0 {
            return None;
        }
        let step = self.cell().x;
        let left = (width - step * self.count as f32) * 0.5;
        let k = ((x - left) / step)
            .floor()
            .clamp(0.0, (self.count - 1) as f32) as usize;
        Some(if self.reversed { self.count - 1 - k } else { k })
    }

    /// Draws it into `rect` (world space), centered.
    pub(crate) fn paint(&self, rect: Rect, out: &mut Vec<Primitive>) {
        let PageDotsStyle {
            color,
            current_color,
            dot,
            gap,
        } = self.style;
        if self.count == 0 || dot <= 0.0 {
            return;
        }
        let step = dot + gap;
        let left = rect.x + (rect.w - step * self.count as f32) * 0.5 + gap * 0.5;
        let y = rect.y + (rect.h - dot) * 0.5;
        for k in 0..self.count {
            let index = if self.reversed { self.count - 1 - k } else { k };
            let fill = if index == self.current {
                current_color
            } else {
                color
            };
            if fill.a <= 0.0 {
                continue;
            }
            out.push(Primitive::Quad(Quad {
                rect: Rect {
                    x: left + step * k as f32,
                    y,
                    w: dot,
                    h: dot,
                },
                color: fill,
                radius: dot * 0.5,
                border: Border::NONE,
            }));
        }
    }
}

impl BuildCx<'_> {
    /// Give viewport `id` page dots over its end edge across: a chrome layer
    /// over it, the dots centered along that edge.
    pub(crate) fn viewport_dots(&mut self, id: NodeId, axis: Axis, style: PageDotsStyle) {
        let viewport = Handle::of(id);
        self.stack.push(id);
        let layer = self.stack(
            StackStyle {
                align: match axis {
                    Axis::Row => Alignment2D::Bottom,
                    Axis::Column => Alignment2D::End,
                },
                size: Sizing::fill(),
                ..StackStyle::default()
            },
            |cx| {
                cx.page_dots(viewport, style);
            },
        );
        self.stack.pop();
        self.store.set_chrome(layer.id(), true);
        self.store.set_hittable(layer.id(), false);
    }

    /// Page dots for snapping viewport `viewport`, looking as `style` says:
    /// a dot per item, the one it rests on lit. A tap on a dot, an arrow key
    /// or an assistive technology's step brings the viewport to its item.
    pub fn page_dots(&mut self, viewport: Handle, style: PageDotsStyle) -> Handle {
        let handle = self.leaf(LeafStyle {
            size: Sizing {
                width: Length::Fit,
                height: Length::Fit,
            },
            style: BoxStyle::NONE,
        });
        let id = handle.id();
        self.store.set_content_payload(
            id,
            Content::PageDots(PageDots {
                style,
                count: 0,
                current: 0,
                reversed: false,
            }),
        );
        self.store.set_semantics(id, Semantics::role(Role::Slider));
        self.store.link_page_dots(viewport.id(), id);
        let viewport = viewport.id();
        self.focusable(handle, true);
        self.on_pointer(handle, move |ev| {
            let Some(p) = ev.pointer() else { return };
            if p.phase != PointerPhase::Up || !p.buttons.contains(PointerButtons::PRIMARY) {
                return;
            }
            let (Some(size), Some(dots)) = (ev.size(), ev.page_dots(id)) else {
                return;
            };
            let at = ev.local(p.x, p.y);
            if let Some(index) = dots.dot_at(at.x, size.x) {
                ev.snap_to_item(viewport, index);
            }
        });
        self.on_key(handle, move |ev| {
            let Some(k) = ev.key() else { return };
            if !k.pressed {
                return;
            }
            let Some(dots) = ev.page_dots(id) else { return };
            let next = match k.key {
                Key::Right | Key::Up => dots.current + 1,
                Key::Left | Key::Down => dots.current.wrapping_sub(1),
                Key::Home => 0,
                Key::End => dots.count.saturating_sub(1),
                _ => return,
            };
            ev.stop_propagation();
            if next < dots.count {
                ev.snap_to_item(viewport, next);
            }
        });
        handle
    }
}

impl NodeStore {
    /// Node `id`'s page dots, if it draws them.
    pub fn page_dots(&self, id: NodeId) -> Option<PageDots> {
        match self.content_payload(id) {
            Some(Content::PageDots(dots)) => Some(*dots),
            _ => None,
        }
    }

    /// Light dot `current` of `count` on page dots `id`, mirrored when it
    /// reads right to left: a new count re-measures it, a new dot repaints
    /// it, and either tells an assistive technology.
    pub(crate) fn show_page(&mut self, id: NodeId, current: usize, count: usize) {
        let reversed = self.direction_at(id.index()) == LayoutDirection::Rtl;
        let Some(dots) = self.page_dots_mut(id) else {
            return;
        };
        let was = *dots;
        dots.count = count;
        dots.current = current.min(count.saturating_sub(1));
        dots.reversed = reversed;
        let now = *dots;
        if now == was {
            return;
        }
        let mut classes = DirtyClass::PAINT;
        if now.count != was.count {
            classes |= DirtyClass::MEASURE | DirtyClass::LAYOUT;
        }
        self.mark_dirty(id, classes);
        if (now.count, now.current) != (was.count, was.current) {
            let pages = now.count.max(1) as f32;
            self.set_semantic_state(
                id,
                SemanticState {
                    value: Some(now.current as f32 + 1.0),
                    range: Some((1.0, pages)),
                    ..SemanticState::default()
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dots(count: usize, current: usize, reversed: bool) -> PageDots {
        PageDots {
            style: PageDotsStyle::default(),
            count,
            current,
            reversed,
        }
    }

    fn painted(dots: &PageDots) -> Vec<(f32, Rgba)> {
        let mut out = Vec::new();
        dots.paint(
            Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 25.0,
            },
            &mut out,
        );
        out.iter()
            .map(|p| match p {
                Primitive::Quad(q) => (q.rect.x, q.color),
                _ => unreachable!(),
            })
            .collect()
    }

    #[test]
    fn a_dot_per_item_centered_the_current_lit() {
        let d = dots(3, 1, false);
        assert_eq!(d.natural(), Vec2 { x: 48.0, y: 25.0 });
        let quads = painted(&d);
        assert_eq!(quads.len(), 3);
        // 48 wide centered in 100: cells at 26, 42, 58, each dot half a gap in.
        assert_eq!(
            quads.iter().map(|q| q.0).collect::<Vec<_>>(),
            [30.5, 46.5, 62.5]
        );
        let style = PageDotsStyle::default();
        assert_eq!(quads[1].1, style.current_color);
        assert_eq!(quads[0].1, style.color);
    }

    #[test]
    fn read_right_to_left_the_first_dot_is_the_rightmost() {
        let d = dots(3, 0, true);
        let quads = painted(&d);
        assert_eq!(quads[2].1, PageDotsStyle::default().current_color);
        assert_eq!(d.dot_at(70.0, 100.0), Some(0));
        assert_eq!(dots(3, 0, false).dot_at(70.0, 100.0), Some(2));
    }

    #[test]
    fn a_tap_anywhere_in_a_cell_picks_its_dot() {
        let d = dots(3, 0, false);
        assert_eq!(d.dot_at(27.0, 100.0), Some(0));
        assert_eq!(d.dot_at(41.9, 100.0), Some(0));
        assert_eq!(d.dot_at(42.1, 100.0), Some(1));
        assert_eq!(d.dot_at(0.0, 100.0), Some(0), "past the row, the end dot");
        assert_eq!(d.dot_at(99.0, 100.0), Some(2));
        assert_eq!(dots(0, 0, false).dot_at(50.0, 100.0), None);
    }
}
