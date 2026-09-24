//! Paint servers: `<linearGradient>`, `<radialGradient>` and `<pattern>`.
//!
//! Attributes and content are inherited along the `href` chain; the result
//! is expressed in the painted element's user space (`objectBoundingBox`
//! units are folded into the server transform).

use std::sync::Arc;

use crate::color::Color;
use crate::convert::{Axis, Converter, State, bbox_transform, obb_fraction};
use crate::geom::{Rect, Transform};
use crate::syntax::{self, Length, Unit};
use crate::tree::*;
use crate::xml::NodeId;

/// `node` followed by its `href` ancestors (cycle-free), restricted to
/// elements named in `kinds`.
pub(crate) fn href_chain(c: &Converter, node: NodeId, kinds: &[&str]) -> Vec<NodeId> {
    let mut out = vec![node];
    let mut cur = node;
    while let Some(next) = c.sd.href(cur) {
        if out.contains(&next) || !c.name(next).is_some_and(|n| kinds.contains(&n)) {
            break;
        }
        out.push(next);
        cur = next;
    }
    out
}

/// The first value of attribute `name` along `chain`.
pub(crate) fn chain_attr<'a>(c: &Converter<'a>, chain: &[NodeId], name: &str) -> Option<&'a str> {
    chain.iter().find_map(|&n| c.attr(n, name))
}

const GRADIENTS: &[&str] = &["linearGradient", "radialGradient"];

pub(crate) fn gradient(
    c: &mut Converter,
    node: NodeId,
    st: &State,
    bbox: Option<Rect>,
) -> Option<Paint> {
    let chain = href_chain(c, node, GRADIENTS);
    let stops = chain
        .iter()
        .find(|&&n| {
            c.sd.doc
                .element_children(n)
                .any(|k| c.name(k) == Some("stop"))
        })
        .map(|&n| collect_stops(c, n))
        .unwrap_or_default();
    match stops.len() {
        0 => return None,
        1 => return Some(Paint::Color(stops[0].color)),
        _ => {}
    }
    let obb = chain_attr(c, &chain, "gradientUnits") != Some("userSpaceOnUse");
    let mut transform = chain_attr(c, &chain, "gradientTransform")
        .and_then(syntax::parse_transform)
        .unwrap_or_default();
    if obb {
        let b = bbox.filter(Rect::is_valid)?;
        transform = bbox_transform(b).pre_concat(&transform);
    }
    if !transform.is_valid() {
        return None;
    }
    let spread = match chain_attr(c, &chain, "spreadMethod") {
        Some("reflect") => SpreadMethod::Reflect,
        Some("repeat") => SpreadMethod::Repeat,
        _ => SpreadMethod::Pad,
    };
    let id = c.attr(node, "id").unwrap_or("").to_string();
    let coord = |name: &str, axis: Axis, default: Length| -> f32 {
        let v = chain_attr(c, &chain, name);
        if obb {
            v.and_then(obb_fraction).unwrap_or(match default.unit {
                Unit::Percent => default.n / 100.0,
                _ => default.n,
            })
        } else {
            st.resolve(v.and_then(syntax::parse_length).unwrap_or(default), axis)
        }
    };
    let pct = |n: f32| Length::new(n, Unit::Percent);
    let last = stops[stops.len() - 1].color;

    if c.name(node) == Some("linearGradient") {
        let x1 = coord("x1", Axis::X, pct(0.0));
        let y1 = coord("y1", Axis::Y, pct(0.0));
        let x2 = coord("x2", Axis::X, pct(100.0));
        let y2 = coord("y2", Axis::Y, pct(0.0));
        if x1 == x2 && y1 == y2 {
            return Some(Paint::Color(last));
        }
        Some(Paint::LinearGradient(Arc::new(LinearGradient {
            id,
            x1,
            y1,
            x2,
            y2,
            transform,
            spread,
            stops,
        })))
    } else {
        let cx = coord("cx", Axis::X, pct(50.0));
        let cy = coord("cy", Axis::Y, pct(50.0));
        let r = coord("r", Axis::Diag, pct(50.0));
        let has = |n: &str| chain_attr(c, &chain, n).is_some();
        let fx = if has("fx") {
            coord("fx", Axis::X, pct(50.0))
        } else {
            cx
        };
        let fy = if has("fy") {
            coord("fy", Axis::Y, pct(50.0))
        } else {
            cy
        };
        let fr = coord("fr", Axis::Diag, pct(0.0));
        if r.is_nan() || r <= 0.0 || fr < 0.0 {
            // A zero radius paints the last stop color (negative is an error).
            return (r == 0.0 && fr >= 0.0).then_some(Paint::Color(last));
        }
        Some(Paint::RadialGradient(Arc::new(RadialGradient {
            id,
            cx,
            cy,
            r,
            fx,
            fy,
            fr,
            transform,
            spread,
            stops,
        })))
    }
}

fn collect_stops(c: &Converter, grad: NodeId) -> Vec<Stop> {
    let mut stops: Vec<Stop> = Vec::new();
    let mut prev = 0.0f32;
    let parent_style = c.sd.style_from_ancestors(grad);
    let kids: Vec<NodeId> =
        c.sd.doc
            .element_children(grad)
            .filter(|&k| c.name(k) == Some("stop"))
            .collect();
    for k in kids {
        let style = crate::style::Style::compute(&parent_style, c.sd, k);
        let offset = c
            .attr(k, "offset")
            .and_then(|s| {
                let l = syntax::parse_length(s)?;
                Some(match l.unit {
                    Unit::Percent => l.n / 100.0,
                    Unit::None => l.n,
                    _ => return None,
                })
            })
            .unwrap_or(0.0)
            .clamp(0.0, 1.0)
            .max(prev);
        prev = offset;
        let base = style.stop_color.resolve(style.color);
        let a = (base.a as f32 * style.stop_opacity)
            .round()
            .clamp(0.0, 255.0) as u8;
        stops.push(Stop {
            offset,
            color: Color::new(base.r, base.g, base.b, a),
        });
    }
    stops
}

pub(crate) fn pattern(
    c: &mut Converter,
    node: NodeId,
    st: &State,
    bbox: Option<Rect>,
) -> Option<Paint> {
    let chain = href_chain(c, node, &["pattern"]);
    let content = *chain
        .iter()
        .find(|&&n| c.sd.doc.element_children(n).next().is_some())?;
    if c.stack.contains(&content) {
        return None;
    }
    let obb_units = chain_attr(c, &chain, "patternUnits") != Some("userSpaceOnUse");
    let obb_content = chain_attr(c, &chain, "patternContentUnits") == Some("objectBoundingBox");
    let view_box = chain_attr(c, &chain, "viewBox")
        .and_then(syntax::parse_view_box)
        .filter(Rect::is_valid);
    let aspect = chain_attr(c, &chain, "preserveAspectRatio")
        .map(syntax::parse_aspect)
        .unwrap_or_default();
    let transform = chain_attr(c, &chain, "patternTransform")
        .and_then(syntax::parse_transform)
        .unwrap_or_default();
    if !transform.is_valid() {
        return None;
    }

    let rect = if obb_units {
        let b = bbox.filter(Rect::is_valid)?;
        let f = |name: &str| {
            chain_attr(c, &chain, name)
                .and_then(obb_fraction)
                .unwrap_or(0.0)
        };
        Rect::new(
            b.x + f("x") * b.w,
            b.y + f("y") * b.h,
            f("width") * b.w,
            f("height") * b.h,
        )
    } else {
        let l = |name: &str, axis| {
            st.resolve(
                chain_attr(c, &chain, name)
                    .and_then(syntax::parse_length)
                    .unwrap_or(Length::ZERO),
                axis,
            )
        };
        Rect::new(
            l("x", Axis::X),
            l("y", Axis::Y),
            l("width", Axis::X),
            l("height", Axis::Y),
        )
    };
    if !rect.is_valid() {
        return None;
    }

    let mut cst = State {
        style: c.sd.style_from_ancestors(content),
        vp: st.vp,
        in_clip: false,
        context: None,
    };
    let content_ts = match view_box {
        Some(vb) => {
            cst.vp = (vb.w, vb.h);
            syntax::view_box_transform(vb, aspect, rect.w, rect.h)
        }
        None if obb_content => {
            let b = bbox.filter(Rect::is_valid)?;
            cst.vp = (1.0, 1.0);
            Transform::scale(b.w, b.h)
        }
        None => Transform::IDENTITY,
    };
    c.stack.push(content);
    let mut inner = Group {
        transform: content_ts,
        ..Group::default()
    };
    c.convert_children(content, &cst, &mut inner.children);
    c.stack.pop();
    if inner.children.is_empty() {
        return None;
    }
    Some(Paint::Pattern(Arc::new(Pattern {
        id: c.attr(node, "id").unwrap_or("").to_string(),
        rect,
        transform,
        root: Group {
            children: vec![Node::Group(Box::new(inner))],
            ..Group::default()
        },
    })))
}
