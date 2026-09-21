//! Paint primitives and their GPU instance layouts.
//!
//! A [`Primitive`] is the renderer-facing *description* of something to draw —
//! high-level Viso geometry (a rounded rect, a glyph run, …), independent of any
//! backend. Each primitive lowers to one `#[derive(GpuPod)]` instance
//! struct whose `#[repr(C)]` layout the backend uploads directly.
//!
//! The instance struct's field names and formats are a three-way contract:
//! - the **shader** (D layer) declares them as an `InstanceSchema`,
//! - `#[derive(GpuPod)]` records the real byte offsets (B layer),
//! - the headless rasterizer reads fields by that name (C layer).
//!
//! `create_pipeline` validates the derived layout against the schema, so a
//! mismatch is caught at pipeline-registration time.

use viso_gpu::{AddressMode, FilterMode, GpuPod, SamplerDesc, TextureId};
use viso_math::{ExtendMode, InterpolationSpace};

// The Quad/Image/Mesh field contracts (`quad_schema`/`image_schema`/
// `mesh_schema`) live with the hand-written MSL in `viso-shader` (layer D);
// re-export them here so the instance structs and their schemas stay visibly
// paired at the primitive definition.
pub use viso_shader::{
    analytic_capsule_schema, analytic_ellipse_schema, analytic_line_schema, analytic_rrect_schema,
    analytic_shadow_schema, glyphrun_schema, gradient_schema, image_schema, mesh_schema,
    quad_schema,
};

/// An axis-aligned rectangle in physical pixels, top-left origin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Top-left corner x.
    pub x: f32,
    /// Top-left corner y.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

impl Rect {
    /// An effectively unbounded rect — the identity for [`intersect`](Self::intersect):
    /// intersecting any finite rect with `INFINITE` yields that rect back. Useful
    /// as the top-level clip when nothing above encloses a subtree. Its origin is
    /// far negative and its extent huge, chosen so `x + w` stays finite (no
    /// overflow) while covering every realistic coordinate.
    pub const INFINITE: Rect = Rect {
        x: -f32::MAX / 4.0,
        y: -f32::MAX / 4.0,
        w: f32::MAX / 2.0,
        h: f32::MAX / 2.0,
    };

    /// The intersection of two rects (both top-left-origin, physical pixels).
    ///
    /// Used to combine nested [`Primitive::Layer`] clip rects: a child clip is
    /// constrained to its parent. If the rects do not overlap, the result is an
    /// empty rect (`w`/`h` clamped to 0) — a clip that draws nothing.
    pub fn intersect(self, other: Rect) -> Rect {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = (self.x + self.w).min(other.x + other.w);
        let y1 = (self.y + self.h).min(other.y + other.h);
        Rect {
            x: x0,
            y: y0,
            w: (x1 - x0).max(0.0),
            h: (y1 - y0).max(0.0),
        }
    }

    /// The bounding box of two rects (both top-left-origin, physical pixels).
    ///
    /// An empty operand (`w <= 0 || h <= 0`) is the identity, so [`Rect::ZERO`]
    /// seeds a running union: `ZERO.union(a) == a`. Used to accumulate an
    /// offscreen pass's content bounds as its subtree is walked, before sizing
    /// the pass's tight render-target ROI (§16.2).
    pub fn union(self, other: Rect) -> Rect {
        if self.w <= 0.0 || self.h <= 0.0 {
            return other;
        }
        if other.w <= 0.0 || other.h <= 0.0 {
            return self;
        }
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = (self.x + self.w).max(other.x + other.w);
        let y1 = (self.y + self.h).max(other.y + other.h);
        Rect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        }
    }

    /// Whether the point `(px, py)` (physical px, same space as the rect) lies
    /// inside this rect. Near edges are inclusive, far edges exclusive
    /// (`[x, x+w)` / `[y, y+h)`), so two rects tiling a shared boundary do not
    /// both claim a point on that seam.
    #[inline]
    pub fn contains(self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }

    /// The empty rect at the origin — the identity a retained bound starts from
    /// before its geometry resolves.
    pub const ZERO: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 0.0,
        h: 0.0,
    };

    /// This rect grown by `amount` physical pixels on every side (origin moves
    /// out, extent grows by twice the amount). A negative `amount` shrinks it;
    /// the extent is clamped at zero. Used to inflate a fill bound by half a
    /// stroke width or a filter radius without re-parsing geometry (§8).
    #[inline]
    pub fn inflate(self, amount: f32) -> Rect {
        Rect {
            x: self.x - amount,
            y: self.y - amount,
            w: (self.w + amount * 2.0).max(0.0),
            h: (self.h + amount * 2.0).max(0.0),
        }
    }
}

/// A straight-alpha (non-premultiplied) linear RGBA color — the wire shape a
/// primitive carries. The backend premultiplies at the point of blend; keeping
/// the public type straight matches how authors reason about a color
/// independent of its opacity. This is the canonical straight-linear color
/// defined in `viso-math`; the renderer exposes it under its historical name.
pub use viso_math::LinearStraight as Rgba;

/// A border stroke drawn inside a quad's edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Border {
    /// Stroke width in pixels (0 = no border).
    pub width: f32,
    /// Stroke color.
    pub color: Rgba,
}

impl Border {
    /// No border.
    pub const NONE: Border = Border {
        width: 0.0,
        color: Rgba::TRANSPARENT,
    };
}

/// A rounded, optionally bordered, axis-aligned rectangle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quad {
    /// The rectangle, in physical pixels.
    pub rect: Rect,
    /// Fill color.
    pub color: Rgba,
    /// Corner radius in pixels (0 = sharp corners).
    pub radius: f32,
    /// Border stroke.
    pub border: Border,
}

impl Quad {
    /// Lower this quad to its GPU instance.
    pub fn to_instance(&self) -> QuadInstance {
        QuadInstance {
            rect_pos: [self.rect.x, self.rect.y],
            rect_size: [self.rect.w, self.rect.h],
            color: [self.color.r, self.color.g, self.color.b, self.color.a],
            radius: self.radius,
            border_width: self.border.width,
            border_color: [
                self.border.color.r,
                self.border.color.g,
                self.border.color.b,
                self.border.color.a,
            ],
        }
    }
}

/// Per-corner radii for an [`AnalyticRRect`], in physical pixels. Order follows
/// the fill's quadrant test: `left_top`, `right_top`, `right_bottom`,
/// `left_bottom`. A corner radius of `0` is a sharp corner. Oversized radii are
/// scaled to fit by [`Corners::normalized`] during lowering (§11.2); the SDF
/// additionally clamps each corner as a final safety net.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Corners {
    /// Top-left corner radius.
    pub left_top: f32,
    /// Top-right corner radius.
    pub right_top: f32,
    /// Bottom-right corner radius.
    pub right_bottom: f32,
    /// Bottom-left corner radius.
    pub left_bottom: f32,
}

impl Corners {
    /// Sharp corners on all four sides.
    pub const SHARP: Corners = Corners {
        left_top: 0.0,
        right_top: 0.0,
        right_bottom: 0.0,
        left_bottom: 0.0,
    };

    /// The same radius on every corner.
    pub const fn uniform(r: f32) -> Corners {
        Corners {
            left_top: r,
            right_top: r,
            right_bottom: r,
            left_bottom: r,
        }
    }

    /// Scale these radii to fit a `width`×`height` box, following the CSS
    /// overlapping-curves rule (§11.2): when two radii sharing an edge sum to
    /// more than that edge's length, every radius shrinks by the *single*
    /// smallest edge ratio, so the shape stays proportional instead of each
    /// corner clamping independently and distorting. Negative inputs floor to
    /// `0`. Widgets pass their authored radii straight through — the normalize
    /// lives here, once, so no caller self-clamps.
    pub fn normalized(self, width: f32, height: f32) -> Corners {
        let r = [
            self.left_top.max(0.0),
            self.right_top.max(0.0),
            self.right_bottom.max(0.0),
            self.left_bottom.max(0.0),
        ];
        // Each edge's two adjacent radii must fit within its length.
        let edge_ratio = |sum: f32, len: f32| if sum > len { len / sum } else { 1.0 };
        let scale = edge_ratio(r[0] + r[1], width) // top
            .min(edge_ratio(r[3] + r[2], width)) // bottom
            .min(edge_ratio(r[0] + r[3], height)) // left
            .min(edge_ratio(r[1] + r[2], height)); // right
        Corners {
            left_top: r[0] * scale,
            right_top: r[1] * scale,
            right_bottom: r[2] * scale,
            left_bottom: r[3] * scale,
        }
    }
}

/// An analytic rounded rectangle with an independent radius per corner.
///
/// Unlike [`Quad`]'s single scalar radius, each corner rounds by its own amount,
/// evaluated by a per-corner rounded-box SDF (no tessellation). Degenerates to a
/// plain filled rectangle when all radii and the border are `0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnalyticRRect {
    /// The rectangle, in physical pixels.
    pub rect: Rect,
    /// Fill color.
    pub color: Rgba,
    /// Per-corner radii in pixels.
    pub radius: Corners,
    /// Border stroke.
    pub border: Border,
}

impl AnalyticRRect {
    /// Lower this rounded rect to its GPU instance. Per-corner radii are
    /// normalized to the rect (§11.2) so oversized authored radii scale down
    /// proportionally rather than distorting corner by corner.
    pub fn to_instance(&self) -> AnalyticRRectInstance {
        let radius = self.radius.normalized(self.rect.w, self.rect.h);
        AnalyticRRectInstance {
            rect_pos: [self.rect.x, self.rect.y],
            rect_size: [self.rect.w, self.rect.h],
            color: [self.color.r, self.color.g, self.color.b, self.color.a],
            radius: [
                radius.left_top,
                radius.right_top,
                radius.right_bottom,
                radius.left_bottom,
            ],
            border_width: self.border.width,
            border_color: [
                self.border.color.r,
                self.border.color.g,
                self.border.color.b,
                self.border.color.a,
            ],
        }
    }
}

/// An analytic axis-aligned ellipse (a circle when the rect is square).
///
/// The ellipse radii are the rect's half-extents, so no radius field is carried;
/// the fill is a scaled-circle SDF (no tessellation). Supports an inner border.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnalyticEllipse {
    /// The bounding rectangle, in physical pixels. The ellipse touches its edges.
    pub rect: Rect,
    /// Fill color.
    pub color: Rgba,
    /// Border stroke.
    pub border: Border,
}

impl AnalyticEllipse {
    /// Lower this ellipse to its GPU instance.
    pub fn to_instance(&self) -> AnalyticEllipseInstance {
        AnalyticEllipseInstance {
            rect_pos: [self.rect.x, self.rect.y],
            rect_size: [self.rect.w, self.rect.h],
            color: [self.color.r, self.color.g, self.color.b, self.color.a],
            border_width: self.border.width,
            border_color: [
                self.border.color.r,
                self.border.color.g,
                self.border.color.b,
                self.border.color.a,
            ],
        }
    }
}

/// An analytic capsule/stadium: a rounded box whose corner radius is the smaller
/// half-extent.
///
/// The short axis is fully rounded and the long axis stays straight; a square
/// rect degenerates to a circle. Like [`AnalyticEllipse`] the shape carries no
/// radius field — the corner radius is derived in the shader as the smaller
/// half-extent — so its instance layout is byte-identical to the ellipse's. The
/// fill is a capsule SDF (no tessellation). Supports an inner border.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnalyticCapsule {
    /// The bounding rectangle, in physical pixels. The capsule touches its edges.
    pub rect: Rect,
    /// Fill color.
    pub color: Rgba,
    /// Border stroke.
    pub border: Border,
}

impl AnalyticCapsule {
    /// Lower this capsule to its GPU instance.
    pub fn to_instance(&self) -> AnalyticCapsuleInstance {
        AnalyticCapsuleInstance {
            rect_pos: [self.rect.x, self.rect.y],
            rect_size: [self.rect.w, self.rect.h],
            color: [self.color.r, self.color.g, self.color.b, self.color.a],
            border_width: self.border.width,
            border_color: [
                self.border.color.r,
                self.border.color.g,
                self.border.color.b,
                self.border.color.a,
            ],
        }
    }
}

/// The silhouette an [`AnalyticShadow`] casts, selecting the SDF the fast lane
/// evaluates its Gaussian coverage against.
///
/// Every requested §15 shape folds onto one of three distance functions, matching
/// the shader/headless `shape` discriminator: a rounded box subsumes `Rect`
/// (radius 0) and `RRect` (per-corner radii); an ellipse subsumes `Circle` and
/// `Ellipse`; a capsule is a rounded box whose radius is the smaller half-extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowShape {
    /// A (optionally per-corner-rounded) box — covers `Rect` and `RRect`.
    RoundedBox,
    /// An axis-aligned ellipse — covers `Circle` and `Ellipse`.
    Ellipse,
    /// A capsule/stadium — corner radius is the smaller half-extent.
    Capsule,
}

impl ShadowShape {
    /// The `u32` this silhouette maps to (matches the analytic shader/headless
    /// `shadow_sdf` dispatch: 0=rounded box, 1=ellipse, 2=capsule).
    const fn as_u32(self) -> u32 {
        match self {
            ShadowShape::RoundedBox => 0,
            ShadowShape::Ellipse => 1,
            ShadowShape::Capsule => 2,
        }
    }
}

/// A soft drop shadow for an analytic shape, drawn by the §15 fast lane: one extra
/// instanced quad under the shape whose coverage is a closed-form Gaussian ramp
/// over the shape's signed distance — no blur target, no offscreen composite.
///
/// The shadow silhouette is `rect` grown by `spread` and shaped by `shape`
/// (rounded box / ellipse / capsule), blurred with standard deviation `sigma`
/// (the blur-radius semantic of §15.2) and displaced by `offset`. `radius` is the
/// per-corner rounding used only when `shape` is [`ShadowShape::RoundedBox`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnalyticShadow {
    /// The source shape's rectangle, in physical pixels (before spread/offset).
    pub rect: Rect,
    /// Shadow color (straight linear RGBA; `a` scales the whole ramp).
    pub color: Rgba,
    /// Per-corner radii in pixels, used only for [`ShadowShape::RoundedBox`].
    pub radius: Corners,
    /// Shadow displacement in physical pixels.
    pub offset: [f32; 2],
    /// Blur standard deviation in pixels (§15.2 blur-radius semantic).
    pub sigma: f32,
    /// Silhouette grow (`+`) / shrink (`-`) in pixels applied before the blur.
    pub spread: f32,
    /// Which silhouette the shadow casts.
    pub shape: ShadowShape,
    /// `false` for an outer drop shadow (fills the silhouette, fades outward);
    /// `true` for an inner shadow (darkens the interior near the edge, fading
    /// toward the center) — the analytic §20.3 lane, no mask/blur target.
    pub inner: bool,
}

impl AnalyticShadow {
    /// Lower this shadow to its GPU instance. Per-corner radii are normalized to
    /// the rect (§11.2) exactly as [`AnalyticRRect::to_instance`], so oversized
    /// authored radii scale down proportionally; `offset`/`sigma`/`spread`/`shape`
    /// pass through unchanged.
    pub fn to_instance(&self) -> ShadowInstance {
        let radius = self.radius.normalized(self.rect.w, self.rect.h);
        ShadowInstance {
            rect_pos: [self.rect.x, self.rect.y],
            rect_size: [self.rect.w, self.rect.h],
            color: [self.color.r, self.color.g, self.color.b, self.color.a],
            radius: [
                radius.left_top,
                radius.right_top,
                radius.right_bottom,
                radius.left_bottom,
            ],
            offset: self.offset,
            sigma: self.sigma,
            spread: self.spread,
            shape: self.shape.as_u32(),
            inner: self.inner as u32,
        }
    }
}

/// An analytic stroked line segment defined by its two endpoints.
///
/// The stroke is centered on the `p0`→`p1` segment with the given `width`; the
/// endpoints are shaped by `cap` and (for future multi-segment use, and to keep
/// the round-endpoint semantics consistent) `join`/`miter_limit`. The fill is a
/// segment SDF evaluated per pixel in the shader (no tessellation), with an
/// optional inner border. Unlike the rect-based analytic families this needs its
/// own [`AnalyticLineInstance`] layout — it is defined by endpoints, not an
/// axis-aligned rect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnalyticLine {
    /// Segment start, in physical pixels.
    pub p0: Point,
    /// Segment end, in physical pixels.
    pub p1: Point,
    /// Stroke width in physical pixels (centered on the segment).
    pub width: f32,
    /// Fill color.
    pub color: Rgba,
    /// End-cap shape.
    pub cap: LineCap,
    /// Corner join shape (for multi-segment / round-endpoint consistency).
    pub join: LineJoin,
    /// Miter length limit as a multiple of the half-width, before a miter join
    /// degenerates to a bevel.
    pub miter_limit: f32,
    /// Border stroke.
    pub border: Border,
}

impl AnalyticLine {
    /// Lower this line to its GPU instance.
    pub fn to_instance(&self) -> AnalyticLineInstance {
        AnalyticLineInstance {
            p0: [self.p0.x, self.p0.y],
            p1: [self.p1.x, self.p1.y],
            width: self.width,
            color: [self.color.r, self.color.g, self.color.b, self.color.a],
            cap: self.cap.as_u32(),
            join: self.join.as_u32(),
            miter_limit: self.miter_limit,
            border_width: self.border.width,
            border_color: [
                self.border.color.r,
                self.border.color.g,
                self.border.color.b,
                self.border.color.a,
            ],
        }
    }
}

/// Which gradient parameterization a [`Gradient`] uses.
///
/// The `u32` codes are the wire contract shared by the shader, the derived
/// instance layout, and the headless fill (0=linear, 1=radial, 2=sweep). Each
/// reuses the instance's `p0`/`p1` pair differently — see [`Gradient`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradientKind {
    /// A linear gradient: `t` is the projection of the sample point onto the
    /// `p0`→`p1` axis (`0` at `p0`, `1` at `p1`).
    Linear,
    /// A radial gradient centered at `p0` with radius `p1.x`: `t` is the
    /// distance from the center over the radius.
    Radial,
    /// A sweep (angular/conic) gradient centered at `p0` starting at angle
    /// `p1.x` (radians): `t` is the normalized turn `[0, 1)` around the center.
    Sweep,
}

impl GradientKind {
    /// The `u32` the [`GradientInstance`] carries (0=linear, 1=radial,
    /// 2=sweep) — the shader/headless dispatch code.
    const fn as_u32(self) -> u32 {
        match self {
            GradientKind::Linear => 0,
            GradientKind::Radial => 1,
            GradientKind::Sweep => 2,
        }
    }
}

/// One color stop of a [`Gradient`]: a fill color at a normalized position
/// along the ramp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GradientStop {
    /// Position along the ramp in `[0, 1]` (`0` = start, `1` = end).
    pub offset: f32,
    /// Straight linear RGBA color at this position.
    pub color: Rgba,
}

/// A gradient fill over an axis-aligned rectangle.
///
/// The fill covers `rect`; the color at each pixel comes from evaluating the
/// gradient parameter `t` (per [`GradientKind`]), applying `extend` for `t`
/// outside `[0, 1]`, then looking up the ramp defined by `stops` interpolated
/// in `interp` space. `p0`/`p1` are in the same physical-pixel space as `rect`
/// and are reused by kind:
///
/// - **linear**: `p0`→`p1` is the gradient axis.
/// - **radial**: `p0` is the center, `p1.x` the radius (`p1.y` unused).
/// - **sweep**: `p0` is the center, `p1.x` the start angle in radians
///   (`p1.y` unused).
///
/// The instance is *not* finalized here: the ramp's storage (an inline 2-stop
/// pair vs. a baked LUT row) is decided during lowering, so [`to_instance`]
/// takes the resolved `lut_v`/`use_lut` the renderer computes after allocating
/// the LUT atlas row.
///
/// [`to_instance`]: Gradient::to_instance
#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    /// The filled rectangle, in physical pixels.
    pub rect: Rect,
    /// Which parameterization (linear/radial/sweep).
    pub kind: GradientKind,
    /// How the ramp extends for `t` outside `[0, 1]`.
    pub extend: ExtendMode,
    /// Gradient origin (linear start / radial center / sweep center).
    pub p0: Point,
    /// Kind-dependent second parameter: linear end / `(radius, _)` /
    /// `(start_angle, _)`.
    pub p1: Point,
    /// The color stops, ordered by ascending `offset`. Two stops lower to an
    /// inline instance pair; three or more bake a LUT row.
    pub stops: Vec<GradientStop>,
    /// The color space the stops are interpolated in (§12.3) — an explicit
    /// choice, never inferred from the target format.
    pub interp: InterpolationSpace,
}

impl Gradient {
    /// Lower this gradient to its GPU instance, given the ramp storage the
    /// renderer resolved during lowering.
    ///
    /// `use_lut` is `1` when the ramp is baked into the LUT atlas (three or more
    /// stops, or a non-linear interpolation space) and `lut_v` is that row's
    /// texture-`v`; `use_lut` is `0` for the inline two-stop fast path, where
    /// `color0`/`color1` carry the two stops **premultiplied** (the shader and
    /// headless fill sample premultiplied and blend without a branch) and
    /// `lut_v` is unused. The single-stop and empty cases are normalized by the
    /// caller before reaching here.
    pub fn to_instance(&self, lut_v: f32, use_lut: bool) -> GradientInstance {
        // Inline path carries the two end stops premultiplied; the LUT path
        // leaves them zeroed (the shader ignores them when use_lut != 0).
        let (color0, color1) = if use_lut {
            ([0.0; 4], [0.0; 4])
        } else {
            let c0 = self
                .stops
                .first()
                .map(|s| s.color)
                .unwrap_or(Rgba::TRANSPARENT);
            let c1 = self
                .stops
                .last()
                .map(|s| s.color)
                .unwrap_or(Rgba::TRANSPARENT);
            let p0 = c0.premultiply();
            let p1 = c1.premultiply();
            ([p0.r, p0.g, p0.b, p0.a], [p1.r, p1.g, p1.b, p1.a])
        };
        GradientInstance {
            rect_pos: [self.rect.x, self.rect.y],
            rect_size: [self.rect.w, self.rect.h],
            kind: self.kind.as_u32(),
            extend: extend_as_u32(self.extend),
            p0: [self.p0.x, self.p0.y],
            p1: [self.p1.x, self.p1.y],
            lut_v,
            use_lut: use_lut as u32,
            color0,
            color1,
        }
    }
}

/// The `u32` an [`ExtendMode`] maps to in the [`GradientInstance`] wire contract
/// (0=clamp, 1=repeat, 2=mirror) — the shader/headless dispatch code.
const fn extend_as_u32(mode: ExtendMode) -> u32 {
    match mode {
        ExtendMode::Clamp => 0,
        ExtendMode::Repeat => 1,
        ExtendMode::Mirror => 2,
    }
}

/// A clip/compositing layer pushed by [`Primitive::Layer`].
///
/// Every following primitive is constrained to `clip` until the matching
/// [`Primitive::LayerEnd`]. `opacity` selects how the layer reaches the screen:
///
/// - `opacity == 1.0`: the layer is a plain rectangular clip container — the
///   subtree draws directly into the current target, bounded by an in-pass
///   hardware scissor. Nested layers intersect their clips.
/// - `opacity < 1.0`: the `Layer..LayerEnd` subtree is rendered into an
///   offscreen texture sized to `clip`, then composited back into the current
///   target as a single textured quad modulated by `opacity`. This makes a
///   whole layer uniformly translucent without double-blending its overlapping
///   contents, at the cost of one offscreen pass per translucent layer.
///
/// `blur_sigma` optionally Gaussian-blurs the layer's own content:
///
/// - `blur_sigma == 0.0` (default): no blur.
/// - `blur_sigma > 0.0`: forces an offscreen pass even at `opacity == 1.0`, and
///   the layer's content is Gaussian-blurred (a separable ladder inserted
///   between the offscreen render and the composite) before compositing.
///
/// `backdrop_sigma` instead blurs what is *behind* the layer (a frosted-glass
/// panel):
///
/// - `backdrop_sigma == 0.0` (default): no backdrop.
/// - `backdrop_sigma > 0.0`: the content already submitted behind this layer is
///   captured into its own render pass over the layer's clip (padded by the
///   blur reach), blurred, and composited under the layer's own content. The
///   capture is an explicit dependency on the producers of that content — never
///   a read of the target this layer draws into — so the layer itself need not
///   go offscreen. Panels that sit over the same backdrop and ask for the same
///   sigma share one capture, one blur ladder, and one set of composites.
///
/// Two backdrop requests are honoured only as far as they can produce a visible
/// difference, and are otherwise silently dropped:
///
/// - a sub-pixel `backdrop_sigma` (at or below the ladder's minimum) plans no
///   blur rung, so the capture would composite the content back unchanged;
/// - a backdrop on a layer nested inside a translucent or blurred layer. Such a
///   layer draws into its parent's offscreen texture, whose pass is closed
///   mid-walk, while a capture's region is only final once the walk ends and no
///   further panel can join its group. The clip and the layer's own content are
///   unaffected; only the frosted backdrop is omitted.
///
/// The offscreen pass is emitted before the main pass (see the render backend's
/// draw-list ordering) and cleared to transparent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerClip {
    /// The clip rectangle, in physical pixels. For a translucent layer this is
    /// also the offscreen texture's extent and world-space origin.
    pub clip: Rect,
    /// Layer opacity in `[0, 1]`. `1.0` clips in-pass; `< 1.0` triggers
    /// offscreen compositing at this opacity.
    pub opacity: f32,
    /// Gaussian blur sigma in physical pixels applied to the layer's own
    /// content. `0.0` (default) means no blur; `> 0.0` forces an offscreen pass
    /// even at `opacity == 1.0` and blurs the content before compositing.
    pub blur_sigma: f32,
    /// Gaussian blur sigma in physical pixels applied to the content *behind*
    /// this layer. `0.0` (default) means no backdrop; `> 0.0` captures the
    /// already-submitted content under `clip` into a dedicated pass, blurs it,
    /// and composites it beneath this layer's own content. Dropped for a
    /// sub-pixel sigma, or on a layer nested inside a translucent/blurred one.
    pub backdrop_sigma: f32,
}

/// A textured image: sample a sub-rect of `texture` into a destination `rect`,
/// modulated by `tint` (a = opacity).
///
/// `uv` is in **normalized** texture coordinates (`0..1` over the full texture),
/// so an atlas caller passes the sub-region occupied by its image; a whole-image
/// draw passes `Rect { x: 0, y: 0, w: 1, h: 1 }`. Carrying an explicit UV
/// sub-rect (rather than deriving it from a corner) is what lets the same
/// path serve glyph/atlas sub-regions in the text step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageDraw {
    /// Destination rectangle, in physical pixels.
    pub rect: Rect,
    /// Source sub-rect in normalized texture coordinates (`0..1`).
    pub uv: Rect,
    /// Straight linear RGBA tint (a = opacity); `Rgba` white/1.0 = unmodified.
    pub tint: Rgba,
    /// The texture to sample.
    pub texture: TextureId,
    /// How the texture is sampled (filter + address). The renderer interns this
    /// to a shared sampler; it selects the bind group, not GPU instance data.
    pub sampler: SamplerDesc,
}

impl ImageDraw {
    /// A whole-image draw with the default sampler (bilinear, clamp-to-edge):
    /// `uv` covers the full texture and the tint is unmodified white.
    pub fn new(rect: Rect, texture: TextureId) -> Self {
        Self {
            rect,
            uv: Rect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
            },
            tint: Rgba {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
            texture,
            sampler: SamplerDesc::LINEAR_CLAMP,
        }
    }

    /// Lower this image to its GPU instance.
    pub fn to_instance(&self) -> ImageInstance {
        ImageInstance {
            rect_pos: [self.rect.x, self.rect.y],
            rect_size: [self.rect.w, self.rect.h],
            uv_pos: [self.uv.x, self.uv.y],
            uv_size: [self.uv.w, self.uv.h],
            color: [self.tint.r, self.tint.g, self.tint.b, self.tint.a],
        }
    }
}

/// How a source image is scaled into its destination rect when their aspect
/// ratios differ (§12.4). The solve is pure geometry, done on the CPU at
/// lowering; the GPU only ever sees the resulting [`ImageDraw`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Stretch to fill the destination exactly, ignoring aspect ratio.
    Fill,
    /// Scale uniformly to fit *inside* the destination (letterbox/pillarbox):
    /// the whole image is visible, `align` positions it in the leftover space.
    Contain,
    /// Scale uniformly to *cover* the destination (crop): no empty space, the
    /// overflow is cropped and `align` chooses which part of the source shows.
    Cover,
    /// No scaling (1:1 source pixels); `align` positions the source in the
    /// destination, cropping any overflow.
    None,
}

/// One axis' alignment of a scaled/positioned image within its destination
/// (used by [`Fit::Contain`]/[`Fit::None`] for leftover space, and by
/// [`Fit::Cover`]/[`Fit::None`] to choose the cropped-away side).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// Left / top.
    Start,
    /// Centered.
    Center,
    /// Right / bottom.
    End,
}

impl Align {
    /// Fraction of the leftover (`free`) space placed *before* the content:
    /// `Start` → 0, `Center` → 0.5, `End` → 1. `free` may be negative (content
    /// larger than the box, i.e. a crop), in which case this is the fraction of
    /// the overflow cropped off the leading edge.
    #[inline]
    fn offset(self, free: f32) -> f32 {
        match self {
            Align::Start => 0.0,
            Align::Center => free * 0.5,
            Align::End => free,
        }
    }
}

/// Two-axis alignment: `x` horizontal, `y` vertical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Align2 {
    /// Horizontal alignment.
    pub x: Align,
    /// Vertical alignment.
    pub y: Align,
}

impl Align2 {
    /// Centered on both axes — the common default.
    pub const CENTER: Align2 = Align2 {
        x: Align::Center,
        y: Align::Center,
    };
}

/// The author-facing image: a source region of a texture drawn into a
/// destination rect with a scaling policy, alignment, opacity, and sampler
/// (§12.4/§12.5). It carries pixel-space inputs and solves fit/align (and, for
/// an atlas sub-region, half-texel bleed) on the CPU into a single low-level
/// [`ImageDraw`]; [`ImageInstance`]'s frozen GPU layout is unchanged.
///
/// `src` is the source sub-region in **texture pixels** (`None` = the whole
/// texture). When `src` is a proper sub-region it is treated as an atlas cell:
/// its normalized UV rect is inset by half a texel on every side so bilinear
/// sampling never bleeds a neighbouring cell (§12.6). A `None`/full-texture
/// source is never inset.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageRect {
    /// Source sub-region in texture pixels; `None` = the whole texture.
    pub src: Option<Rect>,
    /// Destination rectangle on screen, in physical pixels.
    pub dest: Rect,
    /// How the source scales into `dest`.
    pub fit: Fit,
    /// Alignment used by `Contain`/`Cover`/`None` to place/crop the source.
    pub align: Align2,
    /// Multiplied into the tint alpha (`1.0` = fully opaque).
    pub opacity: f32,
    /// The texture to sample.
    pub texture: TextureId,
    /// The texture's full dimensions in texels — needed to normalize `src` and
    /// to size the half-texel bleed inset.
    pub tex_size: [u32; 2],
    /// How the texture is sampled.
    pub sampler: SamplerDesc,
}

impl ImageRect {
    /// A whole-texture image drawn into `dest` with `Fill`, centered, opaque,
    /// and the default (bilinear clamp) sampler.
    pub fn new(dest: Rect, texture: TextureId, tex_size: [u32; 2]) -> Self {
        Self {
            src: None,
            dest,
            fit: Fit::Fill,
            align: Align2::CENTER,
            opacity: 1.0,
            texture,
            tex_size,
            sampler: SamplerDesc::LINEAR_CLAMP,
        }
    }

    /// The source region in texture pixels, defaulting to the whole texture.
    fn src_px(&self) -> Rect {
        self.src.unwrap_or(Rect {
            x: 0.0,
            y: 0.0,
            w: self.tex_size[0] as f32,
            h: self.tex_size[1] as f32,
        })
    }

    /// Solve fit/align/bleed into a single low-level [`ImageDraw`] (pure CPU).
    ///
    /// `Fill` maps the source region to `dest` directly. `Contain`/`Cover`/`None`
    /// keep the source aspect ratio and instead move the *drawn rect* (Contain,
    /// which shrinks it inside `dest`) or the *sampled sub-region* (Cover/None,
    /// which crop the source), positioning the leftover/cropped extent by
    /// `align`. The UV rect is then normalized against `tex_size`, with a
    /// half-texel inset applied when `src` is a real atlas sub-region.
    pub fn to_image_draw(&self) -> ImageDraw {
        let src = self.src_px();
        let (tw, th) = (self.tex_size[0] as f32, self.tex_size[1] as f32);

        // Solve the on-screen rect and the sampled source rect (both still in
        // their native units: dest in px, uv_px in texels).
        let (rect, uv_px) = match self.fit {
            Fit::Fill => (self.dest, src),
            Fit::Contain => {
                // Uniform scale to fit inside dest; shrink the drawn rect,
                // sample the full source.
                let scale = (self.dest.w / src.w).min(self.dest.h / src.h);
                let (w, h) = (src.w * scale, src.h * scale);
                let x = self.dest.x + self.align.x.offset(self.dest.w - w);
                let y = self.dest.y + self.align.y.offset(self.dest.h - h);
                (Rect { x, y, w, h }, src)
            }
            Fit::Cover => {
                // Uniform scale to cover dest; fill the rect, crop the source.
                let scale = (self.dest.w / src.w).max(self.dest.h / src.h);
                let (sw, sh) = (self.dest.w / scale, self.dest.h / scale);
                let sx = src.x + self.align.x.offset(src.w - sw);
                let sy = src.y + self.align.y.offset(src.h - sh);
                (
                    self.dest,
                    Rect {
                        x: sx,
                        y: sy,
                        w: sw,
                        h: sh,
                    },
                )
            }
            Fit::None => {
                // 1:1 source pixels; fill the rect, crop/position the source by
                // the destination extent measured in texels.
                let sx = src.x + self.align.x.offset(src.w - self.dest.w);
                let sy = src.y + self.align.y.offset(src.h - self.dest.h);
                (
                    self.dest,
                    Rect {
                        x: sx,
                        y: sy,
                        w: self.dest.w,
                        h: self.dest.h,
                    },
                )
            }
        };

        // Normalize the sampled rect to 0..1, insetting half a texel per side
        // for a real atlas sub-region so bilinear taps never reach a neighbour.
        let mut u0 = uv_px.x / tw;
        let mut v0 = uv_px.y / th;
        let mut uw = uv_px.w / tw;
        let mut vh = uv_px.h / th;
        if self.src.is_some() {
            let (hx, hy) = (0.5 / tw, 0.5 / th);
            u0 += hx;
            v0 += hy;
            uw -= 2.0 * hx;
            vh -= 2.0 * hy;
        }

        ImageDraw {
            rect,
            uv: Rect {
                x: u0,
                y: v0,
                w: uw,
                h: vh,
            },
            tint: Rgba {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: self.opacity,
            },
            texture: self.texture,
            sampler: self.sampler,
        }
    }
}

/// A single sprite lifted from an atlas: the `region` (in atlas texels) drawn
/// into `dest` (§12.6). This is just the ergonomic name for an [`ImageRect`]
/// whose source is an atlas cell — it carries the same fit/align/opacity and
/// inherits the half-texel bleed guard, so it never samples a neighbour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpriteRegion {
    /// The atlas texture.
    pub atlas: TextureId,
    /// The atlas' full dimensions in texels.
    pub tex_size: [u32; 2],
    /// The sprite's cell within the atlas, in texels.
    pub region: Rect,
    /// Destination rectangle on screen, in physical pixels.
    pub dest: Rect,
    /// How the sprite scales into `dest`.
    pub fit: Fit,
    /// Alignment used by `Contain`/`Cover`/`None`.
    pub align: Align2,
    /// Multiplied into the tint alpha.
    pub opacity: f32,
    /// How the atlas is sampled.
    pub sampler: SamplerDesc,
}

impl SpriteRegion {
    /// A sprite cell stretched to fill `dest`, opaque, bilinear-clamped.
    pub fn new(atlas: TextureId, tex_size: [u32; 2], region: Rect, dest: Rect) -> Self {
        Self {
            atlas,
            tex_size,
            region,
            dest,
            fit: Fit::Fill,
            align: Align2::CENTER,
            opacity: 1.0,
            sampler: SamplerDesc::LINEAR_CLAMP,
        }
    }

    /// The equivalent [`ImageRect`] (`src = region`), from which the low-level
    /// draw and bleed guard follow.
    pub fn to_image_rect(&self) -> ImageRect {
        ImageRect {
            src: Some(self.region),
            dest: self.dest,
            fit: self.fit,
            align: self.align,
            opacity: self.opacity,
            texture: self.atlas,
            tex_size: self.tex_size,
            sampler: self.sampler,
        }
    }

    /// Solve straight to a low-level [`ImageDraw`].
    pub fn to_image_draw(&self) -> ImageDraw {
        self.to_image_rect().to_image_draw()
    }
}

/// A nine-patch image: a `region` split by `insets` into 4 fixed corners, 4
/// single-axis-stretched edges, and a two-axis-stretched center, drawn into
/// `dest` (§12.6). Expanded on the CPU into 9 [`ImageDraw`]s over the Image
/// family — no new pipeline. `insets` is `[left, top, right, bottom]` in the
/// source's own texels; the same absolute inset widths are preserved on screen
/// (corners never scale), and only the interior stretches.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NineSlice {
    /// The texture.
    pub texture: TextureId,
    /// The texture's full dimensions in texels.
    pub tex_size: [u32; 2],
    /// The source region to slice, in texels.
    pub region: Rect,
    /// Border insets `[left, top, right, bottom]` in source texels.
    pub insets: [f32; 4],
    /// Destination rectangle on screen, in physical pixels.
    pub dest: Rect,
    /// How the texture is sampled.
    pub sampler: SamplerDesc,
}

impl NineSlice {
    /// A nine-patch over the whole texture with uniform `inset` on all sides.
    pub fn new(texture: TextureId, tex_size: [u32; 2], dest: Rect, inset: f32) -> Self {
        Self {
            texture,
            tex_size,
            region: Rect {
                x: 0.0,
                y: 0.0,
                w: tex_size[0] as f32,
                h: tex_size[1] as f32,
            },
            insets: [inset; 4],
            dest,
            sampler: SamplerDesc::LINEAR_CLAMP,
        }
    }

    /// Expand into the 9 patches (row-major: top row, middle row, bottom row).
    ///
    /// Each patch is a [`SpriteRegion`] with `Fit::Fill`, so every source
    /// sub-cell carries the atlas half-texel bleed guard independently. Patches
    /// with a zero-sized source or destination are skipped, so a degenerate
    /// inset (edge wider than the region/dest) simply drops that patch rather
    /// than producing a flipped rect.
    pub fn to_image_draws(&self) -> Vec<ImageDraw> {
        let [il, it, ir, ib] = self.insets;
        let r = self.region;
        let d = self.dest;

        // Source column x-edges and row y-edges (texels).
        let sx = [r.x, r.x + il, r.x + r.w - ir, r.x + r.w];
        let sy = [r.y, r.y + it, r.y + r.h - ib, r.y + r.h];
        // Destination edges: corners keep the source inset size, the center
        // absorbs the remaining space.
        let dx = [d.x, d.x + il, d.x + d.w - ir, d.x + d.w];
        let dy = [d.y, d.y + it, d.y + d.h - ib, d.y + d.h];

        let mut out = Vec::with_capacity(9);
        for row in 0..3 {
            for col in 0..3 {
                let (sw, sh) = (sx[col + 1] - sx[col], sy[row + 1] - sy[row]);
                let (dw, dh) = (dx[col + 1] - dx[col], dy[row + 1] - dy[row]);
                if sw <= 0.0 || sh <= 0.0 || dw <= 0.0 || dh <= 0.0 {
                    continue;
                }
                out.push(
                    SpriteRegion {
                        atlas: self.texture,
                        tex_size: self.tex_size,
                        region: Rect {
                            x: sx[col],
                            y: sy[row],
                            w: sw,
                            h: sh,
                        },
                        dest: Rect {
                            x: dx[col],
                            y: dy[row],
                            w: dw,
                            h: dh,
                        },
                        fit: Fit::Fill,
                        align: Align2::CENTER,
                        opacity: 1.0,
                        sampler: self.sampler,
                    }
                    .to_image_draw(),
                );
            }
        }
        out
    }
}

/// A repeating tile: `tile` (a source cell in texels) laid out across `dest`
/// (§12.6). The fast path is a **single** [`ImageDraw`] whose UV rect exceeds
/// `0..1` and relies on a `Repeat`/`Mirror` sampler to wrap — one instance, no
/// CPU expansion. That fast path only holds for a whole-texture tile (`tile`
/// covering the full texture); an atlas sub-cell cannot use wrap-around
/// sampling without bleeding into neighbours, so it expands on the CPU into one
/// [`ImageDraw`] per repetition instead.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TiledImage {
    /// The texture.
    pub texture: TextureId,
    /// The texture's full dimensions in texels.
    pub tex_size: [u32; 2],
    /// The tile cell within the texture, in texels.
    pub tile: Rect,
    /// Destination rectangle on screen, in physical pixels.
    pub dest: Rect,
    /// How the texture is sampled — `address` should be `Repeat` or `Mirror`
    /// for the single-instance fast path to wrap correctly.
    pub sampler: SamplerDesc,
}

impl TiledImage {
    /// Tile the whole texture across `dest` at 1:1 source size, wrapping with a
    /// `Repeat` sampler.
    pub fn new(texture: TextureId, tex_size: [u32; 2], dest: Rect) -> Self {
        Self {
            texture,
            tex_size,
            tile: Rect {
                x: 0.0,
                y: 0.0,
                w: tex_size[0] as f32,
                h: tex_size[1] as f32,
            },
            dest,
            sampler: SamplerDesc {
                filter: FilterMode::Linear,
                address: AddressMode::Repeat,
            },
        }
    }

    /// Whether the tile is the whole texture — the condition for the
    /// single-instance wrap fast path.
    fn tile_is_whole_texture(&self) -> bool {
        let (tw, th) = (self.tex_size[0] as f32, self.tex_size[1] as f32);
        self.tile.x == 0.0 && self.tile.y == 0.0 && self.tile.w == tw && self.tile.h == th
    }

    /// Expand into the draws that fill `dest`.
    ///
    /// Whole-texture tiles take the one-instance wrap fast path: a single
    /// [`ImageDraw`] whose UV rect is `dest / tile` so the sampler's
    /// `Repeat`/`Mirror` address replicates it. An atlas sub-cell instead
    /// expands to one draw per whole/partial repetition (the trailing
    /// row/column is UV-cropped) so it never wraps into a neighbouring cell.
    pub fn to_image_draws(&self) -> Vec<ImageDraw> {
        let (tw, th) = (self.tex_size[0] as f32, self.tex_size[1] as f32);
        if self.tile_is_whole_texture() {
            // Fast path: sampler wraps a UV rect larger than 0..1.
            return vec![ImageDraw {
                rect: self.dest,
                uv: Rect {
                    x: 0.0,
                    y: 0.0,
                    w: self.dest.w / self.tile.w,
                    h: self.dest.h / self.tile.h,
                },
                tint: Rgba {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
                texture: self.texture,
                sampler: self.sampler,
            }];
        }

        // Atlas sub-cell: CPU-expand, cropping the trailing partial tiles.
        let (u0, v0) = (self.tile.x / tw, self.tile.y / th);
        let (uw, vh) = (self.tile.w / tw, self.tile.h / th);
        let cols = (self.dest.w / self.tile.w).ceil() as usize;
        let rows = (self.dest.h / self.tile.h).ceil() as usize;
        let mut out = Vec::with_capacity(cols.saturating_mul(rows));
        for row in 0..rows {
            for col in 0..cols {
                let px = self.dest.x + col as f32 * self.tile.w;
                let py = self.dest.y + row as f32 * self.tile.h;
                // Clip the trailing tile to the destination edge.
                let dw = (self.dest.x + self.dest.w - px).min(self.tile.w);
                let dh = (self.dest.y + self.dest.h - py).min(self.tile.h);
                if dw <= 0.0 || dh <= 0.0 {
                    continue;
                }
                let frac_w = dw / self.tile.w;
                let frac_h = dh / self.tile.h;
                out.push(ImageDraw {
                    rect: Rect {
                        x: px,
                        y: py,
                        w: dw,
                        h: dh,
                    },
                    uv: Rect {
                        x: u0,
                        y: v0,
                        w: uw * frac_w,
                        h: vh * frac_h,
                    },
                    tint: Rgba {
                        r: 1.0,
                        g: 1.0,
                        b: 1.0,
                        a: 1.0,
                    },
                    texture: self.texture,
                    sampler: self.sampler,
                });
            }
        }
        out
    }
}

/// How the renderer should back an image's texels (§12): a caller/upper-layer
/// hint that selects the texture-resource strategy, resolved on the cold
/// upload path (never per frame).
///
/// This round carries the hint and its dispatch surface; the actual atlas
/// packer and external-image platform paths are follow-ups. `External` is a
/// declared-but-unbuilt path — [`ResourcePolicy::resolve`] returns an explicit
/// error for it rather than silently dropping the draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourcePolicy {
    /// Small, immutable UI art — a candidate for atlas packing so many such
    /// images share one texture and one draw. `mipmap` requests a mip chain
    /// (worthwhile when the image is drawn heavily minified).
    AtlasCandidate {
        /// Whether to build/sample a mip chain for this image.
        mipmap: bool,
    },
    /// Large or frequently replaced content — kept in its own texture rather
    /// than packed into an atlas.
    Standalone {
        /// Whether to build/sample a mip chain for this image.
        mipmap: bool,
    },
    /// Platform-provided image (video frame, camera feed). The platform image
    /// path is not built this round; resolving this policy is an explicit
    /// error, never a silent no-op.
    External,
}

/// The texture-resource route [`ResourcePolicy`] resolves to. Consumed by the
/// renderer's cold upload path to choose how an image is backed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceRoute {
    /// Pack into a shared atlas when possible; `mipmap` carries the mip hint.
    Atlas {
        /// Whether a mip chain was requested.
        mipmap: bool,
    },
    /// Give the image its own texture; `mipmap` carries the mip hint.
    Standalone {
        /// Whether a mip chain was requested.
        mipmap: bool,
    },
}

/// Why a [`ResourcePolicy`] could not be resolved to a [`ResourceRoute`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceRouteError {
    /// [`ResourcePolicy::External`] was requested but the platform external-image
    /// path is not implemented yet.
    ExternalUnsupported,
}

impl ResourcePolicy {
    /// The default for author-facing images: an atlas candidate without mips.
    pub const DEFAULT: ResourcePolicy = ResourcePolicy::AtlasCandidate { mipmap: false };

    /// Resolve to a concrete texture route, or an error for a route that is
    /// declared but not built this round.
    pub fn resolve(self) -> Result<ResourceRoute, ResourceRouteError> {
        match self {
            ResourcePolicy::AtlasCandidate { mipmap } => Ok(ResourceRoute::Atlas { mipmap }),
            ResourcePolicy::Standalone { mipmap } => Ok(ResourceRoute::Standalone { mipmap }),
            ResourcePolicy::External => Err(ResourceRouteError::ExternalUnsupported),
        }
    }
}

impl Default for ResourcePolicy {
    fn default() -> Self {
        ResourcePolicy::DEFAULT
    }
}

/// One raster-backed glyph of a [`GlyphRunDraw`]: where it lands on screen and
/// which representation-pool sub-rect holds its pixels.
///
/// The text subsystem ([`viso_text`]) computes these — screen rect and atlas UV
/// are already resolved — so the renderer never re-runs layout. This run uses
/// exact single-channel A8 coverage; scalable, vector, and color representations
/// are retained in their own pools and lower through their matching primitives.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphInstanceData {
    /// Destination rectangle on screen, in physical pixels.
    pub rect: Rect,
    /// Source sub-rect in the atlas, in normalized texture coordinates (`0..1`).
    pub uv: Rect,
}

/// A run of shaped glyphs sharing one atlas texture and one color.
///
/// The glyphs are pre-laid-out by [`viso_text`]; the run carries them as flat
/// per-glyph instance data plus the atlas [`TextureId`] they sample. The whole
/// run is a single color (`color`) — Phase 2 does not support per-glyph color.
#[derive(Debug, Clone, PartialEq)]
pub struct GlyphRunDraw {
    /// The positioned glyphs, one screen quad each.
    pub glyphs: Vec<GlyphInstanceData>,
    /// The single-channel A8 coverage atlas the glyphs sample.
    pub atlas: TextureId,
    /// Straight linear RGBA color applied to the entire run (a = opacity).
    pub color: Rgba,
}

impl GlyphRunDraw {
    /// Lower one glyph to its GPU instance, applying the run's color.
    pub fn instance(&self, glyph: &GlyphInstanceData) -> GlyphInstance {
        GlyphInstance {
            rect_pos: [glyph.rect.x, glyph.rect.y],
            rect_size: [glyph.rect.w, glyph.rect.h],
            uv_pos: [glyph.uv.x, glyph.uv.y],
            uv_size: [glyph.uv.w, glyph.uv.h],
            color: [self.color.r, self.color.g, self.color.b, self.color.a],
        }
    }
}

/// A 2D point in physical pixels, top-left origin. The building block of
/// [`Path`] commands and mesh vertices.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    /// x in physical pixels.
    pub x: f32,
    /// y in physical pixels.
    pub y: f32,
}

impl Point {
    /// Construct a point.
    pub const fn new(x: f32, y: f32) -> Point {
        Point { x, y }
    }
}

/// One command of a [`Path`] outline. Curves are flattened to line segments by
/// the tessellator (De Casteljau, `tolerance`-bounded). Coordinates are physical
/// pixels, top-left origin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathCmd {
    /// Begin a new subpath at this point.
    MoveTo(Point),
    /// Straight line from the current point.
    LineTo(Point),
    /// Quadratic Bézier: one control point, then the endpoint.
    QuadTo(Point, Point),
    /// Cubic Bézier: two control points, then the endpoint.
    CubicTo(Point, Point, Point),
    /// Close the current subpath (line back to its start).
    Close,
}

/// How consecutive stroke segments are joined at a corner.
///
/// A miter join extends the outer edges to a sharp point, falling back to a
/// bevel when the miter length exceeds `miter_limit × half_width`; a bevel join
/// always cuts the corner flat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineJoin {
    /// Sharp corner, bevel fallback past the miter limit.
    Miter,
    /// Flat-cut corner.
    Bevel,
    /// Rounded corner (arc between the outer edges).
    Round,
}

/// How a stroked line's endpoints are shaped.
///
/// A butt cap ends flush at the endpoint; a square cap projects the stroke half
/// its width past the endpoint; a round cap adds a semicircle of that radius.
/// Applies to the open endpoints of a [`Path`] stroke and to each dash segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineCap {
    /// Flush end at the endpoint.
    Butt,
    /// Squared end projecting half the width past the endpoint.
    Square,
    /// Semicircular end of radius half the width.
    Round,
}

impl LineCap {
    /// The `u32` a stroke cap maps to (matches the analytic shader/headless
    /// dispatch: 0=butt, 1=square, 2=round).
    const fn as_u32(self) -> u32 {
        match self {
            LineCap::Butt => 0,
            LineCap::Square => 1,
            LineCap::Round => 2,
        }
    }
}

impl LineJoin {
    /// The `u32` a stroke join maps to (0=miter, 1=bevel, 2=round).
    const fn as_u32(self) -> u32 {
        match self {
            LineJoin::Miter => 0,
            LineJoin::Bevel => 1,
            LineJoin::Round => 2,
        }
    }
}

/// Where a stroke sits relative to the outline it traces.
///
/// Only meaningful for closed subpaths, which have a defined inside/outside; an
/// open subpath has no such distinction and is always stroked centered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StrokeAlign {
    /// Centered on the outline: half the width falls on each side.
    #[default]
    Center,
    /// Entirely inside a closed outline.
    Inner,
    /// Entirely outside a closed outline.
    Outer,
}

impl StrokeAlign {
    /// A stable discriminant for structural fingerprinting.
    const fn as_u32(self) -> u32 {
        match self {
            StrokeAlign::Center => 0,
            StrokeAlign::Inner => 1,
            StrokeAlign::Outer => 2,
        }
    }

    /// The stroke rail offsets `(left, right)` along the left normal for this
    /// alignment and half-width, given whether the subpath is closed. Open
    /// subpaths have no inside/outside and always stroke centered.
    fn rails(self, hw: f32, closed: bool) -> (f32, f32) {
        match self {
            StrokeAlign::Center => (hw, -hw),
            StrokeAlign::Inner if closed => (0.0, -2.0 * hw),
            StrokeAlign::Outer if closed => (2.0 * hw, 0.0),
            _ => (hw, -hw),
        }
    }
}

/// The maximum number of dash on/off lengths carried inline.
pub const DASH_SEGMENTS_MAX: usize = 4;

/// A dash pattern: alternating on/off run lengths (in physical pixels) that the
/// stroke is chopped into, plus a phase offset.
///
/// Lengths are kept inline (up to [`DASH_SEGMENTS_MAX`]) so [`Stroke`] stays
/// `Copy`; `len` is how many of `segments` are used (`1..=DASH_SEGMENTS_MAX`).
/// A single-entry pattern is treated as `[on, on]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DashPattern {
    /// On/off run lengths; the first is an "on" run.
    pub segments: [f32; DASH_SEGMENTS_MAX],
    /// How many entries of `segments` are valid (`1..=DASH_SEGMENTS_MAX`).
    pub len: u8,
    /// Phase offset into the pattern before the first dash.
    pub offset: f32,
}

impl DashPattern {
    /// Build a dash pattern from a slice of run lengths (clamped to
    /// [`DASH_SEGMENTS_MAX`]) and a phase offset.
    pub fn new(lengths: &[f32], offset: f32) -> Self {
        let mut segments = [0.0; DASH_SEGMENTS_MAX];
        let len = lengths.len().min(DASH_SEGMENTS_MAX);
        segments[..len].copy_from_slice(&lengths[..len]);
        DashPattern {
            segments,
            len: len as u8,
            offset,
        }
    }

    /// The valid run lengths.
    fn runs(&self) -> &[f32] {
        &self.segments[..self.len as usize]
    }
}

/// A stroke (outline) applied to a [`Path`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stroke {
    /// Stroke width in physical pixels.
    pub width: f32,
    /// Straight linear RGBA stroke color.
    pub color: Rgba,
    /// How open endpoints (and dash-segment ends) are shaped.
    pub cap: LineCap,
    /// How corners are joined.
    pub join: LineJoin,
    /// Miter length limit as a multiple of the half-width; past it a miter join
    /// falls back to a bevel.
    pub miter_limit: f32,
    /// Where the stroke sits relative to the outline (closed subpaths only).
    pub align: StrokeAlign,
    /// Optional dash pattern; `None` is a solid stroke.
    pub dash: Option<DashPattern>,
    /// When set, ignore `width` and draw a one-device-pixel hairline.
    pub hairline: bool,
}

impl Stroke {
    /// A solid, centered, butt-capped, miter-joined stroke of the given width
    /// and color (miter limit 4.0, no dash, not a hairline).
    pub fn new(width: f32, color: Rgba) -> Self {
        Stroke {
            width,
            color,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter_limit: DEFAULT_MITER_LIMIT,
            align: StrokeAlign::Center,
            dash: None,
            hairline: false,
        }
    }
}

/// A filled and/or stroked vector path.
///
/// The CPU tessellator flattens curves, fan-triangulates the fill (assuming a
/// simple, roughly convex outline), and expands the stroke into segment quads
/// with miter/bevel/round joins and butt/square/round caps, adding a 1px
/// coverage-AA fringe. It does not implement even-odd/nonzero winding fills of
/// self-intersecting outlines.
#[derive(Debug, Clone, PartialEq)]
pub struct Path {
    /// The outline commands.
    pub cmds: Vec<PathCmd>,
    /// Fill color, if the interior is painted.
    pub fill: Option<Rgba>,
    /// Stroke, if the outline is painted (drawn over the fill).
    pub stroke: Option<Stroke>,
    /// A soft shadow cast by this path's silhouette, if any (§15.4/§20.2). Drawn
    /// under the path via the tight-mask fallback lane — the path's own coverage
    /// mask, offset and tinted; no analytic SDF exists for an arbitrary outline.
    pub shadow: Option<PathShadow>,
}

/// A soft shadow cast by an arbitrary [`Path`] silhouette (§15.4/§20.2).
///
/// Unlike [`AnalyticShadow`] there is no closed-form SDF for a general outline, so
/// this lowers through the tight-coverage-mask fallback: the path's own coverage
/// is rasterized to one R8 mask (the same mask lane a self-masked fill uses),
/// then composited under the path, displaced by `offset` and tinted by `color`.
/// The unshaded mask is keyed on `{geometry, sigma}` and reused across frames when
/// only `color`/`offset` change (§20.2).
///
/// `sigma` is recorded and folds into the mask's cache key so the E1 separable
/// blur drops in without re-keying; E0's minimal fallback composites the sharp
/// mask (no convolution pass — full ROI/blur is E1). `inner` selects an inner
/// shadow, which E0 routes through the same mask lane (§20.3 general-path side).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathShadow {
    /// Shadow color (straight linear RGBA; `a` scales the whole ramp).
    pub color: Rgba,
    /// Shadow displacement in physical pixels.
    pub offset: [f32; 2],
    /// Blur standard deviation in pixels (§15.2 blur-radius semantic). Recorded in
    /// the mask cache key; E0 composites the sharp mask, E1 applies the blur.
    pub sigma: f32,
    /// Silhouette grow (`+`) / shrink (`-`) in pixels. Recorded in the key; the E0
    /// fallback does not yet dilate the mask (E1), so it affects the key only.
    pub spread: f32,
    /// `false` = drop shadow under the path; `true` = inner shadow (§20.3).
    pub inner: bool,
}

/// A colored triangle mesh supplied directly by the caller (no tessellation).
///
/// `vertices`/`indices` are consumed as-is: each vertex carries a position, a
/// straight linear color, and an AA `edge` weight (`1` interior, `0` fringe),
/// exactly matching [`MeshVertex`]. This is the escape hatch for geometry Viso's
/// higher-level primitives don't cover; [`Path`] lowers into the same buffers.
#[derive(Debug, Clone, PartialEq)]
pub struct Mesh {
    /// The mesh vertices.
    pub vertices: Vec<MeshVertex>,
    /// Triangle-list indices into `vertices` (3 per triangle).
    pub indices: Vec<u32>,
}

/// Renderer-facing primitive. This expresses *only* what the renderer needs —
/// it is neither a component nor a node. One node may emit several
/// primitives; primitives of the same kind may batch into one draw call.
///
/// The primitive stream is **flat**: [`Primitive::Layer`] pushes a clip and
/// [`Primitive::LayerEnd`] pops it (a push/pop clip stack). Nested layers intersect their
/// clip rects. This keeps `Vec<Primitive>` batchable rather than a recursive
/// tree.
///
/// Every variant carries its draw data.
#[derive(Debug, Clone, PartialEq)]
pub enum Primitive {
    /// A rounded/bordered rectangle.
    Quad(Quad),
    /// A rounded rectangle with an independent radius per corner.
    AnalyticRRect(AnalyticRRect),
    /// An axis-aligned ellipse/circle.
    AnalyticEllipse(AnalyticEllipse),
    /// A capsule/stadium (rounded box, corner radius = smaller half-extent).
    AnalyticCapsule(AnalyticCapsule),
    /// A stroked line segment defined by two endpoints, with cap/join.
    AnalyticLine(AnalyticLine),
    /// A run of shaped glyphs sampling a single-channel A8 coverage atlas.
    GlyphRun(GlyphRunDraw),
    /// A textured image sampled into a rect.
    Image(ImageDraw),
    /// A linear/radial/sweep gradient fill over an axis-aligned rect.
    Gradient(Gradient),
    /// A soft drop shadow for an analytic shape, drawn by the §15 fast lane.
    AnalyticShadow(AnalyticShadow),
    /// A filled/stroked vector path.
    Path(Path),
    /// A colored triangle mesh.
    Mesh(Mesh),
    /// Push a clip/compositing layer. Following primitives are constrained to
    /// its clip (intersected with any enclosing layer's clip) until the matching
    /// [`Primitive::LayerEnd`]. A `LayerClip::opacity < 1` layer additionally
    /// renders its subtree offscreen and composites it back at that opacity.
    Layer(LayerClip),
    /// Pop the most recent [`Primitive::Layer`] clip (and, for a translucent
    /// layer, close its offscreen pass and emit the composite).
    LayerEnd,
}

/// GPU instance for the Quad built-in shader.
///
/// Field names/formats match [`quad_schema`] and the headless `fill_quad`
/// reader. Colors are **straight** (non-premultiplied) linear RGBA — the
/// backend premultiplies. `#[repr(C)]` with only 4-byte-aligned scalars, so the
/// derive's `offset_of!`-based layout has no padding surprises.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, GpuPod)]
pub struct QuadInstance {
    /// Top-left corner in physical pixels.
    pub rect_pos: [f32; 2],
    /// Width/height in physical pixels.
    pub rect_size: [f32; 2],
    /// Straight linear RGBA fill.
    pub color: [f32; 4],
    /// Corner radius in pixels.
    pub radius: f32,
    /// Border stroke width in pixels (0 = none).
    pub border_width: f32,
    /// Straight linear RGBA border color.
    pub border_color: [f32; 4],
}

/// GPU instance for the AnalyticRRect built-in shader.
///
/// Field names/formats match [`analytic_rrect_schema`] and the headless
/// `fill_analytic_rrect` reader. Identical to [`QuadInstance`] except `radius` is
/// a per-corner `[f32; 4]` (`left_top`, `right_top`, `right_bottom`,
/// `left_bottom`) rather than a scalar. Colors are **straight** (non-premultiplied)
/// linear RGBA — the backend premultiplies. `#[repr(C)]` with only 4-byte-aligned
/// scalars/vectors, so the derive's `offset_of!`-based layout has no padding
/// surprises.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, GpuPod)]
pub struct AnalyticRRectInstance {
    /// Top-left corner in physical pixels.
    pub rect_pos: [f32; 2],
    /// Width/height in physical pixels.
    pub rect_size: [f32; 2],
    /// Straight linear RGBA fill.
    pub color: [f32; 4],
    /// Per-corner radii in pixels (`left_top`, `right_top`, `right_bottom`,
    /// `left_bottom`).
    pub radius: [f32; 4],
    /// Border stroke width in pixels (0 = none).
    pub border_width: f32,
    /// Straight linear RGBA border color.
    pub border_color: [f32; 4],
}

/// GPU instance for the AnalyticEllipse built-in shader.
///
/// Field names/formats match [`analytic_ellipse_schema`] and the headless
/// `fill_analytic_ellipse` reader. The ellipse radii are derived from
/// `rect_size * 0.5` in the vertex stage, so there is no radius field. Colors are
/// **straight** (non-premultiplied) linear RGBA — the backend premultiplies.
/// `#[repr(C)]` with only 4-byte-aligned scalars/vectors, so the derive's
/// `offset_of!`-based layout has no padding surprises.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, GpuPod)]
pub struct AnalyticEllipseInstance {
    /// Top-left corner in physical pixels.
    pub rect_pos: [f32; 2],
    /// Width/height in physical pixels.
    pub rect_size: [f32; 2],
    /// Straight linear RGBA fill.
    pub color: [f32; 4],
    /// Border stroke width in pixels (0 = none).
    pub border_width: f32,
    /// Straight linear RGBA border color.
    pub border_color: [f32; 4],
}

/// GPU instance for the AnalyticCapsule built-in shader.
///
/// Field names/formats match [`analytic_capsule_schema`] and the headless
/// `fill_analytic_capsule` reader. Byte-identical to [`AnalyticEllipseInstance`]:
/// the corner radius is derived as the smaller half-extent in the shader, so
/// there is no radius field. Colors are **straight** (non-premultiplied) linear
/// RGBA — the backend premultiplies. `#[repr(C)]` with only 4-byte-aligned
/// scalars/vectors, so the derive's `offset_of!`-based layout has no padding
/// surprises.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, GpuPod)]
pub struct AnalyticCapsuleInstance {
    /// Top-left corner in physical pixels.
    pub rect_pos: [f32; 2],
    /// Width/height in physical pixels.
    pub rect_size: [f32; 2],
    /// Straight linear RGBA fill.
    pub color: [f32; 4],
    /// Border stroke width in pixels (0 = none).
    pub border_width: f32,
    /// Straight linear RGBA border color.
    pub border_color: [f32; 4],
}

/// GPU instance for the AnalyticLine built-in shader.
///
/// Field names/formats match [`analytic_line_schema`] and the headless
/// `fill_analytic_line` reader. `cap`/`join` are scalar `u32` enum codes
/// (cap: 0=butt 1=square 2=round; join: 0=miter 1=bevel 2=round). Colors are
/// **straight** (non-premultiplied) linear RGBA — the backend premultiplies.
/// `#[repr(C)]` with only 4-byte-aligned scalars/vectors, so the derive's
/// `offset_of!`-based layout has no padding: `p0`@0, `p1`@8, `width`@16,
/// `color`@20, `cap`@36, `join`@40, `miter_limit`@44, `border_width`@48,
/// `border_color`@52, stride 68.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, GpuPod)]
pub struct AnalyticLineInstance {
    /// Segment start in physical pixels.
    pub p0: [f32; 2],
    /// Segment end in physical pixels.
    pub p1: [f32; 2],
    /// Stroke width in pixels.
    pub width: f32,
    /// Straight linear RGBA fill.
    pub color: [f32; 4],
    /// Cap code: 0=butt, 1=square, 2=round.
    pub cap: u32,
    /// Join code: 0=miter, 1=bevel, 2=round.
    pub join: u32,
    /// Miter limit as a multiple of the half-width.
    pub miter_limit: f32,
    /// Border stroke width in pixels (0 = none).
    pub border_width: f32,
    /// Straight linear RGBA border color.
    pub border_color: [f32; 4],
}

/// GPU instance for the AnalyticShadow built-in shader.
///
/// Field names/formats match [`analytic_shadow_schema`] and the headless
/// `fill_analytic_shadow` reader. `rect_pos`/`rect_size` are the source shape's
/// rect; `radius` is the per-corner rounding used only when `shape == 0`
/// (rounded box). `offset` displaces the shadow, `sigma` is the blur standard
/// deviation, `spread` grows/shrinks the silhouette, and `shape` selects the SDF
/// (0=rounded box, 1=ellipse, 2=capsule). `inner` selects the shadow side
/// (0=outer drop shadow, 1=inner shadow). `color` is **straight**
/// (non-premultiplied) linear RGBA — the backend premultiplies. `#[repr(C)]` with
/// only 4-byte-aligned scalars/vectors, so the derive's `offset_of!`-based layout
/// has no padding surprises.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, GpuPod)]
pub struct ShadowInstance {
    /// Source shape top-left corner in physical pixels.
    pub rect_pos: [f32; 2],
    /// Source shape width/height in physical pixels.
    pub rect_size: [f32; 2],
    /// Straight linear RGBA shadow color.
    pub color: [f32; 4],
    /// Per-corner radii in pixels (`left_top`, `right_top`, `right_bottom`,
    /// `left_bottom`); used only when `shape == 0`.
    pub radius: [f32; 4],
    /// Shadow displacement in physical pixels.
    pub offset: [f32; 2],
    /// Blur standard deviation in pixels.
    pub sigma: f32,
    /// Silhouette grow/shrink in pixels.
    pub spread: f32,
    /// Silhouette code: 0=rounded box, 1=ellipse, 2=capsule.
    pub shape: u32,
    /// Shadow side: 0=outer drop shadow, 1=inner shadow.
    pub inner: u32,
}

/// GPU instance for the Gradient built-in shader.
///
/// Field names/formats match [`gradient_schema`] and the headless
/// `fill_gradient` reader. `kind` is the [`GradientKind`] code (0=linear,
/// 1=radial, 2=sweep) and `extend` the [`ExtendMode`] code (0=clamp, 1=repeat,
/// 2=mirror). `p0`/`p1` are reused by kind — linear axis endpoints, radial
/// `(center, (radius, _))`, sweep `(center, (start_angle, _))`.
///
/// The ramp is resolved during lowering: when `use_lut != 0` the fragment stage
/// samples the LUT-atlas row at `(t, lut_v)`; when `use_lut == 0` it lerps the
/// two inline stops `color0`/`color1`, which — unlike every other instance's
/// straight-linear color — are stored **premultiplied** so the LUT and inline
/// paths blend identically with no branch. `#[repr(C)]` with only 4-byte-aligned
/// scalars/vectors, so the derive's `offset_of!`-based layout has no padding:
/// `rect_pos`@0, `rect_size`@8, `kind`@16, `extend`@20, `p0`@24, `p1`@32,
/// `lut_v`@40, `use_lut`@44, `color0`@48, `color1`@64, stride 80.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, GpuPod)]
pub struct GradientInstance {
    /// Top-left corner of the filled rect in physical pixels.
    pub rect_pos: [f32; 2],
    /// Width/height of the filled rect in physical pixels.
    pub rect_size: [f32; 2],
    /// Kind code: 0=linear, 1=radial, 2=sweep.
    pub kind: u32,
    /// Extend code: 0=clamp, 1=repeat, 2=mirror.
    pub extend: u32,
    /// Gradient origin (linear start / radial center / sweep center), physical
    /// pixels.
    pub p0: [f32; 2],
    /// Kind-dependent second parameter: linear end / `(radius, _)` /
    /// `(start_angle, _)`.
    pub p1: [f32; 2],
    /// The LUT-atlas row's texture-`v` (used only when `use_lut != 0`).
    pub lut_v: f32,
    /// Whether the ramp is a baked LUT row (`1`) or the inline two-stop pair
    /// (`0`).
    pub use_lut: u32,
    /// First inline stop, **premultiplied** linear RGBA (used only when
    /// `use_lut == 0`).
    pub color0: [f32; 4],
    /// Last inline stop, **premultiplied** linear RGBA (used only when
    /// `use_lut == 0`).
    pub color1: [f32; 4],
}

/// GPU instance for the Image built-in shader.
///
/// Field names/formats match [`image_schema`] and the headless `fill_image`
/// reader. `color` is a **straight** (non-premultiplied) linear RGBA tint;
/// the sampled texel is premultiplied (Viso texture convention) and the shader
/// combines them. `#[repr(C)]` with only 8-byte `[f32; 2]`/`[f32; 4]` fields, so
/// the derive's `offset_of!`-based layout has no padding surprises.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, GpuPod)]
pub struct ImageInstance {
    /// Destination top-left in physical pixels.
    pub rect_pos: [f32; 2],
    /// Destination width/height in physical pixels.
    pub rect_size: [f32; 2],
    /// Source sub-rect origin in normalized texture coords.
    pub uv_pos: [f32; 2],
    /// Source sub-rect size in normalized texture coords.
    pub uv_size: [f32; 2],
    /// Straight linear RGBA tint (a = opacity).
    pub color: [f32; 4],
}

/// GPU instance for the Blur built-in shader.
///
/// Field names/formats match [`blur_schema`](viso_shader::blur_schema) and the
/// headless `fill_blur` reader. Like [`ImageInstance`] it draws one
/// `vertex_id`-generated full-target quad that samples a source texture, but
/// instead of a single tap it walks a separable Gaussian ladder: `dir` is the
/// per-tap step in normalized source uv (one axis, `(step, 0)` horizontal or
/// `(0, step)` vertical), `sigma` is the Gaussian standard deviation in source
/// texels, and `radius` is the tap count on each side of center. `#[repr(C)]`
/// with only 4-byte-aligned scalars/vectors, so the derive's `offset_of!`-based
/// layout has no padding — five `[f32; 2]` plus two `f32`, stride 48.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, GpuPod)]
pub struct BlurInstance {
    /// Destination top-left in physical pixels.
    pub rect_pos: [f32; 2],
    /// Destination width/height in physical pixels.
    pub rect_size: [f32; 2],
    /// Source sub-rect origin in normalized texture coords.
    pub uv_pos: [f32; 2],
    /// Source sub-rect size in normalized texture coords.
    pub uv_size: [f32; 2],
    /// Per-tap step in normalized source uv along the blur axis.
    pub dir: [f32; 2],
    /// Gaussian standard deviation, in source texels.
    pub sigma: f32,
    /// Tap count on each side of the center sample.
    pub radius: f32,
}

/// GPU instance for the GlyphRun built-in shader.
///
/// Field names/formats match [`glyphrun_schema`] and the headless `fill_glyph`
/// reader. Structurally identical to [`ImageInstance`]: the sampled texel is
/// exact per-pixel coverage (the single-channel A8 atlas's R channel), which the
/// shader multiplies the run color by directly — no decode factor. `color` is a
/// **straight** (non-premultiplied) linear RGBA; the shader premultiplies.
/// `#[repr(C)]` with only 8-byte `[f32; 2]`/`[f32; 4]` fields, so the derive's
/// `offset_of!`-based layout has no padding surprises.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, GpuPod)]
pub struct GlyphInstance {
    /// Destination top-left in physical pixels.
    pub rect_pos: [f32; 2],
    /// Destination width/height in physical pixels.
    pub rect_size: [f32; 2],
    /// Atlas sub-rect origin in normalized texture coords.
    pub uv_pos: [f32; 2],
    /// Atlas sub-rect size in normalized texture coords.
    pub uv_size: [f32; 2],
    /// Straight linear RGBA color for the whole run (a = opacity).
    pub color: [f32; 4],
}

/// One vertex of the general triangle mesh built-in (shared by [`Path`] and
/// [`Mesh`]).
///
/// Field names/formats match [`mesh_schema`] and the headless mesh reader.
/// `color` is a **straight** (non-premultiplied) linear RGBA; the shader
/// premultiplies. `edge` is a `[0, 1]` coverage weight — `1` at interior
/// vertices, ramping to `0` at antialiased fringe vertices — interpolated across
/// the triangle so the fragment shader gets smooth edge coverage. Unlike the
/// quad/image instance structs this is *per-vertex* data drawn as an indexed
/// triangle list, not a `vertex_id`-generated quad. `#[repr(C)]`, stride 28.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, GpuPod)]
pub struct MeshVertex {
    /// Position in physical pixels, top-left origin.
    pub pos: [f32; 2],
    /// Straight linear RGBA.
    pub color: [f32; 4],
    /// Coverage-AA weight (`1` interior, `0` fringe).
    pub edge: f32,
}

/// A colorless tessellated vertex: position + AA coverage weight, in the path's
/// own local space. This is the retained-geometry vertex — the part of a
/// [`MeshVertex`] that survives a recolor or a transform-only change. Color is
/// applied at lowering time (per-primitive paint), never baked into the cached
/// geometry, so a recolor never re-tessellates. Internal to the tessellator and
/// the path store; not a GPU ABI type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeoVertex {
    /// Position in the path's local space (physical pixels before transform).
    pub pos: [f32; 2],
    /// Coverage-AA weight (`1` interior, `0` fringe).
    pub edge: f32,
}

/// A triangle-list index buffer at the narrowest width that addresses its mesh:
/// `U16` when the vertex count fits in `u16`, else `U32`. Small icon/SVG
/// geometry pays half the index bandwidth and device memory; large geometry
/// stays correct at 32-bit (§13.4). Chosen once when geometry is built, cached
/// with it, never re-decided per frame.
#[derive(Debug, Clone, PartialEq)]
pub enum IndexBuffer {
    /// 16-bit indices (vertex count ≤ `u16::MAX`).
    U16(Vec<u16>),
    /// 32-bit indices.
    U32(Vec<u32>),
}

impl IndexBuffer {
    /// Build the narrowest buffer that addresses `vertex_count` vertices from a
    /// slice of `u32` indices produced during tessellation.
    fn from_u32(indices: &[u32], vertex_count: usize) -> IndexBuffer {
        if vertex_count <= u16::MAX as usize {
            IndexBuffer::U16(indices.iter().map(|&i| i as u16).collect())
        } else {
            IndexBuffer::U32(indices.to_vec())
        }
    }

    /// Number of indices (3 per triangle).
    pub fn len(&self) -> usize {
        match self {
            IndexBuffer::U16(v) => v.len(),
            IndexBuffer::U32(v) => v.len(),
        }
    }

    /// Whether the buffer holds no indices.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Iterate the indices as `u32` regardless of the stored width.
    pub fn iter_u32(&self) -> impl Iterator<Item = u32> + '_ {
        // Two concrete iterators unified into one enum so callers get a single
        // type without boxing.
        enum Iter<'a> {
            U16(std::slice::Iter<'a, u16>),
            U32(std::slice::Iter<'a, u32>),
        }
        impl Iterator for Iter<'_> {
            type Item = u32;
            fn next(&mut self) -> Option<u32> {
                match self {
                    Iter::U16(it) => it.next().map(|&i| i as u32),
                    Iter::U32(it) => it.next().copied(),
                }
            }
        }
        match self {
            IndexBuffer::U16(v) => Iter::U16(v.iter()),
            IndexBuffer::U32(v) => Iter::U32(v.iter()),
        }
    }
}

/// A retained, **colorless** tessellation of a [`Path`], in the path's local
/// space (§13.4). Holds only what is invariant under a recolor or a
/// transform-only change: vertex positions, AA coverage, and the triangle-list
/// index buffer. Fill vertices come first, then stroke vertices; `fill_vert_end`
/// marks the boundary so lowering paints each span with its own color.
///
/// This is the object keyed by a geometry fingerprint + quality bucket: two
/// primitives with the same outline at the same quality share it, and neither a
/// paint change nor a translate/uniform-scale re-runs the tessellator.
#[derive(Debug, Clone, PartialEq)]
pub struct PathGeometry {
    /// Colorless vertices: fill span `[0, fill_vert_end)`, then stroke span.
    pub verts: Vec<GeoVertex>,
    /// Triangle-list indices into `verts` (base-zero, narrowest width).
    pub indices: IndexBuffer,
    /// Index into `verts` where the stroke vertices begin (== fill vertex count).
    pub fill_vert_end: u32,
    /// Local-space bounding rect of the tessellated geometry.
    pub bounds: Rect,
}

impl PathGeometry {
    /// An empty geometry (degenerate/empty path).
    fn empty() -> PathGeometry {
        PathGeometry {
            verts: Vec::new(),
            indices: IndexBuffer::U16(Vec::new()),
            fill_vert_end: 0,
            bounds: Rect::ZERO,
        }
    }
}

/// The tolerance (max chord deviation, physical pixels) used when flattening
/// Bézier curves to line segments.
const FLATTEN_TOLERANCE: f32 = 0.25;

/// Width of the antialiasing fringe, in physical pixels, added around fills and
/// strokes. The fringe vertices carry `edge = 0`; interior vertices `edge = 1`.
const AA_FRINGE: f32 = 1.0;

/// The default miter limit for [`Stroke::new`]: the bevel-fallback threshold
/// for miter joins, as a multiple of the stroke's half-width. Corners sharper
/// than this switch from miter to bevel.
const DEFAULT_MITER_LIMIT: f32 = 4.0;

/// The minimum arc-length step (physical pixels) used when flattening round
/// caps and joins into a triangle fan.
const ROUND_STEP: f32 = 0.5;

impl Path {
    /// Tessellate this path's **geometry** — colorless local-space vertices and
    /// indices (§13.4). Fill vertices are emitted first (span `[0, fill_vert_end)`),
    /// then stroke vertices, so lowering can paint each span with its own color.
    /// The fill/stroke *colors* are ignored here (only their presence matters):
    /// a recolor reuses this result unchanged. Curves flatten at
    /// [`FLATTEN_TOLERANCE`] (the identity-scale bucket 0); the index width is
    /// picked to fit the vertex count. Use [`tessellate_geometry_at`] to
    /// tessellate at a higher quality bucket.
    ///
    /// [`tessellate_geometry_at`]: Self::tessellate_geometry_at
    pub fn tessellate_geometry(&self) -> PathGeometry {
        self.tessellate_geometry_at(0)
    }

    /// Tessellate this path's geometry at quality `bucket` (§13.4). A higher
    /// bucket flattens curves finer — the chord tolerance is
    /// `FLATTEN_TOLERANCE / (bucket + 1)` — so a path drawn at a larger device
    /// scale stays smooth. Bucket 0 matches [`tessellate_geometry`]. Only the
    /// flatten tolerance changes; the emitted layout is unaffected.
    ///
    /// [`tessellate_geometry`]: Self::tessellate_geometry
    pub fn tessellate_geometry_at(&self, bucket: u16) -> PathGeometry {
        let tolerance = FLATTEN_TOLERANCE / (bucket as f32 + 1.0);
        let subpaths = flatten(&self.cmds, tolerance);

        let mut verts: Vec<GeoVertex> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();

        if self.fill.is_some() {
            for sub in &subpaths {
                fill_subpath(&sub.points, &mut verts, &mut indices);
            }
        }
        let fill_vert_end = verts.len() as u32;
        if let Some(stroke) = self.stroke {
            for sub in &subpaths {
                stroke_subpath(&sub.points, sub.closed, stroke, &mut verts, &mut indices);
            }
        }

        if verts.is_empty() {
            return PathGeometry::empty();
        }

        let bounds = geo_bounds(&verts);
        let index_buffer = IndexBuffer::from_u32(&indices, verts.len());
        PathGeometry {
            verts,
            indices: index_buffer,
            fill_vert_end,
            bounds,
        }
    }

    /// Tessellate this path into colored `verts`/`indices` (appended; absolute
    /// indices). Fill is emitted first (so the stroke draws over it). Emitted
    /// vertices use the `MeshVertex` contract: straight color, `edge` coverage.
    /// This is the CPU half of the Path→mesh lowering for callers that bake color
    /// eagerly (the Mesh family and legacy paths); the retained path store keeps
    /// the colorless [`tessellate_geometry`](Self::tessellate_geometry) and paints
    /// at lowering time so a recolor never re-tessellates.
    pub fn tessellate(&self, verts: &mut Vec<MeshVertex>, indices: &mut Vec<u32>) {
        let geo = self.tessellate_geometry();
        let base = verts.len() as u32;
        let fill_col = self.fill.map(rgba_array).unwrap_or([0.0; 4]);
        let stroke_col = self.stroke.map(|s| rgba_array(s.color)).unwrap_or([0.0; 4]);
        for (i, v) in geo.verts.iter().enumerate() {
            let col = if (i as u32) < geo.fill_vert_end {
                fill_col
            } else {
                stroke_col
            };
            verts.push(MeshVertex {
                pos: v.pos,
                color: col,
                edge: v.edge,
            });
        }
        indices.extend(geo.indices.iter_u32().map(|i| base + i));
    }
}

/// Straight linear RGBA as a 4-element array (the `MeshVertex`/shader form).
pub(crate) fn rgba_array(c: Rgba) -> [f32; 4] {
    [c.r, c.g, c.b, c.a]
}

impl Path {
    /// The first anchor point of the outline — the reference the translation-
    /// invariant fingerprint subtracts, and the origin a transform is measured
    /// from. `None` for an empty command list.
    fn anchor(&self) -> Option<Point> {
        self.cmds.iter().find_map(|c| match *c {
            PathCmd::MoveTo(p) | PathCmd::LineTo(p) => Some(p),
            PathCmd::QuadTo(_, p) | PathCmd::CubicTo(_, _, p) => Some(p),
            PathCmd::Close => None,
        })
    }

    /// A translation-invariant structural fingerprint of the outline: the command
    /// kinds and their coordinates relative to [`anchor`](Self::anchor), quantized
    /// to a sub-pixel grid. Two outlines with this same fingerprint are the same
    /// shape up to a pure translation, so they share a tessellation — the offset
    /// between them is carried as a [`PathTransform`], never re-tessellated
    /// (§13.4). Coordinates quantize at `1/16` px, finer than
    /// [`FLATTEN_TOLERANCE`], so a difference that would change the flattened mesh
    /// always changes the fingerprint.
    pub fn geometry_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let (ax, ay) = match self.anchor() {
            Some(p) => (p.x, p.y),
            None => (0.0, 0.0),
        };
        // Whether the interior is painted changes the fill tessellation but not
        // the coordinates; fold presence (not color) into the key.
        self.fill.is_some().hash(&mut h);
        match &self.stroke {
            Some(s) => {
                1u8.hash(&mut h);
                q(s.width).hash(&mut h);
                s.cap.as_u32().hash(&mut h);
                s.join.as_u32().hash(&mut h);
                q(s.miter_limit).hash(&mut h);
                s.align.as_u32().hash(&mut h);
                (s.hairline as u8).hash(&mut h);
                match &s.dash {
                    Some(d) => {
                        1u8.hash(&mut h);
                        d.len.hash(&mut h);
                        for run in d.runs() {
                            q(*run).hash(&mut h);
                        }
                        q(d.offset).hash(&mut h);
                    }
                    None => 0u8.hash(&mut h),
                }
            }
            None => 0u8.hash(&mut h),
        }
        for cmd in &self.cmds {
            match *cmd {
                PathCmd::MoveTo(p) => {
                    0u8.hash(&mut h);
                    q(p.x - ax).hash(&mut h);
                    q(p.y - ay).hash(&mut h);
                }
                PathCmd::LineTo(p) => {
                    1u8.hash(&mut h);
                    q(p.x - ax).hash(&mut h);
                    q(p.y - ay).hash(&mut h);
                }
                PathCmd::QuadTo(c, p) => {
                    2u8.hash(&mut h);
                    q(c.x - ax).hash(&mut h);
                    q(c.y - ay).hash(&mut h);
                    q(p.x - ax).hash(&mut h);
                    q(p.y - ay).hash(&mut h);
                }
                PathCmd::CubicTo(c0, c1, p) => {
                    3u8.hash(&mut h);
                    q(c0.x - ax).hash(&mut h);
                    q(c0.y - ay).hash(&mut h);
                    q(c1.x - ax).hash(&mut h);
                    q(c1.y - ay).hash(&mut h);
                    q(p.x - ax).hash(&mut h);
                    q(p.y - ay).hash(&mut h);
                }
                PathCmd::Close => 4u8.hash(&mut h),
            }
        }
        h.finish()
    }

    /// If `self` is `other` shifted by a pure translation, the offset `self −
    /// other` (the amount to move `other`'s cached geometry to land on `self`);
    /// `None` if the outlines differ by anything but a translation. Requires
    /// identical command sequences and a single constant coordinate delta across
    /// every point (within a sub-pixel epsilon). Uniform scale and rotation are
    /// out of scope this round (they route through a geometry rebuild).
    pub fn translation_from(&self, other: &Path) -> Option<[f32; 2]> {
        if self.cmds.len() != other.cmds.len() {
            return None;
        }
        if self.fill.is_some() != other.fill.is_some() {
            return None;
        }
        match (&self.stroke, &other.stroke) {
            (Some(a), Some(b)) if stroke_geometry_eq(a, b) => {}
            (None, None) => {}
            _ => return None,
        }
        let mut delta: Option<(f32, f32)> = None;
        // A per-coordinate delta must be one constant vector for a pure move.
        let mut check = |sx: f32, sy: f32, ox: f32, oy: f32| -> bool {
            let (dx, dy) = (sx - ox, sy - oy);
            match delta {
                None => {
                    delta = Some((dx, dy));
                    true
                }
                Some((ex, ey)) => (dx - ex).abs() <= 1e-3 && (dy - ey).abs() <= 1e-3,
            }
        };
        for (s, o) in self.cmds.iter().zip(&other.cmds) {
            let ok = match (*s, *o) {
                (PathCmd::MoveTo(sp), PathCmd::MoveTo(op))
                | (PathCmd::LineTo(sp), PathCmd::LineTo(op)) => check(sp.x, sp.y, op.x, op.y),
                (PathCmd::QuadTo(sc, sp), PathCmd::QuadTo(oc, op)) => {
                    check(sc.x, sc.y, oc.x, oc.y) && check(sp.x, sp.y, op.x, op.y)
                }
                (PathCmd::CubicTo(sc0, sc1, sp), PathCmd::CubicTo(oc0, oc1, op)) => {
                    check(sc0.x, sc0.y, oc0.x, oc0.y)
                        && check(sc1.x, sc1.y, oc1.x, oc1.y)
                        && check(sp.x, sp.y, op.x, op.y)
                }
                (PathCmd::Close, PathCmd::Close) => true,
                _ => return None,
            };
            if !ok {
                return None;
            }
        }
        delta.map(|(dx, dy)| [dx, dy])
    }
}

/// Quantize a coordinate to a `1/16`-px grid for structural hashing.
fn q(v: f32) -> i32 {
    (v * 16.0).round() as i32
}

/// Local-space bounding rect over the tessellated (stroke-widened) vertex ring.
///
/// This folds `GeoVertex` — a strided, non-`repr(C)` struct (position + AA
/// weight) — so it is not a contiguous `[Point]` and is a poor SIMD target
/// (gather-bound, no wide contiguous load). It also must fold the *post*-stroke
/// vertices, whose fringe extends past the centerline, so it cannot be replaced
/// by a bound over the input polyline. The contiguous-`[Point]` min/max fold
/// lives in `viso_math::point_bounds` (SIMD-accelerated) for callers that have a
/// real point array, e.g. the SVG import path; this strided fold stays scalar.
fn geo_bounds(verts: &[GeoVertex]) -> Rect {
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for v in verts {
        min_x = min_x.min(v.pos[0]);
        min_y = min_y.min(v.pos[1]);
        max_x = max_x.max(v.pos[0]);
        max_y = max_y.max(v.pos[1]);
    }
    Rect {
        x: min_x,
        y: min_y,
        w: (max_x - min_x).max(0.0),
        h: (max_y - min_y).max(0.0),
    }
}

/// A flattened subpath: a polyline plus whether it is closed.
struct Subpath {
    points: Vec<Point>,
    closed: bool,
}

/// Flatten path commands into subpaths of line segments (De Casteljau, bounded
/// by [`FLATTEN_TOLERANCE`]). Consecutive duplicate points are dropped so the
/// stroker never sees zero-length segments.
fn flatten(cmds: &[PathCmd], tolerance: f32) -> Vec<Subpath> {
    let mut out: Vec<Subpath> = Vec::new();
    let mut cur: Vec<Point> = Vec::new();
    let mut closed = false;
    let mut start = Point::new(0.0, 0.0);

    let push = |cur: &mut Vec<Point>, p: Point| {
        if cur.last().map(|&l| l != p).unwrap_or(true) {
            cur.push(p);
        }
    };

    for &cmd in cmds {
        match cmd {
            PathCmd::MoveTo(p) => {
                if cur.len() >= 2 {
                    out.push(Subpath {
                        points: std::mem::take(&mut cur),
                        closed,
                    });
                } else {
                    cur.clear();
                }
                closed = false;
                start = p;
                cur.push(p);
            }
            PathCmd::LineTo(p) => push(&mut cur, p),
            PathCmd::QuadTo(c, p) => {
                let from = cur.last().copied().unwrap_or(c);
                flatten_quad(from, c, p, tolerance, &mut cur);
            }
            PathCmd::CubicTo(c0, c1, p) => {
                let from = cur.last().copied().unwrap_or(c0);
                flatten_cubic(from, c0, c1, p, tolerance, &mut cur);
            }
            PathCmd::Close => {
                closed = true;
                push(&mut cur, start);
            }
        }
    }
    if cur.len() >= 2 {
        out.push(Subpath {
            points: cur,
            closed,
        });
    }
    out
}

/// Recursively subdivide a quadratic Bézier until it is flat within tolerance,
/// appending the flattened points (excluding the start) to `out`.
fn flatten_quad(p0: Point, p1: Point, p2: Point, tolerance: f32, out: &mut Vec<Point>) {
    // Distance from the control point to the chord; a good flatness proxy.
    let d = point_line_dist(p1, p0, p2);
    if d <= tolerance {
        out.push(p2);
        return;
    }
    let p01 = midpoint(p0, p1);
    let p12 = midpoint(p1, p2);
    let mid = midpoint(p01, p12);
    flatten_quad(p0, p01, mid, tolerance, out);
    flatten_quad(mid, p12, p2, tolerance, out);
}

/// Recursively subdivide a cubic Bézier until it is flat within tolerance,
/// appending the flattened points (excluding the start) to `out`.
fn flatten_cubic(p0: Point, p1: Point, p2: Point, p3: Point, tolerance: f32, out: &mut Vec<Point>) {
    let d = point_line_dist(p1, p0, p3).max(point_line_dist(p2, p0, p3));
    if d <= tolerance {
        out.push(p3);
        return;
    }
    let p01 = midpoint(p0, p1);
    let p12 = midpoint(p1, p2);
    let p23 = midpoint(p2, p3);
    let p012 = midpoint(p01, p12);
    let p123 = midpoint(p12, p23);
    let mid = midpoint(p012, p123);
    flatten_cubic(p0, p01, p012, mid, tolerance, out);
    flatten_cubic(mid, p123, p23, p3, tolerance, out);
}

fn midpoint(a: Point, b: Point) -> Point {
    Point::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5)
}

/// Perpendicular distance from point `p` to the line through `a`,`b`.
fn point_line_dist(p: Point, a: Point, b: Point) -> f32 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-6 {
        let ex = p.x - a.x;
        let ey = p.y - a.y;
        return (ex * ex + ey * ey).sqrt();
    }
    ((p.x - a.x) * dy - (p.y - a.y) * dx).abs() / len
}

/// Fan-triangulate a filled subpath from its centroid, with a 1px coverage-AA
/// fringe around the outline. Assumes a simple, roughly convex polygon (Phase 2
/// scope). No-op for degenerate outlines (< 3 points).
fn fill_subpath(points: &[Point], verts: &mut Vec<GeoVertex>, indices: &mut Vec<u32>) {
    // Drop a duplicated closing point so the outline is a clean ring.
    let ring: &[Point] = match points.split_last() {
        Some((last, head)) if head.first() == Some(last) && head.len() >= 3 => head,
        _ => points,
    };
    if ring.len() < 3 {
        return;
    }

    let cx = ring.iter().map(|p| p.x).sum::<f32>() / ring.len() as f32;
    let cy = ring.iter().map(|p| p.y).sum::<f32>() / ring.len() as f32;
    let center = Point::new(cx, cy);

    // Interior ring: centroid + each outline point (edge = 1, full coverage).
    let center_idx = verts.len() as u32;
    verts.push(geo_vert(center, 1.0));
    let inner_start = verts.len() as u32;
    for &p in ring {
        verts.push(geo_vert(p, 1.0));
    }
    let n = ring.len() as u32;
    for i in 0..n {
        let a = inner_start + i;
        let b = inner_start + (i + 1) % n;
        indices.extend_from_slice(&[center_idx, a, b]);
    }

    // AA fringe: a ring of edge=0 vertices pushed outward along the outward
    // normal, bridged to the interior ring with two triangles per segment.
    let outer_start = verts.len() as u32;
    for i in 0..ring.len() {
        let p = ring[i];
        let normal = outward_normal(ring, i, center);
        let outer = Point::new(p.x + normal.0 * AA_FRINGE, p.y + normal.1 * AA_FRINGE);
        verts.push(geo_vert(outer, 0.0));
    }
    for i in 0..n {
        let j = (i + 1) % n;
        let i0 = inner_start + i;
        let i1 = inner_start + j;
        let o0 = outer_start + i;
        let o1 = outer_start + j;
        indices.extend_from_slice(&[i0, o0, o1, i0, o1, i1]);
    }
}

/// The (approximate) outward unit normal at outline vertex `i`, using the two
/// adjacent edges and disambiguated against the polygon centroid.
fn outward_normal(ring: &[Point], i: usize, center: Point) -> (f32, f32) {
    let n = ring.len();
    let prev = ring[(i + n - 1) % n];
    let p = ring[i];
    let next = ring[(i + 1) % n];
    // Average the two edge directions, take the perpendicular.
    let d0 = norm(p.x - prev.x, p.y - prev.y);
    let d1 = norm(next.x - p.x, next.y - p.y);
    let tx = d0.0 + d1.0;
    let ty = d0.1 + d1.1;
    let (mut nx, mut ny) = norm(-ty, tx);
    // Flip so it points away from the centroid.
    if (p.x - center.x) * nx + (p.y - center.y) * ny < 0.0 {
        nx = -nx;
        ny = -ny;
    }
    (nx, ny)
}

/// Expand a polyline into a stroke: one quad per segment, plus miter/bevel joins
/// at interior vertices, with a coverage-AA fringe along both sides. Butt ends
/// (no caps) for open subpaths.
/// Whether two strokes share the geometry-affecting style (everything but
/// color). Used to route a color-only change to the paint plane while any shape
/// change routes to a geometry rebuild.
fn stroke_geometry_eq(a: &Stroke, b: &Stroke) -> bool {
    a.width == b.width
        && a.cap == b.cap
        && a.join == b.join
        && a.miter_limit == b.miter_limit
        && a.align == b.align
        && a.hairline == b.hairline
        && a.dash == b.dash
}

/// Split a centerline (`ring`, optionally closed) into the "on" runs of a dash
/// pattern. The pattern alternates on/off starting with the first segment;
/// `offset` advances the starting phase. Returns each on-run as its own point
/// list ready to stroke as an open polyline.
fn dash_runs(ring: &[Point], closed: bool, dash: &DashPattern) -> Vec<Vec<Point>> {
    let runs = dash.runs();
    let period: f32 = runs.iter().sum();
    if period <= 0.0 {
        return vec![ring.to_vec()];
    }
    // Unroll a closed ring into an open polyline that revisits the start.
    let mut pts: Vec<Point> = ring.to_vec();
    if closed && ring.len() >= 2 {
        pts.push(ring[0]);
    }
    if pts.len() < 2 {
        return Vec::new();
    }

    // Phase within the pattern; `on` tracks whether the current run paints.
    let mut phase = dash.offset.rem_euclid(period);
    let (mut idx, mut on) = {
        let mut acc = 0.0;
        let mut i = 0;
        let mut painting = true;
        while acc + runs[i] <= phase && i + 1 < runs.len() {
            acc += runs[i];
            i += 1;
            painting = !painting;
        }
        phase -= acc;
        (i, painting)
    };
    let mut remaining = runs[idx] - phase;

    let mut out: Vec<Vec<Point>> = Vec::new();
    let mut cur: Vec<Point> = Vec::new();
    if on {
        cur.push(pts[0]);
    }

    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let seg = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
        if seg <= 0.0 {
            continue;
        }
        let (dx, dy) = ((b.x - a.x) / seg, (b.y - a.y) / seg);
        let mut travelled = 0.0;
        while seg - travelled > remaining {
            travelled += remaining;
            let p = Point::new(a.x + dx * travelled, a.y + dy * travelled);
            if on {
                // End of an on-run: close it at `p`.
                cur.push(p);
                out.push(std::mem::take(&mut cur));
            } else {
                // Start of an on-run: begin a fresh run at `p`.
                cur.push(p);
            }
            // Advance to the next run.
            idx = (idx + 1) % runs.len();
            on = !on;
            remaining = runs[idx];
        }
        remaining -= seg - travelled;
        if on {
            cur.push(b);
        }
    }
    if on && cur.len() >= 2 {
        out.push(cur);
    }
    out
}

fn stroke_subpath(
    points: &[Point],
    closed: bool,
    stroke: Stroke,
    verts: &mut Vec<GeoVertex>,
    indices: &mut Vec<u32>,
) {
    let hw = if stroke.hairline {
        // One device pixel; DPI scaling is threaded with surface/layer DPI later.
        0.5
    } else {
        stroke.width * 0.5
    };
    if points.len() < 2 || hw <= 0.0 {
        return;
    }

    // Normalize to a point ring (a closed subpath drops its duplicated closing
    // point and wraps).
    let ring: &[Point] = match points.split_last() {
        Some((last, head)) if closed && head.first() == Some(last) => head,
        _ => points,
    };

    match &stroke.dash {
        Some(dash) if dash.len > 0 => {
            for run in dash_runs(ring, closed, dash) {
                // Each "on" run is a standalone open polyline with the stroke's caps.
                stroke_polyline(&run, false, hw, &stroke, verts, indices);
            }
        }
        _ => stroke_polyline(ring, closed, hw, &stroke, verts, indices),
    }
}

/// Stroke one polyline (or closed ring) of centerline points at half-width `hw`.
fn stroke_polyline(
    ring: &[Point],
    closed: bool,
    hw: f32,
    stroke: &Stroke,
    verts: &mut Vec<GeoVertex>,
    indices: &mut Vec<u32>,
) {
    if ring.len() < 2 {
        return;
    }
    let count = ring.len();
    let seg_count = if closed { count } else { count - 1 };
    // Alignment shifts the whole stroke along the left normal by `mid`; open
    // subpaths have no inside/outside and stay centered.
    let (off_l, off_r) = stroke.align.rails(hw, closed);
    let mid = (off_l + off_r) * 0.5;

    for s in 0..seg_count {
        let a = ring[s];
        let b = ring[(s + 1) % count];
        let dir = norm(b.x - a.x, b.y - a.y);
        // Left normal (perpendicular).
        let nx = -dir.1;
        let ny = dir.0;
        emit_stroke_quad(a, b, nx, ny, hw, mid, verts, indices);

        // Join at `b` with the next segment (interior vertices only).
        let is_interior = closed || (s + 1) < seg_count;
        if is_interior {
            let c = ring[(s + 2) % count];
            emit_join(b, a, c, nx, ny, hw, mid, stroke, verts, indices);
        }
    }

    // Caps at the two open endpoints (closed rings have none).
    if !closed {
        let a0 = ring[0];
        let a1 = ring[1];
        emit_cap(
            a0,
            norm(a0.x - a1.x, a0.y - a1.y),
            hw,
            mid,
            stroke,
            verts,
            indices,
        );
        let bn = ring[count - 1];
        let bp = ring[count - 2];
        emit_cap(
            bn,
            norm(bn.x - bp.x, bn.y - bp.y),
            hw,
            mid,
            stroke,
            verts,
            indices,
        );
    }
}

/// Emit one filled+fringed stroke quad for segment `a`→`b` with left normal
/// `(nx, ny)`, half-width `hw`, shifted along the normal by `mid` (alignment).
#[allow(clippy::too_many_arguments)]
fn emit_stroke_quad(
    a: Point,
    b: Point,
    nx: f32,
    ny: f32,
    hw: f32,
    mid: f32,
    verts: &mut Vec<GeoVertex>,
    indices: &mut Vec<u32>,
) {
    let base = verts.len() as u32;
    // Left/right rail offsets along the normal after the alignment shift.
    let ol = mid + hw;
    let or = mid - hw;
    // Core quad corners (edge = 1) then fringe corners (edge = 0) on each side.
    let al = Point::new(a.x + nx * ol, a.y + ny * ol);
    let ar = Point::new(a.x + nx * or, a.y + ny * or);
    let bl = Point::new(b.x + nx * ol, b.y + ny * ol);
    let br = Point::new(b.x + nx * or, b.y + ny * or);
    verts.push(geo_vert(al, 1.0)); // 0
    verts.push(geo_vert(ar, 1.0)); // 1
    verts.push(geo_vert(bl, 1.0)); // 2
    verts.push(geo_vert(br, 1.0)); // 3
    indices.extend_from_slice(&[base, base + 1, base + 2, base + 1, base + 3, base + 2]);

    // Fringe on the left (+normal) and right (-normal) edges.
    let alf = Point::new(al.x + nx * AA_FRINGE, al.y + ny * AA_FRINGE);
    let blf = Point::new(bl.x + nx * AA_FRINGE, bl.y + ny * AA_FRINGE);
    let arf = Point::new(ar.x - nx * AA_FRINGE, ar.y - ny * AA_FRINGE);
    let brf = Point::new(br.x - nx * AA_FRINGE, br.y - ny * AA_FRINGE);
    let f = verts.len() as u32;
    verts.push(geo_vert(alf, 0.0)); // f+0
    verts.push(geo_vert(blf, 0.0)); // f+1
    verts.push(geo_vert(arf, 0.0)); // f+2
    verts.push(geo_vert(brf, 0.0)); // f+3
    // Left fringe bridges core edge (al=base, bl=base+2) to (alf, blf).
    indices.extend_from_slice(&[base, f, f + 1, base, f + 1, base + 2]);
    // Right fringe bridges core edge (ar=base+1, br=base+3) to (arf, brf).
    indices.extend_from_slice(&[base + 1, f + 2, f + 3, base + 1, f + 3, base + 3]);
}

/// Fill the wedge at corner `b` between the incoming segment (left normal
/// `(pnx, pny)`) and the outgoing segment toward `c`. Miter within the limit,
/// round when requested, bevel otherwise.
#[allow(clippy::too_many_arguments)]
fn emit_join(
    b: Point,
    a: Point,
    c: Point,
    pnx: f32,
    pny: f32,
    hw: f32,
    mid: f32,
    stroke: &Stroke,
    verts: &mut Vec<GeoVertex>,
    indices: &mut Vec<u32>,
) {
    let ndir = norm(c.x - b.x, c.y - b.y);
    let nnx = -ndir.1;
    let nny = ndir.0;

    // Turn direction: cross of incoming dir and outgoing dir.
    let idir = norm(b.x - a.x, b.y - a.y);
    let cross = idir.0 * ndir.1 - idir.1 * ndir.0;
    if cross.abs() < 1e-4 {
        return; // straight — nothing to fill.
    }
    // Corner point after the alignment shift (about which the outer rails pivot).
    let bc = Point::new(b.x + pnx * mid, b.y + pny * mid);
    // Outer side is opposite the turn. For a left turn (cross > 0) the outer
    // corner is on the -normal side; for a right turn, the +normal side.
    let sign = if cross > 0.0 { -1.0 } else { 1.0 };
    let p_out = Point::new(bc.x + sign * pnx * hw, bc.y + sign * pny * hw);
    let n_out = Point::new(bc.x + sign * nnx * hw, bc.y + sign * nny * hw);

    if stroke.join == LineJoin::Round {
        fan(bc, p_out, n_out, hw, verts, indices);
        return;
    }

    let base = verts.len() as u32;
    verts.push(geo_vert(bc, 1.0));
    verts.push(geo_vert(p_out, 1.0));
    verts.push(geo_vert(n_out, 1.0));

    // Miter apex: intersection of the two outer edges. Fall back to bevel if the
    // miter grows past `miter_limit × hw`.
    if stroke.join == LineJoin::Miter
        && normals_diverge(pnx, pny, nnx, nny, sign)
        && let Some(apex) = miter_apex(p_out, idir, n_out, ndir)
    {
        let dx = apex.x - bc.x;
        let dy = apex.y - bc.y;
        if (dx * dx + dy * dy).sqrt() <= stroke.miter_limit * hw {
            let apex_idx = verts.len() as u32;
            verts.push(geo_vert(apex, 1.0));
            indices.extend_from_slice(&[base, base + 1, apex_idx, base, apex_idx, base + 2]);
            return;
        }
    }
    // Bevel: single triangle across the corner.
    indices.extend_from_slice(&[base, base + 1, base + 2]);
}

/// Emit the end cap at open endpoint `end` whose outward direction is `dir`
/// (pointing away from the polyline), half-width `hw`, alignment shift `mid`.
#[allow(clippy::too_many_arguments)]
fn emit_cap(
    end: Point,
    dir: (f32, f32),
    hw: f32,
    mid: f32,
    stroke: &Stroke,
    verts: &mut Vec<GeoVertex>,
    indices: &mut Vec<u32>,
) {
    if dir == (0.0, 0.0) {
        return;
    }
    // Left normal of the outward direction.
    let nx = -dir.1;
    let ny = dir.0;
    let center = Point::new(end.x + nx * mid, end.y + ny * mid);
    let l = Point::new(center.x + nx * hw, center.y + ny * hw);
    let r = Point::new(center.x - nx * hw, center.y - ny * hw);
    match stroke.cap {
        LineCap::Butt => {}
        LineCap::Square => {
            // Project the rail ends `hw` along the outward direction.
            let base = verts.len() as u32;
            let lp = Point::new(l.x + dir.0 * hw, l.y + dir.1 * hw);
            let rp = Point::new(r.x + dir.0 * hw, r.y + dir.1 * hw);
            verts.push(geo_vert(l, 1.0));
            verts.push(geo_vert(r, 1.0));
            verts.push(geo_vert(lp, 1.0));
            verts.push(geo_vert(rp, 1.0));
            indices.extend_from_slice(&[base, base + 2, base + 3, base, base + 3, base + 1]);
        }
        LineCap::Round => {
            // Half-circle bulging outward: sweep π starting at the left rail so
            // the arc midpoint points along `dir` (outward), covering the cap and
            // not the interior of the segment. The midpoint of an arc from
            // `start` sweeping `s` is `start + s/2`; choose the sign of `s` so
            // that midpoint aligns with `dir`.
            let start = ny.atan2(nx);
            let dir_ang = dir.1.atan2(dir.0);
            // Rotate from `start` toward `dir` the short way; the half-circle then
            // sweeps that same direction so its midpoint lands on `dir`.
            let mut delta = dir_ang - start;
            while delta > std::f32::consts::PI {
                delta -= std::f32::consts::TAU;
            }
            while delta < -std::f32::consts::PI {
                delta += std::f32::consts::TAU;
            }
            let sweep = std::f32::consts::PI.copysign(delta);
            arc_fan(center, start, sweep, hw, verts, indices);
        }
    }
}

/// A rounded corner: a triangle fan centered at `c` sweeping the shorter arc
/// from `from` to `to`, both at radius `hw`. Falls back to a single triangle
/// when the radius is degenerate.
fn fan(
    c: Point,
    from: Point,
    to: Point,
    hw: f32,
    verts: &mut Vec<GeoVertex>,
    indices: &mut Vec<u32>,
) {
    let a0 = (from.y - c.y).atan2(from.x - c.x);
    let a1 = (to.y - c.y).atan2(to.x - c.x);
    // Sweep the shorter way around.
    let mut sweep = a1 - a0;
    while sweep > std::f32::consts::PI {
        sweep -= std::f32::consts::TAU;
    }
    while sweep < -std::f32::consts::PI {
        sweep += std::f32::consts::TAU;
    }
    arc_fan(c, a0, sweep, hw, verts, indices);
}

/// A triangle fan centered at `c`, radius `hw`, starting at angle `start` and
/// sweeping the signed angle `sweep`. Segment count follows the arc length.
fn arc_fan(
    c: Point,
    start: f32,
    sweep: f32,
    hw: f32,
    verts: &mut Vec<GeoVertex>,
    indices: &mut Vec<u32>,
) {
    let arc = sweep.abs() * hw;
    let steps = ((arc / ROUND_STEP).ceil() as u32).max(1);
    let center_idx = verts.len() as u32;
    verts.push(geo_vert(c, 1.0));
    let mut prev = verts.len() as u32;
    verts.push(geo_vert(
        Point::new(c.x + start.cos() * hw, c.y + start.sin() * hw),
        1.0,
    ));
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        let ang = start + sweep * t;
        let p = Point::new(c.x + ang.cos() * hw, c.y + ang.sin() * hw);
        let cur = verts.len() as u32;
        verts.push(geo_vert(p, 1.0));
        indices.extend_from_slice(&[center_idx, prev, cur]);
        prev = cur;
    }
}

/// Whether the two outer edge normals diverge enough that a miter apex is
/// meaningful (guards against near-parallel edges where the apex shoots to
/// infinity).
fn normals_diverge(pnx: f32, pny: f32, nnx: f32, nny: f32, sign: f32) -> bool {
    let dot = (sign * pnx) * (sign * nnx) + (sign * pny) * (sign * nny);
    dot < 0.999
}

/// Intersection of the outer edge line through `p` along `pdir` with the line
/// through `q` along `qdir`. `None` if near-parallel.
fn miter_apex(p: Point, pdir: (f32, f32), q: Point, qdir: (f32, f32)) -> Option<Point> {
    let denom = pdir.0 * qdir.1 - pdir.1 * qdir.0;
    if denom.abs() < 1e-6 {
        return None;
    }
    let t = ((q.x - p.x) * qdir.1 - (q.y - p.y) * qdir.0) / denom;
    Some(Point::new(p.x + pdir.0 * t, p.y + pdir.1 * t))
}

/// Normalize a 2D vector; returns `(0, 0)` for a near-zero input.
fn norm(x: f32, y: f32) -> (f32, f32) {
    let len = (x * x + y * y).sqrt();
    if len < 1e-6 {
        (0.0, 0.0)
    } else {
        (x / len, y / len)
    }
}

fn geo_vert(p: Point, edge: f32) -> GeoVertex {
    GeoVertex {
        pos: [p.x, p.y],
        edge,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_is_inclusive_near_exclusive_far() {
        let r = Rect {
            x: 10.0,
            y: 20.0,
            w: 30.0,
            h: 40.0,
        };
        // Interior point hits.
        assert!(r.contains(15.0, 25.0));
        // Near corner is inclusive.
        assert!(r.contains(10.0, 20.0));
        // Far edges are exclusive (a point on x+w / y+h misses).
        assert!(!r.contains(40.0, 25.0));
        assert!(!r.contains(15.0, 60.0));
        assert!(!r.contains(40.0, 60.0));
        // Outside on any side misses.
        assert!(!r.contains(9.0, 25.0));
        assert!(!r.contains(15.0, 19.0));
    }

    #[test]
    fn zero_size_rect_contains_nothing() {
        let r = Rect {
            x: 5.0,
            y: 5.0,
            w: 0.0,
            h: 0.0,
        };
        // Its own origin is on the (coincident) far edge, so it never hits.
        assert!(!r.contains(5.0, 5.0));
    }

    #[test]
    fn quad_instance_layout_matches_schema() {
        // The derived layout must validate against the shader schema — the same
        // check `create_pipeline` performs at registration.
        assert_eq!(
            QuadInstance::LAYOUT.validate_against(&quad_schema()),
            Ok(())
        );
    }

    #[test]
    fn analytic_rrect_instance_layout_matches_schema() {
        assert_eq!(
            AnalyticRRectInstance::LAYOUT.validate_against(&analytic_rrect_schema()),
            Ok(())
        );
    }

    #[test]
    fn analytic_ellipse_instance_layout_matches_schema() {
        assert_eq!(
            AnalyticEllipseInstance::LAYOUT.validate_against(&analytic_ellipse_schema()),
            Ok(())
        );
    }

    #[test]
    fn analytic_rrect_lowers_to_instance_with_per_corner_radii() {
        let r = AnalyticRRect {
            rect: Rect {
                x: 10.0,
                y: 20.0,
                w: 30.0,
                h: 40.0,
            },
            color: Rgba {
                r: 1.0,
                g: 0.5,
                b: 0.25,
                a: 1.0,
            },
            radius: Corners {
                left_top: 1.0,
                right_top: 2.0,
                right_bottom: 3.0,
                left_bottom: 4.0,
            },
            border: Border {
                width: 2.0,
                color: Rgba {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
            },
        };
        let inst = r.to_instance();
        assert_eq!(inst.rect_pos, [10.0, 20.0]);
        assert_eq!(inst.rect_size, [30.0, 40.0]);
        assert_eq!(inst.color, [1.0, 0.5, 0.25, 1.0]);
        // Corner order: left_top, right_top, right_bottom, left_bottom.
        assert_eq!(inst.radius, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(inst.border_width, 2.0);
        assert_eq!(inst.border_color, [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn analytic_ellipse_lowers_to_instance() {
        let e = AnalyticEllipse {
            rect: Rect {
                x: 10.0,
                y: 20.0,
                w: 30.0,
                h: 40.0,
            },
            color: Rgba {
                r: 0.2,
                g: 0.4,
                b: 0.6,
                a: 0.8,
            },
            border: Border {
                width: 3.0,
                color: Rgba {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
            },
        };
        let inst = e.to_instance();
        assert_eq!(inst.rect_pos, [10.0, 20.0]);
        assert_eq!(inst.rect_size, [30.0, 40.0]);
        assert_eq!(inst.color, [0.2, 0.4, 0.6, 0.8]);
        assert_eq!(inst.border_width, 3.0);
        assert_eq!(inst.border_color, [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn analytic_capsule_instance_layout_matches_schema() {
        assert_eq!(
            AnalyticCapsuleInstance::LAYOUT.validate_against(&analytic_capsule_schema()),
            Ok(())
        );
    }

    #[test]
    fn analytic_capsule_lowers_to_instance() {
        let c = AnalyticCapsule {
            rect: Rect {
                x: 10.0,
                y: 20.0,
                w: 30.0,
                h: 40.0,
            },
            color: Rgba {
                r: 0.2,
                g: 0.4,
                b: 0.6,
                a: 0.8,
            },
            border: Border {
                width: 3.0,
                color: Rgba {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
            },
        };
        let inst = c.to_instance();
        assert_eq!(inst.rect_pos, [10.0, 20.0]);
        assert_eq!(inst.rect_size, [30.0, 40.0]);
        assert_eq!(inst.color, [0.2, 0.4, 0.6, 0.8]);
        assert_eq!(inst.border_width, 3.0);
        assert_eq!(inst.border_color, [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn analytic_shadow_instance_layout_matches_schema() {
        assert_eq!(
            ShadowInstance::LAYOUT.validate_against(&analytic_shadow_schema()),
            Ok(())
        );
    }

    #[test]
    fn analytic_shadow_lowers_to_instance() {
        let s = AnalyticShadow {
            rect: Rect {
                x: 10.0,
                y: 20.0,
                w: 30.0,
                h: 40.0,
            },
            color: Rgba {
                r: 0.1,
                g: 0.2,
                b: 0.3,
                a: 0.5,
            },
            radius: Corners::uniform(6.0),
            offset: [2.0, -3.0],
            sigma: 4.0,
            spread: 1.5,
            shape: ShadowShape::Capsule,
            inner: false,
        };
        let inst = s.to_instance();
        assert_eq!(inst.rect_pos, [10.0, 20.0]);
        assert_eq!(inst.rect_size, [30.0, 40.0]);
        assert_eq!(inst.color, [0.1, 0.2, 0.3, 0.5]);
        // 6px radii fit within the 30x40 rect, so normalization is a no-op.
        assert_eq!(inst.radius, [6.0, 6.0, 6.0, 6.0]);
        assert_eq!(inst.offset, [2.0, -3.0]);
        assert_eq!(inst.sigma, 4.0);
        assert_eq!(inst.spread, 1.5);
        assert_eq!(inst.shape, 2);
        assert_eq!(inst.inner, 0);
    }

    #[test]
    fn shadow_shape_maps_to_stable_codes() {
        assert_eq!(ShadowShape::RoundedBox.as_u32(), 0);
        assert_eq!(ShadowShape::Ellipse.as_u32(), 1);
        assert_eq!(ShadowShape::Capsule.as_u32(), 2);
    }

    #[test]
    fn analytic_line_instance_layout_matches_schema() {
        assert_eq!(
            AnalyticLineInstance::LAYOUT.validate_against(&analytic_line_schema()),
            Ok(())
        );
    }

    #[test]
    fn analytic_line_lowers_to_instance() {
        let l = AnalyticLine {
            p0: Point::new(10.0, 20.0),
            p1: Point::new(30.0, 40.0),
            width: 4.0,
            color: Rgba {
                r: 0.2,
                g: 0.4,
                b: 0.6,
                a: 0.8,
            },
            cap: LineCap::Round,
            join: LineJoin::Bevel,
            miter_limit: 4.0,
            border: Border {
                width: 1.5,
                color: Rgba {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
            },
        };
        let inst = l.to_instance();
        assert_eq!(inst.p0, [10.0, 20.0]);
        assert_eq!(inst.p1, [30.0, 40.0]);
        assert_eq!(inst.width, 4.0);
        assert_eq!(inst.color, [0.2, 0.4, 0.6, 0.8]);
        assert_eq!(inst.cap, 2); // round
        assert_eq!(inst.join, 1); // bevel
        assert_eq!(inst.miter_limit, 4.0);
        assert_eq!(inst.border_width, 1.5);
        assert_eq!(inst.border_color, [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn gradient_instance_layout_matches_schema() {
        assert_eq!(
            GradientInstance::LAYOUT.validate_against(&gradient_schema()),
            Ok(())
        );
    }

    #[test]
    fn gradient_lowers_to_instance() {
        // A radial gradient: p1 carries (radius, _). Two stops → inline path.
        let g = Gradient {
            rect: Rect {
                x: 10.0,
                y: 20.0,
                w: 30.0,
                h: 40.0,
            },
            kind: GradientKind::Radial,
            extend: ExtendMode::Repeat,
            p0: Point::new(25.0, 40.0),
            p1: Point::new(15.0, 0.0),
            stops: vec![
                GradientStop {
                    offset: 0.0,
                    color: Rgba::new(1.0, 0.0, 0.0, 1.0),
                },
                GradientStop {
                    offset: 1.0,
                    color: Rgba::new(0.0, 0.0, 1.0, 0.5),
                },
            ],
            interp: InterpolationSpace::LinearRgb,
        };
        // Inline two-stop path: use_lut == 0, colors carried premultiplied.
        let inst = g.to_instance(0.0, false);
        assert_eq!(inst.rect_pos, [10.0, 20.0]);
        assert_eq!(inst.rect_size, [30.0, 40.0]);
        assert_eq!(inst.kind, 1); // radial
        assert_eq!(inst.extend, 1); // repeat
        assert_eq!(inst.p0, [25.0, 40.0]);
        assert_eq!(inst.p1, [15.0, 0.0]);
        assert_eq!(inst.use_lut, 0);
        // Premultiplied: opaque red stays, translucent blue scales rgb by a.
        assert_eq!(inst.color0, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(inst.color1, [0.0, 0.0, 0.5, 0.5]);

        // LUT path: colors zeroed (shader ignores them), lut_v/use_lut set.
        let lut = g.to_instance(0.375, true);
        assert_eq!(lut.use_lut, 1);
        assert_eq!(lut.lut_v, 0.375);
        assert_eq!(lut.color0, [0.0; 4]);
        assert_eq!(lut.color1, [0.0; 4]);
    }

    #[test]
    fn corners_within_the_box_pass_through_unscaled() {
        let c = Corners {
            left_top: 4.0,
            right_top: 4.0,
            right_bottom: 4.0,
            left_bottom: 4.0,
        };
        // Every edge sum (8) fits within 20 → no scaling.
        assert_eq!(c.normalized(20.0, 20.0), c);
    }

    #[test]
    fn oversized_corners_scale_by_one_uniform_factor() {
        // Top edge wants 30+10 across a 20-wide box → the tightest ratio is
        // 20/40 = 0.5, and every corner shrinks by that same factor so the
        // shape stays proportional (CSS overlapping-curves rule, §11.2).
        let c = Corners {
            left_top: 30.0,
            right_top: 10.0,
            right_bottom: 10.0,
            left_bottom: 30.0,
        };
        let n = c.normalized(20.0, 100.0);
        assert_eq!(
            n,
            Corners {
                left_top: 15.0,
                right_top: 5.0,
                right_bottom: 5.0,
                left_bottom: 15.0,
            }
        );
    }

    #[test]
    fn negative_corner_radii_floor_to_zero() {
        let c = Corners {
            left_top: -5.0,
            right_top: 2.0,
            right_bottom: 0.0,
            left_bottom: -1.0,
        };
        let n = c.normalized(100.0, 100.0);
        assert_eq!(
            n,
            Corners {
                left_top: 0.0,
                right_top: 2.0,
                right_bottom: 0.0,
                left_bottom: 0.0,
            }
        );
    }

    #[test]
    fn image_instance_layout_matches_schema() {
        assert_eq!(
            ImageInstance::LAYOUT.validate_against(&image_schema()),
            Ok(())
        );
    }

    #[test]
    fn glyphrun_instance_layout_matches_schema() {
        assert_eq!(
            GlyphInstance::LAYOUT.validate_against(&glyphrun_schema()),
            Ok(())
        );
    }

    #[test]
    fn glyphrun_lowers_to_instance_with_run_color() {
        let run = GlyphRunDraw {
            glyphs: vec![],
            atlas: TextureId::new(0),
            color: Rgba {
                r: 0.1,
                g: 0.2,
                b: 0.3,
                a: 0.9,
            },
        };
        let glyph = GlyphInstanceData {
            rect: Rect {
                x: 5.0,
                y: 6.0,
                w: 7.0,
                h: 8.0,
            },
            uv: Rect {
                x: 0.1,
                y: 0.2,
                w: 0.3,
                h: 0.4,
            },
        };
        let inst = run.instance(&glyph);
        assert_eq!(inst.rect_pos, [5.0, 6.0]);
        assert_eq!(inst.rect_size, [7.0, 8.0]);
        assert_eq!(inst.uv_pos, [0.1, 0.2]);
        assert_eq!(inst.uv_size, [0.3, 0.4]);
        // The run's color, not per-glyph.
        assert_eq!(inst.color, [0.1, 0.2, 0.3, 0.9]);
    }

    #[test]
    fn image_lowers_to_instance() {
        let img = ImageDraw {
            rect: Rect {
                x: 10.0,
                y: 20.0,
                w: 30.0,
                h: 40.0,
            },
            uv: Rect {
                x: 0.25,
                y: 0.5,
                w: 0.25,
                h: 0.5,
            },
            tint: Rgba {
                r: 1.0,
                g: 0.5,
                b: 0.25,
                a: 0.8,
            },
            texture: TextureId::new(0),
            sampler: SamplerDesc::LINEAR_CLAMP,
        };
        let inst = img.to_instance();
        assert_eq!(inst.rect_pos, [10.0, 20.0]);
        assert_eq!(inst.rect_size, [30.0, 40.0]);
        assert_eq!(inst.uv_pos, [0.25, 0.5]);
        assert_eq!(inst.uv_size, [0.25, 0.5]);
        assert_eq!(inst.color, [1.0, 0.5, 0.25, 0.8]);
    }

    #[test]
    fn rect_intersect_overlap_and_disjoint() {
        let a = Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0,
        };
        let b = Rect {
            x: 50.0,
            y: 40.0,
            w: 100.0,
            h: 100.0,
        };
        // Overlap: the common region.
        assert_eq!(
            a.intersect(b),
            Rect {
                x: 50.0,
                y: 40.0,
                w: 50.0,
                h: 60.0,
            }
        );
        // Disjoint: empty (w/h clamped to 0), never negative.
        let far = Rect {
            x: 200.0,
            y: 200.0,
            w: 10.0,
            h: 10.0,
        };
        let r = a.intersect(far);
        assert_eq!(r.w, 0.0);
        assert_eq!(r.h, 0.0);
    }

    #[test]
    fn quad_lowers_to_instance() {
        let q = Quad {
            rect: Rect {
                x: 10.0,
                y: 20.0,
                w: 30.0,
                h: 40.0,
            },
            color: Rgba {
                r: 1.0,
                g: 0.5,
                b: 0.25,
                a: 1.0,
            },
            radius: 4.0,
            border: Border {
                width: 2.0,
                color: Rgba {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
            },
        };
        let inst = q.to_instance();
        assert_eq!(inst.rect_pos, [10.0, 20.0]);
        assert_eq!(inst.rect_size, [30.0, 40.0]);
        assert_eq!(inst.color, [1.0, 0.5, 0.25, 1.0]);
        assert_eq!(inst.radius, 4.0);
        assert_eq!(inst.border_width, 2.0);
        assert_eq!(inst.border_color, [0.0, 0.0, 0.0, 1.0]);
    }

    const TEX: TextureId = TextureId::new(9);

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-5, "expected {b}, got {a}");
    }

    fn approx_rect(r: Rect, x: f32, y: f32, w: f32, h: f32) {
        approx(r.x, x);
        approx(r.y, y);
        approx(r.w, w);
        approx(r.h, h);
    }

    #[test]
    fn image_rect_fill_maps_whole_texture_to_dest() {
        // Fill + no src: the drawn rect is dest verbatim, uv is the full 0..1
        // (no atlas inset for a whole-texture source), opacity lands in tint.a.
        let ir = ImageRect {
            src: None,
            dest: Rect {
                x: 10.0,
                y: 20.0,
                w: 100.0,
                h: 50.0,
            },
            fit: Fit::Fill,
            align: Align2::CENTER,
            opacity: 0.5,
            texture: TEX,
            tex_size: [64, 32],
            sampler: SamplerDesc::LINEAR_CLAMP,
        };
        let d = ir.to_image_draw();
        approx_rect(d.rect, 10.0, 20.0, 100.0, 50.0);
        approx_rect(d.uv, 0.0, 0.0, 1.0, 1.0);
        approx(d.tint.a, 0.5);
    }

    #[test]
    fn image_rect_contain_letterboxes_and_aligns() {
        // 100x100 source into a 200x100 dest: contain scale = 1.0 (limited by
        // height), so a 100x100 rect is centered horizontally → x offset 50.
        let ir = ImageRect {
            src: None,
            dest: Rect {
                x: 0.0,
                y: 0.0,
                w: 200.0,
                h: 100.0,
            },
            fit: Fit::Contain,
            align: Align2::CENTER,
            opacity: 1.0,
            texture: TEX,
            tex_size: [100, 100],
            sampler: SamplerDesc::LINEAR_CLAMP,
        };
        let d = ir.to_image_draw();
        approx_rect(d.rect, 50.0, 0.0, 100.0, 100.0);
        // Full source sampled.
        approx_rect(d.uv, 0.0, 0.0, 1.0, 1.0);

        // Start alignment pins the drawn rect to the left edge.
        let ir_start = ImageRect {
            align: Align2 {
                x: Align::Start,
                y: Align::Start,
            },
            ..ir
        };
        approx_rect(ir_start.to_image_draw().rect, 0.0, 0.0, 100.0, 100.0);
    }

    #[test]
    fn image_rect_cover_crops_source_and_fills_dest() {
        // 100x100 source into 200x100 dest: cover scale = 2.0 (width), sampled
        // source height = dest.h/scale = 50, centered vertically → sy = 25.
        // The drawn rect fills the whole dest.
        let ir = ImageRect {
            src: None,
            dest: Rect {
                x: 0.0,
                y: 0.0,
                w: 200.0,
                h: 100.0,
            },
            fit: Fit::Cover,
            align: Align2::CENTER,
            opacity: 1.0,
            texture: TEX,
            tex_size: [100, 100],
            sampler: SamplerDesc::LINEAR_CLAMP,
        };
        let d = ir.to_image_draw();
        approx_rect(d.rect, 0.0, 0.0, 200.0, 100.0);
        // Sampled source: full width (u 0..1), cropped height 50/100 centered.
        approx_rect(d.uv, 0.0, 0.25, 1.0, 0.5);
    }

    #[test]
    fn image_rect_none_samples_pixel_for_pixel() {
        // Fit::None into a dest smaller than the source: 1:1 sampling, the
        // source is cropped to the dest extent (in texels), centered.
        let ir = ImageRect {
            src: None,
            dest: Rect {
                x: 0.0,
                y: 0.0,
                w: 40.0,
                h: 40.0,
            },
            fit: Fit::None,
            align: Align2::CENTER,
            opacity: 1.0,
            texture: TEX,
            tex_size: [100, 100],
            sampler: SamplerDesc::LINEAR_CLAMP,
        };
        let d = ir.to_image_draw();
        approx_rect(d.rect, 0.0, 0.0, 40.0, 40.0);
        // Source cropped to 40x40 of a 100-tex, centered → offset 30 → uv 0.3.
        approx_rect(d.uv, 0.3, 0.3, 0.4, 0.4);
    }

    #[test]
    fn atlas_subregion_insets_half_a_texel_per_side() {
        // A real src sub-region gets the half-texel bleed guard; a 100-wide tex
        // → 0.5/100 = 0.005 inset per side.
        let ir = ImageRect {
            src: Some(Rect {
                x: 0.0,
                y: 0.0,
                w: 50.0,
                h: 50.0,
            }),
            dest: Rect {
                x: 0.0,
                y: 0.0,
                w: 50.0,
                h: 50.0,
            },
            fit: Fit::Fill,
            align: Align2::CENTER,
            opacity: 1.0,
            texture: TEX,
            tex_size: [100, 100],
            sampler: SamplerDesc::LINEAR_CLAMP,
        };
        let d = ir.to_image_draw();
        // uv0 = 0 + 0.005, uv_size = 0.5 - 2*0.005 = 0.49.
        approx_rect(d.uv, 0.005, 0.005, 0.49, 0.49);
    }

    #[test]
    fn nine_slice_expands_to_nine_tiling_patches() {
        // 90x90 texture, uniform 30px inset, into a 300x300 dest. The 9 patches
        // must tile dest exactly with no gaps/overlaps: corners 30x30, edges
        // stretch one axis, center fills the 240x240 middle.
        let ns = NineSlice::new(
            TEX,
            [90, 90],
            Rect {
                x: 0.0,
                y: 0.0,
                w: 300.0,
                h: 300.0,
            },
            30.0,
        );
        let draws = ns.to_image_draws();
        assert_eq!(draws.len(), 9);
        // Top-left corner keeps source inset size.
        approx_rect(draws[0].rect, 0.0, 0.0, 30.0, 30.0);
        // Center patch (index 4) fills the interior.
        approx_rect(draws[4].rect, 30.0, 30.0, 240.0, 240.0);
        // Bottom-right corner (index 8) sits at the far edge.
        approx_rect(draws[8].rect, 270.0, 270.0, 30.0, 30.0);
        // The union of all patch rects covers dest with no overhang.
        let (mut maxx, mut maxy) = (0.0f32, 0.0f32);
        for d in &draws {
            maxx = maxx.max(d.rect.x + d.rect.w);
            maxy = maxy.max(d.rect.y + d.rect.h);
        }
        approx(maxx, 300.0);
        approx(maxy, 300.0);
    }

    #[test]
    fn tiled_whole_texture_uses_single_wrapping_instance() {
        // Whole-texture tile → one draw whose uv exceeds 0..1 (dest/tile) so the
        // Repeat sampler replicates it.
        let t = TiledImage::new(
            TEX,
            [50, 50],
            Rect {
                x: 0.0,
                y: 0.0,
                w: 200.0,
                h: 150.0,
            },
        );
        let draws = t.to_image_draws();
        assert_eq!(draws.len(), 1);
        approx_rect(draws[0].uv, 0.0, 0.0, 4.0, 3.0);
        assert_eq!(draws[0].sampler.address, AddressMode::Repeat);
    }

    #[test]
    fn tiled_atlas_subcell_expands_and_crops_trailing_tiles() {
        // A sub-cell tile (not the whole texture) can't wrap-sample, so it is
        // CPU-expanded. A 40px tile across a 100px dest → 3 columns (40,40,20);
        // the trailing column is cropped in both rect and uv.
        let t = TiledImage {
            texture: TEX,
            tex_size: [100, 100],
            tile: Rect {
                x: 0.0,
                y: 0.0,
                w: 40.0,
                h: 100.0,
            },
            dest: Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            },
            sampler: SamplerDesc {
                filter: FilterMode::Nearest,
                address: AddressMode::Repeat,
            },
        };
        let draws = t.to_image_draws();
        assert_eq!(draws.len(), 3);
        // Trailing tile: 20px wide, uv width cropped to half the tile (0.4→0.2).
        approx_rect(draws[2].rect, 80.0, 0.0, 20.0, 100.0);
        approx(draws[2].uv.w, 0.2);
    }

    #[test]
    fn resource_policy_resolves_routes_and_rejects_external() {
        assert_eq!(
            ResourcePolicy::AtlasCandidate { mipmap: true }.resolve(),
            Ok(ResourceRoute::Atlas { mipmap: true })
        );
        assert_eq!(
            ResourcePolicy::Standalone { mipmap: false }.resolve(),
            Ok(ResourceRoute::Standalone { mipmap: false })
        );
        assert_eq!(
            ResourcePolicy::External.resolve(),
            Err(ResourceRouteError::ExternalUnsupported)
        );
        assert_eq!(
            ResourcePolicy::default(),
            ResourcePolicy::AtlasCandidate { mipmap: false }
        );
    }

    // ---- D3.3 stroke geometry --------------------------------------------

    /// A short open horizontal polyline (two points) with the given stroke.
    fn open_line(stroke: Stroke) -> Path {
        Path {
            cmds: vec![
                PathCmd::MoveTo(Point::new(10.0, 10.0)),
                PathCmd::LineTo(Point::new(40.0, 10.0)),
            ],
            fill: None,
            stroke: Some(stroke),
            shadow: None,
        }
    }

    /// Bounding box of a path's stroke vertices (core + fringe).
    fn stroke_bounds(p: &Path) -> Rect {
        let geo = p.tessellate_geometry();
        geo.bounds
    }

    #[test]
    fn butt_cap_leaves_ends_flush_square_extends_them() {
        let butt = open_line(Stroke::new(4.0, Rgba::new(0.0, 0.0, 0.0, 1.0)));
        let square = open_line(Stroke {
            cap: LineCap::Square,
            ..Stroke::new(4.0, Rgba::new(0.0, 0.0, 0.0, 1.0))
        });
        let bb = stroke_bounds(&butt);
        let sb = stroke_bounds(&square);
        // Square caps extend the run by hw (=2) at each end; butt does not
        // (modulo the AA fringe, which is symmetric and tiny).
        assert!(sb.x < bb.x - 1.5, "square cap should reach left of butt");
        assert!(
            sb.x + sb.w > bb.x + bb.w + 1.5,
            "square cap should reach right of butt"
        );
    }

    #[test]
    fn round_cap_bulges_past_butt_ends() {
        let butt = open_line(Stroke::new(4.0, Rgba::new(0.0, 0.0, 0.0, 1.0)));
        let round = open_line(Stroke {
            cap: LineCap::Round,
            ..Stroke::new(4.0, Rgba::new(0.0, 0.0, 0.0, 1.0))
        });
        let bb = stroke_bounds(&butt);
        let rb = stroke_bounds(&round);
        assert!(rb.x < bb.x - 1.0, "round cap should bulge left");
        assert!(
            rb.x + rb.w > bb.x + bb.w + 1.0,
            "round cap should bulge right"
        );
    }

    /// An L-shaped open polyline (one interior corner) with the given join.
    fn corner_path(stroke: Stroke) -> Path {
        Path {
            cmds: vec![
                PathCmd::MoveTo(Point::new(10.0, 10.0)),
                PathCmd::LineTo(Point::new(40.0, 10.0)),
                PathCmd::LineTo(Point::new(40.0, 40.0)),
            ],
            fill: None,
            stroke: Some(stroke),
            shadow: None,
        }
    }

    #[test]
    fn round_join_emits_more_vertices_than_bevel() {
        let base = Stroke::new(8.0, Rgba::new(0.0, 0.0, 0.0, 1.0));
        let bevel = corner_path(Stroke {
            join: LineJoin::Bevel,
            ..base
        });
        let round = corner_path(Stroke {
            join: LineJoin::Round,
            ..base
        });
        let bv = bevel.tessellate_geometry().verts.len();
        let rv = round.tessellate_geometry().verts.len();
        assert!(rv > bv, "round join fan ({rv}) must exceed bevel ({bv})");
    }

    #[test]
    fn tight_miter_limit_degrades_to_bevel() {
        // A 90° corner has a miter ratio of √2 ≈ 1.414. A limit below that must
        // fall back to bevel (fewer vertices than the miter apex path).
        let wide = corner_path(Stroke {
            join: LineJoin::Miter,
            miter_limit: 4.0,
            ..Stroke::new(8.0, Rgba::new(0.0, 0.0, 0.0, 1.0))
        });
        let tight = corner_path(Stroke {
            join: LineJoin::Miter,
            miter_limit: 1.0,
            ..Stroke::new(8.0, Rgba::new(0.0, 0.0, 0.0, 1.0))
        });
        let wv = wide.tessellate_geometry().verts.len();
        let tv = tight.tessellate_geometry().verts.len();
        assert!(
            tv < wv,
            "tight miter ({tv}) should drop the apex vertex ({wv})"
        );
    }

    #[test]
    fn hairline_ignores_width_and_uses_half_pixel() {
        // A hairline stroke has half-width 0.5 regardless of `width`; its bbox
        // thickness is ~1px + fringe, far thinner than a width-10 stroke.
        let thick = open_line(Stroke::new(10.0, Rgba::new(0.0, 0.0, 0.0, 1.0)));
        let hair = open_line(Stroke {
            hairline: true,
            width: 10.0,
            ..Stroke::new(10.0, Rgba::new(0.0, 0.0, 0.0, 1.0))
        });
        let tb = stroke_bounds(&thick);
        let hb = stroke_bounds(&hair);
        assert!(tb.h > 9.0, "thick stroke ~10px tall, got {}", tb.h);
        assert!(hb.h < 3.5, "hairline ~1px core + fringe, got {}", hb.h);
    }

    #[test]
    fn inner_outer_align_shifts_a_closed_ring() {
        // A closed square: inner/outer alignment shift the stroke to one side, so
        // their bounding boxes differ (outer grows, inner shrinks) vs centered.
        let sq = |align: StrokeAlign| Path {
            cmds: vec![
                PathCmd::MoveTo(Point::new(20.0, 20.0)),
                PathCmd::LineTo(Point::new(60.0, 20.0)),
                PathCmd::LineTo(Point::new(60.0, 60.0)),
                PathCmd::LineTo(Point::new(20.0, 60.0)),
                PathCmd::Close,
            ],
            fill: None,
            stroke: Some(Stroke {
                align,
                ..Stroke::new(8.0, Rgba::new(0.0, 0.0, 0.0, 1.0))
            }),
            shadow: None,
        };
        let center = stroke_bounds(&sq(StrokeAlign::Center));
        let outer = stroke_bounds(&sq(StrokeAlign::Outer));
        let inner = stroke_bounds(&sq(StrokeAlign::Inner));
        // Inner and outer shift the stroke to opposite sides of the ring, so both
        // differ from the centered bbox and from each other.
        assert!(
            (outer.w - center.w).abs() > 1.0,
            "outer ({}) should differ from center ({})",
            outer.w,
            center.w
        );
        assert!(
            (inner.w - outer.w).abs() > 1.0,
            "inner ({}) should differ from outer ({})",
            inner.w,
            outer.w
        );
    }

    #[test]
    fn dash_splits_a_line_into_multiple_runs() {
        // A 30-long segment with a 5-on/5-off dash yields 3 painted runs.
        let solid = open_line(Stroke::new(2.0, Rgba::new(0.0, 0.0, 0.0, 1.0)));
        let dashed = open_line(Stroke {
            dash: Some(DashPattern::new(&[5.0, 5.0], 0.0)),
            ..Stroke::new(2.0, Rgba::new(0.0, 0.0, 0.0, 1.0))
        });
        let sv = solid.tessellate_geometry().verts.len();
        let dv = dashed.tessellate_geometry().verts.len();
        // Three separate quads emit more vertices than one solid quad.
        assert!(
            dv > sv,
            "dashed ({dv}) should emit more geometry than solid ({sv})"
        );
    }

    #[test]
    fn dash_offset_changes_geometry_fingerprint() {
        let a = open_line(Stroke {
            dash: Some(DashPattern::new(&[5.0, 5.0], 0.0)),
            ..Stroke::new(2.0, Rgba::new(0.0, 0.0, 0.0, 1.0))
        });
        let b = open_line(Stroke {
            dash: Some(DashPattern::new(&[5.0, 5.0], 2.5)),
            ..Stroke::new(2.0, Rgba::new(0.0, 0.0, 0.0, 1.0))
        });
        assert_ne!(a.geometry_fingerprint(), b.geometry_fingerprint());
    }

    #[test]
    fn color_change_keeps_geometry_fingerprint_and_translation() {
        let a = open_line(Stroke::new(3.0, Rgba::new(0.1, 0.2, 0.3, 1.0)));
        let b = open_line(Stroke::new(3.0, Rgba::new(0.9, 0.8, 0.7, 1.0)));
        assert_eq!(a.geometry_fingerprint(), b.geometry_fingerprint());
        // Identical geometry → zero translation reuse (not a rebuild).
        assert_eq!(a.translation_from(&b), Some([0.0, 0.0]));
    }

    #[test]
    fn stroke_style_change_breaks_fingerprint_and_translation() {
        let base = open_line(Stroke::new(3.0, Rgba::new(0.1, 0.2, 0.3, 1.0)));
        let fields: [Stroke; 5] = [
            Stroke {
                cap: LineCap::Round,
                ..Stroke::new(3.0, Rgba::new(0.1, 0.2, 0.3, 1.0))
            },
            Stroke {
                join: LineJoin::Round,
                ..Stroke::new(3.0, Rgba::new(0.1, 0.2, 0.3, 1.0))
            },
            Stroke {
                miter_limit: 2.0,
                ..Stroke::new(3.0, Rgba::new(0.1, 0.2, 0.3, 1.0))
            },
            Stroke {
                align: StrokeAlign::Outer,
                ..Stroke::new(3.0, Rgba::new(0.1, 0.2, 0.3, 1.0))
            },
            Stroke {
                hairline: true,
                ..Stroke::new(3.0, Rgba::new(0.1, 0.2, 0.3, 1.0))
            },
        ];
        for s in fields {
            let other = open_line(s);
            assert_ne!(
                base.geometry_fingerprint(),
                other.geometry_fingerprint(),
                "stroke style change must alter the geometry fingerprint"
            );
            assert_eq!(
                base.translation_from(&other),
                None,
                "stroke style change must block translation reuse"
            );
        }
    }
}
