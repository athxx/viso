//! The cascade and computed style.
//!
//! [`StyledDoc`] resolves, per element, every declaration that applies to it
//! (presentation attributes, `<style>` rules, the `style` attribute and
//! `!important`) into one list in ascending priority. [`Style::compute`] then
//! folds that list over the parent's computed style: inherited properties start
//! from the parent, the rest from their initial value, and a later valid
//! declaration overrides an earlier one. Invalid values are skipped, which is
//! exactly CSS's "drop the invalid declaration" rule.
//!
//! Inheritance follows the *render* tree, not the XML tree: the converter
//! passes the parent style explicitly, so `<use>` content inherits from the
//! `<use>` element as SVG requires.

use std::collections::HashMap;

use crate::color::{Color, parse_color};
use crate::css::{self, Declaration};
use crate::geom::Transform;
use crate::syntax::{self, Length, Stream, Unit};
use crate::tree::{BlendMode, FillRule, LineCap, LineJoin, MaskType};
use crate::xml::{NodeId, XmlDoc, XmlKind};

/// Properties the renderer understands. Anything else in CSS is ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Prop {
    ClipPath,
    ClipRule,
    Color,
    ColorInterpolationFilters,
    Display,
    Fill,
    FillOpacity,
    FillRule,
    Filter,
    FloodColor,
    FloodOpacity,
    FontSize,
    ImageRendering,
    Isolation,
    LightingColor,
    MarkerStart,
    MarkerMid,
    MarkerEnd,
    Mask,
    MaskType,
    MixBlendMode,
    Opacity,
    Overflow,
    PaintOrder,
    ShapeRendering,
    StopColor,
    StopOpacity,
    Stroke,
    StrokeDasharray,
    StrokeDashoffset,
    StrokeLinecap,
    StrokeLinejoin,
    StrokeMiterlimit,
    StrokeOpacity,
    StrokeWidth,
    Transform,
    TransformOrigin,
    VectorEffect,
    Visibility,
    // Geometry properties (SVG 2): settable from CSS, read by the converter.
    X,
    Y,
    Width,
    Height,
    Cx,
    Cy,
    R,
    Rx,
    Ry,
    D,
}

fn prop_from_name(name: &str) -> Option<Prop> {
    Some(match name {
        "clip-path" => Prop::ClipPath,
        "clip-rule" => Prop::ClipRule,
        "color" => Prop::Color,
        "color-interpolation-filters" => Prop::ColorInterpolationFilters,
        "display" => Prop::Display,
        "fill" => Prop::Fill,
        "fill-opacity" => Prop::FillOpacity,
        "fill-rule" => Prop::FillRule,
        "filter" => Prop::Filter,
        "flood-color" => Prop::FloodColor,
        "flood-opacity" => Prop::FloodOpacity,
        "font-size" => Prop::FontSize,
        "image-rendering" => Prop::ImageRendering,
        "isolation" => Prop::Isolation,
        "lighting-color" => Prop::LightingColor,
        "marker-start" => Prop::MarkerStart,
        "marker-mid" => Prop::MarkerMid,
        "marker-end" => Prop::MarkerEnd,
        "mask" => Prop::Mask,
        "mask-type" => Prop::MaskType,
        "mix-blend-mode" => Prop::MixBlendMode,
        "opacity" => Prop::Opacity,
        "overflow" => Prop::Overflow,
        "paint-order" => Prop::PaintOrder,
        "shape-rendering" => Prop::ShapeRendering,
        "stop-color" => Prop::StopColor,
        "stop-opacity" => Prop::StopOpacity,
        "stroke" => Prop::Stroke,
        "stroke-dasharray" => Prop::StrokeDasharray,
        "stroke-dashoffset" => Prop::StrokeDashoffset,
        "stroke-linecap" => Prop::StrokeLinecap,
        "stroke-linejoin" => Prop::StrokeLinejoin,
        "stroke-miterlimit" => Prop::StrokeMiterlimit,
        "stroke-opacity" => Prop::StrokeOpacity,
        "stroke-width" => Prop::StrokeWidth,
        "transform" => Prop::Transform,
        "transform-origin" => Prop::TransformOrigin,
        "vector-effect" => Prop::VectorEffect,
        "visibility" => Prop::Visibility,
        _ => return None,
    })
}

/// Geometry properties are only settable from CSS; as attributes they are read
/// directly by the converter.
fn geometry_prop(name: &str) -> Option<Prop> {
    Some(match name {
        "x" => Prop::X,
        "y" => Prop::Y,
        "width" => Prop::Width,
        "height" => Prop::Height,
        "cx" => Prop::Cx,
        "cy" => Prop::Cy,
        "r" => Prop::R,
        "rx" => Prop::Rx,
        "ry" => Prop::Ry,
        "d" => Prop::D,
        _ => return None,
    })
}

/// Cascade order of a declaration: (priority tier, specificity, source order).
type CascadeKey = (u8, (u32, u32, u32), usize);

/// A parsed document with every element's applicable declarations resolved.
pub(crate) struct StyledDoc {
    pub doc: XmlDoc,
    /// Per node: `(property, value)` in ascending cascade priority.
    decls: Vec<Vec<(Prop, String)>>,
    ids: HashMap<String, NodeId>,
}

impl StyledDoc {
    pub(crate) fn new(doc: XmlDoc) -> StyledDoc {
        let mut rules = Vec::new();
        let mut order = 0usize;
        for id in 0..doc.nodes.len() {
            if doc.element_name(id) == Some("style") {
                let ty = doc.attr(id, "type").unwrap_or("text/css").trim();
                if ty.is_empty() || ty.eq_ignore_ascii_case("text/css") {
                    css::parse_stylesheet(&doc.text_content(id), &mut rules, &mut order);
                }
            }
        }

        let mut ids = HashMap::new();
        let mut decls = Vec::with_capacity(doc.nodes.len());
        for id in 0..doc.nodes.len() {
            if !matches!(doc.nodes[id].kind, XmlKind::Element { .. }) {
                decls.push(Vec::new());
                continue;
            }
            if let Some(v) = doc.attr(id, "id") {
                ids.entry(v.to_string()).or_insert(id);
            }
            if doc.element_name(id).is_none() {
                decls.push(Vec::new());
                continue;
            }
            let mut list: Vec<(CascadeKey, Prop, String)> = Vec::new();
            for (k, v) in doc.attrs(id) {
                if let Some(p) = prop_from_name(k) {
                    list.push(((0, (0, 0, 0), 0), p, v.clone()));
                }
            }
            let mut push = |tier: u8, spec, order, d: &Declaration| {
                let tier = if d.important { tier + 2 } else { tier };
                if d.name == "marker" {
                    for p in [Prop::MarkerStart, Prop::MarkerMid, Prop::MarkerEnd] {
                        list.push(((tier, spec, order), p, d.value.clone()));
                    }
                } else if let Some(p) = prop_from_name(&d.name).or_else(|| geometry_prop(&d.name)) {
                    list.push(((tier, spec, order), p, d.value.clone()));
                }
            };
            for rule in &rules {
                if rule.selector.matches(&doc, id) {
                    for d in rule.decls.iter() {
                        push(1, rule.specificity, rule.order, d);
                    }
                }
            }
            if let Some(style) = doc.attr(id, "style") {
                for (i, d) in css::parse_declarations(style).iter().enumerate() {
                    // The style attribute beats any selector at the same
                    // importance: tier 2 (normal) / 4 (important).
                    push(2, (0, 0, 0), i, d);
                }
            }
            // Stable: equal keys keep insertion (source) order.
            list.sort_by_key(|e| e.0);
            decls.push(list.into_iter().map(|(_, p, v)| (p, v)).collect());
        }
        StyledDoc { doc, decls, ids }
    }

    pub(crate) fn by_id(&self, id: &str) -> Option<NodeId> {
        self.ids.get(id).copied()
    }

    /// Resolve `href="#id"` on `node`.
    pub(crate) fn href(&self, node: NodeId) -> Option<NodeId> {
        self.by_id(syntax::parse_iri(self.doc.attr(node, "href")?)?)
    }

    /// A geometry value: the CSS-specified one if any, else the attribute.
    pub(crate) fn geometry(&self, node: NodeId, name: &str) -> Option<&str> {
        if let Some(p) = geometry_prop(name)
            && let Some((_, v)) = self.decls[node].iter().rev().find(|(q, _)| *q == p)
        {
            return Some(v.as_str());
        }
        self.doc.attr(node, name)
    }

    /// Computed style of `node` inheriting along its XML ancestors (used for
    /// resources, which inherit from where they are defined).
    pub(crate) fn style_from_ancestors(&self, node: NodeId) -> Style {
        let mut chain = vec![node];
        let mut cur = self.doc.nodes[node].parent;
        while let Some(p) = cur {
            chain.push(p);
            cur = self.doc.nodes[p].parent;
        }
        let mut style = Style::root();
        for &n in chain.iter().rev() {
            style = Style::compute(&style, self, n);
        }
        style
    }
}

/// A `fill`/`stroke` value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PaintSpec {
    None,
    Color(Color),
    CurrentColor,
    /// `url(#id)` with an optional fallback (`none`, a color, `currentColor`).
    Url(String, Option<Box<PaintSpec>>),
    ContextFill,
    ContextStroke,
}

fn parse_paint(s: &str) -> Option<PaintSpec> {
    let s = s.trim();
    if s.starts_with("url(") {
        let (id, rest) = syntax::parse_func_iri(s)?;
        let fallback = if rest.is_empty() {
            None
        } else {
            Some(Box::new(parse_simple_paint(rest)?))
        };
        return Some(PaintSpec::Url(id.to_string(), fallback));
    }
    parse_simple_paint(s)
}

fn parse_simple_paint(s: &str) -> Option<PaintSpec> {
    let s = s.trim();
    Some(match s.to_ascii_lowercase().as_str() {
        "none" => PaintSpec::None,
        "currentcolor" => PaintSpec::CurrentColor,
        "context-fill" => PaintSpec::ContextFill,
        "context-stroke" => PaintSpec::ContextStroke,
        _ => PaintSpec::Color(parse_color_value(s)?),
    })
}

/// A color, tolerating a trailing SVG 1.1 `icc-color(...)` specification.
fn parse_color_value(s: &str) -> Option<Color> {
    let s = match s.find("icc-color(") {
        Some(i) => s[..i].trim(),
        None => s,
    };
    parse_color(s)
}

/// A color property value that may be `currentColor`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ColorSpec {
    Color(Color),
    CurrentColor,
}

impl ColorSpec {
    pub(crate) fn resolve(self, current: Color) -> Color {
        match self {
            ColorSpec::Color(c) => c,
            ColorSpec::CurrentColor => current,
        }
    }
}

fn parse_color_spec(s: &str) -> Option<ColorSpec> {
    if s.trim().eq_ignore_ascii_case("currentcolor") {
        Some(ColorSpec::CurrentColor)
    } else {
        parse_color_value(s).map(ColorSpec::Color)
    }
}

/// Paint order layers, in painting order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaintLayer {
    Fill,
    Stroke,
    Markers,
}

fn parse_paint_order(s: &str) -> Option<[PaintLayer; 3]> {
    let s = s.trim();
    if s == "normal" {
        return Some([PaintLayer::Fill, PaintLayer::Stroke, PaintLayer::Markers]);
    }
    let mut out: Vec<PaintLayer> = Vec::with_capacity(3);
    for w in s.split_ascii_whitespace() {
        let l = match w {
            "fill" => PaintLayer::Fill,
            "stroke" => PaintLayer::Stroke,
            "markers" => PaintLayer::Markers,
            _ => return None,
        };
        if out.contains(&l) {
            return None;
        }
        out.push(l);
    }
    if out.is_empty() {
        return None;
    }
    for l in [PaintLayer::Fill, PaintLayer::Stroke, PaintLayer::Markers] {
        if !out.contains(&l) {
            out.push(l);
        }
    }
    Some([out[0], out[1], out[2]])
}

/// A reference property (`clip-path`, `mask`, `marker-*`): `none` or an id.
fn parse_ref(s: &str) -> Option<Option<String>> {
    let s = s.trim();
    if s == "none" {
        return Some(None);
    }
    syntax::parse_func_iri(s).map(|(id, _)| Some(id.to_string()))
}

/// Computed style: the subset of CSS the converter consumes.
#[derive(Debug, Clone)]
pub(crate) struct Style {
    // Inherited.
    pub fill: PaintSpec,
    pub fill_opacity: f32,
    pub fill_rule: FillRule,
    pub stroke: PaintSpec,
    pub stroke_width: Length,
    pub stroke_opacity: f32,
    pub linecap: LineCap,
    pub linejoin: LineJoin,
    pub miterlimit: f32,
    pub dasharray: Option<Vec<Length>>,
    pub dashoffset: Length,
    pub color: Color,
    /// Computed font size in px (for `em`/`ex`).
    pub font_size: f32,
    pub visible: bool,
    pub marker_start: Option<String>,
    pub marker_mid: Option<String>,
    pub marker_end: Option<String>,
    pub clip_rule: FillRule,
    pub paint_order: [PaintLayer; 3],
    pub anti_alias: bool,
    pub pixelated: bool,
    pub filters_linear_rgb: bool,
    // Not inherited.
    pub display: bool,
    pub opacity: f32,
    pub clip_path: Option<String>,
    pub mask: Option<String>,
    /// Raw `filter` value (not `none`), parsed by the filter converter.
    pub filter: Option<String>,
    pub stop_color: ColorSpec,
    pub stop_opacity: f32,
    pub flood_color: ColorSpec,
    pub flood_opacity: f32,
    pub lighting_color: ColorSpec,
    pub blend_mode: BlendMode,
    pub isolate: bool,
    /// `overflow` is `visible`/`auto` (no viewport clip).
    pub overflow_visible: bool,
    pub non_scaling_stroke: bool,
    pub mask_type: MaskType,
    pub transform: Transform,
    pub transform_origin: Option<(Length, Length)>,
}

impl Style {
    /// Initial values (the parent of the root element).
    pub(crate) fn root() -> Style {
        Style {
            fill: PaintSpec::Color(Color::BLACK),
            fill_opacity: 1.0,
            fill_rule: FillRule::NonZero,
            stroke: PaintSpec::None,
            stroke_width: Length::new(1.0, Unit::None),
            stroke_opacity: 1.0,
            linecap: LineCap::Butt,
            linejoin: LineJoin::Miter,
            miterlimit: 4.0,
            dasharray: None,
            dashoffset: Length::ZERO,
            color: Color::BLACK,
            font_size: 16.0,
            visible: true,
            marker_start: None,
            marker_mid: None,
            marker_end: None,
            clip_rule: FillRule::NonZero,
            paint_order: [PaintLayer::Fill, PaintLayer::Stroke, PaintLayer::Markers],
            anti_alias: true,
            pixelated: false,
            filters_linear_rgb: true,
            display: true,
            opacity: 1.0,
            clip_path: None,
            mask: None,
            filter: None,
            stop_color: ColorSpec::Color(Color::BLACK),
            stop_opacity: 1.0,
            flood_color: ColorSpec::Color(Color::BLACK),
            flood_opacity: 1.0,
            lighting_color: ColorSpec::Color(Color::WHITE),
            blend_mode: BlendMode::Normal,
            isolate: false,
            overflow_visible: false,
            non_scaling_stroke: false,
            mask_type: MaskType::Luminance,
            transform: Transform::IDENTITY,
            transform_origin: None,
        }
    }

    /// Compute `node`'s style from its parent's.
    pub(crate) fn compute(parent: &Style, sd: &StyledDoc, node: NodeId) -> Style {
        let init = Style::root();
        let mut s = parent.clone();
        // Reset non-inherited properties.
        s.display = init.display;
        s.opacity = init.opacity;
        s.clip_path = None;
        s.mask = None;
        s.filter = None;
        s.stop_color = init.stop_color;
        s.stop_opacity = init.stop_opacity;
        s.flood_color = init.flood_color;
        s.flood_opacity = init.flood_opacity;
        s.lighting_color = init.lighting_color;
        s.blend_mode = init.blend_mode;
        s.isolate = init.isolate;
        s.overflow_visible = init.overflow_visible;
        s.non_scaling_stroke = init.non_scaling_stroke;
        s.mask_type = init.mask_type;
        s.transform = Transform::IDENTITY;
        s.transform_origin = None;

        // `color` and `font-size` first: other values may depend on them.
        for (p, v) in &sd.decls[node] {
            match p {
                Prop::Color => {
                    if v.trim() == "inherit" {
                        s.color = parent.color;
                    } else if let Some(c) = parse_color_spec(v) {
                        s.color = c.resolve(parent.color);
                    }
                }
                Prop::FontSize => {
                    if let Some(fs) = parse_font_size(v, parent.font_size) {
                        s.font_size = fs;
                    }
                }
                _ => {}
            }
        }
        for (p, v) in &sd.decls[node] {
            if v.trim() == "inherit" {
                s.inherit(*p, parent);
            } else {
                s.apply(*p, v);
            }
        }
        s
    }

    fn inherit(&mut self, p: Prop, parent: &Style) {
        match p {
            Prop::Display => self.display = parent.display,
            Prop::Opacity => self.opacity = parent.opacity,
            Prop::ClipPath => self.clip_path = parent.clip_path.clone(),
            Prop::Mask => self.mask = parent.mask.clone(),
            Prop::Filter => self.filter = parent.filter.clone(),
            Prop::StopColor => self.stop_color = parent.stop_color,
            Prop::StopOpacity => self.stop_opacity = parent.stop_opacity,
            Prop::FloodColor => self.flood_color = parent.flood_color,
            Prop::FloodOpacity => self.flood_opacity = parent.flood_opacity,
            Prop::LightingColor => self.lighting_color = parent.lighting_color,
            Prop::MixBlendMode => self.blend_mode = parent.blend_mode,
            Prop::Isolation => self.isolate = parent.isolate,
            Prop::Overflow => self.overflow_visible = parent.overflow_visible,
            Prop::VectorEffect => self.non_scaling_stroke = parent.non_scaling_stroke,
            Prop::MaskType => self.mask_type = parent.mask_type,
            Prop::Transform => self.transform = parent.transform,
            Prop::TransformOrigin => self.transform_origin = parent.transform_origin,
            // Inherited properties already hold the parent's value.
            _ => {}
        }
    }

    fn apply(&mut self, p: Prop, v: &str) {
        let t = v.trim();
        match p {
            Prop::Fill => {
                if let Some(x) = parse_paint(t) {
                    self.fill = x;
                }
            }
            Prop::Stroke => {
                if let Some(x) = parse_paint(t) {
                    self.stroke = x;
                }
            }
            Prop::FillOpacity => set(&mut self.fill_opacity, syntax::parse_opacity(t)),
            Prop::StrokeOpacity => set(&mut self.stroke_opacity, syntax::parse_opacity(t)),
            Prop::Opacity => set(&mut self.opacity, syntax::parse_opacity(t)),
            Prop::StopOpacity => set(&mut self.stop_opacity, syntax::parse_opacity(t)),
            Prop::FloodOpacity => set(&mut self.flood_opacity, syntax::parse_opacity(t)),
            Prop::FillRule => set(&mut self.fill_rule, parse_fill_rule(t)),
            Prop::ClipRule => set(&mut self.clip_rule, parse_fill_rule(t)),
            Prop::StrokeWidth => set(
                &mut self.stroke_width,
                syntax::parse_length(t).filter(|l| l.n >= 0.0),
            ),
            Prop::StrokeLinecap => set(
                &mut self.linecap,
                match t {
                    "butt" => Some(LineCap::Butt),
                    "round" => Some(LineCap::Round),
                    "square" => Some(LineCap::Square),
                    _ => None,
                },
            ),
            Prop::StrokeLinejoin => set(
                &mut self.linejoin,
                match t {
                    "miter" => Some(LineJoin::Miter),
                    "miter-clip" => Some(LineJoin::MiterClip),
                    "round" => Some(LineJoin::Round),
                    "bevel" => Some(LineJoin::Bevel),
                    // SVG 2 `arcs` falls back to miter in every browser.
                    "arcs" => Some(LineJoin::Miter),
                    _ => None,
                },
            ),
            Prop::StrokeMiterlimit => set(
                &mut self.miterlimit,
                syntax::parse_number(t).filter(|&m| m >= 1.0),
            ),
            Prop::StrokeDasharray => {
                if t == "none" {
                    self.dasharray = None;
                } else if let Some(list) = syntax::parse_length_list(t) {
                    // A negative entry invalidates the whole value (renders solid).
                    self.dasharray = if list.iter().any(|l| l.n < 0.0) || list.is_empty() {
                        None
                    } else {
                        Some(list)
                    };
                }
            }
            Prop::StrokeDashoffset => set(&mut self.dashoffset, syntax::parse_length(t)),
            Prop::Visibility => set(
                &mut self.visible,
                match t {
                    "visible" => Some(true),
                    "hidden" | "collapse" => Some(false),
                    _ => None,
                },
            ),
            Prop::Display => self.display = t != "none",
            Prop::MarkerStart => set(&mut self.marker_start, parse_ref(t)),
            Prop::MarkerMid => set(&mut self.marker_mid, parse_ref(t)),
            Prop::MarkerEnd => set(&mut self.marker_end, parse_ref(t)),
            Prop::ClipPath => set(&mut self.clip_path, parse_ref(t)),
            Prop::Mask => set(&mut self.mask, parse_ref(t)),
            Prop::Filter => self.filter = (t != "none" && !t.is_empty()).then(|| t.to_string()),
            Prop::PaintOrder => set(&mut self.paint_order, parse_paint_order(t)),
            Prop::ShapeRendering => set(
                &mut self.anti_alias,
                match t {
                    "auto" | "geometricPrecision" => Some(true),
                    "crispEdges" | "optimizeSpeed" => Some(false),
                    _ => None,
                },
            ),
            Prop::ImageRendering => set(
                &mut self.pixelated,
                match t {
                    "auto" | "optimizeQuality" | "smooth" | "high-quality" => Some(false),
                    "optimizeSpeed" | "pixelated" | "crisp-edges" => Some(true),
                    _ => None,
                },
            ),
            Prop::ColorInterpolationFilters => set(
                &mut self.filters_linear_rgb,
                match t {
                    "auto" | "linearRGB" => Some(true),
                    "sRGB" => Some(false),
                    _ => None,
                },
            ),
            Prop::StopColor => set(&mut self.stop_color, parse_color_spec(t)),
            Prop::FloodColor => set(&mut self.flood_color, parse_color_spec(t)),
            Prop::LightingColor => set(&mut self.lighting_color, parse_color_spec(t)),
            Prop::MixBlendMode => set(&mut self.blend_mode, parse_blend_mode(t)),
            Prop::Isolation => set(
                &mut self.isolate,
                match t {
                    "auto" => Some(false),
                    "isolate" => Some(true),
                    _ => None,
                },
            ),
            Prop::Overflow => set(
                &mut self.overflow_visible,
                match t {
                    "visible" | "auto" => Some(true),
                    "hidden" | "scroll" | "clip" => Some(false),
                    _ => None,
                },
            ),
            Prop::VectorEffect => set(
                &mut self.non_scaling_stroke,
                match t {
                    "none" => Some(false),
                    "non-scaling-stroke" => Some(true),
                    _ => None,
                },
            ),
            Prop::MaskType => set(
                &mut self.mask_type,
                match t {
                    "luminance" => Some(MaskType::Luminance),
                    "alpha" => Some(MaskType::Alpha),
                    _ => None,
                },
            ),
            Prop::Transform => set(&mut self.transform, syntax::parse_transform(t)),
            Prop::TransformOrigin => set(
                &mut self.transform_origin,
                parse_transform_origin(t).map(Some),
            ),
            // Handled in the first pass / read directly by the converter.
            Prop::Color | Prop::FontSize => {}
            Prop::X
            | Prop::Y
            | Prop::Width
            | Prop::Height
            | Prop::Cx
            | Prop::Cy
            | Prop::R
            | Prop::Rx
            | Prop::Ry
            | Prop::D => {}
        }
    }
}

#[inline]
fn set<T>(dst: &mut T, v: Option<T>) {
    if let Some(v) = v {
        *dst = v;
    }
}

fn parse_fill_rule(s: &str) -> Option<FillRule> {
    match s {
        "nonzero" => Some(FillRule::NonZero),
        "evenodd" => Some(FillRule::EvenOdd),
        _ => None,
    }
}

pub(crate) fn parse_blend_mode(s: &str) -> Option<BlendMode> {
    Some(match s {
        "normal" => BlendMode::Normal,
        "multiply" => BlendMode::Multiply,
        "screen" => BlendMode::Screen,
        "overlay" => BlendMode::Overlay,
        "darken" => BlendMode::Darken,
        "lighten" => BlendMode::Lighten,
        "color-dodge" => BlendMode::ColorDodge,
        "color-burn" => BlendMode::ColorBurn,
        "hard-light" => BlendMode::HardLight,
        "soft-light" => BlendMode::SoftLight,
        "difference" => BlendMode::Difference,
        "exclusion" => BlendMode::Exclusion,
        "hue" => BlendMode::Hue,
        "saturation" => BlendMode::Saturation,
        "color" => BlendMode::Color,
        "luminosity" => BlendMode::Luminosity,
        _ => return None,
    })
}

fn parse_font_size(s: &str, parent: f32) -> Option<f32> {
    let s = s.trim();
    let kw = match s {
        "xx-small" => Some(9.0),
        "x-small" => Some(10.0),
        "small" => Some(13.0),
        "medium" => Some(16.0),
        "large" => Some(18.0),
        "x-large" => Some(24.0),
        "xx-large" => Some(32.0),
        "xxx-large" => Some(48.0),
        "smaller" => Some(parent / 1.2),
        "larger" => Some(parent * 1.2),
        _ => None,
    };
    if kw.is_some() {
        return kw;
    }
    let l = syntax::parse_length(s)?;
    if l.n < 0.0 {
        return None;
    }
    Some(match l.unit {
        Unit::Percent => parent * l.n / 100.0,
        Unit::Em => parent * l.n,
        Unit::Ex => parent * l.n / 2.0,
        _ => l.resolve(parent, parent),
    })
}

/// `transform-origin: <x> [<y>]` with keywords; percentages stay unresolved
/// (they are relative to the viewport for SVG's default `view-box` box).
fn parse_transform_origin(s: &str) -> Option<(Length, Length)> {
    let pct = |n: f32| Length::new(n, Unit::Percent);
    let mut xs: Option<Length> = None;
    let mut ys: Option<Length> = None;
    let mut pending_center = 0;
    let mut st = Stream::new(s);
    let mut words = 0;
    loop {
        st.skip_ws();
        if st.at_end() {
            break;
        }
        words += 1;
        if words > 3 {
            return None;
        }
        if st.consume_ident("left") {
            xs = Some(pct(0.0));
        } else if st.consume_ident("right") {
            xs = Some(pct(100.0));
        } else if st.consume_ident("top") {
            ys = Some(pct(0.0));
        } else if st.consume_ident("bottom") {
            ys = Some(pct(100.0));
        } else if st.consume_ident("center") {
            pending_center += 1;
        } else {
            let l = st.parse_length()?;
            if xs.is_none() && words == 1 {
                xs = Some(l);
            } else if ys.is_none() {
                ys = Some(l);
            } else {
                // A third value is the z offset: irrelevant in 2D.
            }
        }
    }
    for _ in 0..pending_center {
        if xs.is_none() {
            xs = Some(pct(50.0));
        } else if ys.is_none() {
            ys = Some(pct(50.0));
        }
    }
    Some((xs.unwrap_or(pct(50.0)), ys.unwrap_or(pct(50.0))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml;

    fn styled(src: &str) -> StyledDoc {
        StyledDoc::new(xml::parse(src).unwrap())
    }

    #[test]
    fn cascade_priority_order() {
        let sd = styled(
            r#"<svg xmlns="http://www.w3.org/2000/svg">
            <style>rect { fill: blue } #a { fill: green } .b { stroke: red !important }</style>
            <rect id="a" class="b" fill="yellow" stroke="black" style="stroke: lime"/>
            <rect fill="yellow"/>
            </svg>"#,
        );
        let root = Style::compute(&Style::root(), &sd, sd.doc.root);
        let kids: Vec<_> = sd.doc.element_children(sd.doc.root).collect();
        let a = Style::compute(&root, &sd, kids[1]);
        assert_eq!(a.fill, PaintSpec::Color(Color::rgb(0, 128, 0)));
        assert_eq!(a.stroke, PaintSpec::Color(Color::rgb(255, 0, 0)));
        let b = Style::compute(&root, &sd, kids[2]);
        assert_eq!(b.fill, PaintSpec::Color(Color::rgb(0, 0, 255)));
    }

    #[test]
    fn inheritance_and_invalid_values() {
        let sd = styled(
            r#"<svg xmlns="http://www.w3.org/2000/svg" fill="red" opacity="0.5" color="blue">
            <g stroke="currentColor" stroke-width="-1" fill="bogus"><rect stroke-width="inherit"/></g></svg>"#,
        );
        let root = Style::compute(&Style::root(), &sd, sd.doc.root);
        let g_id = sd.doc.element_children(sd.doc.root).next().unwrap();
        let g = Style::compute(&root, &sd, g_id);
        assert_eq!(
            g.fill,
            PaintSpec::Color(Color::rgb(255, 0, 0)),
            "invalid fill ignored"
        );
        assert_eq!(g.opacity, 1.0, "opacity not inherited");
        assert_eq!(g.stroke, PaintSpec::CurrentColor);
        assert_eq!(g.stroke_width.n, 1.0, "negative width invalid");
        let r = Style::compute(&g, &sd, sd.doc.element_children(g_id).next().unwrap());
        assert_eq!(r.color, Color::rgb(0, 0, 255));
    }

    #[test]
    fn paint_order_and_urls() {
        assert_eq!(
            parse_paint_order("stroke"),
            Some([PaintLayer::Stroke, PaintLayer::Fill, PaintLayer::Markers])
        );
        assert_eq!(
            parse_paint("url(#g) none"),
            Some(PaintSpec::Url("g".into(), Some(Box::new(PaintSpec::None))))
        );
    }
}
