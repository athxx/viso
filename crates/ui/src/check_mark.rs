//! A check's own drawing beside its caption: a `CheckBox`'s box with its
//! mark, a `Toggle`'s track with its knob.
//!
//! A check is one node: its caption is its text content, and the mark sits
//! at its start edge (its end edge right to left), centred on the caption's
//! first line, the caption following after a gap. The mark is sized from the
//! caption's first line, so it scales with the text, and inked with the
//! caption's color, so a style that recolors the text recolors the mark too.

use viso_render::{
    Border, LineCap, LineJoin, Path, PathCmd, Point, Primitive, Quad, Rect, Rgba, Stroke,
};

use crate::layout::Vec2;

/// How a check draws its state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckShape {
    /// A box, filled and ticked while checked.
    Box,
    /// A track with a knob at its start, at its end while checked.
    Switch,
}

/// What a check draws beside its caption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckMark {
    pub shape: CheckShape,
    pub checked: bool,
}

/// The least side of a mark, whatever its caption.
const SIDE_MIN: f32 = 16.0;

/// A first line's height over its baseline: an ascender is about four fifths
/// of a line.
const LINE_OVER_BASELINE: f32 = 1.25;

/// Where a check's mark and caption sit in its node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MarkPlace {
    /// The mark's box, node-local.
    pub mark: Rect,
    /// Where the caption's top-left lands, node-local.
    pub text: Vec2,
}

impl CheckMark {
    /// The mark's size beside a caption whose first line has `baseline`.
    fn size(self, baseline: f32) -> Vec2 {
        let side = (baseline * LINE_OVER_BASELINE).max(SIDE_MIN);
        match self.shape {
            CheckShape::Box => Vec2 { x: side, y: side },
            CheckShape::Switch => Vec2 {
                x: side * 1.75,
                y: side,
            },
        }
    }

    /// How far the caption sits from the start edge: the mark and, before a
    /// caption that draws anything, the gap after it.
    fn lead(self, text: Vec2, baseline: f32) -> f32 {
        let mark = self.size(baseline);
        if text.x > 0.0 {
            mark.x + mark.y * 0.5
        } else {
            mark.x
        }
    }

    /// The natural size of the check over a caption of natural size `text`
    /// whose first line has `baseline`.
    pub(crate) fn natural(self, text: Vec2, baseline: f32) -> Vec2 {
        let place = self.place(text, baseline, 0.0, false);
        Vec2 {
            x: self.lead(text, baseline) + text.x,
            y: (place.mark.y + place.mark.h).max(place.text.y + text.y),
        }
    }

    /// The width the caption has in a check `width` wide.
    pub(crate) fn caption_width(self, width: f32, text: Vec2, baseline: f32) -> f32 {
        (width - self.lead(text, baseline)).max(0.0)
    }

    /// Where the mark and a caption of natural size `text` (first line at
    /// `baseline`) sit in a check `width` wide, mirrored right to left.
    pub(crate) fn place(self, text: Vec2, baseline: f32, width: f32, rtl: bool) -> MarkPlace {
        let mark = self.size(baseline);
        let line = if baseline > 0.0 {
            (baseline * LINE_OVER_BASELINE).min(text.y.max(baseline))
        } else {
            text.y
        };
        let mark_y = ((line - mark.y) * 0.5).max(0.0);
        let text_y = ((mark.y - line) * 0.5).max(0.0);
        let lead = self.lead(text, baseline);
        let (mark_x, text_x) = if rtl {
            (width - mark.x, (width - lead - text.x).max(0.0))
        } else {
            (0.0, lead)
        };
        MarkPlace {
            mark: Rect {
                x: mark_x,
                y: mark_y,
                w: mark.x,
                h: mark.y,
            },
            text: Vec2 {
                x: text_x,
                y: text_y,
            },
        }
    }

    /// Draws the mark into `rect` (world space) inked `ink`.
    pub(crate) fn paint(self, rect: Rect, ink: Rgba, out: &mut Vec<Primitive>) {
        let on_ink = contrast(ink);
        match (self.shape, self.checked) {
            (CheckShape::Box, checked) => {
                let radius = rect.h * 0.2;
                out.push(Primitive::Quad(Quad {
                    rect,
                    color: if checked { ink } else { Rgba::TRANSPARENT },
                    radius,
                    border: if checked {
                        Border::NONE
                    } else {
                        Border {
                            width: (rect.h * 0.09).max(1.0),
                            color: faded(ink, 0.6),
                        }
                    },
                }));
                if checked {
                    let at = |x: f32, y: f32| Point {
                        x: rect.x + rect.w * x,
                        y: rect.y + rect.h * y,
                    };
                    let mut stroke = Stroke::new(rect.h * 0.12, on_ink);
                    stroke.cap = LineCap::Round;
                    stroke.join = LineJoin::Round;
                    out.push(Primitive::Path(Box::new(Path {
                        cmds: vec![
                            PathCmd::MoveTo(at(0.26, 0.52)),
                            PathCmd::LineTo(at(0.43, 0.69)),
                            PathCmd::LineTo(at(0.75, 0.34)),
                        ],
                        fill: None,
                        stroke: Some(stroke),
                        shadow: None,
                    })));
                }
            }
            (CheckShape::Switch, checked) => {
                out.push(Primitive::Quad(Quad {
                    rect,
                    color: if checked { ink } else { faded(ink, 0.28) },
                    radius: rect.h * 0.5,
                    border: Border::NONE,
                }));
                let inset = rect.h * 0.125;
                let knob = rect.h - inset * 2.0;
                let x = if checked {
                    rect.x + rect.w - inset - knob
                } else {
                    rect.x + inset
                };
                out.push(Primitive::Quad(Quad {
                    rect: Rect {
                        x,
                        y: rect.y + inset,
                        w: knob,
                        h: knob,
                    },
                    color: if checked { on_ink } else { ink },
                    radius: knob * 0.5,
                    border: Border::NONE,
                }));
            }
        }
    }
}

/// `color` at `alpha` of its opacity.
fn faded(color: Rgba, alpha: f32) -> Rgba {
    Rgba {
        a: color.a * alpha,
        ..color
    }
}

/// A color that reads over `ink`: near-black over a light ink, white over a
/// dark one.
fn contrast(ink: Rgba) -> Rgba {
    let luminance = 0.2126 * ink.r + 0.7152 * ink.g + 0.0722 * ink.b;
    if luminance > 0.4 {
        Rgba {
            r: 0.02,
            g: 0.02,
            b: 0.03,
            a: ink.a,
        }
    } else {
        Rgba {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: ink.a,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOX: CheckMark = CheckMark {
        shape: CheckShape::Box,
        checked: false,
    };

    #[test]
    fn the_caption_follows_the_mark_after_a_gap_and_the_mark_mirrors_right_to_left() {
        // A 20-high caption on a 16 baseline: a 20 side, centred on it.
        let text = Vec2 { x: 50.0, y: 20.0 };
        let place = BOX.place(text, 16.0, 100.0, false);
        assert_eq!(
            place.mark,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 20.0,
                h: 20.0
            }
        );
        assert_eq!(place.text, Vec2 { x: 30.0, y: 0.0 });
        assert_eq!(BOX.natural(text, 16.0), Vec2 { x: 80.0, y: 20.0 });
        assert_eq!(BOX.caption_width(100.0, text, 16.0), 70.0);
        let rtl = BOX.place(text, 16.0, 100.0, true);
        assert_eq!((rtl.mark.x, rtl.text.x), (80.0, 20.0));
    }

    #[test]
    fn a_check_with_no_caption_is_its_mark_alone() {
        let switch = CheckMark {
            shape: CheckShape::Switch,
            checked: true,
        };
        assert_eq!(switch.natural(Vec2::ZERO, 0.0), Vec2 { x: 28.0, y: 16.0 });
    }
}
