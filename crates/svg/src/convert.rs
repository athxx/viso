//! DOM → normalized [`Tree`].
//!
//! Walks the styled document from the root `<svg>`, expanding `<use>`,
//! `<symbol>`, nested `<svg>` and `<switch>`, converting basic shapes to
//! paths, resolving units against the nearest viewport, and building paint
//! servers, clip paths, masks, markers and filters in the referencing
//! element's user space.

use std::sync::Arc;

use crate::color::Color;
use crate::geom::{PathData, Rect, Transform};
use crate::path_data::{self, CmdSpan};
use crate::style::{PaintLayer, PaintSpec, Style, StyledDoc};
use crate::syntax::{self, Length, Unit};
use crate::tree::*;
use crate::xml::NodeId;

/// Upper bound on converted elements; `<use>` chains can grow a small
/// document exponentially.
const ELEMENT_BUDGET: usize = 1_000_000;

#[derive(Debug, Clone, Copy)]
pub(crate) enum Axis {
    X,
    Y,
    Diag,
}

/// Inherited conversion state.
#[derive(Clone)]
pub(crate) struct State {
    pub style: Style,
    /// Nearest viewport size, the base for percentages.
    pub vp: (f32, f32),
    /// Inside a `<clipPath>`: only raw geometry matters.
    pub in_clip: bool,
    /// `context-fill` / `context-stroke` paints (inside markers).
    pub context: Option<(Option<Paint>, Option<Paint>)>,
}

impl State {
    pub(crate) fn resolve(&self, l: Length, axis: Axis) -> f32 {
        let base = match axis {
            Axis::X => self.vp.0,
            Axis::Y => self.vp.1,
            Axis::Diag => ((self.vp.0 * self.vp.0 + self.vp.1 * self.vp.1) / 2.0).sqrt(),
        };
        l.resolve(self.style.font_size, base)
    }
}

/// Outcome of resolving a clip/mask reference.
pub(crate) enum Ref<T> {
    /// No (usable) reference: render without the effect.
    None,
    Some(T),
    /// A broken reference that makes the element not render.
    Invalid,
}

pub(crate) struct Converter<'a> {
    pub sd: &'a StyledDoc,
    /// Elements being expanded (`use` targets, resources) for cycle detection.
    pub stack: Vec<NodeId>,
    budget: usize,
}

/// The transform mapping the unit square onto `r`.
pub(crate) fn bbox_transform(r: Rect) -> Transform {
    Transform::new(r.w, 0.0, 0.0, r.h, r.x, r.y)
}

/// Convert a styled document into a render tree.
pub(crate) fn convert_doc(sd: &StyledDoc) -> Result<Tree, ParseError> {
    let root = sd.doc.root;
    if sd.doc.element_name(root) != Some("svg") {
        return Err(ParseError::NotSvg);
    }
    let style = Style::compute(&Style::root(), sd, root);
    let view_box = sd
        .doc
        .attr(root, "viewBox")
        .and_then(syntax::parse_view_box)
        .filter(Rect::is_valid);
    let aspect = sd
        .doc
        .attr(root, "preserveAspectRatio")
        .map(syntax::parse_aspect)
        .unwrap_or_default();

    let len = |name: &str| sd.geometry(root, name).and_then(syntax::parse_length);
    let abs = |l: Option<Length>| {
        l.filter(|l| l.unit != Unit::Percent)
            .map(|l| l.resolve(style.font_size, 0.0))
    };
    let pct = |l: Option<Length>| l.filter(|l| l.unit == Unit::Percent).map(|l| l.n / 100.0);
    let (mut w, mut h) = (abs(len("width")), abs(len("height")));
    match (w, h, view_box) {
        (Some(w0), None, Some(vb)) => h = Some(w0 * vb.h / vb.w),
        (None, Some(h0), Some(vb)) => w = Some(h0 * vb.w / vb.h),
        _ => {}
    }
    let w = w.unwrap_or_else(|| view_box.map_or(100.0, |v| v.w) * pct(len("width")).unwrap_or(1.0));
    let h =
        h.unwrap_or_else(|| view_box.map_or(100.0, |v| v.h) * pct(len("height")).unwrap_or(1.0));
    if !(w > 0.0 && h > 0.0 && w.is_finite() && h.is_finite()) {
        return Err(ParseError::InvalidSize);
    }

    let vp = view_box.map_or((w, h), |v| (v.w, v.h));
    let st = State {
        style,
        vp,
        in_clip: false,
        context: None,
    };
    let vb_ts = view_box.map_or(Transform::IDENTITY, |vb| {
        syntax::view_box_transform(vb, aspect, w, h)
    });
    let mut c = Converter {
        sd,
        stack: Vec::new(),
        budget: ELEMENT_BUDGET,
    };
    let mut out = Vec::new();
    c.with_group(root, &st, Transform::IDENTITY, &mut out, |c, st, kids| {
        let mut inner = Group {
            transform: vb_ts,
            ..Group::default()
        };
        c.convert_children(root, st, &mut inner.children);
        push_group(kids, inner);
    });
    let root_group = match out.len() {
        1 if matches!(out[0], Node::Group(_)) => match out.pop() {
            Some(Node::Group(g)) => *g,
            _ => unreachable!(),
        },
        _ => Group {
            children: out,
            ..Group::default()
        },
    };
    Ok(Tree {
        size: (w, h),
        view_box: view_box.unwrap_or(Rect::new(0.0, 0.0, w, h)),
        root: root_group,
    })
}

/// Push `g`, or just its children when it is a plain identity group.
fn push_group(out: &mut Vec<Node>, mut g: Group) {
    if g.children.is_empty() && g.filters.is_empty() {
        return;
    }
    if g.transform.is_identity() && !g.needs_layer() {
        out.append(&mut g.children);
    } else {
        out.push(Node::Group(Box::new(g)));
    }
}

fn is_shape(name: &str) -> bool {
    matches!(
        name,
        "rect" | "circle" | "ellipse" | "line" | "polyline" | "polygon" | "path"
    )
}

impl<'a> Converter<'a> {
    pub(crate) fn attr(&self, node: NodeId, name: &str) -> Option<&'a str> {
        self.sd.doc.attr(node, name)
    }

    pub(crate) fn name(&self, node: NodeId) -> Option<&'a str> {
        self.sd.doc.element_name(node)
    }

    /// A length attribute/geometry property resolved in `st`.
    pub(crate) fn length(
        &self,
        node: NodeId,
        name: &str,
        st: &State,
        axis: Axis,
        default: Length,
    ) -> f32 {
        let l = self
            .sd
            .geometry(node, name)
            .and_then(syntax::parse_length)
            .unwrap_or(default);
        st.resolve(l, axis)
    }

    fn opt_length(&self, node: NodeId, name: &str, st: &State, axis: Axis) -> Option<f32> {
        let l = self
            .sd
            .geometry(node, name)
            .and_then(syntax::parse_length)?;
        Some(st.resolve(l, axis))
    }

    pub(crate) fn convert_children(&mut self, parent: NodeId, st: &State, out: &mut Vec<Node>) {
        let kids: Vec<NodeId> = self.sd.doc.element_children(parent).collect();
        for k in kids {
            self.convert_element(k, st, out);
        }
    }

    /// Conditional processing attributes (SVG 2 §5.8).
    fn conditions_pass(&self, node: NodeId) -> bool {
        if self.attr(node, "requiredExtensions").is_some() {
            // We implement no extensions; an empty list is false too.
            return false;
        }
        if let Some(langs) = self.attr(node, "systemLanguage") {
            return langs.split(',').map(str::trim).any(|l| {
                let l = l.to_ascii_lowercase();
                l == "en" || l.starts_with("en-")
            });
        }
        true
    }

    pub(crate) fn convert_element(&mut self, node: NodeId, parent: &State, out: &mut Vec<Node>) {
        let Some(name) = self.name(node) else { return };
        if parent.in_clip && !is_shape(name) && name != "use" {
            return;
        }
        if !matches!(name, "g" | "a" | "switch" | "svg" | "use" | "image") && !is_shape(name) {
            return;
        }
        if !self.conditions_pass(node) || self.budget == 0 {
            return;
        }
        self.budget -= 1;
        let style = Style::compute(&parent.style, self.sd, node);
        if !style.display {
            return;
        }
        let st = State {
            style,
            ..parent.clone()
        };
        match name {
            "g" | "a" => self.with_group(node, &st, Transform::IDENTITY, out, |c, st, kids| {
                c.convert_children(node, st, kids)
            }),
            "switch" => self.with_group(node, &st, Transform::IDENTITY, out, |c, st, kids| {
                let child =
                    c.sd.doc
                        .element_children(node)
                        .find(|&k| c.name(k).is_some() && c.conditions_pass(k));
                if let Some(k) = child {
                    c.convert_element(k, st, kids);
                }
            }),
            "svg" => self.convert_nested_svg(node, None, &st, out),
            "use" => self.convert_use(node, &st, out),
            "image" => self.convert_image(node, &st, out),
            _ => self.with_group(node, &st, Transform::IDENTITY, out, |c, st, kids| {
                c.convert_shape(node, name, st, kids)
            }),
        }
    }

    /// The element's own transform (`transform` + `transform-origin`).
    fn element_transform(&self, st: &State) -> Transform {
        let t = st.style.transform;
        match st.style.transform_origin {
            Some((ox, oy)) if !t.is_identity() => {
                let (ox, oy) = (st.resolve(ox, Axis::X), st.resolve(oy, Axis::Y));
                Transform::translate(ox, oy)
                    .pre_concat(&t)
                    .pre_concat(&Transform::translate(-ox, -oy))
            }
            _ => t,
        }
    }

    /// Build the element's group (transform + compositing effects) around the
    /// content produced by `build`, then push it (or its children, when the
    /// group turns out to be trivial) onto `out`.
    pub(crate) fn with_group(
        &mut self,
        node: NodeId,
        st: &State,
        extra: Transform,
        out: &mut Vec<Node>,
        build: impl FnOnce(&mut Self, &State, &mut Vec<Node>),
    ) {
        let transform = self.element_transform(st).pre_concat(&extra);
        if !transform.is_valid() {
            return;
        }
        let mut g = Group {
            id: self.attr(node, "id").unwrap_or("").to_string(),
            transform,
            ..Group::default()
        };
        build(self, st, &mut g.children);

        if !st.in_clip {
            g.opacity = st.style.opacity;
            g.blend_mode = st.style.blend_mode;
            g.isolate = st.style.isolate;
        }
        let needs_bbox = st.style.clip_path.is_some()
            || (!st.in_clip && (st.style.mask.is_some() || st.style.filter.is_some()));
        let bbox = if needs_bbox { g.children_bbox() } else { None };

        if let Some(id) = &st.style.clip_path {
            match self.resolve_clip(id, st, bbox) {
                Ref::None => {}
                Ref::Some(c) => g.clip_path = Some(c),
                Ref::Invalid => return,
            }
        }
        if !st.in_clip {
            if let Some(id) = &st.style.mask {
                match self.resolve_mask(id, st, bbox) {
                    Ref::None => {}
                    Ref::Some(m) => g.mask = Some(m),
                    Ref::Invalid => return,
                }
            }
            if let Some(f) = st.style.filter.clone() {
                match crate::filter::resolve_filters(self, &f, st, bbox) {
                    Ref::None => {}
                    Ref::Some(fs) => g.filters = fs,
                    Ref::Invalid => return,
                }
            }
        }
        if g.opacity <= 0.0 && g.filters.is_empty() {
            return;
        }
        fold_opacity(&mut g);
        push_group(out, g);
    }

    fn convert_nested_svg(
        &mut self,
        node: NodeId,
        use_node: Option<NodeId>,
        st: &State,
        out: &mut Vec<Node>,
    ) {
        let x = self.length(node, "x", st, Axis::X, Length::ZERO);
        let y = self.length(node, "y", st, Axis::Y, Length::ZERO);
        let full_w = Length::new(100.0, Unit::Percent);
        let dim = |c: &Self, name: &str, axis| {
            use_node
                .and_then(|u| c.opt_length(u, name, st, axis))
                .unwrap_or_else(|| c.length(node, name, st, axis, full_w))
        };
        let w = dim(self, "width", Axis::X);
        let h = dim(self, "height", Axis::Y);
        self.viewport_element(node, st, Rect::new(x, y, w, h), out);
    }

    /// `<svg>`/`<symbol>` content placed into viewport `rect`.
    fn viewport_element(&mut self, node: NodeId, st: &State, rect: Rect, out: &mut Vec<Node>) {
        if !rect.is_valid() {
            return;
        }
        let view_box = self.attr(node, "viewBox").and_then(syntax::parse_view_box);
        if view_box.is_some_and(|v| !v.is_valid()) {
            return;
        }
        let aspect = self
            .attr(node, "preserveAspectRatio")
            .map(syntax::parse_aspect)
            .unwrap_or_default();
        let inner_ts = Transform::translate(rect.x, rect.y).pre_concat(
            &view_box.map_or(Transform::IDENTITY, |vb| {
                syntax::view_box_transform(vb, aspect, rect.w, rect.h)
            }),
        );
        let mut inner_st = st.clone();
        inner_st.vp = view_box.map_or((rect.w, rect.h), |v| (v.w, v.h));
        let clip = !st.style.overflow_visible;
        self.with_group(node, st, Transform::IDENTITY, out, |c, _, kids| {
            let mut inner = Group {
                transform: inner_ts,
                ..Group::default()
            };
            c.convert_children(node, &inner_st, &mut inner.children);
            if clip && !inner.children.is_empty() {
                let viewport = Group {
                    clip_path: Some(Arc::new(rect_clip(rect))),
                    children: vec![Node::Group(Box::new(inner))],
                    ..Group::default()
                };
                kids.push(Node::Group(Box::new(viewport)));
            } else {
                push_group(kids, inner);
            }
        });
    }

    fn convert_use(&mut self, node: NodeId, st: &State, out: &mut Vec<Node>) {
        let Some(target) = self.sd.href(node) else {
            return;
        };
        if self.stack.contains(&target) || self.is_ancestor(target, node) {
            return;
        }
        let Some(tname) = self.name(target) else {
            return;
        };
        if st.in_clip && !is_shape(tname) {
            return;
        }
        let x = self.length(node, "x", st, Axis::X, Length::ZERO);
        let y = self.length(node, "y", st, Axis::Y, Length::ZERO);
        self.stack.push(target);
        self.with_group(
            node,
            st,
            Transform::translate(x, y),
            out,
            |c, st, kids| match tname {
                "symbol" => {
                    if !c.conditions_pass(target) {
                        return;
                    }
                    let style = Style::compute(&st.style, c.sd, target);
                    if !style.display {
                        return;
                    }
                    let sst = State {
                        style,
                        ..st.clone()
                    };
                    let full = Length::new(100.0, Unit::Percent);
                    let w = c
                        .opt_length(node, "width", st, Axis::X)
                        .unwrap_or_else(|| c.length(target, "width", st, Axis::X, full));
                    let h = c
                        .opt_length(node, "height", st, Axis::Y)
                        .unwrap_or_else(|| c.length(target, "height", st, Axis::Y, full));
                    c.viewport_element(target, &sst, Rect::new(0.0, 0.0, w, h), kids);
                }
                "svg" => {
                    let style = Style::compute(&st.style, c.sd, target);
                    if !style.display {
                        return;
                    }
                    let sst = State {
                        style,
                        ..st.clone()
                    };
                    c.convert_nested_svg(target, Some(node), &sst, kids);
                }
                _ => c.convert_element(target, st, kids),
            },
        );
        self.stack.pop();
    }

    fn is_ancestor(&self, a: NodeId, mut n: NodeId) -> bool {
        while let Some(p) = self.sd.doc.nodes[n].parent {
            if p == a {
                return true;
            }
            n = p;
        }
        a == n
    }

    pub(crate) fn convert_image(&mut self, node: NodeId, st: &State, out: &mut Vec<Node>) {
        if !st.style.visible {
            return;
        }
        let Some(href) = self.attr(node, "href") else {
            return;
        };
        let Some(data) = crate::data_url::decode(href) else {
            return;
        };
        let x = self.length(node, "x", st, Axis::X, Length::ZERO);
        let y = self.length(node, "y", st, Axis::Y, Length::ZERO);
        let aspect = self
            .attr(node, "preserveAspectRatio")
            .map(syntax::parse_aspect)
            .unwrap_or_default();

        if data.mime.contains("svg") || data.bytes.trim_ascii_start().starts_with(b"<") {
            // Nested SVG documents are fully converted and inlined.
            let Ok(tree) = Tree::from_data(&data.bytes) else {
                return;
            };
            let (iw, ih) = tree.size;
            let w = self.opt_length(node, "width", st, Axis::X).unwrap_or(iw);
            let h = self.opt_length(node, "height", st, Axis::Y).unwrap_or(ih);
            let rect = Rect::new(x, y, w, h);
            if !rect.is_valid() {
                return;
            }
            let ts = Transform::translate(x, y).pre_concat(&syntax::view_box_transform(
                Rect::new(0.0, 0.0, iw, ih),
                aspect,
                w,
                h,
            ));
            self.with_group(node, st, Transform::IDENTITY, out, |_, _, kids| {
                let inner = Group {
                    transform: ts,
                    children: vec![Node::Group(Box::new(tree.root))],
                    ..Group::default()
                };
                kids.push(Node::Group(Box::new(Group {
                    clip_path: Some(Arc::new(rect_clip(rect))),
                    children: vec![Node::Group(Box::new(inner))],
                    ..Group::default()
                })));
            });
            return;
        }

        let Some((kind, (iw, ih))) = crate::data_url::sniff_image(data.bytes) else {
            return;
        };
        let w = self
            .opt_length(node, "width", st, Axis::X)
            .unwrap_or(iw as f32);
        let h = self
            .opt_length(node, "height", st, Axis::Y)
            .unwrap_or(ih as f32);
        let viewport = Rect::new(x, y, w, h);
        if !viewport.is_valid() || iw == 0 || ih == 0 {
            return;
        }
        // Fit the intrinsic size into the viewport per preserveAspectRatio;
        // the renderer clips to `view_rect ∩ viewport`.
        let ts =
            syntax::view_box_transform(Rect::new(0.0, 0.0, iw as f32, ih as f32), aspect, w, h);
        let fitted = Rect::new(x + ts.e, y + ts.f, iw as f32 * ts.a, ih as f32 * ts.d);
        let pixelated = st.style.pixelated;
        let id = self.attr(node, "id").unwrap_or("").to_string();
        self.with_group(node, st, Transform::IDENTITY, out, |_, _, kids| {
            let img = Node::Image(Box::new(Image {
                id,
                view_rect: fitted,
                kind,
                pixelated,
            }));
            if aspect.slice {
                kids.push(Node::Group(Box::new(Group {
                    clip_path: Some(Arc::new(rect_clip(viewport))),
                    children: vec![img],
                    ..Group::default()
                })));
            } else {
                kids.push(img);
            }
        });
    }

    /// Outline of a basic shape or `<path>`, plus marker vertex spans.
    fn shape_outline(
        &self,
        node: NodeId,
        name: &str,
        st: &State,
    ) -> Option<(PathData, Vec<CmdSpan>)> {
        let z = Length::ZERO;
        let mut p = PathData::new();
        match name {
            "rect" => {
                let x = self.length(node, "x", st, Axis::X, z);
                let y = self.length(node, "y", st, Axis::Y, z);
                let w = self.length(node, "width", st, Axis::X, z);
                let h = self.length(node, "height", st, Axis::Y, z);
                if !(w > 0.0 && h > 0.0) {
                    return None;
                }
                let rx = self
                    .opt_length(node, "rx", st, Axis::X)
                    .filter(|v| *v >= 0.0);
                let ry = self
                    .opt_length(node, "ry", st, Axis::Y)
                    .filter(|v| *v >= 0.0);
                let (rx, ry) = match (rx, ry) {
                    (None, None) => (0.0, 0.0),
                    (Some(rx), None) => (rx, rx),
                    (None, Some(ry)) => (ry, ry),
                    (Some(rx), Some(ry)) => (rx, ry),
                };
                p.push_rounded_rect(Rect::new(x, y, w, h), rx.min(w / 2.0), ry.min(h / 2.0));
                let spans = path_data::simple_spans(&p);
                Some((p, spans))
            }
            "circle" => {
                let cx = self.length(node, "cx", st, Axis::X, z);
                let cy = self.length(node, "cy", st, Axis::Y, z);
                let r = self.length(node, "r", st, Axis::Diag, z);
                if r.is_nan() || r <= 0.0 {
                    return None;
                }
                p.push_ellipse(cx, cy, r, r);
                Some((p, Vec::new()))
            }
            "ellipse" => {
                let cx = self.length(node, "cx", st, Axis::X, z);
                let cy = self.length(node, "cy", st, Axis::Y, z);
                let rx = self
                    .opt_length(node, "rx", st, Axis::X)
                    .filter(|v| *v >= 0.0);
                let ry = self
                    .opt_length(node, "ry", st, Axis::Y)
                    .filter(|v| *v >= 0.0);
                let (rx, ry) = match (rx, ry) {
                    (Some(rx), Some(ry)) => (rx, ry),
                    (Some(r), None) | (None, Some(r)) => (r, r),
                    (None, None) => return None,
                };
                if !(rx > 0.0 && ry > 0.0) {
                    return None;
                }
                p.push_ellipse(cx, cy, rx, ry);
                Some((p, Vec::new()))
            }
            "line" => {
                let x1 = self.length(node, "x1", st, Axis::X, z);
                let y1 = self.length(node, "y1", st, Axis::Y, z);
                let x2 = self.length(node, "x2", st, Axis::X, z);
                let y2 = self.length(node, "y2", st, Axis::Y, z);
                p.move_to(x1, y1);
                p.line_to(x2, y2);
                let spans = path_data::simple_spans(&p);
                Some((p, spans))
            }
            "polyline" | "polygon" => {
                let pts = syntax::parse_number_list(self.attr(node, "points")?);
                if pts.len() < 4 {
                    return None;
                }
                for (i, &[x, y]) in pts.as_chunks::<2>().0.iter().enumerate() {
                    if i == 0 {
                        p.move_to(x, y);
                    } else {
                        p.line_to(x, y);
                    }
                }
                if name == "polygon" {
                    p.close();
                }
                let spans = path_data::simple_spans(&p);
                Some((p, spans))
            }
            "path" => {
                let d = self.sd.geometry(node, "d")?;
                let d = d.trim();
                let d = match d.strip_prefix("path(") {
                    Some(rest) => rest
                        .trim_end()
                        .strip_suffix(')')?
                        .trim()
                        .trim_matches(|c| c == '"' || c == '\''),
                    None => d,
                };
                let (p, spans) = path_data::parse_path(d);
                (p.len() >= 2).then_some((p, spans))
            }
            _ => None,
        }
    }

    fn convert_shape(&mut self, node: NodeId, name: &str, st: &State, out: &mut Vec<Node>) {
        let Some((data, spans)) = self.shape_outline(node, name, st) else {
            return;
        };
        let id = self.attr(node, "id").unwrap_or("").to_string();
        if st.in_clip {
            if st.style.visible {
                out.push(Node::Path(Box::new(Path {
                    id,
                    data: Arc::new(data),
                    fill: Some(Fill {
                        paint: Paint::Color(Color::BLACK),
                        opacity: 1.0,
                        rule: st.style.clip_rule,
                    }),
                    stroke: None,
                    anti_alias: st.style.anti_alias,
                })));
            }
            return;
        }
        let bbox = data.bounds();
        let fill = self.resolve_fill(st, bbox);
        let stroke = self.resolve_stroke(st, bbox);
        let data = Arc::new(data);
        let markers = if matches!(name, "path" | "line" | "polyline" | "polygon") {
            crate::marker::convert_markers(self, &data, &spans, st, &fill, &stroke)
        } else {
            Vec::new()
        };
        let visible = st.style.visible;
        let make = |fill: Option<Fill>, stroke: Option<Stroke>| {
            Node::Path(Box::new(Path {
                id: id.clone(),
                data: data.clone(),
                fill,
                stroke,
                anti_alias: st.style.anti_alias,
            }))
        };
        let order = st.style.paint_order;
        let mut markers = Some(markers);
        if order[0] == PaintLayer::Fill && order[1] == PaintLayer::Stroke {
            if visible && (fill.is_some() || stroke.is_some()) {
                out.push(make(fill, stroke));
            }
            out.extend(markers.take().into_iter().flatten());
            return;
        }
        for layer in order {
            match layer {
                PaintLayer::Fill if visible && fill.is_some() => out.push(make(fill.clone(), None)),
                PaintLayer::Stroke if visible && stroke.is_some() => {
                    out.push(make(None, stroke.clone()))
                }
                PaintLayer::Markers => out.extend(markers.take().into_iter().flatten()),
                _ => {}
            }
        }
    }

    pub(crate) fn resolve_fill(&mut self, st: &State, bbox: Option<Rect>) -> Option<Fill> {
        let paint = self.resolve_paint(&st.style.fill, st, bbox)?;
        Some(Fill {
            paint,
            opacity: st.style.fill_opacity,
            rule: st.style.fill_rule,
        })
    }

    pub(crate) fn resolve_stroke(&mut self, st: &State, bbox: Option<Rect>) -> Option<Stroke> {
        let width = st.resolve(st.style.stroke_width, Axis::Diag);
        if !(width > 0.0 && width.is_finite()) {
            return None;
        }
        let paint = self.resolve_paint(&st.style.stroke, st, bbox)?;
        let dasharray = st.style.dasharray.as_ref().and_then(|list| {
            let mut d: Vec<f32> = list.iter().map(|l| st.resolve(*l, Axis::Diag)).collect();
            if d.len() % 2 == 1 {
                d.extend_from_within(..);
            }
            let sum: f32 = d.iter().sum();
            (sum > 0.0 && sum.is_finite()).then_some(d)
        });
        Some(Stroke {
            paint,
            opacity: st.style.stroke_opacity,
            width,
            cap: st.style.linecap,
            join: st.style.linejoin,
            miter_limit: st.style.miterlimit,
            dashoffset: if dasharray.is_some() {
                st.resolve(st.style.dashoffset, Axis::Diag)
            } else {
                0.0
            },
            dasharray,
            non_scaling: st.style.non_scaling_stroke,
        })
    }

    fn resolve_paint(&mut self, spec: &PaintSpec, st: &State, bbox: Option<Rect>) -> Option<Paint> {
        match spec {
            PaintSpec::None => None,
            PaintSpec::Color(c) => Some(Paint::Color(*c)),
            PaintSpec::CurrentColor => Some(Paint::Color(st.style.color)),
            PaintSpec::ContextFill => st.context.as_ref().and_then(|c| c.0.clone()),
            PaintSpec::ContextStroke => st.context.as_ref().and_then(|c| c.1.clone()),
            PaintSpec::Url(id, fallback) => {
                let server = self.sd.by_id(id).and_then(|n| Some((n, self.name(n)?)));
                match server {
                    Some((n, "linearGradient" | "radialGradient")) => {
                        crate::paint::gradient(self, n, st, bbox)
                    }
                    Some((n, "pattern")) => crate::paint::pattern(self, n, st, bbox),
                    _ => fallback
                        .as_ref()
                        .and_then(|f| self.resolve_paint(f, st, bbox)),
                }
            }
        }
    }

    fn resolve_clip(&mut self, id: &str, st: &State, bbox: Option<Rect>) -> Ref<Arc<ClipPath>> {
        let Some(cp) = self.sd.by_id(id) else {
            return Ref::None;
        };
        if self.name(cp) != Some("clipPath") {
            return Ref::None;
        }
        if self.stack.contains(&cp) {
            return Ref::Invalid;
        }
        let cstyle = self.sd.style_from_ancestors(cp);
        let mut transform = cstyle.transform;
        if self.attr(cp, "clipPathUnits") == Some("objectBoundingBox") {
            let Some(b) = bbox.filter(Rect::is_valid) else {
                return Ref::Invalid;
            };
            transform = bbox_transform(b).pre_concat(&transform);
        }
        if !transform.is_valid() {
            return Ref::Invalid;
        }
        self.stack.push(cp);
        let cst = State {
            style: cstyle.clone(),
            vp: st.vp,
            in_clip: true,
            context: None,
        };
        let mut root = Group::default();
        self.convert_children(cp, &cst, &mut root.children);
        let nested = match &cstyle.clip_path {
            Some(nid) => match self.resolve_clip(nid, st, bbox) {
                Ref::None => None,
                Ref::Some(c) => Some(c),
                Ref::Invalid => {
                    self.stack.pop();
                    return Ref::Invalid;
                }
            },
            None => None,
        };
        self.stack.pop();
        Ref::Some(Arc::new(ClipPath {
            id: id.to_string(),
            transform,
            clip_path: nested,
            root,
        }))
    }

    fn resolve_mask(&mut self, id: &str, st: &State, bbox: Option<Rect>) -> Ref<Arc<Mask>> {
        let Some(m) = self.sd.by_id(id) else {
            return Ref::None;
        };
        if self.name(m) != Some("mask") {
            return Ref::None;
        }
        if self.stack.contains(&m) {
            return Ref::Invalid;
        }
        let mstyle = self.sd.style_from_ancestors(m);
        let obb_units = self.attr(m, "maskUnits") != Some("userSpaceOnUse");
        let obb_content = self.attr(m, "maskContentUnits") == Some("objectBoundingBox");
        let mut mst = State {
            style: mstyle.clone(),
            vp: st.vp,
            in_clip: false,
            context: None,
        };
        let rect = if obb_units {
            let Some(b) = bbox.filter(Rect::is_valid) else {
                return Ref::Invalid;
            };
            let f = |name: &str, d: f32| self.attr(m, name).and_then(obb_fraction).unwrap_or(d);
            Rect::new(
                b.x + f("x", -0.1) * b.w,
                b.y + f("y", -0.1) * b.h,
                f("width", 1.2) * b.w,
                f("height", 1.2) * b.h,
            )
        } else {
            let pc = |n: f32| Length::new(n, Unit::Percent);
            Rect::new(
                self.length(m, "x", &mst, Axis::X, pc(-10.0)),
                self.length(m, "y", &mst, Axis::Y, pc(-10.0)),
                self.length(m, "width", &mst, Axis::X, pc(120.0)),
                self.length(m, "height", &mst, Axis::Y, pc(120.0)),
            )
        };
        if !rect.is_valid() {
            return Ref::Invalid;
        }
        self.stack.push(m);
        let mut content = Group::default();
        if obb_content {
            let Some(b) = bbox.filter(Rect::is_valid) else {
                self.stack.pop();
                return Ref::Invalid;
            };
            content.transform = bbox_transform(b);
            mst.vp = (1.0, 1.0);
        }
        self.convert_children(m, &mst, &mut content.children);
        let nested = match &mstyle.mask {
            Some(nid) => match self.resolve_mask(nid, st, bbox) {
                Ref::None => None,
                Ref::Some(x) => Some(x),
                Ref::Invalid => {
                    self.stack.pop();
                    return Ref::Invalid;
                }
            },
            None => None,
        };
        self.stack.pop();
        let mut root = Group::default();
        push_group(&mut root.children, content);
        Ref::Some(Arc::new(Mask {
            id: id.to_string(),
            rect,
            kind: mstyle.mask_type,
            mask: nested,
            root,
        }))
    }
}

/// An `objectBoundingBox`-units value: a fraction (`0.5`) or percent (`50%`).
pub(crate) fn obb_fraction(s: &str) -> Option<f32> {
    let l = syntax::parse_length(s)?;
    Some(match l.unit {
        Unit::Percent => l.n / 100.0,
        _ => l.resolve(16.0, 1.0),
    })
}

/// A clip path consisting of one rectangle (viewport clipping).
pub(crate) fn rect_clip(r: Rect) -> ClipPath {
    ClipPath {
        id: String::new(),
        transform: Transform::IDENTITY,
        clip_path: None,
        root: Group {
            children: vec![Node::Path(Box::new(Path {
                id: String::new(),
                data: Arc::new(r.to_path()),
                fill: Some(Fill {
                    paint: Paint::Color(Color::BLACK),
                    opacity: 1.0,
                    rule: FillRule::NonZero,
                }),
                stroke: None,
                anti_alias: true,
            }))],
            ..Group::default()
        },
    }
}

/// Fold a plain group opacity into a lone single-paint path child: identical
/// output without an offscreen layer.
fn fold_opacity(g: &mut Group) {
    if g.opacity >= 1.0
        || g.clip_path.is_some()
        || g.mask.is_some()
        || !g.filters.is_empty()
        || g.blend_mode != BlendMode::Normal
        || g.isolate
        || g.children.len() != 1
    {
        return;
    }
    if let Node::Path(p) = &mut g.children[0] {
        match (&mut p.fill, &mut p.stroke) {
            (Some(f), None) => f.opacity *= g.opacity,
            (None, Some(s)) => s.opacity *= g.opacity,
            _ => return,
        }
        g.opacity = 1.0;
    }
}
