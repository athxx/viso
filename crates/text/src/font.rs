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
    /// Number of glyphs in the face — a coarse coverage weight used only to
    /// order fallback candidates when several cover a character (a face with a
    /// larger glyph repertoire is a better general fallback).
    pub glyph_count: u16,
    /// Whether the face carries color-bitmap strikes (`CBDT`/`CBLC` or `sbix`).
    /// Decided once at load so the glyph path only probes the color atlas for
    /// faces that could have a strike — pure-text faces stay on the SDF path
    /// with no per-glyph raster-image lookup.
    has_color: bool,
    /// Whether this face is a color-emoji face that must be rasterized through
    /// the platform text engine rather than `ttf-parser`. Set by the provider
    /// when it resolves an emoji face: a system emoji face arrives with its
    /// `sbix`/`CBDT` strikes stripped (they are ~180 MB and dropped at load), so
    /// `has_color` reads `false` and the glyph path cannot detect it as color.
    /// This flag is the out-of-band signal that its color glyphs come from the
    /// platform rasterizer keyed on [`postscript_name`](Self::postscript_name).
    is_color_emoji: bool,
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
        let glyph_count = face.number_of_glyphs();
        let tables = face.tables();
        let has_color = tables.cbdt.is_some() || tables.sbix.is_some();
        Some(Self {
            bytes,
            index,
            units_per_em: upem,
            ascender_em,
            descender_em,
            line_gap_em,
            glyph_count,
            has_color,
            is_color_emoji: false,
        })
    }

    /// Mark this face as a color-emoji face whose color glyphs are rasterized
    /// through the platform text engine. Called by the provider when it resolves
    /// an emoji face (its strikes are stripped at load, so it cannot be detected
    /// from the table set); see [`is_color_emoji`](Self::is_color_emoji).
    pub fn set_color_emoji(&mut self, yes: bool) {
        self.is_color_emoji = yes;
    }

    /// Whether this face is a platform-rasterized color-emoji face. When `true`,
    /// the glyph path routes its color glyphs to the [`ColorGlyphRasterizer`]
    /// (keyed on [`postscript_name`](Self::postscript_name)) instead of the
    /// `ttf-parser` raster-image path.
    ///
    /// [`ColorGlyphRasterizer`]: crate::ColorGlyphRasterizer
    pub fn is_color_emoji(&self) -> bool {
        self.is_color_emoji
    }

    /// The PostScript name (name id 6) of this face, if present, used to re-open
    /// the face in the platform text engine for color rasterization.
    ///
    /// `ttf-parser`'s own name decoding only accepts Unicode/Windows records
    /// (UTF-16BE); Apple's color-emoji face records its PostScript name only on a
    /// Macintosh/Roman record, which that decoder rejects, so we read the raw
    /// bytes and decode them as ASCII directly — the PostScript name is
    /// constrained to printable ASCII, so any all-printable record is the name.
    pub fn postscript_name(&self) -> Option<String> {
        let face = self.ttf();
        face.names()
            .into_iter()
            .filter(|n| n.name_id == ttf_parser::name_id::POST_SCRIPT_NAME)
            .find_map(|n| {
                let raw = n.name;
                raw.iter()
                    .all(|&b| (0x20..0x7f).contains(&b))
                    .then(|| String::from_utf8_lossy(raw).into_owned())
            })
    }

    /// Whether the face has color-bitmap strikes worth probing. When `false`,
    /// every glyph rasters as an SDF outline and the color atlas is never
    /// touched for this face.
    pub fn has_color_strikes(&self) -> bool {
        self.has_color
    }

    /// Baseline-to-baseline advance for successive lines, in em.
    pub fn line_height_em(&self) -> f32 {
        self.ascender_em - self.descender_em + self.line_gap_em
    }

    /// Whether this face has a glyph for `c` in its character map. A cheap
    /// `cmap` lookup — the coverage probe the fallback chain walks to pick the
    /// first face that can render a character before committing it to shaping.
    ///
    /// Coverage here means "the cmap maps this code point to a non-`.notdef`
    /// glyph"; it does not attest that shaping will keep that glyph (a complex
    /// script may still substitute or drop it), which is why shaping remains the
    /// authority and this is only the ordering hint for the chain.
    pub fn has_char(&self, c: char) -> bool {
        self.ttf().glyph_index(c).is_some()
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

/// A registry of loaded faces plus an ordered **fallback chain**. `FontId`
/// indexes the face table; the chain is the sequence of faces shaping tries in
/// order — the first face renders what it covers, and every character it leaves
/// as `.notdef` is reshaped against the next face in the chain (this is the
/// ordering the facade's itemized shaping consumes; see the crate docs).
///
/// The chain is authored explicitly with [`FontStore::push_fallback`]: loading
/// a face registers it but does **not** add it to the chain, so a face can be
/// resolved on demand (a system-font query) and appended only once, without
/// every load implicitly changing shaping order. A newly loaded face is the
/// primary if the chain was empty (so a single-face store behaves as before).
#[derive(Default)]
pub struct FontStore {
    faces: Vec<FontFace>,
    /// The fallback chain: faces to try in order. `chain[0]` is the primary.
    chain: Vec<FontId>,
}

impl FontStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load a face from raw sfnt bytes, registering it in the face table. If the
    /// chain was empty, the new face becomes the primary (chain head), so a
    /// store loaded with a single face shapes with it exactly as before. Returns
    /// `None` if unparseable.
    pub fn load(&mut self, bytes: impl Into<Box<[u8]>>, index: u32) -> Option<FontId> {
        let face = FontFace::new(bytes.into(), index)?;
        let id = FontId(self.faces.len() as u32);
        self.faces.push(face);
        if self.chain.is_empty() {
            self.chain.push(id);
        }
        Some(id)
    }

    /// Append `id` to the fallback chain if it is not already present. The
    /// caller resolves a fallback face (bundled or system) and adds it here once;
    /// shaping then reshapes any `.notdef` run against it. A duplicate append is
    /// a no-op, so re-resolving the same face never lengthens the chain.
    pub fn push_fallback(&mut self, id: FontId) {
        debug_assert!((id.0 as usize) < self.faces.len(), "unregistered FontId");
        if !self.chain.contains(&id) {
            self.chain.push(id);
        }
    }

    /// The fallback chain: the faces shaping tries, in order. `chain()[0]` is the
    /// primary face. Empty until the first face is loaded.
    pub fn chain(&self) -> &[FontId] {
        &self.chain
    }

    /// The primary (first) face id, or `None` if no face is loaded.
    pub fn primary(&self) -> Option<FontId> {
        self.chain.first().copied()
    }

    /// The first face in the chain whose cmap covers `c`, or `None` if no chain
    /// face covers it (the character will shape to `.notdef` and its script
    /// becomes a system-font query candidate). A cheap per-face
    /// [`FontFace::has_char`] probe, used to pick a fallback without a trial
    /// shaping pass.
    pub fn first_covering(&self, c: char) -> Option<FontId> {
        self.chain
            .iter()
            .copied()
            .find(|&id| self.face(id).has_char(c))
    }

    /// Borrow a loaded face by id.
    pub fn face(&self, id: FontId) -> &FontFace {
        &self.faces[id.0 as usize]
    }

    /// Mark a loaded face as a platform-rasterized color-emoji face. Called by
    /// [`SystemFallback`](crate::SystemFallback) right after it loads a face for
    /// the [`FontRole::Emoji`](crate::FontRole) branch: the system emoji face
    /// arrives with its strikes stripped, so it cannot be detected from the table
    /// set and must be flagged out of band (see [`FontFace::is_color_emoji`]).
    pub fn mark_color_emoji(&mut self, id: FontId) {
        self.faces[id.0 as usize].set_color_emoji(true);
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
