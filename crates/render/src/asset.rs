//! Assets as the renderer draws them: a resource's identity, its format sniffed
//! from its bytes, a raster decoded to the texture convention, and a vector
//! scene placed into a box.
//!
//! A resource is named by the [`ResourceId`] of its path under the package's
//! `assets/` directory; the path exists only while compiling and packaging.
//! Every payload a resource takes on over a session — a build, an edit — has its
//! own [`ResourceRevision`].
//!
//! Rasters decode through `viso-image`, bounded by [`MAX_DIMENSION`] and
//! [`MAX_PIXELS`]; SVG through `viso-svg` and [`crate::svg`]. An image the
//! build converted to its target's texel format is read, not decoded: its
//! texels stay where the asset's bytes lie ([`decode_raster_asset`]).

use std::sync::Arc;

use crate::primitive::{Path, PathCmd, Point, Rect, Rgba};

pub use viso_image::raster::{
    AssetError, MAX_DIMENSION, MAX_PIXELS, RasterImage, Texels, decode_raster, decode_raster_asset,
};
pub use viso_schema::resource::{
    AssetBytes, AssetFormat, AssetKind, ResourceId, ResourceRevision, normalize_path,
};

/// An asset decoded, by how it draws.
#[derive(Debug, Clone, PartialEq)]
pub enum DecodedAsset {
    Raster(RasterImage),
    Vector(VectorScene),
}

/// A vector asset: filled and stroked paths in its own space, `(0, 0)` its
/// top-left and `size` its natural extent.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorScene {
    pub paths: Arc<[Path]>,
    pub size: [f32; 2],
}

impl VectorScene {
    /// About the bytes it holds.
    pub fn bytes(&self) -> u64 {
        let cmds: usize = self.paths.iter().map(|p| p.cmds.capacity()).sum();
        (self.paths.len() * size_of::<Path>() + cmds * size_of::<PathCmd>()) as u64
    }

    /// Its paths placed into `rect`, stretched to fill it, each color multiplied
    /// by `tint` when given (white keeps the colors, a color with full alpha
    /// recolors a monochrome icon by its alpha).
    pub fn place<'s>(&'s self, rect: Rect, tint: Option<Rgba>) -> impl Iterator<Item = Path> + 's {
        let sx = if self.size[0] > 0.0 {
            rect.w / self.size[0]
        } else {
            0.0
        };
        let sy = if self.size[1] > 0.0 {
            rect.h / self.size[1]
        } else {
            0.0
        };
        let at = move |p: Point| Point {
            x: rect.x + p.x * sx,
            y: rect.y + p.y * sy,
        };
        // A stroke scales by the geometric mean, exact for a uniform scale.
        let width_scale = (sx * sy).abs().sqrt();
        let tinted = move |c: Rgba| match tint {
            Some(t) => Rgba {
                r: c.r * t.r,
                g: c.g * t.g,
                b: c.b * t.b,
                a: c.a * t.a,
            },
            None => c,
        };
        self.paths.iter().map(move |path| Path {
            cmds: path
                .cmds
                .iter()
                .map(|cmd| match *cmd {
                    PathCmd::MoveTo(p) => PathCmd::MoveTo(at(p)),
                    PathCmd::LineTo(p) => PathCmd::LineTo(at(p)),
                    PathCmd::QuadTo(c, p) => PathCmd::QuadTo(at(c), at(p)),
                    PathCmd::CubicTo(a, b, p) => PathCmd::CubicTo(at(a), at(b), at(p)),
                    PathCmd::Close => PathCmd::Close,
                })
                .collect(),
            fill: path.fill.map(tinted),
            stroke: path.stroke.map(|mut s| {
                s.width *= width_scale;
                s.color = tinted(s.color);
                if let Some(dash) = &mut s.dash {
                    for seg in &mut dash.segments {
                        *seg *= width_scale;
                    }
                    dash.offset *= width_scale;
                }
                s
            }),
            shadow: path.shadow,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_normalizes_to_one_spelling_and_never_leaves_assets() {
        assert_eq!(
            normalize_path("images/./icons//a.png").as_deref(),
            Some("images/icons/a.png")
        );
        assert_eq!(
            normalize_path("images\\a.png").as_deref(),
            Some("images/a.png")
        );
        for bad in [
            "/etc/passwd",
            "../secret.png",
            "a/../../b",
            "",
            "./",
            "C:/x.png",
        ] {
            assert_eq!(normalize_path(bad), None, "{bad}");
        }
        assert_eq!(
            ResourceId::of("images/a.png"),
            ResourceId::of(&normalize_path("images//a.png").unwrap())
        );
        assert_ne!(ResourceId::of("a.png"), ResourceId::of("b.png"));
    }

    #[test]
    fn a_vector_scene_stretches_into_its_box_and_takes_a_tint() {
        let path = Path {
            cmds: vec![
                PathCmd::MoveTo(Point { x: 0.0, y: 0.0 }),
                PathCmd::LineTo(Point { x: 10.0, y: 20.0 }),
                PathCmd::Close,
            ],
            fill: Some(Rgba {
                r: 1.0,
                g: 0.5,
                b: 0.0,
                a: 1.0,
            }),
            stroke: Some(crate::primitive::Stroke::new(
                2.0,
                Rgba {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
            )),
            shadow: None,
        };
        let scene = VectorScene {
            paths: Arc::from(vec![path]),
            size: [10.0, 20.0],
        };
        let rect = Rect {
            x: 100.0,
            y: 50.0,
            w: 40.0,
            h: 80.0,
        };
        let tint = Rgba {
            r: 0.0,
            g: 0.0,
            b: 1.0,
            a: 0.5,
        };
        let placed: Vec<Path> = scene.place(rect, Some(tint)).collect();
        assert_eq!(
            placed[0].cmds[1],
            PathCmd::LineTo(Point { x: 140.0, y: 130.0 })
        );
        assert_eq!(placed[0].stroke.unwrap().width, 8.0);
        assert_eq!(
            placed[0].fill,
            Some(Rgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.5
            })
        );
    }
}
