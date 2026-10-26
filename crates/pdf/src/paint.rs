//! One page's draws lowered to a content stream and the objects it uses.

use std::collections::BTreeSet;
use std::io::Write;

use viso_math::{Affine2, Vec2, linear_to_srgb, srgb_to_linear};
use viso_render::color_effect::{ColorOp, fuse};
use viso_render::gradient_lut::LutKey;
use viso_render::{
    AnalyticLine, Blend, Border, Corners, GradientKind, LayerClip, LineCap, LineJoin, Mesh, Path,
    PathCmd, Primitive, Rect, Rgba, Stroke, StrokeAlign,
};

use crate::content::{Content, inset, shrink};
use crate::object::{Ref, num};
use crate::shading::{self, Geometry};
use crate::{Document, Draw, Part, RASTER_MAX, Raster, Source, TextRun, image};

/// What a graphics state sets: fill and stroke alpha in 255ths, the blend
/// mode's index in [`BLENDS`] and a soft mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct StateKey {
    fill: u8,
    stroke: u8,
    blend: u8,
    mask: Option<Ref>,
}

/// The blend modes PDF has, by index; `Normal` first.
const BLENDS: [&str; 16] = [
    "Normal",
    "Multiply",
    "Screen",
    "Overlay",
    "Darken",
    "Lighten",
    "ColorDodge",
    "ColorBurn",
    "HardLight",
    "SoftLight",
    "Difference",
    "Exclusion",
    "Hue",
    "Saturation",
    "Color",
    "Luminosity",
];

/// The PDF blend mode closest to `blend`, by index in [`BLENDS`]; the
/// compositing operators PDF lacks draw normally.
fn blend_index(blend: Blend) -> u8 {
    match blend {
        Blend::Multiply => 1,
        Blend::Screen => 2,
        Blend::Overlay => 3,
        Blend::Darken => 4,
        Blend::Lighten => 5,
        Blend::ColorDodge => 6,
        Blend::ColorBurn => 7,
        Blend::HardLight => 8,
        Blend::SoftLight => 9,
        Blend::Difference => 10,
        Blend::Exclusion => 11,
        Blend::Hue => 12,
        Blend::Saturation => 13,
        Blend::Color => 14,
        Blend::Luminosity => 15,
        _ => 0,
    }
}

/// The objects a page's streams name, by number.
#[derive(Debug, Default)]
pub(crate) struct Used {
    fonts: BTreeSet<u32>,
    xobjects: BTreeSet<u32>,
    states: BTreeSet<u32>,
    shadings: BTreeSet<u32>,
}

impl Used {
    /// Writes the resource dictionary naming them.
    pub(crate) fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"<<");
        for (key, prefix, set) in [
            ("Font", 'F', &self.fonts),
            ("XObject", 'X', &self.xobjects),
            ("ExtGState", 'G', &self.states),
            ("Shading", 'S', &self.shadings),
        ] {
            if set.is_empty() {
                continue;
            }
            let _ = write!(out, "/{key}<<");
            for n in set {
                let _ = write!(out, "/{prefix}{n} {n} 0 R");
            }
            out.extend_from_slice(b">>");
        }
        out.extend_from_slice(b">>");
    }
}

/// A transparency group being drawn: its stream, and how it composites.
struct Group {
    content: Content,
    bbox: Rect,
    opacity: f32,
    blend: u8,
}

/// What an opening marker opened, closed by its end.
enum Open {
    Layer { group: bool, effects: bool },
    Transform,
}

pub(crate) struct Painter<'a> {
    doc: &'a mut Document,
    source: &'a mut dyn Source,
    places: &'a [Affine2],
    resources: Ref,
    /// The paper's size, the area the part shows in, its scale and origin.
    size: [f32; 2],
    area: Rect,
    scale: f32,
    origin: [f32; 2],
    raster_scale: f32,
    used: &'a mut Used,
    /// The page's stream, then each group open inside it.
    base: Content,
    groups: Vec<Group>,
    open: Vec<Open>,
    /// The color effects of the layers open, outermost first.
    effects: Vec<Vec<ColorOp>>,
    /// Each open transform root's place, from content pixels to content
    /// pixels as painted; the identity at the bottom.
    spaces: Vec<Affine2>,
    /// The content pixels the page shows.
    view: Rect,
}

impl<'a> Painter<'a> {
    pub(crate) fn new(
        doc: &'a mut Document,
        source: &'a mut dyn Source,
        part: &Part<'a>,
        size: [f32; 2],
        resources: Ref,
        raster_scale: f32,
        used: &'a mut Used,
    ) -> Painter<'a> {
        let s = part.scale.max(1e-6);
        let view = Rect::new(
            part.origin[0],
            part.origin[1],
            part.area.w / s,
            part.area.h / s,
        );
        Painter {
            doc,
            source,
            places: part.places,
            resources,
            size,
            area: part.area,
            scale: s,
            origin: part.origin,
            raster_scale,
            used,
            base: Content::default(),
            groups: Vec::new(),
            open: Vec::new(),
            effects: Vec::new(),
            spaces: vec![Affine2::IDENTITY],
            view,
        }
    }

    /// The page's content stream drawing `draws`.
    pub(crate) fn page(&mut self, draws: &[Draw<'_>]) -> Vec<u8> {
        let (area, s, origin) = (self.area, self.scale, self.origin);
        let top = self.size[1] - area.y;
        self.base.save();
        self.base
            .rect(Rect::new(area.x, top - area.h, area.w, area.h));
        self.base.op(&[], "W n");
        self.base
            .transform([s, 0.0, 0.0, -s, area.x - origin[0] * s, top + origin[1] * s]);
        let ends = matching_ends(draws);
        let mut i = 0;
        while i < draws.len() {
            match draws[i] {
                Draw::Text(run) => self.text(run),
                Draw::Primitive(p) => match p {
                    Primitive::Layer(layer) => {
                        let mut j = i + 1;
                        while matches!(
                            draws.get(j),
                            Some(Draw::Primitive(
                                Primitive::ColorEffect(_)
                                    | Primitive::Blend(_)
                                    | Primitive::ClipCorners(_)
                            ))
                        ) {
                            j += 1;
                        }
                        if !self.visible(layer.clip) {
                            i = ends[i].map_or(draws.len(), |end| end + 1);
                            continue;
                        }
                        self.open_layer(layer, &draws[i + 1..j]);
                        i = j;
                        continue;
                    }
                    Primitive::LayerEnd => self.close(false),
                    Primitive::Transform(slot) => {
                        let place = self
                            .places
                            .get(slot.0 as usize)
                            .copied()
                            .unwrap_or(Affine2::IDENTITY);
                        let parent = *self.spaces.last().expect("the base space");
                        let relative = place.then(&parent.inverse());
                        self.cur().save();
                        if !relative.is_identity() {
                            let m = relative;
                            self.cur().transform([m.a, m.b, m.c, m.d, m.e, m.f]);
                        }
                        self.spaces.push(place);
                        self.open.push(Open::Transform);
                    }
                    Primitive::TransformEnd => self.close(true),
                    Primitive::ColorEffect(_) | Primitive::Blend(_) | Primitive::ClipCorners(_) => {
                    }
                    other => self.primitive(other),
                },
            }
            i += 1;
        }
        while let Some(open) = self.open.last() {
            let transform = matches!(open, Open::Transform);
            self.close(transform);
        }
        self.base.restore();
        std::mem::take(&mut self.base.buf)
    }

    /// The stream drawing now: the innermost group's, else the page's.
    fn cur(&mut self) -> &mut Content {
        match self.groups.last_mut() {
            Some(group) => &mut group.content,
            None => &mut self.base,
        }
    }

    /// Whether the box `r`, in the current space, reaches the page.
    fn visible(&self, r: Rect) -> bool {
        let space = self.spaces.last().expect("the base space");
        world(space, r).intersects(self.view.inflate(1.0))
    }

    fn open_layer(&mut self, layer: &LayerClip, markers: &[Draw<'_>]) {
        let mut effects = Vec::new();
        let mut blend = Blend::SrcOver;
        let mut corners = None;
        for marker in markers {
            match marker {
                Draw::Primitive(Primitive::ColorEffect(effect)) => effects.push(**effect),
                Draw::Primitive(Primitive::Blend(b)) => blend = *b,
                Draw::Primitive(Primitive::ClipCorners(c)) => corners = Some(*c),
                _ => {}
            }
        }
        let clip = layer.clip;
        let c = self.cur();
        c.save();
        match corners {
            Some(corners) if !corners.is_sharp() => c.rrect(clip, corners),
            _ => c.rect(clip),
        }
        c.op(&[], "W n");
        let blend = blend_index(blend);
        let group = layer.opacity < 0.999 || blend != 0;
        if group {
            self.groups.push(Group {
                content: Content::default(),
                bbox: clip,
                opacity: layer.opacity.clamp(0.0, 1.0),
                blend,
            });
        }
        let mut ops = Vec::new();
        fuse(effects, &mut ops);
        let effects = !ops.is_empty();
        if effects {
            self.effects.push(ops);
        }
        self.open.push(Open::Layer { group, effects });
    }

    /// Closes the innermost open marker, a transform when `transform`; an
    /// end that matches nothing open is ignored.
    fn close(&mut self, transform: bool) {
        match (self.open.last(), transform) {
            (Some(Open::Transform), true) => {
                self.open.pop();
                self.spaces.pop();
                self.cur().restore();
            }
            (Some(Open::Layer { .. }), false) => {
                let Some(Open::Layer { group, effects }) = self.open.pop() else {
                    unreachable!()
                };
                if effects {
                    self.effects.pop();
                }
                if group {
                    let group = self.groups.pop().expect("a group open");
                    let form = self.form(&group.content.buf, group.bbox, "/CS/DeviceRGB");
                    let alpha = (group.opacity * 255.0 + 0.5) as u8;
                    let state = self.state(StateKey {
                        fill: alpha,
                        stroke: alpha,
                        blend: group.blend,
                        mask: None,
                    });
                    let c = self.cur();
                    c.named(&format!("G{}", state.0), "gs");
                    c.named(&format!("X{}", form.0), "Do");
                }
                self.cur().restore();
            }
            _ => {}
        }
    }

    /// Writes a transparency-group form drawing `content` within `bbox`,
    /// its group's color space `space`, naming the page's resources.
    fn form(&mut self, content: &[u8], bbox: Rect, space: &str) -> Ref {
        let at = self.doc.w.alloc();
        let resources = self.resources;
        self.doc.w.stream(
            at,
            |out| {
                out.extend_from_slice(b"/Type/XObject/Subtype/Form/BBox[");
                for v in [bbox.x, bbox.y, bbox.x + bbox.w, bbox.y + bbox.h] {
                    num(out, v);
                    out.push(b' ');
                }
                let _ = write!(out, "]/Group<</S/Transparency{space}>>/Resources ");
                resources.put(out);
            },
            content,
        );
        self.used.xobjects.insert(at.0);
        at
    }

    /// The graphics state setting `key`, written once a document.
    fn state(&mut self, key: StateKey) -> Ref {
        let at = match self.doc.states.get(&key) {
            Some(&at) => at,
            None => {
                let at = self.doc.w.alloc();
                self.doc.w.obj(at, |out| {
                    out.extend_from_slice(b"<</Type/ExtGState/ca ");
                    num(out, f32::from(key.fill) / 255.0);
                    out.extend_from_slice(b"/CA ");
                    num(out, f32::from(key.stroke) / 255.0);
                    let _ = write!(out, "/BM/{}", BLENDS[usize::from(key.blend)]);
                    if let Some(mask) = key.mask {
                        out.extend_from_slice(b"/SMask<</Type/Mask/S/Luminosity/G ");
                        mask.put(out);
                        out.extend_from_slice(b">>");
                    }
                    out.extend_from_slice(b">>");
                });
                self.doc.states.insert(key, at);
                at
            }
        };
        self.used.states.insert(at.0);
        at
    }

    /// `color` through the open layers' effects: sRGB and alpha.
    fn color(&self, color: Rgba) -> ([f32; 3], f32) {
        let mut c = [color.r, color.g, color.b, color.a];
        for ops in self.effects.iter().rev() {
            for op in ops {
                c = op.apply(c);
            }
        }
        (
            [
                linear_to_srgb(c[0].clamp(0.0, 1.0)),
                linear_to_srgb(c[1].clamp(0.0, 1.0)),
                linear_to_srgb(c[2].clamp(0.0, 1.0)),
            ],
            c[3].clamp(0.0, 1.0),
        )
    }

    /// Runs `draw` with fill alpha `fill` and stroke alpha `stroke` in
    /// effect, in a saved state when either is not opaque.
    fn alpha(&mut self, fill: f32, stroke: f32, draw: impl FnOnce(&mut Content)) {
        let (fill, stroke) = ((fill * 255.0 + 0.5) as u8, (stroke * 255.0 + 0.5) as u8);
        if fill == 255 && stroke == 255 {
            draw(self.cur());
            return;
        }
        let state = self.state(StateKey {
            fill,
            stroke,
            blend: 0,
            mask: None,
        });
        let c = self.cur();
        c.save();
        c.named(&format!("G{}", state.0), "gs");
        draw(c);
        c.restore();
    }

    /// Fills the subpaths `path` adds with `color`, even-odd when `even_odd`.
    fn fill(&mut self, color: Rgba, even_odd: bool, path: impl FnOnce(&mut Content)) {
        let (rgb, a) = self.color(color);
        if a <= 0.0 {
            return;
        }
        self.alpha(a, 1.0, |c| {
            c.fill_rgb(rgb);
            path(c);
            c.op(&[], if even_odd { "f*" } else { "f" });
        });
    }

    fn primitive(&mut self, p: &Primitive) {
        match p {
            Primitive::Quad(q) => {
                self.rounded(q.rect, Corners::uniform(q.radius), q.color, q.border)
            }
            Primitive::AnalyticRRect(r) => self.rounded(r.rect, r.radius, r.color, r.border),
            Primitive::AnalyticCapsule(r) => {
                let radius = r.rect.w.min(r.rect.h) * 0.5;
                self.rounded(r.rect, Corners::uniform(radius), r.color, r.border);
            }
            Primitive::AnalyticEllipse(e) => {
                if !self.visible(e.rect) {
                    return;
                }
                let rect = e.rect;
                self.fill(e.color, false, |c| c.ellipse(rect));
                let bw = e.border.width;
                if bw > 0.0 {
                    let inner = inset(rect, bw);
                    self.fill(e.border.color, true, |c| {
                        c.ellipse(rect);
                        if inner.w > 0.0 && inner.h > 0.0 {
                            c.ellipse(inner);
                        }
                    });
                }
            }
            Primitive::AnalyticLine(line) => self.line(line),
            Primitive::Gradient(g) => self.gradient(g),
            Primitive::Path(path) => {
                if path.shadow.is_some() {
                    let bounds = path_bounds(path);
                    self.raster(p, bounds);
                } else {
                    self.path(path);
                }
            }
            Primitive::Mesh(mesh) => self.mesh(mesh),
            Primitive::Frosted(glass) => {
                // The blur has nothing behind it to read on paper; its ring
                // still draws.
                let opacity = glass.opacity.clamp(0.0, 1.0);
                if let Some(ring) = glass.border_rrect() {
                    let mut border = ring.border;
                    border.color.a *= opacity;
                    self.rounded(ring.rect, ring.radius, Rgba::TRANSPARENT, border);
                }
            }
            Primitive::AnalyticShadow(shadow) => {
                let bounds = if shadow.inner {
                    shadow.rect
                } else {
                    let reach = shadow.spread.max(0.0) + 3.0 * shadow.sigma.max(0.0);
                    Rect::new(
                        shadow.rect.x + shadow.offset[0],
                        shadow.rect.y + shadow.offset[1],
                        shadow.rect.w,
                        shadow.rect.h,
                    )
                    .inflate(reach)
                };
                self.raster(p, bounds);
            }
            Primitive::Image(image) => {
                let bounds = match image.mask {
                    Some(mask) => image.rect.intersect(mask.rect),
                    None => image.rect,
                };
                self.raster(p, bounds);
            }
            Primitive::Shader(shader) => self.raster(p, shader.rect),
            Primitive::Scene(scene) => self.raster(p, scene.rect),
            Primitive::GlyphRun(run) => {
                let bounds = run
                    .glyphs
                    .iter()
                    .map(|g| g.rect)
                    .reduce(Rect::union)
                    .unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
                self.raster(p, bounds);
            }
            Primitive::Layer(_)
            | Primitive::LayerEnd
            | Primitive::Transform(_)
            | Primitive::TransformEnd
            | Primitive::ColorEffect(_)
            | Primitive::Blend(_)
            | Primitive::ClipCorners(_) => {}
        }
    }

    /// A box rounded by `corners`, filled with `color` and bordered inside
    /// its edge by `border`.
    fn rounded(&mut self, rect: Rect, corners: Corners, color: Rgba, border: Border) {
        if !self.visible(rect) {
            return;
        }
        let corners = corners.normalized(rect.w, rect.h);
        self.fill(color, false, |c| c.rrect(rect, corners));
        let bw = border.width;
        if bw > 0.0 {
            let inner = inset(rect, bw);
            let inner_corners = shrink(corners, bw);
            self.fill(border.color, true, |c| {
                c.rrect(rect, corners);
                if inner.w > 0.0 && inner.h > 0.0 {
                    c.rrect(inner, inner_corners);
                }
            });
        }
    }

    fn line(&mut self, line: &AnalyticLine) {
        let bounds = Rect::new(
            line.p0.x.min(line.p1.x),
            line.p0.y.min(line.p1.y),
            (line.p1.x - line.p0.x).abs(),
            (line.p1.y - line.p0.y).abs(),
        )
        .inflate(line.width);
        if !self.visible(bounds) {
            return;
        }
        let stroke = |painter: &mut Self, width: f32, color: Rgba| {
            let (rgb, a) = painter.color(color);
            if a <= 0.0 || width <= 0.0 {
                return;
            }
            let (p0, p1) = (line.p0, line.p1);
            painter.alpha(1.0, a, |c| {
                c.stroke_rgb(rgb);
                c.op(&[width], "w");
                c.op(&[cap(line.cap)], "J");
                c.op(&[p0.x, p0.y], "m");
                c.op(&[p1.x, p1.y], "l");
                c.op(&[], "S");
            });
        };
        let bw = line.border.width;
        if bw > 0.0 {
            stroke(self, line.width, line.border.color);
            stroke(self, line.width - 2.0 * bw, line.color);
        } else {
            stroke(self, line.width, line.color);
        }
    }

    fn path(&mut self, path: &Path) {
        let bounds = path_bounds(path);
        if !self.visible(bounds) {
            return;
        }
        let cmds = &path.cmds;
        if let Some(fill) = path.fill {
            self.fill(fill, false, |c| c.path(cmds));
        }
        if let Some(stroke) = path.stroke {
            self.stroke(cmds, &stroke, bounds);
        }
    }

    fn stroke(&mut self, cmds: &[PathCmd], stroke: &Stroke, bounds: Rect) {
        let (rgb, a) = self.color(stroke.color);
        if a <= 0.0 {
            return;
        }
        let closed = cmds.iter().any(|c| matches!(c, PathCmd::Close));
        let align = if closed {
            stroke.align
        } else {
            StrokeAlign::Center
        };
        let width = if stroke.hairline {
            0.0
        } else {
            match align {
                StrokeAlign::Center => stroke.width,
                _ => stroke.width * 2.0,
            }
        };
        let outside = bounds.inflate(stroke.width * 2.0 + 1.0);
        self.alpha(1.0, a, |c| {
            match align {
                StrokeAlign::Center => {}
                StrokeAlign::Inner => {
                    c.save();
                    c.path(cmds);
                    c.op(&[], "W n");
                }
                StrokeAlign::Outer => {
                    c.save();
                    c.rect(outside);
                    c.path(cmds);
                    c.op(&[], "W* n");
                }
            }
            c.stroke_rgb(rgb);
            c.op(&[width], "w");
            c.op(&[cap(stroke.cap)], "J");
            c.op(
                &[match stroke.join {
                    LineJoin::Miter => 0.0,
                    LineJoin::Round => 1.0,
                    LineJoin::Bevel => 2.0,
                }],
                "j",
            );
            c.op(&[(stroke.miter_limit * 0.5).max(1.0)], "M");
            if let Some(dash) = stroke.dash {
                let runs = &dash.segments[..usize::from(dash.len).clamp(1, dash.segments.len())];
                c.raw(b"[");
                for &run in runs {
                    num(&mut c.buf, run.max(0.0));
                    c.raw(b" ");
                }
                if runs.len() == 1 {
                    num(&mut c.buf, runs[0].max(0.0));
                }
                c.raw(b"] ");
                c.op(&[dash.offset], "d");
            }
            c.path(cmds);
            c.op(&[], "S");
            if align != StrokeAlign::Center {
                c.restore();
            }
        });
    }

    fn mesh(&mut self, mesh: &Mesh) {
        let vertices = &mesh.vertices;
        if vertices.is_empty() || mesh.indices.len() < 3 {
            return;
        }
        let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for v in vertices {
            b[0] = b[0].min(v.pos[0]);
            b[1] = b[1].min(v.pos[1]);
            b[2] = b[2].max(v.pos[0]);
            b[3] = b[3].max(v.pos[1]);
        }
        let bounds = Rect::new(b[0], b[1], b[2] - b[0], b[3] - b[1]);
        if !self.visible(bounds) {
            return;
        }
        let colors: Vec<([f32; 3], f32)> = vertices
            .iter()
            .map(|v| self.color(Rgba::new(v.color[0], v.color[1], v.color[2], v.color[3])))
            .collect();
        let mut rgb = Vec::with_capacity(mesh.indices.len());
        let mut alpha = Vec::with_capacity(mesh.indices.len());
        for tri in mesh.indices.as_chunks::<3>().0 {
            for &i in tri {
                let Some(v) = vertices.get(i as usize) else {
                    return;
                };
                let (c, a) = colors[i as usize];
                rgb.push((v.pos, c));
                alpha.push((v.pos, [a, 0.0, 0.0]));
            }
        }
        let first = alpha[0].1[0];
        let flat = alpha.iter().all(|a| (a.1[0] - first).abs() < 1.0 / 255.0);
        let color = self.doc.w.alloc();
        shading::write_mesh(&mut self.doc.w, color, b, &rgb, 3);
        let mask = (!flat).then(|| {
            let at = self.doc.w.alloc();
            shading::write_mesh(&mut self.doc.w, at, b, &alpha, 1);
            at
        });
        self.shade(color, mask, if flat { first } else { 1.0 }, bounds);
    }

    fn gradient(&mut self, g: &viso_render::Gradient) {
        let rect = g.rect;
        if !self.visible(rect) || g.stops.is_empty() {
            return;
        }
        let key = LutKey::new(&g.stops, g.interp, g.extend);
        let corners = [
            [rect.x, rect.y],
            [rect.x + rect.w, rect.y],
            [rect.x, rect.y + rect.h],
            [rect.x + rect.w, rect.y + rect.h],
        ];
        let (p0, p1) = ([g.p0.x, g.p0.y], [g.p1.x, g.p1.y]);
        let last = g.stops.last().expect("a stop").color;
        match g.kind {
            GradientKind::Linear => {
                let d = [p1[0] - p0[0], p1[1] - p0[1]];
                let len2 = d[0] * d[0] + d[1] * d[1];
                if len2 < 1e-9 {
                    self.fill(last, false, |c| c.rect(rect));
                    return;
                }
                let ts = corners.map(|p| ((p[0] - p0[0]) * d[0] + (p[1] - p0[1]) * d[1]) / len2);
                let (t0, t1) = range(&ts);
                let geometry = Geometry::Axial {
                    from: [p0[0] + d[0] * t0, p0[1] + d[1] * t0],
                    to: [p0[0] + d[0] * t1, p0[1] + d[1] * t1],
                };
                self.ramp(&key, &geometry, t0, t1, rect);
            }
            GradientKind::Radial => {
                let radius = p1[0];
                if radius <= 1e-6 {
                    self.fill(last, false, |c| c.rect(rect));
                    return;
                }
                let far = corners
                    .iter()
                    .map(|p| (p[0] - p0[0]).hypot(p[1] - p0[1]))
                    .fold(0.0f32, f32::max);
                let t1 = (far / radius).max(1.0);
                let geometry = Geometry::Radial {
                    center: p0,
                    radius: radius * t1,
                };
                self.ramp(&key, &geometry, 0.0, t1, rect);
            }
            GradientKind::Sweep => self.sweep(&key, p0, p1[0], rect),
        }
    }

    /// Paints `rect` with `key`'s ramp over `t0..t1` laid along `geometry`.
    fn ramp(&mut self, key: &LutKey, geometry: &Geometry, t0: f32, t1: f32, rect: Rect) {
        let samples = ((t1 - t0).abs().ceil() as usize * 64).clamp(256, 4096);
        let mut rgb = Vec::with_capacity(samples * 3);
        let mut alpha = Vec::with_capacity(samples);
        for i in 0..samples {
            let t = t0 + (t1 - t0) * i as f32 / (samples - 1) as f32;
            let (c, a) = self.color(key.sample(t));
            rgb.extend_from_slice(&c);
            alpha.push(a);
        }
        let first = alpha[0];
        let flat = alpha.iter().all(|a| (a - first).abs() < 1.0 / 255.0);
        let color = self.doc.w.alloc();
        shading::write_ramp(&mut self.doc.w, color, geometry, &rgb, 3);
        let mask = (!flat).then(|| {
            let at = self.doc.w.alloc();
            shading::write_ramp(&mut self.doc.w, at, geometry, &alpha, 1);
            at
        });
        self.shade(color, mask, if flat { first } else { 1.0 }, rect);
    }

    /// Paints `rect` with `key`'s ramp turning around `center` from the
    /// angle `start`, as a fan of fine wedges.
    fn sweep(&mut self, key: &LutKey, center: [f32; 2], start: f32, rect: Rect) {
        const WEDGES: usize = 256;
        let far = [
            [rect.x, rect.y],
            [rect.x + rect.w, rect.y],
            [rect.x, rect.y + rect.h],
            [rect.x + rect.w, rect.y + rect.h],
        ]
        .iter()
        .map(|p| (p[0] - center[0]).hypot(p[1] - center[1]))
        .fold(0.0f32, f32::max)
            + 1.0;
        let at = |i: usize| {
            let t = i as f32 / WEDGES as f32;
            let angle = start + t * std::f32::consts::TAU;
            (
                [center[0] + far * angle.cos(), center[1] + far * angle.sin()],
                t.min(1.0 - 1e-4),
            )
        };
        let mut rgb = Vec::with_capacity(WEDGES * 3);
        let mut alpha = Vec::with_capacity(WEDGES * 3);
        let mut b = [center[0], center[1], center[0], center[1]];
        for i in 0..WEDGES {
            let (pa, ta) = at(i);
            let (pb, tb) = at(i + 1);
            for (p, t) in [(center, (ta + tb) * 0.5), (pa, ta), (pb, tb)] {
                let (c, a) = self.color(key.sample(t));
                rgb.push((p, c));
                alpha.push((p, [a, 0.0, 0.0]));
                b = [
                    b[0].min(p[0]),
                    b[1].min(p[1]),
                    b[2].max(p[0]),
                    b[3].max(p[1]),
                ];
            }
        }
        let first = alpha[0].1[0];
        let flat = alpha.iter().all(|a| (a.1[0] - first).abs() < 1.0 / 255.0);
        let color = self.doc.w.alloc();
        shading::write_mesh(&mut self.doc.w, color, b, &rgb, 3);
        let mask = (!flat).then(|| {
            let at = self.doc.w.alloc();
            shading::write_mesh(&mut self.doc.w, at, b, &alpha, 1);
            at
        });
        self.shade(color, mask, if flat { first } else { 1.0 }, rect);
    }

    /// Paints the shading `color` within `rect`, through the gray shading
    /// `mask` as its alpha, or at the constant `alpha`.
    fn shade(&mut self, color: Ref, mask: Option<Ref>, alpha: f32, rect: Rect) {
        if mask.is_none() && alpha <= 0.0 {
            return;
        }
        self.used.shadings.insert(color.0);
        let state = match mask {
            Some(mask) => {
                self.used.shadings.insert(mask.0);
                let mut form = Content::default();
                form.named(&format!("S{}", mask.0), "sh");
                let form = self.form(&form.buf, rect, "/CS/DeviceGray");
                Some(self.state(StateKey {
                    fill: 255,
                    stroke: 255,
                    blend: 0,
                    mask: Some(form),
                }))
            }
            None if alpha < 1.0 => {
                let a = (alpha * 255.0 + 0.5) as u8;
                Some(self.state(StateKey {
                    fill: a,
                    stroke: a,
                    blend: 0,
                    mask: None,
                }))
            }
            None => None,
        };
        let c = self.cur();
        c.save();
        c.rect(rect);
        c.op(&[], "W n");
        if let Some(state) = state {
            c.named(&format!("G{}", state.0), "gs");
        }
        c.named(&format!("S{}", color.0), "sh");
        c.restore();
    }

    /// Draws `p`, which has no vector form, as a raster of the box `bounds`.
    fn raster(&mut self, p: &Primitive, bounds: Rect) {
        if bounds.w <= 0.0 || bounds.h <= 0.0 || !self.visible(bounds) {
            return;
        }
        let plain = self.effects.is_empty();
        let cached = plain
            .then(|| {
                self.doc
                    .rasters
                    .iter()
                    .find(|(q, r, _)| q == p && *r == bounds)
                    .map(|(_, _, at)| *at)
            })
            .flatten();
        let image = match cached {
            Some(at) => at,
            None => {
                let space = self.spaces.last().expect("the base space");
                let mut scale = self.raster_scale * space.max_scale().max(1e-3);
                scale = scale.min(RASTER_MAX / bounds.w.max(bounds.h));
                let Some(mut raster) = self.source.raster(std::slice::from_ref(p), bounds, scale)
                else {
                    return;
                };
                let count = (raster.width * raster.height) as usize;
                if raster.width == 0 || raster.height == 0 || raster.pixels.len() < count * 4 {
                    return;
                }
                if !plain {
                    self.recolor(&mut raster);
                }
                let at = self.doc.w.alloc();
                image::write_rgba(
                    &mut self.doc.w,
                    at,
                    raster.width,
                    raster.height,
                    &raster.pixels,
                );
                if plain {
                    self.doc.rasters.push((p.clone(), bounds, at));
                }
                at
            }
        };
        self.used.xobjects.insert(image.0);
        let c = self.cur();
        c.save();
        c.transform([bounds.w, 0.0, 0.0, -bounds.h, bounds.x, bounds.y + bounds.h]);
        c.named(&format!("X{}", image.0), "Do");
        c.restore();
    }

    /// Passes every pixel of `raster` through the open layers' effects.
    fn recolor(&self, raster: &mut Raster) {
        for px in raster.pixels.as_chunks_mut::<4>().0 {
            let linear = |v: u8| srgb_to_linear(f32::from(v) / 255.0);
            let color = Rgba::new(
                linear(px[0]),
                linear(px[1]),
                linear(px[2]),
                f32::from(px[3]) / 255.0,
            );
            let (rgb, a) = self.color(color);
            for (out, v) in px.iter_mut().zip(rgb) {
                *out = (v * 255.0 + 0.5) as u8;
            }
            px[3] = (a * 255.0 + 0.5) as u8;
        }
    }

    fn text(&mut self, run: &TextRun) {
        let Some(first) = run.glyphs.first() else {
            return;
        };
        let size = run.size;
        let mut b = [
            first.origin[0],
            first.origin[1],
            first.origin[0],
            first.origin[1],
        ];
        for g in &run.glyphs {
            b = [
                b[0].min(g.origin[0]),
                b[1].min(g.origin[1]),
                b[2].max(g.origin[0]),
                b[3].max(g.origin[1]),
            ];
        }
        let bounds = Rect::new(b[0], b[1], b[2] - b[0], b[3] - b[1]).inflate(size * 1.5);
        if !self.visible(bounds) {
            return;
        }
        let (rgb, a) = self.color(run.color);
        if a <= 0.0 {
            return;
        }
        let mut body = Content::default();
        body.raw(b"BT\n");
        body.fill_rgb(rgb);
        let mut font = None;
        let Painter {
            doc, source, used, ..
        } = self;
        // The pen after the last glyph shown, where the next is drawn unless
        // moved: glyphs on one baseline go in one array, each moved off the
        // pen by its own adjustment, so a reader sees words, not glyphs.
        let mut pen: Option<[f32; 2]> = None;
        let mut open = false;
        for g in &run.glyphs {
            let Some((at, code)) = doc.fonts.code(&mut doc.w, run.face, g.glyph, &g.text, || {
                source.glyph(run.face, g.glyph)
            }) else {
                continue;
            };
            let advance = doc.fonts.advance(run.face, g.glyph).unwrap_or(0.0) * size;
            if font != Some(at) {
                if open {
                    body.raw(b"] TJ\n");
                    open = false;
                }
                used.fonts.insert(at.0);
                let _ = write!(body.buf, "/F{} ", at.0);
                body.op(&[size], "Tf");
                font = Some(at);
            }
            let along = pen.filter(|p| (p[1] - g.origin[1]).abs() < 0.01 && open);
            match along {
                Some(p) => {
                    // A move in thousandths of an em, against the pen.
                    let shift = (p[0] - g.origin[0]) * 1000.0 / size;
                    if shift.abs() >= 0.5 {
                        let _ = write!(body.buf, " {}", shift.round() as i32);
                    }
                }
                None => {
                    if open {
                        body.raw(b"] TJ\n");
                    }
                    body.op(&[1.0, 0.0, 0.0, -1.0, g.origin[0], g.origin[1]], "Tm");
                    body.raw(b"[");
                    open = true;
                }
            }
            let _ = write!(body.buf, "<{code:02X}>");
            pen = Some([g.origin[0] + advance, g.origin[1]]);
        }
        if open {
            body.raw(b"] TJ\n");
        }
        body.raw(b"ET\n");
        if font.is_none() {
            return;
        }
        self.alpha(a, 1.0, |c| c.raw(&body.buf));
    }
}

/// For each opening marker of `draws`, the index of the end matching it.
fn matching_ends(draws: &[Draw<'_>]) -> Vec<Option<usize>> {
    let mut ends = vec![None; draws.len()];
    let mut stack = Vec::new();
    for (i, draw) in draws.iter().enumerate() {
        match draw {
            Draw::Primitive(Primitive::Layer(_) | Primitive::Transform(_)) => stack.push(i),
            Draw::Primitive(Primitive::LayerEnd | Primitive::TransformEnd) => {
                if let Some(open) = stack.pop() {
                    ends[open] = Some(i);
                }
            }
            _ => {}
        }
    }
    ends
}

/// The box `r` covers once `space` places it.
fn world(space: &Affine2, r: Rect) -> Rect {
    if space.is_identity() {
        return r;
    }
    let corners = [
        Vec2 { x: r.x, y: r.y },
        Vec2 {
            x: r.x + r.w,
            y: r.y,
        },
        Vec2 {
            x: r.x,
            y: r.y + r.h,
        },
        Vec2 {
            x: r.x + r.w,
            y: r.y + r.h,
        },
    ]
    .map(|p| space.transform_point(p));
    let xs = corners.map(|p| p.x);
    let ys = corners.map(|p| p.y);
    let (x0, x1) = range(&xs);
    let (y0, y1) = range(&ys);
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

fn range(values: &[f32]) -> (f32, f32) {
    values
        .iter()
        .fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)))
}

/// The box of a path's outline, its stroke and shadow included.
fn path_bounds(path: &Path) -> Rect {
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    let mut add = |p: viso_render::Point| {
        b = [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)];
    };
    for cmd in &path.cmds {
        match *cmd {
            PathCmd::MoveTo(p) | PathCmd::LineTo(p) => add(p),
            PathCmd::QuadTo(a, p) => {
                add(a);
                add(p);
            }
            PathCmd::CubicTo(a, c, p) => {
                add(a);
                add(c);
                add(p);
            }
            PathCmd::Close => {}
        }
    }
    if b[0] > b[2] {
        return Rect::new(0.0, 0.0, 0.0, 0.0);
    }
    let mut rect = Rect::new(b[0], b[1], b[2] - b[0], b[3] - b[1]);
    if let Some(stroke) = path.stroke {
        rect = rect.inflate(stroke.width.max(1.0) * stroke.miter_limit.max(1.0));
    }
    if let Some(shadow) = path.shadow {
        let reach = shadow.spread.max(0.0) + 3.0 * shadow.sigma.max(0.0);
        let cast = Rect::new(
            rect.x + shadow.offset[0],
            rect.y + shadow.offset[1],
            rect.w,
            rect.h,
        )
        .inflate(reach);
        rect = rect.union(cast);
    }
    rect
}

fn cap(cap: LineCap) -> f32 {
    match cap {
        LineCap::Butt => 0.0,
        LineCap::Round => 1.0,
        LineCap::Square => 2.0,
    }
}
