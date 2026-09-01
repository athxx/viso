//! Font loading, metrics, and glyph outline collection.
//!
//! A [`FontFace`] owns its raw sfnt bytes and reconstructs a parser/shaper face
//! on demand — parsing an already-loaded table set is cheap, and holding an
//! owned buffer avoids the self-referential borrow that `ttf_parser::Face<'a>`
//! and `rustybuzz::Face<'a>` would otherwise force.
//!
//! All metrics are stored in **em units** (raw font units divided by
//! `units_per_em`), so shaping and layout stay decoupled from pixel size; the
//! caller multiplies by `font_size_px` only when producing screen geometry.

use crate::FontId;

/// One outline segment, in **raw font units**, Y-up (font design space).
///
/// Coordinates are exactly as reported by the outline builder; the rasterizer
/// applies the em→pixel transform and the Y flip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Command {
    MoveTo {
        x: f32,
        y: f32,
    },
    LineTo {
        x: f32,
        y: f32,
    },
    QuadTo {
        cx: f32,
        cy: f32,
        x: f32,
        y: f32,
    },
    CurveTo {
        c1x: f32,
        c1y: f32,
        c2x: f32,
        c2y: f32,
        x: f32,
        y: f32,
    },
    Close,
}

/// A loaded font face: owned bytes plus cached em-space metrics.
pub struct FontFace {
    bytes: Box<[u8]>,
    index: u32,
    /// Design units per em — the divisor that maps raw units to em space.
    pub units_per_em: f32,
    /// Distance from baseline up to the top of glyphs, in em.
    pub ascender_em: f32,
    /// Distance from baseline down to the bottom of glyphs, in em (negative).
    pub descender_em: f32,
    /// Extra leading between lines, in em.
    pub line_gap_em: f32,
}

impl FontFace {
    /// Parse metrics from `bytes` (a single face at `index`). Returns `None` if
    /// the data is not a parseable sfnt face.
    fn new(bytes: Box<[u8]>, index: u32) -> Option<Self> {
        let face = ttf_parser::Face::parse(&bytes, index).ok()?;
        let upem = face.units_per_em() as f32;
        let ascender_em = face.ascender() as f32 / upem;
        let descender_em = face.descender() as f32 / upem;
        let line_gap_em = face.line_gap() as f32 / upem;
        Some(Self {
            bytes,
            index,
            units_per_em: upem,
            ascender_em,
            descender_em,
            line_gap_em,
        })
    }

    /// Baseline-to-baseline advance for successive lines, in em.
    pub fn line_height_em(&self) -> f32 {
        self.ascender_em - self.descender_em + self.line_gap_em
    }

    /// Construct a fresh `ttf_parser::Face` borrowing our owned bytes. Cheap —
    /// re-parses the already-validated table directory.
    pub fn ttf(&self) -> ttf_parser::Face<'_> {
        ttf_parser::Face::parse(&self.bytes, self.index).expect("font bytes validated at load time")
    }

    /// Construct a fresh `rustybuzz::Face` borrowing our owned bytes.
    pub fn rb(&self) -> rustybuzz::Face<'_> {
        rustybuzz::Face::from_slice(&self.bytes, self.index)
            .expect("font bytes validated at load time")
    }

    /// Collect the outline of `glyph_id` as [`Command`]s in raw font units,
    /// Y-up. Returns `None` if the glyph has no outline (e.g. whitespace).
    pub fn outline(&self, glyph_id: u16) -> Option<Vec<Command>> {
        let face = self.ttf();
        let mut collector = OutlineCollector { cmds: Vec::new() };
        face.outline_glyph(ttf_parser::GlyphId(glyph_id), &mut collector)?;
        Some(collector.cmds)
    }
}

/// A registry of loaded faces. `FontId` indexes into it.
#[derive(Default)]
pub struct FontStore {
    faces: Vec<FontFace>,
}

impl FontStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load a face from raw sfnt bytes. Returns `None` if unparseable.
    pub fn load(&mut self, bytes: impl Into<Box<[u8]>>, index: u32) -> Option<FontId> {
        let face = FontFace::new(bytes.into(), index)?;
        let id = FontId(self.faces.len() as u32);
        self.faces.push(face);
        Some(id)
    }

    /// Borrow a loaded face by id.
    pub fn face(&self, id: FontId) -> &FontFace {
        &self.faces[id.0 as usize]
    }
}

/// Collects `ttf_parser` outline callbacks into [`Command`]s (raw units, Y-up).
struct OutlineCollector {
    cmds: Vec<Command>,
}

impl ttf_parser::OutlineBuilder for OutlineCollector {
    fn move_to(&mut self, x: f32, y: f32) {
        self.cmds.push(Command::MoveTo { x, y });
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.cmds.push(Command::LineTo { x, y });
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.cmds.push(Command::QuadTo { cx, cy, x, y });
    }
    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        self.cmds.push(Command::CurveTo {
            c1x,
            c1y,
            c2x,
            c2y,
            x,
            y,
        });
    }
    fn close(&mut self) {
        self.cmds.push(Command::Close);
    }
}
