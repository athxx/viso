//! The normalized render tree.
//!
//! This is what a parsed document becomes after the cascade, `<use>`/`<symbol>`
//! expansion, unit/percentage resolution, shape→path conversion and paint
//! server resolution. Every node is in its parent group's coordinate system;
//! only [`Group`] carries a transform. Paint servers with
//! `objectBoundingBox` units are already folded into user space, so a consumer
//! never needs the element's bounding box to interpret paint.

use std::sync::Arc;

pub use crate::color::Color;
pub use crate::geom::{PathData, Point, Rect, Segment, Transform};
pub use crate::xml::XmlError;

/// A parsed, normalized SVG document.
#[derive(Debug, Clone, PartialEq)]
pub struct Tree {
    /// Intrinsic document size in CSS pixels (the resolved root `width`/`height`).
    pub size: (f32, f32),
    /// The root `viewBox` (or `0 0 width height` when absent).
    pub view_box: Rect,
    /// Root group, in document pixel space (`0 0 size`); the `viewBox`
    /// mapping is already part of its subtree's transforms.
    pub root: Group,
}

/// A node of the render tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Group(Box<Group>),
    Path(Box<Path>),
    Image(Box<Image>),
}

/// Group compositing blend mode (`mix-blend-mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

/// A container with its own transform and optional compositing effects.
///
/// A group needs an offscreen layer only when [`needs_layer`](Self::needs_layer)
/// — plain transform groups are free.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    /// Element id, when the source element had one.
    pub id: String,
    /// Transform relative to the parent group.
    pub transform: Transform,
    /// Group opacity in `[0, 1]`.
    pub opacity: f32,
    pub blend_mode: BlendMode,
    /// `isolation: isolate`.
    pub isolate: bool,
    pub clip_path: Option<Arc<ClipPath>>,
    pub mask: Option<Arc<Mask>>,
    /// Filter chain (applied in order), in this group's coordinate system.
    pub filters: Vec<Arc<Filter>>,
    pub children: Vec<Node>,
}

impl Default for Group {
    fn default() -> Self {
        Group {
            id: String::new(),
            transform: Transform::IDENTITY,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            isolate: false,
            clip_path: None,
            mask: None,
            filters: Vec::new(),
            children: Vec::new(),
        }
    }
}

impl Group {
    /// Whether rendering this group requires compositing its children into a
    /// separate layer first.
    pub fn needs_layer(&self) -> bool {
        self.opacity < 1.0
            || self.clip_path.is_some()
            || self.mask.is_some()
            || !self.filters.is_empty()
            || self.blend_mode != BlendMode::Normal
            || self.isolate
    }

    /// Visit every path in this subtree with its transform relative to this
    /// group's parent (i.e. `self.transform` already applied).
    pub fn for_each_path(&self, parent: &Transform, f: &mut dyn FnMut(&Path, &Transform)) {
        let ts = parent.pre_concat(&self.transform);
        for c in &self.children {
            match c {
                Node::Group(g) => g.for_each_path(&ts, f),
                Node::Path(p) => f(p, &ts),
                Node::Image(_) => {}
            }
        }
    }

    /// Fill-geometry bounding box of the children, in this group's coordinate
    /// system (before `self.transform`). Strokes are not included.
    pub fn children_bbox(&self) -> Option<Rect> {
        let mut out: Option<Rect> = None;
        let mut add = |r: Rect| out = Some(out.map_or(r, |o| o.union(&r)));
        for c in &self.children {
            match c {
                Node::Group(g) => {
                    if let Some(b) = g.bbox_in_parent(&Transform::IDENTITY) {
                        add(b);
                    }
                }
                Node::Path(p) => {
                    if let Some(b) = p.data.bounds() {
                        add(b);
                    }
                }
                Node::Image(i) => add(i.view_rect),
            }
        }
        out
    }

    fn bbox_in_parent(&self, parent: &Transform) -> Option<Rect> {
        let ts = parent.pre_concat(&self.transform);
        let mut out: Option<Rect> = None;
        for c in &self.children {
            let b = match c {
                Node::Group(g) => g.bbox_in_parent(&ts),
                Node::Path(p) => p.data.bounds_with(&ts),
                Node::Image(i) => Some(i.view_rect.transform(&ts)),
            };
            if let Some(b) = b {
                out = Some(out.map_or(b, |o| o.union(&b)));
            }
        }
        out
    }
}

/// Fill rule for interiors (`fill-rule` / `clip-rule`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FillRule {
    #[default]
    NonZero,
    EvenOdd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineCap {
    #[default]
    Butt,
    Round,
    Square,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineJoin {
    #[default]
    Miter,
    /// SVG 2 `miter-clip`: past the limit the miter is clipped, not beveled.
    MiterClip,
    Round,
    Bevel,
}

/// Gradient spread (`spreadMethod`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpreadMethod {
    #[default]
    Pad,
    Reflect,
    Repeat,
}

/// One gradient color stop (offset already clamped and made monotonic).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stop {
    pub offset: f32,
    /// Stop color with `stop-opacity` folded into its alpha.
    pub color: Color,
}

/// A linear gradient in user space: `transform` maps gradient coordinates
/// (where `x1..y2` live) to the painted element's user space.
#[derive(Debug, Clone, PartialEq)]
pub struct LinearGradient {
    pub id: String,
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub transform: Transform,
    pub spread: SpreadMethod,
    pub stops: Vec<Stop>,
}

/// A two-point conical (SVG 2 radial) gradient: from the focal circle
/// `(fx, fy, fr)` to the end circle `(cx, cy, r)`.
#[derive(Debug, Clone, PartialEq)]
pub struct RadialGradient {
    pub id: String,
    pub cx: f32,
    pub cy: f32,
    pub r: f32,
    pub fx: f32,
    pub fy: f32,
    pub fr: f32,
    pub transform: Transform,
    pub spread: SpreadMethod,
    pub stops: Vec<Stop>,
}

/// A pattern paint: tiles repeat every `rect` in pattern space, mapped to
/// user space by `transform`. `root` is one tile's content in tile
/// coordinates, whose origin is the tile's top-left corner (`rect.x, rect.y`).
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    pub id: String,
    pub rect: Rect,
    pub transform: Transform,
    pub root: Group,
}

/// How an area or outline is painted.
#[derive(Debug, Clone, PartialEq)]
pub enum Paint {
    Color(Color),
    LinearGradient(Arc<LinearGradient>),
    RadialGradient(Arc<RadialGradient>),
    Pattern(Arc<Pattern>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fill {
    pub paint: Paint,
    /// `fill-opacity` in `[0, 1]`.
    pub opacity: f32,
    pub rule: FillRule,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stroke {
    pub paint: Paint,
    /// `stroke-opacity` in `[0, 1]`.
    pub opacity: f32,
    /// Stroke width in user units (always `> 0`).
    pub width: f32,
    pub cap: LineCap,
    pub join: LineJoin,
    pub miter_limit: f32,
    /// Resolved dash lengths (even count, all `>= 0`, positive sum); `None`
    /// is a solid stroke.
    pub dasharray: Option<Vec<f32>>,
    pub dashoffset: f32,
    /// `vector-effect: non-scaling-stroke`: `width` is in device pixels.
    pub non_scaling: bool,
}

/// A filled and/or stroked outline.
#[derive(Debug, Clone, PartialEq)]
pub struct Path {
    pub id: String,
    pub data: Arc<PathData>,
    pub fill: Option<Fill>,
    pub stroke: Option<Stroke>,
    /// `shape-rendering: crispEdges`/`optimizeSpeed` → aliased coverage.
    pub anti_alias: bool,
}

/// An embedded raster image (`<image>` with a `data:` URL). SVG images are
/// inlined as groups at parse time and never appear here.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub id: String,
    /// The rectangle the image is fitted into (after `preserveAspectRatio`).
    pub view_rect: Rect,
    /// Encoded bytes and their detected format.
    pub kind: ImageKind,
    /// `image-rendering: pixelated`/`optimizeSpeed`/`crisp-edges`.
    pub pixelated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImageKind {
    Png(Arc<Vec<u8>>),
    Jpeg(Arc<Vec<u8>>),
    Gif(Arc<Vec<u8>>),
    Webp(Arc<Vec<u8>>),
}

/// A clip path: the union of its children's fill geometry (paint ignored).
#[derive(Debug, Clone, PartialEq)]
pub struct ClipPath {
    pub id: String,
    /// Maps clip content to the clipped element's user space (includes the
    /// `objectBoundingBox` mapping when `clipPathUnits` asks for it).
    pub transform: Transform,
    /// A clip path may itself be clipped.
    pub clip_path: Option<Arc<ClipPath>>,
    pub root: Group,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MaskType {
    #[default]
    Luminance,
    Alpha,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Mask {
    pub id: String,
    /// The mask region in the masked element's user space; outside it the mask
    /// is zero.
    pub rect: Rect,
    pub kind: MaskType,
    /// A mask may itself be masked.
    pub mask: Option<Arc<Mask>>,
    /// Mask content (already transformed for `maskContentUnits`).
    pub root: Group,
}

/// A filter: a primitive graph evaluated over `region` (user space).
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    pub id: String,
    pub region: Rect,
    /// Primitives in document order; all lengths already resolved to user
    /// space (including `primitiveUnits="objectBoundingBox"`).
    pub primitives: Vec<FilterPrimitive>,
}

/// Filter primitive inputs.
#[derive(Debug, Clone, PartialEq)]
pub enum FilterInput {
    SourceGraphic,
    SourceAlpha,
    /// Output of an earlier primitive by `result` name.
    Reference(String),
    /// The previous primitive's result (or `SourceGraphic` for the first one).
    Previous,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FilterPrimitive {
    /// Subregion in user space.
    pub region: Rect,
    /// Operate in linearRGB (`color-interpolation-filters`, default) or sRGB.
    pub linear_rgb: bool,
    pub result: String,
    pub kind: FilterKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FilterKind {
    GaussianBlur {
        input: FilterInput,
        /// Standard deviation in user units.
        std_dev_x: f32,
        std_dev_y: f32,
    },
    Offset {
        input: FilterInput,
        dx: f32,
        dy: f32,
    },
    Flood {
        color: Color,
    },
    Blend {
        input1: FilterInput,
        input2: FilterInput,
        mode: BlendMode,
    },
    Composite {
        input1: FilterInput,
        input2: FilterInput,
        op: CompositeOp,
    },
    Merge {
        inputs: Vec<FilterInput>,
    },
    ColorMatrix {
        input: FilterInput,
        /// Row-major 4×5 matrix over straight RGBA in `[0, 1]`.
        matrix: [f32; 20],
    },
    ComponentTransfer {
        input: FilterInput,
        funcs: [TransferFunc; 4],
    },
    Morphology {
        input: FilterInput,
        dilate: bool,
        rx: f32,
        ry: f32,
    },
    Tile {
        input: FilterInput,
    },
    DropShadow {
        input: FilterInput,
        dx: f32,
        dy: f32,
        std_dev_x: f32,
        std_dev_y: f32,
        color: Color,
    },
    Turbulence {
        base_frequency_x: f32,
        base_frequency_y: f32,
        num_octaves: u32,
        seed: i32,
        stitch_tiles: bool,
        /// `type="fractalNoise"` (else `turbulence`).
        fractal_noise: bool,
    },
    ConvolveMatrix {
        input: FilterInput,
        order_x: u32,
        order_y: u32,
        /// `order_x * order_y` values, row-major.
        kernel: Vec<f32>,
        divisor: f32,
        bias: f32,
        target_x: u32,
        target_y: u32,
        edge_mode: EdgeMode,
        preserve_alpha: bool,
    },
    DisplacementMap {
        input1: FilterInput,
        input2: FilterInput,
        scale: f32,
        x_channel: ColorChannel,
        y_channel: ColorChannel,
    },
    DiffuseLighting {
        input: FilterInput,
        surface_scale: f32,
        diffuse_constant: f32,
        color: Color,
        light: LightSource,
    },
    SpecularLighting {
        input: FilterInput,
        surface_scale: f32,
        specular_constant: f32,
        specular_exponent: f32,
        color: Color,
        light: LightSource,
    },
    /// `feImage`: the referenced element or embedded image, in user space.
    Image {
        root: Group,
    },
    /// An invalid or unsupported primitive: transparent black.
    Empty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EdgeMode {
    None,
    #[default]
    Duplicate,
    Wrap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorChannel {
    R,
    G,
    B,
    A,
}

/// Light source of a lighting primitive; positions in user space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LightSource {
    Distant {
        azimuth: f32,
        elevation: f32,
    },
    Point {
        x: f32,
        y: f32,
        z: f32,
    },
    Spot {
        x: f32,
        y: f32,
        z: f32,
        points_at_x: f32,
        points_at_y: f32,
        points_at_z: f32,
        specular_exponent: f32,
        /// Degrees; `None` is unlimited.
        limiting_cone_angle: Option<f32>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CompositeOp {
    Over,
    In,
    Out,
    Atop,
    Xor,
    Arithmetic { k1: f32, k2: f32, k3: f32, k4: f32 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum TransferFunc {
    Identity,
    Table(Vec<f32>),
    Discrete(Vec<f32>),
    Linear {
        slope: f32,
        intercept: f32,
    },
    Gamma {
        amplitude: f32,
        exponent: f32,
        offset: f32,
    },
}

/// Why a document could not be converted into a [`Tree`].
#[derive(Debug, Clone, PartialEq)]
pub enum ParseError {
    /// Input is not UTF-8.
    NotUtf8,
    Xml(XmlError),
    /// The root element is not `<svg>`.
    NotSvg,
    /// The root size is zero, negative or not finite.
    InvalidSize,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::NotUtf8 => f.write_str("input is not UTF-8"),
            ParseError::Xml(e) => write!(f, "{e}"),
            ParseError::NotSvg => f.write_str("root element is not <svg>"),
            ParseError::InvalidSize => f.write_str("invalid document size"),
        }
    }
}

impl std::error::Error for ParseError {}

impl From<XmlError> for ParseError {
    fn from(e: XmlError) -> Self {
        ParseError::Xml(e)
    }
}

impl Tree {
    /// Parse and normalize an SVG document.
    pub fn parse(text: &str) -> Result<Tree, ParseError> {
        let doc = crate::xml::parse(text)?;
        let sd = crate::style::StyledDoc::new(doc);
        crate::convert::convert_doc(&sd)
    }

    /// [`parse`](Self::parse) over UTF-8 bytes (a leading BOM is skipped).
    pub fn from_data(data: &[u8]) -> Result<Tree, ParseError> {
        let data = data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(data);
        Tree::parse(std::str::from_utf8(data).map_err(|_| ParseError::NotUtf8)?)
    }
}
