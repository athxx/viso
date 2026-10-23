//! Segments: the thumb a segmented control's selected segment sits on.
//!
//! Segments are a container's content ([`Content::Segments`]): the
//! container's children are its segments, and under them it draws a thumb on
//! the selected one and a hairline between each two neighbours the thumb is
//! not near. A new selection slides the thumb from where it shows to the
//! segment over a spring (at once while motion is reduced, or before the
//! segments are laid out). The thumb reads the segments' boxes as they are
//! laid out, so it follows them through a resize, a new caption or a turn of
//! direction. Segments hold no selection of their own: whoever drives them —
//! the `SegmentedControl` widget, a view's `SegmentedControl` — keeps it and
//! tells them ([`NodeStore::select_segment`]).

use viso_render::{AnalyticShadow, Border, Corners, Primitive, Quad, Rect, Rgba, ShadowShape};

use crate::animation::{LookValue, Spring, Timing};
use crate::component::NodeStore;
use crate::content::Content;
use crate::dirty::DirtyClass;
use crate::node::NodeId;

/// The thumb's corner radius: the track's, less the room round the
/// segments.
const THUMB_RADIUS: f32 = 6.0;

/// How near the thumb comes to a hairline, in dp, before the hairline has
/// faded out.
const FADE: f32 = 6.0;

/// What a segmented control's container draws under its segments.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segments {
    /// The thumb's fill.
    pub thumb: Rgba,
    /// The hairlines between segments.
    pub divider: Rgba,
    /// The segment the thumb sits on, or slides to.
    pub selected: Option<u32>,
    /// Where the thumb slides from, relative to the container's box.
    from: Option<Rect>,
    /// How far it has slid from `from`, `0` to `1`.
    shown: f32,
}

impl Segments {
    /// Segments with a `thumb` on segment `selected`, `divider` hairlines
    /// between the others.
    pub fn new(thumb: Rgba, divider: Rgba, selected: Option<u32>) -> Self {
        Segments {
            thumb,
            divider,
            selected,
            from: None,
            shown: 1.0,
        }
    }

    /// How far the thumb has slid to the selected segment, `0` to `1`.
    pub fn shown(&self) -> f32 {
        self.shown
    }

    /// The thumb's slide.
    pub(crate) fn timing() -> Timing {
        Timing::spring(Spring::with_response(0.3, 1.0))
    }

    /// Draws the thumb and the hairlines of container `node` at `world`.
    pub(crate) fn paint(
        &self,
        store: &NodeStore,
        node: NodeId,
        world: Rect,
        out: &mut Vec<Primitive>,
    ) {
        let origin = store.bounds(node);
        let at = |r: Rect| Rect {
            x: r.x + world.x,
            y: r.y + world.y,
            ..r
        };
        let thumb = store.thumb_at(node).map(at);
        if let Some(thumb) = thumb {
            let radius = Corners::uniform(THUMB_RADIUS.min(thumb.h * 0.5));
            out.push(Primitive::AnalyticShadow(AnalyticShadow {
                rect: thumb,
                color: Rgba::new(0.0, 0.0, 0.0, 0.2 * self.thumb.a),
                radius,
                offset: [0.0, 1.0],
                sigma: 2.0,
                spread: 0.0,
                shape: ShadowShape::RoundedBox,
                inner: false,
            }));
            out.push(Primitive::Quad(Quad {
                rect: thumb,
                color: self.thumb,
                radius: radius.left_top,
                border: Border::NONE,
            }));
        }
        let mut before: Option<Rect> = None;
        for child in store.arena().children(node) {
            if store.hidden(child) {
                continue;
            }
            let b = at(local(store.bounds(child), origin));
            if let Some(a) = before {
                let (left, right) = if a.x <= b.x { (a, b) } else { (b, a) };
                let x = (left.x + left.w + right.x) * 0.5;
                let near = thumb.map_or(f32::INFINITY, |t| (t.x - x).max(x - t.x - t.w).max(0.0));
                let alpha = (near / FADE).min(1.0);
                if alpha > 0.0 {
                    let h = a.h.min(b.h);
                    let line = (h / 28.0).max(1.0);
                    out.push(Primitive::Quad(Quad {
                        rect: Rect::new(x - line * 0.5, a.y.max(b.y) + h * 0.25, line, h * 0.5),
                        color: Rgba {
                            a: self.divider.a * alpha,
                            ..self.divider
                        },
                        radius: 0.0,
                        border: Border::NONE,
                    }));
                }
            }
            before = Some(b);
        }
    }
}

/// `r` relative to `origin`'s corner.
fn local(r: Rect, origin: Rect) -> Rect {
    Rect {
        x: r.x - origin.x,
        y: r.y - origin.y,
        ..r
    }
}

/// `a` moved `t` of the way to `b`.
fn toward(a: Rect, b: Rect, t: f32) -> Rect {
    let mix = |p: f32, q: f32| p + (q - p) * t;
    Rect::new(mix(a.x, b.x), mix(a.y, b.y), mix(a.w, b.w), mix(a.h, b.h))
}

impl NodeStore {
    /// Node `id`'s segments, if it draws them.
    pub fn segments(&self, id: NodeId) -> Option<&Segments> {
        match self.content_payload(id) {
            Some(Content::Segments(segments)) => Some(segments),
            _ => None,
        }
    }

    pub(crate) fn segments_mut(&mut self, id: NodeId) -> Option<&mut Segments> {
        if !self.arena().is_live(id) {
            return None;
        }
        match self.content_payload_mut(id) {
            Some(Content::Segments(segments)) => Some(segments),
            _ => None,
        }
    }

    /// Where segments `id`'s thumb shows, relative to its box: on its
    /// selected segment, or on the way there. `None` with no segment
    /// selected.
    pub fn thumb_at(&self, id: NodeId) -> Option<Rect> {
        let segments = self.segments(id)?;
        let origin = self.bounds(id);
        let to = segments
            .selected
            .and_then(|i| self.arena().children(id).nth(i as usize))
            .map(|segment| local(self.bounds(segment), origin))?;
        Some(match segments.from {
            Some(from) if segments.shown < 1.0 => toward(from, to, segments.shown),
            _ => to,
        })
    }

    /// Tell segments `id` the segment `selected` is, sliding the thumb there
    /// from where it shows. A live-guarded write that repaints it.
    pub fn select_segment(&mut self, id: NodeId, selected: Option<u32>) {
        let Some(segments) = self.segments(id) else {
            return;
        };
        if segments.selected == selected {
            return;
        }
        let laid_out = self.bounds(id).w > 0.0;
        let from = laid_out.then(|| self.thumb_at(id)).flatten();
        self.settle(id, &LookValue::Thumb(0.0));
        if let Some(segments) = self.segments_mut(id) {
            segments.selected = selected;
            segments.from = from;
            segments.shown = if from.is_some() { 0.0 } else { 1.0 };
        }
        self.mark_dirty(id, DirtyClass::PAINT);
        if from.is_some() {
            self.transition(id, LookValue::Thumb(1.0), Segments::timing());
        }
    }

    /// Show segments `id`'s thumb `shown` of the way to the selected
    /// segment.
    pub(crate) fn slide_thumb(&mut self, id: NodeId, shown: f32) {
        if let Some(segments) = self.segments_mut(id)
            && segments.shown != shown
        {
            segments.shown = shown;
            self.mark_dirty(id, DirtyClass::PAINT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_thumb_slides_from_where_it_showed() {
        let a = Rect::new(0.0, 0.0, 40.0, 28.0);
        let b = Rect::new(40.0, 0.0, 80.0, 28.0);
        assert_eq!(toward(a, b, 0.5), Rect::new(20.0, 0.0, 60.0, 28.0));
        assert_eq!(
            local(
                Rect::new(12.0, 7.0, 5.0, 5.0),
                Rect::new(10.0, 5.0, 50.0, 50.0)
            ),
            Rect::new(2.0, 2.0, 5.0, 5.0)
        );
        let s = Segments::new(Rgba::new(1.0, 1.0, 1.0, 1.0), Rgba::TRANSPARENT, Some(1));
        assert_eq!((s.selected, s.shown()), (Some(1), 1.0));
    }
}
