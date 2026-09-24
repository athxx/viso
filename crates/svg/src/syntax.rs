//! Microsyntax parsers for SVG attribute values: numbers, lengths, lists,
//! transform lists, `viewBox`, `preserveAspectRatio` and `url(#id)` references.

use crate::geom::{Rect, Transform};

/// A byte cursor over an attribute value.
pub(crate) struct Stream<'a> {
    s: &'a [u8],
    pub pos: usize,
}

impl<'a> Stream<'a> {
    pub(crate) fn new(s: &'a str) -> Self {
        Stream {
            s: s.as_bytes(),
            pos: 0,
        }
    }

    #[inline]
    pub(crate) fn at_end(&self) -> bool {
        self.pos >= self.s.len()
    }

    #[inline]
    pub(crate) fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    pub(crate) fn rest(&self) -> &'a str {
        std::str::from_utf8(&self.s[self.pos.min(self.s.len())..]).unwrap_or("")
    }

    pub(crate) fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b'\x0C') {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// Skip whitespace, at most one comma, then whitespace.
    pub(crate) fn skip_comma_ws(&mut self) {
        self.skip_ws();
        if self.peek() == Some(b',') {
            self.pos += 1;
            self.skip_ws();
        }
    }

    pub(crate) fn consume(&mut self, b: u8) -> bool {
        if self.peek() == Some(b) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// Case-insensitive keyword match at the cursor; advances on success.
    pub(crate) fn consume_ident(&mut self, word: &str) -> bool {
        let w = word.as_bytes();
        if self.s.len() >= self.pos + w.len()
            && self.s[self.pos..self.pos + w.len()].eq_ignore_ascii_case(w)
        {
            self.pos += w.len();
            true
        } else {
            false
        }
    }

    /// An SVG/CSS number: `[+-]? (digits [. digits?] | . digits) ([eE] [+-]? digits)?`.
    ///
    /// The exponent is only taken when a digit follows, so `1em`/`2ex` lex as a
    /// number followed by a unit. Compact path syntax (`1.5.5` = `1.5 .5`,
    /// `1-2` = `1 -2`) falls out of stopping at the first byte that can't
    /// continue the number.
    pub(crate) fn parse_number(&mut self) -> Option<f32> {
        self.skip_ws();
        let start = self.pos;
        let mut i = self.pos;
        let s = self.s;
        if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
            i += 1;
        }
        let int_start = i;
        while i < s.len() && s[i].is_ascii_digit() {
            i += 1;
        }
        let mut digits = i > int_start;
        if i < s.len() && s[i] == b'.' {
            let frac_start = i + 1;
            let mut j = frac_start;
            while j < s.len() && s[j].is_ascii_digit() {
                j += 1;
            }
            if j > frac_start || digits {
                digits |= j > frac_start;
                i = j;
            }
        }
        if !digits {
            return None;
        }
        if i < s.len() && (s[i] == b'e' || s[i] == b'E') {
            let mut j = i + 1;
            if j < s.len() && (s[j] == b'+' || s[j] == b'-') {
                j += 1;
            }
            if j < s.len() && s[j].is_ascii_digit() {
                while j < s.len() && s[j].is_ascii_digit() {
                    j += 1;
                }
                i = j;
            }
        }
        let text = std::str::from_utf8(&s[start..i]).ok()?;
        let v: f64 = text.parse().ok()?;
        let v = v as f32;
        if !v.is_finite() {
            return None;
        }
        self.pos = i;
        Some(v)
    }

    /// A number followed by an optional unit.
    pub(crate) fn parse_length(&mut self) -> Option<Length> {
        let n = self.parse_number()?;
        let unit = if self.consume(b'%') {
            Unit::Percent
        } else if self.consume_ident("px") {
            Unit::Px
        } else if self.consume_ident("em") {
            Unit::Em
        } else if self.consume_ident("ex") {
            Unit::Ex
        } else if self.consume_ident("in") {
            Unit::In
        } else if self.consume_ident("cm") {
            Unit::Cm
        } else if self.consume_ident("mm") {
            Unit::Mm
        } else if self.consume_ident("pt") {
            Unit::Pt
        } else if self.consume_ident("pc") {
            Unit::Pc
        } else if self.consume_ident("q") {
            Unit::Q
        } else {
            Unit::None
        };
        Some(Length { n, unit })
    }

    /// A single-character path flag (`0`/`1`), which may be unseparated from
    /// the following number (`a1 1 0 00 1 1`).
    pub(crate) fn parse_flag(&mut self) -> Option<bool> {
        self.skip_ws();
        let r = match self.peek()? {
            b'0' => false,
            b'1' => true,
            _ => return None,
        };
        self.pos += 1;
        self.skip_comma_ws();
        Some(r)
    }
}

/// Length units (CSS absolute units resolve at 96 dpi).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Unit {
    None,
    Px,
    Em,
    Ex,
    In,
    Cm,
    Mm,
    Pt,
    Pc,
    Q,
    Percent,
}

/// A number with a unit, unresolved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Length {
    pub n: f32,
    pub unit: Unit,
}

impl Length {
    pub const ZERO: Length = Length {
        n: 0.0,
        unit: Unit::None,
    };

    pub const fn new(n: f32, unit: Unit) -> Self {
        Length { n, unit }
    }

    /// Resolve to user units. `font_size` backs `em`/`ex`; `percent_base` is
    /// what `100%` means in this context.
    pub fn resolve(&self, font_size: f32, percent_base: f32) -> f32 {
        let n = self.n;
        match self.unit {
            Unit::None | Unit::Px => n,
            Unit::Em => n * font_size,
            Unit::Ex => n * font_size / 2.0,
            Unit::In => n * 96.0,
            Unit::Cm => n * 96.0 / 2.54,
            Unit::Mm => n * 96.0 / 25.4,
            Unit::Pt => n * 4.0 / 3.0,
            Unit::Pc => n * 16.0,
            Unit::Q => n * 96.0 / 101.6,
            Unit::Percent => n * percent_base / 100.0,
        }
    }
}

/// Parse a complete single length (trailing garbage → `None`).
pub(crate) fn parse_length(s: &str) -> Option<Length> {
    let mut st = Stream::new(s);
    let l = st.parse_length()?;
    st.skip_ws();
    st.at_end().then_some(l)
}

/// Parse a complete single number (trailing garbage → `None`).
pub(crate) fn parse_number(s: &str) -> Option<f32> {
    let mut st = Stream::new(s);
    let n = st.parse_number()?;
    st.skip_ws();
    st.at_end().then_some(n)
}

/// A number or percentage in `[0, 1]` (opacity-style values).
pub(crate) fn parse_opacity(s: &str) -> Option<f32> {
    let mut st = Stream::new(s);
    let n = st.parse_number()?;
    let v = if st.consume(b'%') { n / 100.0 } else { n };
    st.skip_ws();
    st.at_end().then_some(v.clamp(0.0, 1.0))
}

/// A whitespace/comma separated number list. Stops (returning what parsed so
/// far) at the first malformed entry, matching the SVG error-handling rule for
/// `points` ("render up to the error").
pub(crate) fn parse_number_list(s: &str) -> Vec<f32> {
    let mut st = Stream::new(s);
    let mut out = Vec::new();
    loop {
        st.skip_ws();
        if st.at_end() {
            break;
        }
        match st.parse_number() {
            Some(n) => out.push(n),
            None => break,
        }
        st.skip_comma_ws();
    }
    out
}

/// A whitespace/comma separated length list; `None` on any malformed entry.
pub(crate) fn parse_length_list(s: &str) -> Option<Vec<Length>> {
    let mut st = Stream::new(s);
    let mut out = Vec::new();
    loop {
        st.skip_ws();
        if st.at_end() {
            break;
        }
        out.push(st.parse_length()?);
        st.skip_comma_ws();
    }
    Some(out)
}

/// `viewBox="min-x min-y width height"`. Non-positive sizes disable rendering
/// (the caller treats `None` from a *present* attribute accordingly).
pub(crate) fn parse_view_box(s: &str) -> Option<Rect> {
    let v = parse_number_list(s);
    if v.len() != 4 {
        return None;
    }
    Some(Rect::new(v[0], v[1], v[2], v[3]))
}

/// Parse a transform list (`translate(..) rotate(..) ...`).
///
/// Accepts both the SVG attribute syntax and the CSS `transform` property
/// syntax (`px`/`deg`/`rad`/`turn` units, `translateX` etc.). A malformed list
/// yields `None`, which SVG treats as the identity for attributes; callers
/// decide.
pub(crate) fn parse_transform(s: &str) -> Option<Transform> {
    let mut st = Stream::new(s);
    let mut ts = Transform::IDENTITY;
    loop {
        st.skip_comma_ws();
        if st.at_end() {
            break;
        }
        if st.consume_ident("none") {
            continue;
        }
        let start = st.pos;
        while let Some(c) = st.peek() {
            if c.is_ascii_alphanumeric() {
                st.pos += 1;
            } else {
                break;
            }
        }
        let name = s.get(start..st.pos)?.to_ascii_lowercase();
        st.skip_ws();
        if !st.consume(b'(') {
            return None;
        }
        let mut args: Vec<f32> = Vec::with_capacity(6);
        loop {
            st.skip_comma_ws();
            if st.consume(b')') {
                break;
            }
            let n = st.parse_number()?;
            // CSS units on transform arguments.
            let n = if st.consume_ident("deg") || st.consume_ident("px") {
                n
            } else if st.consume_ident("rad") {
                n.to_degrees()
            } else if st.consume_ident("grad") {
                n * 0.9
            } else if st.consume_ident("turn") {
                n * 360.0
            } else {
                n
            };
            args.push(n);
            if args.len() > 6 {
                return None;
            }
        }
        let t = match (name.as_str(), args.as_slice()) {
            ("matrix", &[a, b, c, d, e, f]) => Transform::new(a, b, c, d, e, f),
            ("translate", &[x]) => Transform::translate(x, 0.0),
            ("translate", &[x, y]) => Transform::translate(x, y),
            ("translatex", &[x]) => Transform::translate(x, 0.0),
            ("translatey", &[y]) => Transform::translate(0.0, y),
            ("scale", &[x]) => Transform::scale(x, x),
            ("scale", &[x, y]) => Transform::scale(x, y),
            ("scalex", &[x]) => Transform::scale(x, 1.0),
            ("scaley", &[y]) => Transform::scale(1.0, y),
            ("rotate", &[a]) => Transform::rotate(a),
            ("rotate", &[a, cx, cy]) => Transform::translate(cx, cy)
                .pre_concat(&Transform::rotate(a))
                .pre_concat(&Transform::translate(-cx, -cy)),
            ("skewx", &[a]) => Transform::skew_x(a),
            ("skewy", &[a]) => Transform::skew_y(a),
            ("skew", &[a]) => Transform::skew_x(a),
            ("skew", &[a, b]) => Transform::new(
                1.0,
                b.to_radians().tan(),
                a.to_radians().tan(),
                1.0,
                0.0,
                0.0,
            ),
            _ => return None,
        };
        ts = ts.pre_concat(&t);
    }
    Some(ts)
}

/// `preserveAspectRatio` alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    None,
    XMinYMin,
    XMidYMin,
    XMaxYMin,
    XMinYMid,
    XMidYMid,
    XMaxYMid,
    XMinYMax,
    XMidYMax,
    XMaxYMax,
}

/// `preserveAspectRatio="<align> [meet|slice]"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AspectRatio {
    pub align: Align,
    pub slice: bool,
}

impl Default for AspectRatio {
    fn default() -> Self {
        AspectRatio {
            align: Align::XMidYMid,
            slice: false,
        }
    }
}

pub(crate) fn parse_aspect(s: &str) -> AspectRatio {
    let mut words = s.split_ascii_whitespace();
    let mut w = words.next();
    if w == Some("defer") {
        w = words.next();
    }
    let align = match w {
        Some("none") => Align::None,
        Some("xMinYMin") => Align::XMinYMin,
        Some("xMidYMin") => Align::XMidYMin,
        Some("xMaxYMin") => Align::XMaxYMin,
        Some("xMinYMid") => Align::XMinYMid,
        Some("xMidYMid") => Align::XMidYMid,
        Some("xMaxYMid") => Align::XMaxYMid,
        Some("xMinYMax") => Align::XMinYMax,
        Some("xMidYMax") => Align::XMidYMax,
        Some("xMaxYMax") => Align::XMaxYMax,
        _ => return AspectRatio::default(),
    };
    let slice = words.next() == Some("slice");
    AspectRatio { align, slice }
}

/// The transform mapping `view_box` into a viewport of `size` at the origin,
/// honoring `preserveAspectRatio` (SVG 2 §8.2 "viewBox" algorithm).
pub(crate) fn view_box_transform(view_box: Rect, aspect: AspectRatio, w: f32, h: f32) -> Transform {
    let sx = w / view_box.w;
    let sy = h / view_box.h;
    if aspect.align == Align::None {
        return Transform::new(sx, 0.0, 0.0, sy, -view_box.x * sx, -view_box.y * sy);
    }
    let s = if aspect.slice { sx.max(sy) } else { sx.min(sy) };
    let (ax, ay) = match aspect.align {
        Align::None | Align::XMinYMin => (0.0, 0.0),
        Align::XMidYMin => (0.5, 0.0),
        Align::XMaxYMin => (1.0, 0.0),
        Align::XMinYMid => (0.0, 0.5),
        Align::XMidYMid => (0.5, 0.5),
        Align::XMaxYMid => (1.0, 0.5),
        Align::XMinYMax => (0.0, 1.0),
        Align::XMidYMax => (0.5, 1.0),
        Align::XMaxYMax => (1.0, 1.0),
    };
    let tx = -view_box.x * s + (w - view_box.w * s) * ax;
    let ty = -view_box.y * s + (h - view_box.h * s) * ay;
    Transform::new(s, 0.0, 0.0, s, tx, ty)
}

/// Extract the fragment id from `url(#id)` / `url("#id")` (plus any trailing
/// fallback text, returned second), or from a bare `#id` IRI.
pub(crate) fn parse_func_iri(s: &str) -> Option<(&str, &str)> {
    let s = s.trim();
    let rest = s.strip_prefix("url(")?;
    let close = rest.find(')')?;
    let inner = rest[..close]
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .trim();
    let id = inner
        .strip_prefix('#')
        .or_else(|| inner.rsplit_once('#').map(|(_, id)| id))?;
    Some((id, rest[close + 1..].trim()))
}

/// `href="#id"` → `id`. Only same-document references are supported.
pub(crate) fn parse_iri(s: &str) -> Option<&str> {
    s.trim().strip_prefix('#')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_follow_svg_grammar() {
        let mut st = Stream::new("1.5.5-2e2 1em");
        assert_eq!(st.parse_number(), Some(1.5));
        assert_eq!(st.parse_number(), Some(0.5));
        assert_eq!(st.parse_number(), Some(-200.0));
        let l = st.parse_length().unwrap();
        assert_eq!((l.n, l.unit), (1.0, Unit::Em));
    }

    #[test]
    fn transform_list_composes_left_to_right() {
        let t = parse_transform("translate(10, 20) scale(2)").unwrap();
        assert_eq!(t, Transform::new(2.0, 0.0, 0.0, 2.0, 10.0, 20.0));
        let r = parse_transform("rotate(90 10 10)").unwrap();
        let p = r.apply(crate::geom::Point::new(20.0, 10.0));
        assert!((p.x - 10.0).abs() < 1e-4 && (p.y - 20.0).abs() < 1e-4);
        assert!(parse_transform("translate(1,").is_none());
    }

    #[test]
    fn view_box_meet_centers() {
        let t = view_box_transform(
            Rect::new(0.0, 0.0, 10.0, 20.0),
            AspectRatio::default(),
            100.0,
            100.0,
        );
        assert_eq!(t, Transform::new(5.0, 0.0, 0.0, 5.0, 25.0, 0.0));
    }

    #[test]
    fn func_iri_forms() {
        assert_eq!(parse_func_iri("url(#a)"), Some(("a", "")));
        assert_eq!(parse_func_iri("url( '#b' ) red"), Some(("b", "red")));
        assert_eq!(parse_func_iri("red"), None);
    }
}
