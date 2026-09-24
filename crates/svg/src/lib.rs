//! `viso-svg` — the SVG input lane (§13).
//!
//! SVG is an **input format**, not a per-frame renderer: bytes are parsed once
//! into a normalized vector scene and lowered to the same retained
//! [`viso_render::Primitive`] stream every other source produces, so the result
//! is a cacheable Render IR input rather than a live XML DOM. [`parse_svg`] is
//! the whole public surface — a synchronous, allocation-bounded CPU pass. Static
//! assets can run it at build time and dynamic assets on a worker thread (§26);
//! the call site owns that policy, this crate just does the transform.
//!
//! The crate owns the whole parse, dependency-free (std only):
//!
//! ```text
//! bytes ─► xml ─► css cascade ─► convert ─► tree::Tree ─► lower ─► Primitive
//! ```
//!
//! [`tree::Tree`] is fully normalized — shapes are paths, `<use>`/`<symbol>`/
//! `<switch>` are expanded, units and percentages are resolved, and paint
//! servers, clip paths, masks, markers and filters are in the referencing
//! element's user space. It is public for advanced consumers that need more
//! than the lowered scene. Not supported: `<text>`, external (non-`data:`)
//! resources, SVGZ.
//!
//! Lowering is thin: each
//! tree `Path` becomes one [`viso_render::Path`], with its accumulated transform
//! *baked into the coordinates* (an SVG transform is a parse-time one-shot,
//! never a per-frame matrix) and into the stroke width/dashes (by the
//! transform's mean scale), its solid fill/stroke paint converted to
//! straight-linear [`Rgba`](viso_render::Rgba), and its stroke cap/join/miter
//! mapped onto [`viso_render::Stroke`].
//!
//! Scope this round: paths with solid fill and/or solid stroke. Group opacity is
//! multiplied into each descendant's paint alpha (exact for non-overlapping
//! content). Gradient/pattern paints, images, and text are skipped rather than
//! mis-rendered; clip-paths, masks, and filters are ignored (their content
//! paths still render) until the renderer grows layers.

#![forbid(unsafe_op_in_unsafe_fn)]

mod color;
mod convert;
mod css;
mod data_url;
mod filter;
mod geom;
mod lower;
mod marker;
mod paint;
mod path_data;
mod style;
mod syntax;
pub mod tree;
mod xml;

use std::fmt;

use viso_math::{Point as MPoint, Rect};
use viso_render::{PathCmd, Primitive};

/// A parsed SVG document lowered to a retained primitive scene.
///
/// `prims` is in document paint order (a group's children after the group's
/// earlier siblings), ready to hand to the renderer or cache as Render IR.
/// `size` is the document's intrinsic size in its user-space units (the resolved
/// `width`/`height`, i.e. the `viewBox` mapped through any outer dimensions),
/// the natural layout box for the asset.
#[derive(Debug, Clone, PartialEq)]
pub struct SvgScene {
    /// The lowered primitives, in paint order.
    pub prims: Vec<Primitive>,
    /// Intrinsic document size `(width, height)` in user-space units.
    pub size: (f32, f32),
}

impl SvgScene {
    /// Axis-aligned bounding box of all drawable content, in the same
    /// physical-pixel space as [`size`](Self::size), or `None` when the scene
    /// has no lowered paths (an empty document, or the gradient/pattern-only
    /// paints this round skips).
    ///
    /// Distinct from `size`, which is the *document* box: this is a box
    /// over every path's baked anchor and Bézier control points. Including the
    /// control points makes it a conservative bound — the control hull contains
    /// the curve, so a bulging curve is never under-reported. The fold runs
    /// through [`viso_math::point_bounds`] over one contiguous point array, the
    /// SIMD-accelerated min/max reduction (§13.6); the strided post-stroke
    /// `geo_bounds` fold in `viso-render` stays scalar and points contiguous
    /// callers such as this one here.
    pub fn content_bounds(&self) -> Option<Rect> {
        let mut pts: Vec<MPoint> = Vec::new();
        for prim in &self.prims {
            if let Primitive::Path(path) = prim {
                for cmd in &path.cmds {
                    match *cmd {
                        PathCmd::MoveTo(p) | PathCmd::LineTo(p) => {
                            pts.push(MPoint::new(p.x, p.y));
                        }
                        PathCmd::QuadTo(c, p) => {
                            pts.push(MPoint::new(c.x, c.y));
                            pts.push(MPoint::new(p.x, p.y));
                        }
                        PathCmd::CubicTo(c0, c1, p) => {
                            pts.push(MPoint::new(c0.x, c0.y));
                            pts.push(MPoint::new(c1.x, c1.y));
                            pts.push(MPoint::new(p.x, p.y));
                        }
                        PathCmd::Close => {}
                    }
                }
            }
        }
        if pts.is_empty() {
            return None;
        }
        Some(viso_math::point_bounds(&pts))
    }
}

/// Why an SVG failed to parse or lower.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SvgError {
    /// The bytes are not a valid SVG document.
    Parse(String),
}

impl fmt::Display for SvgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SvgError::Parse(msg) => write!(f, "invalid SVG: {msg}"),
        }
    }
}

impl std::error::Error for SvgError {}

/// Parse SVG `bytes` and lower them to a retained [`SvgScene`].
///
/// A synchronous CPU pass: parse with `viso-svg`, then walk the tree
/// depth-first in paint order, emitting one [`Primitive::Path`] per solid
/// filled/stroked path. Curves are kept as `Path` curve commands (the
/// tessellator flattens them); the accumulated transform is baked into every
/// coordinate. Paths with no solid paint we can represent are skipped.
pub fn parse_svg(bytes: &[u8]) -> Result<SvgScene, SvgError> {
    let tree = tree::Tree::from_data(bytes).map_err(|e| SvgError::Parse(e.to_string()))?;
    Ok(SvgScene {
        prims: lower::lower_tree(&tree),
        size: tree.size,
    })
}
