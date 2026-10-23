//! A check's own drawing beside its caption: a `CheckBox`'s box with its
//! mark, a `Toggle`'s track with its knob, a radio option's circle with its
//! dot, a disclosure header's chevron; and a plate's caption, centred on the
//! plate its node's background draws.
//!
//! A check is one node: its caption is its text content, and the mark sits
//! at its start edge (its end edge right to left), centred on the caption's
//! first line, the caption following after a gap. The mark is sized from the
//! caption's first line, so it scales with the text, and inked with the
//! caption's color, so a style that recolors the text recolors the mark too.
//!
//! A plate draws no mark: its caption sits in the middle of its node, which
//! measures at least as wide as tall around it — a badge's count, an
//! avatar's initials — or, with nothing in the caption, to a dot.

use viso_render::{
    Border, LineCap, LineJoin, Path, PathCmd, Point, Primitive, Quad, Rect, Rgba, Stroke,
};

use core::time::Duration;

use crate::animation::{Easing, Spring, Timing};
use crate::layout::Vec2;

/// How a check draws its state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckShape {
    /// A box, filled and ticked while checked.
    Box,
    /// A track with a knob at its start, at its end while checked.
    Switch,
    /// A chevron pointing toward the end edge, turned to point down while
    /// checked: what a disclosure's header shows of whether it is expanded.
    Chevron,
    /// A circle, filled with a dot in it while checked: a radio option,
    /// checked while it is the one selected.
    Radio,
    /// No mark: the caption centred on the plate the node's background
    /// draws, which is at least as wide as tall; with an empty caption, a
    /// dot. It has no checked state.
    Plate,
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

/// The least height of a plate with a caption, and the side of one without.
const PLATE_MIN: f32 = 16.0;
const DOT: f32 = 8.0;

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
            CheckShape::Box | CheckShape::Chevron | CheckShape::Radio => Vec2 { x: side, y: side },
            CheckShape::Switch => Vec2 {
                x: side * 1.75,
                y: side,
            },
            CheckShape::Plate => Vec2::ZERO,
        }
    }

    /// A plate's natural size round a caption of natural size `text`, and
    /// the room it keeps on each side of the caption.
    fn plate(text: Vec2) -> (Vec2, f32) {
        if text.x <= 0.0 {
            return (Vec2 { x: DOT, y: DOT }, 0.0);
        }
        let height = text.y.max(PLATE_MIN);
        let room = height * 0.25;
        let width = (text.x + room * 2.0).max(height);
        (
            Vec2 {
                x: width,
                y: height,
            },
            room,
        )
    }

    /// How far the caption sits from the start edge: the mark and, before a
    /// caption that draws anything, the gap after it.
    fn lead(self, text: Vec2, baseline: f32) -> f32 {
        if self.shape == CheckShape::Plate {
            let (plate, _) = Self::plate(text);
            return (plate.x - text.x) * 0.5;
        }
        let mark = self.size(baseline);
        // A chevron's stroke sits well inside its box, which spaces it already.
        let gap = match self.shape {
            CheckShape::Chevron => 0.25,
            _ => 0.5,
        };
        if text.x > 0.0 {
            mark.x + mark.y * gap
        } else {
            mark.x
        }
    }

    /// The natural size of the check over a caption of natural size `text`
    /// whose first line has `baseline`.
    pub(crate) fn natural(self, text: Vec2, baseline: f32) -> Vec2 {
        if self.shape == CheckShape::Plate {
            return Self::plate(text).0;
        }
        let place = self.place(text, baseline, Vec2::ZERO, false);
        Vec2 {
            x: self.lead(text, baseline) + text.x,
            y: (place.mark.y + place.mark.h).max(place.text.y + text.y),
        }
    }

    /// The width the caption has in a check `width` wide.
    pub(crate) fn caption_width(self, width: f32, text: Vec2, baseline: f32) -> f32 {
        if self.shape == CheckShape::Plate {
            return (width - Self::plate(text).1 * 2.0).max(0.0);
        }
        (width - self.lead(text, baseline)).max(0.0)
    }

    /// Where the mark and a caption of natural size `text` (first line at
    /// `baseline`) sit in a check of `size` (its natural size where zero),
    /// mirrored right to left.
    pub(crate) fn place(self, text: Vec2, baseline: f32, size: Vec2, rtl: bool) -> MarkPlace {
        if self.shape == CheckShape::Plate {
            let natural = Self::plate(text).0;
            let (w, h) = if size.x > 0.0 {
                (size.x, size.y)
            } else {
                (natural.x, natural.y)
            };
            return MarkPlace {
                mark: Rect {
                    x: 0.0,
                    y: 0.0,
                    w,
                    h,
                },
                text: Vec2 {
                    x: ((w - text.x) * 0.5).max(0.0),
                    y: ((h - text.y) * 0.5).max(0.0),
                },
            };
        }
        let width = size.x;
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

    /// How far its mark has moved toward checked at rest: `1` checked, `0`
    /// not.
    pub(crate) fn rest(self) -> f32 {
        if self.checked { 1.0 } else { 0.0 }
    }

    /// How its mark moves between its states: a box fills and ticks, a
    /// circle fills and its dot grows and a chevron turns over a short ease,
    /// a knob slides on a spring that carries a quick change of mind on.
    pub(crate) fn timing(self) -> Timing {
        match self.shape {
            CheckShape::Box | CheckShape::Chevron | CheckShape::Radio | CheckShape::Plate => {
                Timing::eased(Duration::from_millis(150), Easing::EASE_OUT)
            }
            CheckShape::Switch => Timing::spring(Spring::with_response(0.28, 1.0)),
        }
    }

    /// Draws the mark into `rect` (world space) inked `ink`, `progress` of
    /// the way from unchecked to checked: a box's fill fades in and its tick
    /// draws along its stroke, a circle's fill fades in as its dot grows from
    /// its centre, a knob slides along its track as the track
    /// fills, a chevron turns a quarter clockwise (counterclockwise right to
    /// left, where it points left at rest).
    pub(crate) fn paint(
        self,
        rect: Rect,
        ink: Rgba,
        progress: f32,
        rtl: bool,
        out: &mut Vec<Primitive>,
    ) {
        let p = progress.clamp(0.0, 1.0);
        let on_ink = contrast(ink);
        match self.shape {
            CheckShape::Plate => {}
            CheckShape::Box => {
                out.push(Primitive::Quad(Quad {
                    rect,
                    color: faded(ink, p),
                    radius: rect.h * 0.2,
                    border: if p >= 1.0 {
                        Border::NONE
                    } else {
                        Border {
                            width: (rect.h * 0.09).max(1.0),
                            color: faded(ink, 0.6 + 0.4 * p),
                        }
                    },
                }));
                if p > 0.0 {
                    let at = |(x, y): (f32, f32)| Point {
                        x: rect.x + rect.w * x,
                        y: rect.y + rect.h * y,
                    };
                    let mut stroke = Stroke::new(rect.h * 0.12, on_ink);
                    stroke.cap = LineCap::Round;
                    stroke.join = LineJoin::Round;
                    out.push(Primitive::Path(Box::new(Path {
                        cmds: tick(p)
                            .map(|(i, point)| match i {
                                0 => PathCmd::MoveTo(at(point)),
                                _ => PathCmd::LineTo(at(point)),
                            })
                            .collect(),
                        fill: None,
                        stroke: Some(stroke),
                        shadow: None,
                    })));
                }
            }
            CheckShape::Chevron => {
                let (sin, cos) = (p * core::f32::consts::FRAC_PI_2).sin_cos();
                let (side, mirror) = (rect.h, if rtl { -1.0 } else { 1.0 });
                let center = (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5);
                // Each corner about the centre, mirrored right to left, then
                // turned toward the start of the box's bottom edge.
                let at = |(x, y): (f32, f32)| {
                    let (x, y) = (x * mirror * side, y * side);
                    Point {
                        x: center.0 + x * cos - mirror * y * sin,
                        y: center.1 + mirror * x * sin + y * cos,
                    }
                };
                let mut stroke = Stroke::new(side * 0.1, ink);
                stroke.cap = LineCap::Round;
                stroke.join = LineJoin::Round;
                out.push(Primitive::Path(Box::new(Path {
                    cmds: vec![
                        PathCmd::MoveTo(at(CHEVRON[0])),
                        PathCmd::LineTo(at(CHEVRON[1])),
                        PathCmd::LineTo(at(CHEVRON[2])),
                    ],
                    fill: None,
                    stroke: Some(stroke),
                    shadow: None,
                })));
            }
            CheckShape::Radio => {
                out.push(Primitive::Quad(Quad {
                    rect,
                    color: faded(ink, p),
                    radius: rect.h * 0.5,
                    border: if p >= 1.0 {
                        Border::NONE
                    } else {
                        Border {
                            width: (rect.h * 0.09).max(1.0),
                            color: faded(ink, 0.6 + 0.4 * p),
                        }
                    },
                }));
                let dot = rect.h * 0.4 * p;
                if dot > 0.0 {
                    out.push(Primitive::Quad(Quad {
                        rect: Rect {
                            x: rect.x + (rect.w - dot) * 0.5,
                            y: rect.y + (rect.h - dot) * 0.5,
                            w: dot,
                            h: dot,
                        },
                        color: on_ink,
                        radius: dot * 0.5,
                        border: Border::NONE,
                    }));
                }
            }
            CheckShape::Switch => {
                out.push(Primitive::Quad(Quad {
                    rect,
                    color: faded(ink, 0.28 + 0.72 * p),
                    radius: rect.h * 0.5,
                    border: Border::NONE,
                }));
                let inset = rect.h * 0.125;
                let knob = rect.h - inset * 2.0;
                let travel = rect.w - inset * 2.0 - knob;
                out.push(Primitive::Quad(Quad {
                    rect: Rect {
                        x: rect.x + inset + travel * p,
                        y: rect.y + inset,
                        w: knob,
                        h: knob,
                    },
                    color: mix(ink, on_ink, p),
                    radius: knob * 0.5,
                    border: Border::NONE,
                }));
            }
        }
    }
}

/// A chevron's corners pointing toward the end edge, as shares of its box
/// about its centre.
const CHEVRON: [(f32, f32); 3] = [(-0.11, -0.22), (0.11, 0.0), (-0.11, 0.22)];

/// The tick's corners, as shares of its box.
const TICK: [(f32, f32); 3] = [(0.26, 0.52), (0.43, 0.69), (0.75, 0.34)];

/// The points of the tick drawn `share` of its length from its start, each
/// with its place in the stroke: its corners up to where the stroke ends,
/// and that end.
fn tick(share: f32) -> impl Iterator<Item = (usize, (f32, f32))> {
    let length = |a: (f32, f32), b: (f32, f32)| ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
    let first = length(TICK[0], TICK[1]);
    let total = first + length(TICK[1], TICK[2]);
    let reach = share * total;
    let toward =
        |a: (f32, f32), b: (f32, f32), t: f32| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
    let end = if share >= 1.0 {
        TICK[2]
    } else if reach <= first {
        toward(TICK[0], TICK[1], reach / first)
    } else {
        toward(TICK[1], TICK[2], (reach - first) / (total - first))
    };
    let corner = (reach > first).then_some(TICK[1]);
    [Some(TICK[0]), corner, Some(end)]
        .into_iter()
        .flatten()
        .enumerate()
}

/// `a` moved `t` of the way to `b`, channel by channel.
fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    let at = |x: f32, y: f32| x + (y - x) * t;
    Rgba {
        r: at(a.r, b.r),
        g: at(a.g, b.g),
        b: at(a.b, b.b),
        a: at(a.a, b.a),
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
        let place = BOX.place(text, 16.0, Vec2 { x: 100.0, y: 20.0 }, false);
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
        let rtl = BOX.place(text, 16.0, Vec2 { x: 100.0, y: 20.0 }, true);
        assert_eq!((rtl.mark.x, rtl.text.x), (80.0, 20.0));
    }

    #[test]
    fn a_plate_centres_its_caption_at_least_as_wide_as_tall_or_is_a_dot() {
        let plate = CheckMark {
            shape: CheckShape::Plate,
            checked: false,
        };
        // One digit: a circle.
        let digit = Vec2 { x: 7.0, y: 13.0 };
        assert_eq!(plate.natural(digit, 10.0), Vec2 { x: 16.0, y: 16.0 });
        // Three: a pill with room on each side.
        let three = Vec2 { x: 20.0, y: 13.0 };
        assert_eq!(plate.natural(three, 10.0), Vec2 { x: 28.0, y: 16.0 });
        let place = plate.place(three, 10.0, Vec2::ZERO, false);
        assert_eq!(place.text, Vec2 { x: 4.0, y: 1.5 });
        let wide = plate.place(digit, 10.0, Vec2 { x: 40.0, y: 40.0 }, false);
        assert_eq!(wide.text, Vec2 { x: 16.5, y: 13.5 });
        assert_eq!(
            plate.natural(Vec2 { x: 0.0, y: 13.0 }, 10.0),
            Vec2 { x: 8.0, y: 8.0 }
        );
    }

    #[test]
    fn a_check_with_no_caption_is_its_mark_alone() {
        let switch = CheckMark {
            shape: CheckShape::Switch,
            checked: true,
        };
        assert_eq!(switch.natural(Vec2::ZERO, 0.0), Vec2 { x: 28.0, y: 16.0 });
    }

    #[test]
    fn a_knob_slides_along_its_track_as_the_track_fills() {
        let switch = CheckMark {
            shape: CheckShape::Switch,
            checked: true,
        };
        let ink = Rgba {
            r: 0.1,
            g: 0.2,
            b: 0.9,
            a: 1.0,
        };
        let knob_x = |p| {
            let mut out = Vec::new();
            switch.paint(Rect::new(0.0, 0.0, 28.0, 16.0), ink, p, false, &mut out);
            match (&out[0], &out[1]) {
                (Primitive::Quad(track), Primitive::Quad(knob)) => (track.color.a, knob.rect.x),
                _ => panic!("{out:?}"),
            }
        };
        assert_eq!(knob_x(0.0), (0.28, 2.0));
        assert_eq!(knob_x(1.0), (1.0, 14.0));
        let (fill, x) = knob_x(0.5);
        assert!((fill - 0.64).abs() < 1e-6 && x == 8.0);
    }

    #[test]
    fn a_radio_fills_its_circle_and_grows_its_dot_from_the_centre() {
        let radio = CheckMark {
            shape: CheckShape::Radio,
            checked: true,
        };
        let draw = |p| {
            let mut out = Vec::new();
            radio.paint(
                Rect::new(0.0, 0.0, 20.0, 20.0),
                Rgba::new(0.1, 0.2, 0.9, 1.0),
                p,
                false,
                &mut out,
            );
            out.into_iter()
                .map(|p| match p {
                    Primitive::Quad(q) => q,
                    other => panic!("{other:?}"),
                })
                .collect::<Vec<_>>()
        };
        let [circle] = draw(0.0)[..] else {
            panic!("an empty circle")
        };
        assert_eq!((circle.color.a, circle.radius), (0.0, 10.0));
        assert!(circle.border.width > 0.0);
        let [circle, dot] = draw(1.0)[..] else {
            panic!("a circle and its dot")
        };
        assert_eq!(circle.color.a, 1.0);
        assert_eq!(dot.rect, Rect::new(6.0, 6.0, 8.0, 8.0));
        assert_eq!(dot.radius, 4.0);
    }

    #[test]
    fn a_chevron_turns_from_the_end_edge_to_point_down() {
        let chevron = CheckMark {
            shape: CheckShape::Chevron,
            checked: false,
        };
        let tip = |p, rtl| {
            let mut out = Vec::new();
            chevron.paint(
                Rect::new(0.0, 0.0, 20.0, 20.0),
                Rgba::new(1.0, 1.0, 1.0, 1.0),
                p,
                rtl,
                &mut out,
            );
            match &out[..] {
                [Primitive::Path(path)] => match path.cmds[1] {
                    PathCmd::LineTo(at) => ((at.x * 100.0).round(), (at.y * 100.0).round()),
                    _ => panic!("{path:?}"),
                },
                _ => panic!("{out:?}"),
            }
        };
        assert_eq!(tip(0.0, false), (1220.0, 1000.0), "points right");
        assert_eq!(tip(1.0, false), (1000.0, 1220.0), "turned down");
        assert_eq!(tip(0.0, true), (780.0, 1000.0), "points left");
        assert_eq!(tip(1.0, true), (1000.0, 1220.0), "turned down");
    }

    #[test]
    fn a_tick_draws_along_its_stroke() {
        assert_eq!(tick(1.0).map(|(_, p)| p).collect::<Vec<_>>(), TICK.to_vec());
        let short: Vec<_> = tick(0.1).collect();
        assert_eq!(short.len(), 2);
        assert!(short[1].1.0 > TICK[0].0 && short[1].1.0 < TICK[1].0);
        assert_eq!(tick(0.9).count(), 3);
    }
}
