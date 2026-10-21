//! Variable-font instances: the design-axis coordinates a face draws at.
//!
//! A variable face carries every weight, width and slant of its family in one
//! file; a request picks an instance of it by setting the registered axes
//! (`wght`, `wdth`, `ital`, `slnt`) from its style, each clamped to the range
//! the face declares, the way CSS maps `font-weight` and friends onto a
//! variable font. The coordinates travel with the face's bytes, and every
//! reader of the bytes — shaping (advances through `HVAR`), outlines (`gvar`,
//! `CFF2`), metrics (`MVAR`), rasterization — applies them, so an instance is
//! a face of its own with its own [`FontFaceId`](crate::FontFaceId).
//!
//! A platform that resolves the instance itself (CoreText's live font, the
//! Android matcher) reports its coordinates instead, which are authoritative:
//! they are the instance the platform rasterizes.

use crate::font_request::{FontSlant, FontWeight, FontWidth};

/// The most axes an instance sets.
const MAX_AXES: usize = 8;

/// The oblique angle a slanted request leans by, in degrees: CSS's default.
const OBLIQUE_DEGREES: f32 = 14.0;

/// The coordinates a face draws at: each axis it sets and the value, the
/// default instance when empty. Fixed-size and `Copy`, so a face's bytes and
/// its instance cross threads together without an allocation.
#[derive(Debug, Clone, Copy, Default)]
pub struct Variations {
    axes: [([u8; 4], f32); MAX_AXES],
    len: u8,
}

impl Variations {
    /// The default instance: no axis set.
    pub const DEFAULT: Self = Self {
        axes: [([0; 4], 0.0); MAX_AXES],
        len: 0,
    };

    /// Coordinates as a platform reports them, the first [`MAX_AXES`] kept.
    pub fn from_axes(axes: impl IntoIterator<Item = ([u8; 4], f32)>) -> Self {
        let mut out = Self::DEFAULT;
        for (tag, value) in axes {
            out.push(tag, value);
        }
        out
    }

    /// The coordinates the face in `sfnt` at `index` takes for a request of
    /// `weight`, `width` and `slant`: each registered axis it declares, clamped
    /// to its range and left out at its default. Empty for a face that is not
    /// variable or does not parse.
    pub fn for_style(
        sfnt: &[u8],
        index: u32,
        weight: FontWeight,
        width: FontWidth,
        slant: FontSlant,
    ) -> Self {
        let Ok(face) = ttf_parser::Face::parse(sfnt, index) else {
            return Self::DEFAULT;
        };
        Self::of_face(&face, weight, width, slant)
    }

    /// [`for_style`](Self::for_style) over a parsed face.
    pub fn of_face(
        face: &ttf_parser::Face<'_>,
        weight: FontWeight,
        width: FontWidth,
        slant: FontSlant,
    ) -> Self {
        let mut out = Self::DEFAULT;
        if !face.is_variable() {
            return out;
        }
        let axes = face.variation_axes();
        let has_ital = axes.into_iter().any(|a| a.tag.to_bytes() == *b"ital");
        for axis in axes {
            let tag = axis.tag.to_bytes();
            let wanted = match &tag {
                b"wght" => f32::from(weight.0),
                b"wdth" => width_percent(width),
                b"ital" => match slant {
                    FontSlant::Italic => 1.0,
                    _ => 0.0,
                },
                // A slanted request leans the upright design, and an italic
                // one does too where the face has no italic axis of its own.
                b"slnt" => match slant {
                    FontSlant::Oblique => -OBLIQUE_DEGREES,
                    FontSlant::Italic if !has_ital => -OBLIQUE_DEGREES,
                    _ => 0.0,
                },
                _ => continue,
            };
            let value = wanted.clamp(axis.min_value, axis.max_value);
            if value != axis.def_value {
                out.push(tag, value);
            }
        }
        out
    }

    fn push(&mut self, tag: [u8; 4], value: f32) {
        if !value.is_finite() || usize::from(self.len) == MAX_AXES {
            return;
        }
        self.axes[usize::from(self.len)] = (tag, value);
        self.len += 1;
    }

    /// Whether this is the default instance.
    pub fn is_default(&self) -> bool {
        self.len == 0
    }

    /// Each axis set and its value.
    pub fn as_slice(&self) -> &[([u8; 4], f32)] {
        &self.axes[..usize::from(self.len)]
    }

    /// Sets these coordinates on a parsed face, for its outlines, advances
    /// and metrics.
    pub fn apply(&self, face: &mut ttf_parser::Face<'_>) {
        for &(tag, value) in self.as_slice() {
            face.set_variation(ttf_parser::Tag::from_bytes(&tag), value);
        }
    }

    /// Sets these coordinates on a shaping face.
    pub fn apply_shaping(&self, face: &mut rustybuzz::Face<'_>) {
        if self.is_default() {
            return;
        }
        let variations: [rustybuzz::Variation; MAX_AXES] = std::array::from_fn(|at| {
            let (tag, value) = self.axes[at];
            rustybuzz::Variation {
                tag: ttf_parser::Tag::from_bytes(&tag),
                value,
            }
        });
        face.set_variations(&variations[..usize::from(self.len)]);
    }
}

impl PartialEq for Variations {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice().len() == other.as_slice().len()
            && self
                .as_slice()
                .iter()
                .zip(other.as_slice())
                .all(|(a, b)| a.0 == b.0 && a.1.to_bits() == b.1.to_bits())
    }
}

impl Eq for Variations {}

impl std::hash::Hash for Variations {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        for &(tag, value) in self.as_slice() {
            tag.hash(state);
            value.to_bits().hash(state);
        }
    }
}

/// A `usWidthClass` as the `wdth` axis's percentage of normal width.
fn width_percent(width: FontWidth) -> f32 {
    match width.0 {
        0 | 1 => 50.0,
        2 => 62.5,
        3 => 75.0,
        4 => 87.5,
        5 => 100.0,
        6 => 112.5,
        7 => 125.0,
        8 => 150.0,
        _ => 200.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEJAVU: &[u8] = include_bytes!("../tests/fixtures/DejaVuSans-subset.ttf");
    /// Roboto's variable instance space: `wght` 100–900 (400), `wdth` 75–100
    /// (100), `ital` 0–1 (0).
    const ROBOTO_VF: &[u8] = include_bytes!("../tests/fixtures/RobotoVF-subset.ttf");

    #[test]
    fn a_static_face_has_only_the_default_instance() {
        let v = Variations::for_style(DEJAVU, 0, FontWeight::BOLD, FontWidth(3), FontSlant::Italic);
        assert!(v.is_default());
    }

    #[test]
    fn a_variable_face_takes_its_axes_from_the_style_clamped_and_default_left_out() {
        let regular = Variations::for_style(
            ROBOTO_VF,
            0,
            FontWeight::REGULAR,
            FontWidth::NORMAL,
            FontSlant::Normal,
        );
        assert!(regular.is_default(), "the default instance");
        let bold = Variations::for_style(
            ROBOTO_VF,
            0,
            FontWeight::BOLD,
            FontWidth::NORMAL,
            FontSlant::Normal,
        );
        assert_eq!(bold.as_slice(), [(*b"wght", 700.0)]);
        let wide_italic = Variations::for_style(
            ROBOTO_VF,
            0,
            FontWeight(1000),
            FontWidth(9),
            FontSlant::Italic,
        );
        assert_eq!(
            wide_italic.as_slice(),
            [(*b"wght", 900.0), (*b"ital", 1.0)],
            "clamped to the axes, `wdth` past its maximum is its default"
        );
        let narrow = Variations::for_style(
            ROBOTO_VF,
            0,
            FontWeight::REGULAR,
            FontWidth(1),
            FontSlant::Oblique,
        );
        assert_eq!(narrow.as_slice(), [(*b"wdth", 75.0)], "no `slnt` axis");
    }

    #[test]
    fn an_instance_draws_and_shapes_at_its_coordinates() {
        let advance = |v: Variations| {
            let mut face = ttf_parser::Face::parse(ROBOTO_VF, 0).unwrap();
            v.apply(&mut face);
            let glyph = face.glyph_index('o').unwrap();
            face.glyph_hor_advance(glyph).unwrap()
        };
        // Roboto's black `o` is narrower than its regular one.
        let bold = Variations::from_axes([(*b"wght", 900.0)]);
        assert_eq!(advance(Variations::DEFAULT), 1168);
        assert_eq!(advance(bold), 1151);
        let mut shaping = rustybuzz::Face::from_slice(ROBOTO_VF, 0).unwrap();
        bold.apply_shaping(&mut shaping);
        let mut buffer = rustybuzz::UnicodeBuffer::new();
        buffer.push_str("o");
        let shaped = rustybuzz::shape(&shaping, &[], buffer);
        assert_eq!(
            shaped.glyph_positions()[0].x_advance,
            i32::from(advance(bold))
        );
    }

    #[test]
    fn reported_coordinates_keep_their_order_and_compare_by_value() {
        let a = Variations::from_axes([(*b"wght", 700.0), (*b"opsz", 17.0)]);
        let b = Variations::from_axes([(*b"wght", 700.0), (*b"opsz", 17.0)]);
        assert_eq!(a, b);
        assert_ne!(a, Variations::from_axes([(*b"wght", 700.0)]));
        assert_eq!(a.as_slice()[1], (*b"opsz", 17.0));
        assert!(!a.is_default());
        assert_eq!(width_percent(FontWidth::NORMAL), 100.0);
    }
}
