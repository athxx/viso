//! `filter` property: `<filter>` references and CSS filter functions.
//!
//! Every length is resolved to the filtered element's user space here
//! (including `primitiveUnits="objectBoundingBox"`), so the tree's
//! [`Filter`]s can be evaluated without the element's bounding box.

use std::sync::Arc;

use crate::color::Color;
use crate::convert::{Axis, Converter, Ref, State, obb_fraction};
use crate::geom::Rect;
use crate::marker::parse_angle;
use crate::paint::{chain_attr, href_chain};
use crate::style::Style;
use crate::syntax::{self, Length, Stream, Unit};
use crate::tree::*;
use crate::xml::NodeId;

pub(crate) fn resolve_filters(
    c: &mut Converter,
    value: &str,
    st: &State,
    bbox: Option<Rect>,
) -> Ref<Vec<Arc<Filter>>> {
    let Some(items) = parse_filter_list(value) else {
        // An unparsable value is dropped by the cascade in browsers.
        return Ref::None;
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let f = match item {
            FilterItem::Url(id) => {
                let Some(node) = c.sd.by_id(&id).filter(|&n| c.name(n) == Some("filter")) else {
                    // A reference to a missing/non-filter element ignores the
                    // whole chain (Filter Effects §5).
                    return Ref::None;
                };
                match url_filter(c, node, &id, st, bbox) {
                    Some(f) => f,
                    None => return Ref::Invalid,
                }
            }
            FilterItem::Func(name, args) => match css_filter(&name, &args, st, bbox) {
                Some(f) => f,
                None => return Ref::None,
            },
        };
        out.push(Arc::new(f));
    }
    if out.is_empty() {
        Ref::None
    } else {
        Ref::Some(out)
    }
}

enum FilterItem {
    Url(String),
    Func(String, String),
}

fn parse_filter_list(s: &str) -> Option<Vec<FilterItem>> {
    let mut out = Vec::new();
    let mut rest = s.trim();
    while !rest.is_empty() {
        let open = rest.find('(')?;
        let name = rest[..open].trim().to_ascii_lowercase();
        let close = open + rest[open..].find(')')?;
        let args = rest[open + 1..close].trim();
        if name == "url" {
            let (id, _) = syntax::parse_func_iri(&rest[..=close])?;
            out.push(FilterItem::Url(id.to_string()));
        } else {
            out.push(FilterItem::Func(name, args.to_string()));
        }
        rest = rest[close + 1..].trim_start();
    }
    Some(out)
}

/// Default filter region: the bounding box grown by 10% on each side.
fn default_region(bbox: Option<Rect>) -> Option<Rect> {
    let b = bbox.filter(Rect::is_valid)?;
    Some(Rect::new(
        b.x - 0.1 * b.w,
        b.y - 0.1 * b.h,
        1.2 * b.w,
        1.2 * b.h,
    ))
}

fn css_filter(name: &str, args: &str, st: &State, bbox: Option<Rect>) -> Option<Filter> {
    let region = default_region(bbox)?;
    // `<number> | <percentage>`, with a default when omitted.
    let amount = |default: f32| -> Option<f32> {
        if args.is_empty() {
            return Some(default);
        }
        let l = syntax::parse_length(args)?;
        match l.unit {
            Unit::Percent => Some(l.n / 100.0),
            Unit::None => Some(l.n),
            _ => None,
        }
    };
    let input = FilterInput::SourceGraphic;
    let kind = match name {
        "blur" => {
            let sd = if args.is_empty() {
                0.0
            } else {
                st.resolve(syntax::parse_length(args)?, Axis::Diag)
            };
            if sd < 0.0 {
                return None;
            }
            FilterKind::GaussianBlur {
                input,
                std_dev_x: sd,
                std_dev_y: sd,
            }
        }
        "drop-shadow" => drop_shadow_func(args, st)?,
        "grayscale" => matrix(input, saturate_matrix(1.0 - amount(1.0)?.clamp(0.0, 1.0))),
        "saturate" => matrix(input, saturate_matrix(amount(1.0)?.max(0.0))),
        "sepia" => {
            let a = amount(1.0)?.clamp(0.0, 1.0);
            let k = 1.0 - a;
            #[rustfmt::skip]
            let m = [
                0.393 + 0.607 * k, 0.769 - 0.769 * k, 0.189 - 0.189 * k, 0.0, 0.0,
                0.349 - 0.349 * k, 0.686 + 0.314 * k, 0.168 - 0.168 * k, 0.0, 0.0,
                0.272 - 0.272 * k, 0.534 - 0.534 * k, 0.131 + 0.869 * k, 0.0, 0.0,
                0.0, 0.0, 0.0, 1.0, 0.0,
            ];
            matrix(input, m)
        }
        "hue-rotate" => {
            let deg = if args.is_empty() {
                0.0
            } else {
                parse_angle(args)?
            };
            matrix(input, hue_rotate_matrix(deg))
        }
        "invert" => {
            let a = amount(1.0)?.clamp(0.0, 1.0);
            let f = TransferFunc::Table(vec![a, 1.0 - a]);
            transfer(input, [f.clone(), f.clone(), f, TransferFunc::Identity])
        }
        "opacity" => {
            let a = amount(1.0)?.clamp(0.0, 1.0);
            let f = TransferFunc::Table(vec![0.0, a]);
            transfer(
                input,
                [
                    TransferFunc::Identity,
                    TransferFunc::Identity,
                    TransferFunc::Identity,
                    f,
                ],
            )
        }
        "brightness" => {
            let a = amount(1.0)?.max(0.0);
            let f = TransferFunc::Linear {
                slope: a,
                intercept: 0.0,
            };
            transfer(input, [f.clone(), f.clone(), f, TransferFunc::Identity])
        }
        "contrast" => {
            let a = amount(1.0)?.max(0.0);
            let f = TransferFunc::Linear {
                slope: a,
                intercept: -0.5 * a + 0.5,
            };
            transfer(input, [f.clone(), f.clone(), f, TransferFunc::Identity])
        }
        _ => return None,
    };
    Some(Filter {
        id: String::new(),
        region,
        primitives: vec![FilterPrimitive {
            region,
            // CSS shorthand filters operate in sRGB except blur/drop-shadow,
            // whose results are identical either way for opaque content.
            linear_rgb: false,
            result: String::new(),
            kind,
        }],
    })
}

fn drop_shadow_func(args: &str, st: &State) -> Option<FilterKind> {
    // `<color>? <length>{2,3}` in either order.
    let mut lens = Vec::new();
    let mut color = None;
    let mut s = Stream::new(args);
    loop {
        s.skip_ws();
        if s.at_end() {
            break;
        }
        let save = s.pos;
        if let Some(l) = s.parse_length() {
            lens.push(l);
            continue;
        }
        s.pos = save;
        // A color token extends to the next top-level space.
        let rest = s.rest();
        let end = if rest.contains('(') {
            rest.find(')').map_or(rest.len(), |i| i + 1)
        } else {
            rest.find(char::is_whitespace).unwrap_or(rest.len())
        };
        color = Some(match &rest[..end] {
            t if t.eq_ignore_ascii_case("currentcolor") => st.style.color,
            t => crate::color::parse_color(t)?,
        });
        s.pos += end;
    }
    if !(2..=3).contains(&lens.len()) {
        return None;
    }
    let sd = lens.get(2).map_or(0.0, |l| st.resolve(*l, Axis::Diag));
    Some(FilterKind::DropShadow {
        input: FilterInput::SourceGraphic,
        dx: st.resolve(lens[0], Axis::X),
        dy: st.resolve(lens[1], Axis::Y),
        std_dev_x: sd,
        std_dev_y: sd,
        color: color.unwrap_or(st.style.color),
    })
}

fn matrix(input: FilterInput, m: [f32; 20]) -> FilterKind {
    FilterKind::ColorMatrix { input, matrix: m }
}

fn transfer(input: FilterInput, funcs: [TransferFunc; 4]) -> FilterKind {
    FilterKind::ComponentTransfer { input, funcs }
}

#[rustfmt::skip]
fn saturate_matrix(s: f32) -> [f32; 20] {
    [
        0.213 + 0.787 * s, 0.715 - 0.715 * s, 0.072 - 0.072 * s, 0.0, 0.0,
        0.213 - 0.213 * s, 0.715 + 0.285 * s, 0.072 - 0.072 * s, 0.0, 0.0,
        0.213 - 0.213 * s, 0.715 - 0.715 * s, 0.072 + 0.928 * s, 0.0, 0.0,
        0.0, 0.0, 0.0, 1.0, 0.0,
    ]
}

#[rustfmt::skip]
fn hue_rotate_matrix(deg: f32) -> [f32; 20] {
    let (s, c) = deg.to_radians().sin_cos();
    [
        0.213 + c * 0.787 - s * 0.213, 0.715 - c * 0.715 - s * 0.715, 0.072 - c * 0.072 + s * 0.928, 0.0, 0.0,
        0.213 - c * 0.213 + s * 0.143, 0.715 + c * 0.285 + s * 0.140, 0.072 - c * 0.072 - s * 0.283, 0.0, 0.0,
        0.213 - c * 0.213 - s * 0.787, 0.715 - c * 0.715 + s * 0.715, 0.072 + c * 0.928 + s * 0.072, 0.0, 0.0,
        0.0, 0.0, 0.0, 1.0, 0.0,
    ]
}

#[rustfmt::skip]
const LUMINANCE_TO_ALPHA: [f32; 20] = [
    0.0, 0.0, 0.0, 0.0, 0.0,
    0.0, 0.0, 0.0, 0.0, 0.0,
    0.0, 0.0, 0.0, 0.0, 0.0,
    0.2125, 0.7154, 0.0721, 0.0, 0.0,
];

#[rustfmt::skip]
const IDENTITY_MATRIX: [f32; 20] = [
    1.0, 0.0, 0.0, 0.0, 0.0,
    0.0, 1.0, 0.0, 0.0, 0.0,
    0.0, 0.0, 1.0, 0.0, 0.0,
    0.0, 0.0, 0.0, 1.0, 0.0,
];

/// Unit context for primitive attribute values.
struct Units<'s> {
    obb: Option<Rect>,
    st: &'s State,
}

impl Units<'_> {
    /// A coordinate (`x`, `dx`, light position) along `axis`. `offset` adds
    /// the bbox origin for absolute positions.
    fn coord(&self, v: f32, axis: Axis, offset: bool) -> f32 {
        match self.obb {
            Some(b) => match axis {
                Axis::X => v * b.w + if offset { b.x } else { 0.0 },
                Axis::Y => v * b.h + if offset { b.y } else { 0.0 },
                Axis::Diag => v * ((b.w * b.w + b.h * b.h) / 2.0).sqrt(),
            },
            None => v,
        }
    }

    fn length_attr(&self, c: &Converter, node: NodeId, name: &str, axis: Axis) -> Option<f32> {
        let s = c.attr(node, name)?;
        match self.obb {
            Some(_) => obb_fraction(s).map(|v| self.coord(v, axis, false)),
            None => syntax::parse_length(s).map(|l| self.st.resolve(l, axis)),
        }
    }
}

fn number(c: &Converter, node: NodeId, name: &str, default: f32) -> f32 {
    c.attr(node, name)
        .and_then(syntax::parse_number)
        .unwrap_or(default)
}

/// `"a [b]"` number pairs (`stdDeviation`, `radius`, `baseFrequency`, `order`).
fn number_pair(c: &Converter, node: NodeId, name: &str) -> Option<(f32, f32)> {
    let v = syntax::parse_number_list(c.attr(node, name)?);
    match v.as_slice() {
        [a] => Some((*a, *a)),
        [a, b] => Some((*a, *b)),
        _ => None,
    }
}

fn url_filter(
    c: &mut Converter,
    node: NodeId,
    id: &str,
    st: &State,
    bbox: Option<Rect>,
) -> Option<Filter> {
    let chain = href_chain(c, node, &["filter"]);
    let valid_bbox = bbox.filter(Rect::is_valid);
    let obb_units = chain_attr(c, &chain, "filterUnits") != Some("userSpaceOnUse");
    let obb_prims = chain_attr(c, &chain, "primitiveUnits") == Some("objectBoundingBox");

    let region = if obb_units {
        let b = valid_bbox?;
        let f = |name: &str, d: f32| {
            chain_attr(c, &chain, name)
                .and_then(obb_fraction)
                .unwrap_or(d)
        };
        Rect::new(
            b.x + f("x", -0.1) * b.w,
            b.y + f("y", -0.1) * b.h,
            f("width", 1.2) * b.w,
            f("height", 1.2) * b.h,
        )
    } else {
        let pc = |n: f32| Length::new(n, Unit::Percent);
        let l = |name: &str, axis, d| {
            st.resolve(
                chain_attr(c, &chain, name)
                    .and_then(syntax::parse_length)
                    .unwrap_or(d),
                axis,
            )
        };
        Rect::new(
            l("x", Axis::X, pc(-10.0)),
            l("y", Axis::Y, pc(-10.0)),
            l("width", Axis::X, pc(120.0)),
            l("height", Axis::Y, pc(120.0)),
        )
    };
    if !region.is_valid() {
        return None;
    }
    let units = Units {
        obb: if obb_prims { Some(valid_bbox?) } else { None },
        st,
    };

    let Some(&content) = chain
        .iter()
        .find(|&&n| c.sd.doc.element_children(n).next().is_some())
    else {
        return Some(Filter {
            id: id.to_string(),
            region,
            primitives: Vec::new(),
        });
    };
    if c.stack.contains(&content) {
        return None;
    }
    c.stack.push(content);
    let mut prims: Vec<FilterPrimitive> = Vec::new();
    let kids: Vec<NodeId> = c.sd.doc.element_children(content).collect();
    for k in kids {
        let Some(name) = c.name(k) else { continue };
        if !name.starts_with("fe") {
            continue;
        }
        let style = c.sd.style_from_ancestors(k);
        let results: Vec<String> = prims.iter().map(|p| p.result.clone()).collect();
        let kind = primitive_kind(c, k, name, &style, &units, st, &results);
        let sub = |attr: &str, axis, base: f32, len: bool| {
            units
                .length_attr(c, k, attr, axis)
                .map(|v| {
                    if len || units.obb.is_none() {
                        v
                    } else {
                        // Positions in bbox units are offset by the bbox origin.
                        v + match axis {
                            Axis::X => units.obb.map_or(0.0, |b| b.x),
                            _ => units.obb.map_or(0.0, |b| b.y),
                        }
                    }
                })
                .unwrap_or(base)
        };
        let prim_region = Rect::new(
            sub("x", Axis::X, region.x, false),
            sub("y", Axis::Y, region.y, false),
            sub("width", Axis::X, region.w, true),
            sub("height", Axis::Y, region.h, true),
        );
        prims.push(FilterPrimitive {
            region: prim_region,
            linear_rgb: style.filters_linear_rgb,
            result: c.attr(k, "result").unwrap_or("").to_string(),
            kind,
        });
    }
    c.stack.pop();
    Some(Filter {
        id: id.to_string(),
        region,
        primitives: prims,
    })
}

fn parse_input(v: Option<&str>, results: &[String]) -> FilterInput {
    match v.map(str::trim) {
        None | Some("") => FilterInput::Previous,
        Some("SourceGraphic" | "BackgroundImage" | "FillPaint" | "StrokePaint") => {
            FilterInput::SourceGraphic
        }
        Some("SourceAlpha" | "BackgroundAlpha") => FilterInput::SourceAlpha,
        // A reference to an unknown (or later) result acts as if `in` were
        // omitted.
        Some(r) if results.iter().any(|x| x == r) => FilterInput::Reference(r.to_string()),
        Some(_) => FilterInput::Previous,
    }
}

fn flood(style: &Style, color: crate::style::ColorSpec, opacity: f32) -> Color {
    let c = color.resolve(style.color);
    Color::new(
        c.r,
        c.g,
        c.b,
        (c.a as f32 * opacity).round().clamp(0.0, 255.0) as u8,
    )
}

fn primitive_kind(
    c: &mut Converter,
    k: NodeId,
    name: &str,
    style: &Style,
    units: &Units,
    st: &State,
    results: &[String],
) -> FilterKind {
    let input = |attr: &str| parse_input(c.attr(k, attr), results);
    let std_dev = |c: &Converter| -> Option<(f32, f32)> {
        let (x, y) = number_pair(c, k, "stdDeviation").unwrap_or((0.0, 0.0));
        if x < 0.0 || y < 0.0 {
            return None;
        }
        Some((
            units.coord(x, Axis::X, false),
            units.coord(y, Axis::Y, false),
        ))
    };
    match name {
        "feGaussianBlur" => match std_dev(c) {
            Some((sx, sy)) => FilterKind::GaussianBlur {
                input: input("in"),
                std_dev_x: sx,
                std_dev_y: sy,
            },
            None => FilterKind::Empty,
        },
        "feOffset" => FilterKind::Offset {
            input: input("in"),
            dx: units.coord(number(c, k, "dx", 0.0), Axis::X, false),
            dy: units.coord(number(c, k, "dy", 0.0), Axis::Y, false),
        },
        "feFlood" => FilterKind::Flood {
            color: flood(style, style.flood_color, style.flood_opacity),
        },
        "feBlend" => FilterKind::Blend {
            input1: input("in"),
            input2: input("in2"),
            mode: c
                .attr(k, "mode")
                .and_then(crate::style::parse_blend_mode)
                .unwrap_or_default(),
        },
        "feComposite" => FilterKind::Composite {
            input1: input("in"),
            input2: input("in2"),
            op: match c.attr(k, "operator") {
                Some("in") => CompositeOp::In,
                Some("out") => CompositeOp::Out,
                Some("atop") => CompositeOp::Atop,
                Some("xor") => CompositeOp::Xor,
                Some("arithmetic") => CompositeOp::Arithmetic {
                    k1: number(c, k, "k1", 0.0),
                    k2: number(c, k, "k2", 0.0),
                    k3: number(c, k, "k3", 0.0),
                    k4: number(c, k, "k4", 0.0),
                },
                _ => CompositeOp::Over,
            },
        },
        "feMerge" => FilterKind::Merge {
            inputs: c
                .sd
                .doc
                .element_children(k)
                .filter(|&n| c.name(n) == Some("feMergeNode"))
                .map(|n| parse_input(c.attr(n, "in"), results))
                .collect(),
        },
        "feColorMatrix" => {
            let values = c.attr(k, "values").map(syntax::parse_number_list);
            let m = match c.attr(k, "type").unwrap_or("matrix") {
                "saturate" => match values.as_deref() {
                    Some([s]) => saturate_matrix(s.max(0.0)),
                    _ => saturate_matrix(1.0),
                },
                "hueRotate" => match values.as_deref() {
                    Some([d]) => hue_rotate_matrix(*d),
                    _ => hue_rotate_matrix(0.0),
                },
                "luminanceToAlpha" => LUMINANCE_TO_ALPHA,
                _ => match values {
                    Some(v) if v.len() == 20 => {
                        let mut m = [0.0; 20];
                        m.copy_from_slice(&v);
                        m
                    }
                    _ => IDENTITY_MATRIX,
                },
            };
            matrix(input("in"), m)
        }
        "feComponentTransfer" => {
            let mut funcs = [
                TransferFunc::Identity,
                TransferFunc::Identity,
                TransferFunc::Identity,
                TransferFunc::Identity,
            ];
            let kids: Vec<NodeId> = c.sd.doc.element_children(k).collect();
            for f in kids {
                let slot = match c.name(f) {
                    Some("feFuncR") => 0,
                    Some("feFuncG") => 1,
                    Some("feFuncB") => 2,
                    Some("feFuncA") => 3,
                    _ => continue,
                };
                funcs[slot] = transfer_func(c, f);
            }
            transfer(input("in"), funcs)
        }
        "feMorphology" => {
            let (rx, ry) = number_pair(c, k, "radius").unwrap_or((0.0, 0.0));
            if rx < 0.0 || ry < 0.0 {
                return FilterKind::Empty;
            }
            FilterKind::Morphology {
                input: input("in"),
                dilate: c.attr(k, "operator") == Some("dilate"),
                rx: units.coord(rx, Axis::X, false),
                ry: units.coord(ry, Axis::Y, false),
            }
        }
        "feTile" => FilterKind::Tile { input: input("in") },
        "feDropShadow" => {
            let (sx, sy) = match number_pair(c, k, "stdDeviation") {
                Some(p) if p.0 >= 0.0 && p.1 >= 0.0 => p,
                Some(_) => return FilterKind::Empty,
                None => (2.0, 2.0),
            };
            FilterKind::DropShadow {
                input: input("in"),
                dx: units.coord(number(c, k, "dx", 2.0), Axis::X, false),
                dy: units.coord(number(c, k, "dy", 2.0), Axis::Y, false),
                std_dev_x: units.coord(sx, Axis::X, false),
                std_dev_y: units.coord(sy, Axis::Y, false),
                color: flood(style, style.flood_color, style.flood_opacity),
            }
        }
        "feTurbulence" => {
            let (fx, fy) = number_pair(c, k, "baseFrequency").unwrap_or((0.0, 0.0));
            if fx < 0.0 || fy < 0.0 {
                return FilterKind::Empty;
            }
            FilterKind::Turbulence {
                base_frequency_x: fx,
                base_frequency_y: fy,
                num_octaves: number(c, k, "numOctaves", 1.0).max(0.0) as u32,
                seed: number(c, k, "seed", 0.0).trunc() as i32,
                stitch_tiles: c.attr(k, "stitchTiles") == Some("stitch"),
                fractal_noise: c.attr(k, "type") == Some("fractalNoise"),
            }
        }
        "feConvolveMatrix" => convolve(c, k, &input),
        "feDisplacementMap" => {
            let ch = |attr: &str| match c.attr(k, attr) {
                Some("R") => ColorChannel::R,
                Some("G") => ColorChannel::G,
                Some("B") => ColorChannel::B,
                _ => ColorChannel::A,
            };
            FilterKind::DisplacementMap {
                input1: input("in"),
                input2: input("in2"),
                scale: units.coord(number(c, k, "scale", 0.0), Axis::Diag, false),
                x_channel: ch("xChannelSelector"),
                y_channel: ch("yChannelSelector"),
            }
        }
        "feDiffuseLighting" | "feSpecularLighting" => {
            let Some(light) = light_source(c, k, units) else {
                return FilterKind::Empty;
            };
            let color = style.lighting_color.resolve(style.color);
            let surface_scale = number(c, k, "surfaceScale", 1.0);
            if name == "feDiffuseLighting" {
                FilterKind::DiffuseLighting {
                    input: input("in"),
                    surface_scale,
                    diffuse_constant: number(c, k, "diffuseConstant", 1.0),
                    color,
                    light,
                }
            } else {
                let exp = number(c, k, "specularExponent", 1.0);
                if !(1.0..=128.0).contains(&exp) {
                    return FilterKind::Empty;
                }
                FilterKind::SpecularLighting {
                    input: input("in"),
                    surface_scale,
                    specular_constant: number(c, k, "specularConstant", 1.0),
                    specular_exponent: exp,
                    color,
                    light,
                }
            }
        }
        "feImage" => fe_image(c, k, st),
        _ => FilterKind::Empty,
    }
}

fn transfer_func(c: &Converter, f: NodeId) -> TransferFunc {
    let table = || {
        c.attr(f, "tableValues")
            .map(syntax::parse_number_list)
            .unwrap_or_default()
    };
    match c.attr(f, "type") {
        Some("table") => {
            let t = table();
            if t.is_empty() {
                TransferFunc::Identity
            } else {
                TransferFunc::Table(t)
            }
        }
        Some("discrete") => {
            let t = table();
            if t.is_empty() {
                TransferFunc::Identity
            } else {
                TransferFunc::Discrete(t)
            }
        }
        Some("linear") => TransferFunc::Linear {
            slope: number(c, f, "slope", 1.0),
            intercept: number(c, f, "intercept", 0.0),
        },
        Some("gamma") => TransferFunc::Gamma {
            amplitude: number(c, f, "amplitude", 1.0),
            exponent: number(c, f, "exponent", 1.0),
            offset: number(c, f, "offset", 0.0),
        },
        _ => TransferFunc::Identity,
    }
}

fn convolve(c: &Converter, k: NodeId, input: &dyn Fn(&str) -> FilterInput) -> FilterKind {
    let (ox, oy) = number_pair(c, k, "order").unwrap_or((3.0, 3.0));
    if ox < 1.0 || oy < 1.0 || ox.fract() != 0.0 || oy.fract() != 0.0 {
        return FilterKind::Empty;
    }
    let (ox, oy) = (ox as u32, oy as u32);
    let kernel = c
        .attr(k, "kernelMatrix")
        .map(syntax::parse_number_list)
        .unwrap_or_default();
    if kernel.len() != (ox * oy) as usize {
        return FilterKind::Empty;
    }
    let sum: f32 = kernel.iter().sum();
    let divisor = match c.attr(k, "divisor").and_then(syntax::parse_number) {
        Some(d) if d != 0.0 => d,
        _ if sum != 0.0 => sum,
        _ => 1.0,
    };
    let target = |attr: &str, order: u32| -> Option<u32> {
        match c.attr(k, attr).and_then(syntax::parse_number) {
            None => Some(order / 2),
            Some(t) if t >= 0.0 && (t as u32) < order => Some(t as u32),
            Some(_) => None,
        }
    };
    let (Some(tx), Some(ty)) = (target("targetX", ox), target("targetY", oy)) else {
        return FilterKind::Empty;
    };
    FilterKind::ConvolveMatrix {
        input: input("in"),
        order_x: ox,
        order_y: oy,
        kernel,
        divisor,
        bias: number(c, k, "bias", 0.0),
        target_x: tx,
        target_y: ty,
        edge_mode: match c.attr(k, "edgeMode") {
            Some("none") => EdgeMode::None,
            Some("wrap") => EdgeMode::Wrap,
            _ => EdgeMode::Duplicate,
        },
        preserve_alpha: c.attr(k, "preserveAlpha") == Some("true"),
    }
}

fn light_source(c: &Converter, k: NodeId, units: &Units) -> Option<LightSource> {
    let l = c.sd.doc.element_children(k).find(|&n| {
        matches!(
            c.name(n),
            Some("feDistantLight" | "fePointLight" | "feSpotLight")
        )
    })?;
    let n = |name: &str, d: f32| number(c, l, name, d);
    let pos = |name: &str, axis| units.coord(n(name, 0.0), axis, !matches!(axis, Axis::Diag));
    Some(match c.name(l)? {
        "feDistantLight" => LightSource::Distant {
            azimuth: n("azimuth", 0.0),
            elevation: n("elevation", 0.0),
        },
        "fePointLight" => LightSource::Point {
            x: pos("x", Axis::X),
            y: pos("y", Axis::Y),
            z: pos("z", Axis::Diag),
        },
        _ => LightSource::Spot {
            x: pos("x", Axis::X),
            y: pos("y", Axis::Y),
            z: pos("z", Axis::Diag),
            points_at_x: pos("pointsAtX", Axis::X),
            points_at_y: pos("pointsAtY", Axis::Y),
            points_at_z: pos("pointsAtZ", Axis::Diag),
            specular_exponent: n("specularExponent", 1.0),
            limiting_cone_angle: c
                .attr(l, "limitingConeAngle")
                .and_then(syntax::parse_number),
        },
    })
}

/// `feImage`: an element reference (rendered like `use`) or a data URL image
/// (placed like `<image>` by the primitive's `x`/`y`/`width`/`height`).
fn fe_image(c: &mut Converter, k: NodeId, st: &State) -> FilterKind {
    let Some(href) = c.attr(k, "href") else {
        return FilterKind::Empty;
    };
    let mut root = Group::default();
    if let Some(id) = syntax::parse_iri(href) {
        let Some(target) = c.sd.by_id(id) else {
            return FilterKind::Empty;
        };
        if c.stack.contains(&target) {
            return FilterKind::Empty;
        }
        let parent_style = match c.sd.doc.nodes[target].parent {
            Some(p) => c.sd.style_from_ancestors(p),
            None => Style::root(),
        };
        let pst = State {
            style: parent_style,
            vp: st.vp,
            in_clip: false,
            context: None,
        };
        c.stack.push(target);
        c.convert_element(target, &pst, &mut root.children);
        c.stack.pop();
    } else {
        let ist = State {
            style: c.sd.style_from_ancestors(k),
            vp: st.vp,
            in_clip: false,
            context: None,
        };
        c.convert_image(k, &ist, &mut root.children);
    }
    if root.children.is_empty() {
        return FilterKind::Empty;
    }
    FilterKind::Image { root }
}
