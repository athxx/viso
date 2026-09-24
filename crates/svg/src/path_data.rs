//! The `d` attribute grammar (SVG 2 §9.3).
//!
//! All commands (`MLHVCSQTAZ`, both cases), implicit command repetition,
//! compact number syntax and smooth-curve reflection. Per the SVG error rule,
//! a malformed path renders up to (not including) the first bad command.

use crate::geom::{PathData, Point};
use crate::syntax::Stream;

/// What kind of source command produced a [`CmdSpan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CmdKind {
    Move,
    Draw,
    Close,
}

/// The canonical segments `start..end` produced by one source command. Markers
/// need source-command vertices, not canonical ones (an arc becomes several
/// cubics but is one vertex).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CmdSpan {
    pub kind: CmdKind,
    pub start: usize,
    pub end: usize,
}

/// Parse path data, returning the outline and per-command segment spans.
pub(crate) fn parse_path(s: &str) -> (PathData, Vec<CmdSpan>) {
    let mut st = Stream::new(s);
    let mut p = PathData::new();
    let mut spans: Vec<CmdSpan> = Vec::new();
    let mut prev: Option<u8> = None;
    // Second control point of the previous C/S, or control of the previous Q/T.
    let mut last_ctrl = Point::default();

    loop {
        st.skip_ws();
        let Some(c) = st.peek() else { break };
        let cmd = if c.is_ascii_alphabetic() {
            st.pos += 1;
            c
        } else if let Some(pc) = prev
            && (c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.'))
            && !matches!(pc, b'Z' | b'z')
        {
            // Implicit repetition; a repeated moveto is a lineto.
            match pc {
                b'M' => b'L',
                b'm' => b'l',
                other => other,
            }
        } else {
            break;
        };
        if prev.is_none() && cmd != b'M' && cmd != b'm' {
            break;
        }
        let rel = cmd.is_ascii_lowercase();
        let upper = cmd.to_ascii_uppercase();
        let cur = p.current();
        let (ox, oy) = if rel { (cur.x, cur.y) } else { (0.0, 0.0) };
        let start = p.len();

        macro_rules! num {
            () => {{
                match st.parse_number() {
                    Some(v) => {
                        st.skip_comma_ws();
                        v
                    }
                    None => break,
                }
            }};
        }

        match upper {
            b'M' => {
                let x = num!() + ox;
                let y = num!() + oy;
                let collapsed =
                    matches!(p.segments().last(), Some(crate::geom::Segment::MoveTo(_)));
                p.move_to(x, y);
                if collapsed && spans.last().is_some_and(|s| s.kind == CmdKind::Move) {
                    spans.pop();
                }
                prev = Some(cmd);
                spans.push(CmdSpan {
                    kind: CmdKind::Move,
                    start: p.len() - 1,
                    end: p.len(),
                });
                last_ctrl = p.current();
                continue;
            }
            b'L' => {
                let x = num!() + ox;
                let y = num!() + oy;
                p.line_to(x, y);
            }
            b'H' => {
                let x = num!() + ox;
                p.line_to(x, cur.y);
            }
            b'V' => {
                let y = num!() + oy;
                p.line_to(cur.x, y);
            }
            b'C' => {
                let x1 = num!() + ox;
                let y1 = num!() + oy;
                let x2 = num!() + ox;
                let y2 = num!() + oy;
                let x = num!() + ox;
                let y = num!() + oy;
                p.cubic_to(x1, y1, x2, y2, x, y);
                last_ctrl = Point::new(x2, y2);
                prev = Some(cmd);
                push_draw(&mut spans, start, p.len());
                continue;
            }
            b'S' => {
                let x2 = num!() + ox;
                let y2 = num!() + oy;
                let x = num!() + ox;
                let y = num!() + oy;
                let c1 = if matches!(prev, Some(b'C' | b'c' | b'S' | b's')) {
                    cur * 2.0 - last_ctrl
                } else {
                    cur
                };
                p.cubic_to(c1.x, c1.y, x2, y2, x, y);
                last_ctrl = Point::new(x2, y2);
                prev = Some(cmd);
                push_draw(&mut spans, start, p.len());
                continue;
            }
            b'Q' => {
                let x1 = num!() + ox;
                let y1 = num!() + oy;
                let x = num!() + ox;
                let y = num!() + oy;
                p.quad_to(x1, y1, x, y);
                last_ctrl = Point::new(x1, y1);
                prev = Some(cmd);
                push_draw(&mut spans, start, p.len());
                continue;
            }
            b'T' => {
                let x = num!() + ox;
                let y = num!() + oy;
                let c = if matches!(prev, Some(b'Q' | b'q' | b'T' | b't')) {
                    cur * 2.0 - last_ctrl
                } else {
                    cur
                };
                p.quad_to(c.x, c.y, x, y);
                last_ctrl = c;
                prev = Some(cmd);
                push_draw(&mut spans, start, p.len());
                continue;
            }
            b'A' => {
                let rx = num!();
                let ry = num!();
                let rot = num!();
                let Some(large) = st.parse_flag() else { break };
                let Some(sweep) = st.parse_flag() else { break };
                let x = num!() + ox;
                let y = num!() + oy;
                p.arc_to(rx, ry, rot, large, sweep, x, y);
            }
            b'Z' => {
                let open = matches!(
                    p.segments().last(),
                    Some(s) if !matches!(s, crate::geom::Segment::Close)
                );
                p.close();
                st.skip_comma_ws();
                prev = Some(cmd);
                last_ctrl = p.current();
                if open {
                    spans.push(CmdSpan {
                        kind: CmdKind::Close,
                        start,
                        end: p.len(),
                    });
                }
                continue;
            }
            _ => break,
        }
        prev = Some(cmd);
        last_ctrl = p.current();
        push_draw(&mut spans, start, p.len());
    }
    (p, spans)
}

fn push_draw(spans: &mut Vec<CmdSpan>, start: usize, end: usize) {
    spans.push(CmdSpan {
        kind: CmdKind::Draw,
        start,
        end,
    });
}

/// Spans for a path built without arcs or implicit movetos (polylines,
/// polygons, lines): one command per canonical segment.
pub(crate) fn simple_spans(p: &PathData) -> Vec<CmdSpan> {
    use crate::geom::Segment;
    p.segments()
        .iter()
        .enumerate()
        .map(|(i, s)| CmdSpan {
            kind: match s {
                Segment::MoveTo(_) => CmdKind::Move,
                Segment::Close => CmdKind::Close,
                _ => CmdKind::Draw,
            },
            start: i,
            end: i + 1,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Segment;

    fn segs(d: &str) -> Vec<Segment> {
        parse_path(d).0.segments().to_vec()
    }

    #[test]
    fn relative_and_implicit_commands() {
        let s = segs("m10 10 20 0 0 20z l5 5");
        assert_eq!(
            s,
            vec![
                Segment::MoveTo(Point::new(10.0, 10.0)),
                Segment::LineTo(Point::new(30.0, 10.0)),
                Segment::LineTo(Point::new(30.0, 30.0)),
                Segment::Close,
                Segment::MoveTo(Point::new(10.0, 10.0)),
                Segment::LineTo(Point::new(15.0, 15.0)),
            ]
        );
    }

    #[test]
    fn smooth_reflection_and_compact_arcs() {
        let s = segs("M0 0C0 10 10 10 10 0S20-10 20 0");
        assert_eq!(
            s[2],
            Segment::CubicTo(
                Point::new(10.0, -10.0),
                Point::new(20.0, -10.0),
                Point::new(20.0, 0.0)
            )
        );
        let (p, spans) = parse_path("M0 0a5 5 0 1010 0");
        assert!(p.len() >= 3, "arc converted to cubics");
        assert_eq!(spans.len(), 2, "one span per source command");
    }

    #[test]
    fn errors_render_up_to_the_error() {
        assert_eq!(segs("M0 0 L10 10 L20").len(), 2);
        assert!(segs("L10 10").is_empty());
        assert_eq!(segs("M1 2 H 5 V 6 h-1 v-1").len(), 5);
    }
}
