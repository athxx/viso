//! The icons labels show, by name: SVG documents an app hands the view, and
//! a built-in dot.

use std::collections::HashMap;
use std::sync::Arc;

use viso_render::svg::{SvgError, parse_svg};
use viso_render::{Mesh, Primitive, Rgba};
use viso_ui::{Drawing, Vec2};

use crate::tess::MeshBuilder;

enum Art {
    Dot,
    Svg {
        prims: Arc<[Primitive]>,
        size: [f32; 2],
        tint: bool,
    },
}

/// Icons by name. Every set has `"dot"`, a small disc in the icon's color.
pub struct IconSet {
    icons: HashMap<String, Art>,
}

impl std::fmt::Debug for IconSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IconSet")
            .field("names", &self.icons.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Default for IconSet {
    fn default() -> Self {
        Self::new()
    }
}

impl IconSet {
    /// A set of only the dot.
    pub fn new() -> Self {
        let mut icons = HashMap::new();
        icons.insert("dot".to_owned(), Art::Dot);
        Self { icons }
    }

    /// With `svg` as icon `name`; `tint` paints its fills and strokes in the
    /// label's icon color.
    pub fn svg(mut self, name: &str, svg: &[u8], tint: bool) -> Result<Self, SvgError> {
        let scene = parse_svg(svg)?;
        self.icons.insert(
            name.to_owned(),
            Art::Svg {
                prims: scene.prims.into(),
                size: [scene.size.0.max(1.0), scene.size.1.max(1.0)],
                tint,
            },
        );
        Ok(self)
    }

    /// Whether it has icon `name`.
    pub fn has(&self, name: &str) -> bool {
        self.icons.contains_key(name)
    }

    /// Icon `name` drawn `side` points across in `color` (straight linear),
    /// with `b`'s scratch.
    pub fn drawing(
        &self,
        name: &str,
        color: [f32; 4],
        side: f32,
        b: &mut MeshBuilder,
    ) -> Option<Drawing> {
        match self.icons.get(name)? {
            Art::Dot => {
                b.clear();
                let c = side / 2.0;
                b.circle([c, c], side * 0.3, color, Some((1.0, [1.0, 1.0, 1.0, 1.0])));
                let mesh = Mesh::new(b.vertices.as_slice(), b.indices.as_slice());
                Some(Drawing::new(
                    vec![Primitive::Mesh(mesh)],
                    Vec2::new(side, side),
                ))
            }
            Art::Svg { prims, size, tint } => {
                let paint = Rgba::new(color[0], color[1], color[2], color[3]);
                let prims: Vec<Primitive> = prims
                    .iter()
                    .map(|p| match (p, tint) {
                        (Primitive::Path(path), true) => {
                            let mut path = path.clone();
                            if path.fill.is_some() {
                                path.fill = Some(paint);
                            }
                            if let Some(s) = &mut path.stroke {
                                s.color = paint;
                            }
                            Primitive::Path(path)
                        }
                        _ => p.clone(),
                    })
                    .collect();
                Some(Drawing::new(prims, Vec2::new(size[0], size[1])))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUP: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="12" height="12" viewBox="0 0 12 12"><path d="M2 3h7v5a3 3 0 0 1-3 3H5a3 3 0 0 1-3-3z" fill="#000"/></svg>"##;

    #[test]
    fn an_svg_icon_draws_tinted_and_the_dot_is_always_there() {
        let set = IconSet::new().svg("cafe", CUP, true).unwrap();
        let mut b = MeshBuilder::new();
        let red = [1.0, 0.0, 0.0, 1.0];
        let d = set.drawing("cafe", red, 14.0, &mut b).unwrap();
        let Primitive::Path(p) = &d.primitives()[0] else {
            panic!("a path");
        };
        assert_eq!(p.fill, Some(Rgba::new(1.0, 0.0, 0.0, 1.0)));
        assert_eq!(d.extent(), Vec2::new(12.0, 12.0));
        assert!(set.drawing("dot", red, 12.0, &mut b).is_some());
        assert!(set.drawing("nope", red, 12.0, &mut b).is_none());
        assert!(IconSet::new().svg("bad", b"<nope", false).is_err());
    }
}
