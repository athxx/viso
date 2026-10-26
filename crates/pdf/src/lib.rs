//! `viso-pdf` — PDF documents drawn from the renderer's primitives.
//!
//! A [`Document`] takes pages of [`Draw`]s — the primitives a paint walk
//! emits, with text handed over as [`TextRun`]s of positioned glyphs — and
//! writes a PDF 1.7 file of them:
//!
//! - boxes, rounded boxes, ellipses, capsules, lines, paths and meshes as
//!   vector paths and shadings, their borders and strokes exact;
//! - gradients as smooth shadings, linear and radial native, sweeps as fine
//!   triangle fans, each sampled from the renderer's own ramp so the stops
//!   interpolate as on screen;
//! - text as Type 3 fonts built from each glyph's outline, mapped back to
//!   the text it shows, so a reader selects, searches and copies it;
//! - layers as clips, translucent and blended layers as transparency
//!   groups, color effects folded into every color they cover;
//! - what has no vector form — images, shadows, programs, scenes — drawn by
//!   the [`Source`] into a raster at the document's resolution.
//!
//! Positions are in content pixels; a [`Part`] maps a slice of them
//! onto the paper. Glyphs and rasters come from the [`Source`] the caller
//! implements, as they are first drawn; a glyph is written once however many
//! pages show it, and a raster once per distinct primitive.

#![forbid(unsafe_code)]

mod content;
mod font;
mod image;
mod object;
mod paint;
mod shading;

use std::collections::HashMap;
use std::io::Write;

pub use viso_math::Affine2;
pub use viso_render::{PathCmd, Point, Primitive, Rect, Rgba};

use crate::font::Fonts;
use crate::object::{Ref, Writer, text};

/// What a document says about itself.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Info {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    /// The app that made it.
    pub creator: Option<String>,
    /// Its language, a BCP 47 tag.
    pub lang: Option<String>,
}

/// One stretch of content a page shows: a slice of content pixels mapped
/// into a box of the paper. A page shows its body this way, and its header
/// and footer.
#[derive(Debug, Clone, Copy)]
pub struct Part<'a> {
    /// The box on the paper the content shows in, in points from the top
    /// left; nothing of it draws outside.
    pub area: Rect,
    /// Points per content pixel.
    pub scale: f32,
    /// The content pixel at the area's top left.
    pub origin: [f32; 2],
    /// What it draws, in paint order.
    pub draws: &'a [Draw<'a>],
    /// Where each transform root of `draws` is placed.
    pub places: &'a [Affine2],
}

/// One thing a page draws, in paint order.
#[derive(Debug, Clone, Copy)]
pub enum Draw<'a> {
    Primitive(&'a Primitive),
    Text(&'a TextRun),
}

/// A run of glyphs of one face, size and color.
#[derive(Debug, Clone, PartialEq)]
pub struct TextRun {
    /// The face, as the [`Source`] names it.
    pub face: u32,
    /// The size in content pixels per em.
    pub size: f32,
    pub color: Rgba,
    pub glyphs: Vec<TextGlyph>,
}

/// A glyph placed on its baseline.
#[derive(Debug, Clone, PartialEq)]
pub struct TextGlyph {
    pub glyph: u16,
    /// Its pen position on the baseline, in content pixels.
    pub origin: [f32; 2],
    /// The text it shows; empty for a glyph that continues another's.
    pub text: Box<str>,
}

/// A glyph's design, in font units, y up from the baseline.
#[derive(Debug, Clone, PartialEq)]
pub struct Glyph {
    pub units_per_em: f32,
    /// The face's reach above the baseline and below it (negative).
    pub ascender: f32,
    pub descender: f32,
    pub advance: f32,
    pub shape: GlyphShape,
}

/// What a glyph draws.
#[derive(Debug, Clone, PartialEq)]
pub enum GlyphShape {
    /// Its outline, filled nonzero.
    Outline(Vec<PathCmd>),
    /// A coverage mask, for a face whose outline is out of reach: `width ×
    /// height` bytes, rows top first, its bottom left at `origin` and each
    /// pixel `unit` font units wide.
    Mask {
        width: u32,
        height: u32,
        coverage: Vec<u8>,
        origin: [f32; 2],
        unit: f32,
    },
    /// Nothing: a space.
    Empty,
}

/// A raster of primitives: `width × height` straight-alpha sRGB RGBA8
/// pixels, rows top first.
#[derive(Debug, Clone, PartialEq)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Where a document gets what it draws besides its primitives.
pub trait Source {
    /// Glyph `glyph` of face `face`; `None` where the face has no such
    /// glyph.
    fn glyph(&mut self, face: u32, glyph: u16) -> Option<Glyph>;

    /// `draws` drawn over transparency into a raster of the box `rect` of
    /// content pixels at `scale` raster pixels per content pixel; `None`
    /// where they cannot be, and they are left out.
    fn raster(&mut self, draws: &[Primitive], rect: Rect, scale: f32) -> Option<Raster>;
}

/// The resolution rasters are drawn at by default, in pixels per inch.
pub const RASTER_DPI: f32 = 300.0;

/// The largest side a raster has, in pixels.
const RASTER_MAX: f32 = 8192.0;

/// A PDF document being written.
pub struct Document {
    w: Writer,
    pages_at: Ref,
    pages: Vec<Ref>,
    info: Info,
    fonts: Fonts,
    /// The graphics states written, by what they set.
    states: HashMap<paint::StateKey, Ref>,
    /// The rasters written, by the primitive they show.
    rasters: Vec<(Primitive, Rect, Ref)>,
    raster_dpi: f32,
}

impl Document {
    pub fn new(info: Info) -> Document {
        let mut w = Writer::new();
        let pages_at = w.alloc();
        Document {
            w,
            pages_at,
            pages: Vec::new(),
            info,
            fonts: Fonts::default(),
            states: HashMap::new(),
            rasters: Vec::new(),
            raster_dpi: RASTER_DPI,
        }
    }

    /// Draws rasters at `dpi` pixels per inch from now on.
    pub fn set_raster_dpi(&mut self, dpi: f32) {
        self.raster_dpi = dpi.clamp(36.0, 1200.0);
    }

    /// The pages added so far.
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Adds a page of paper `size` (in points) showing `parts`, in order,
    /// what they draw besides primitives from `source`.
    pub fn page(&mut self, size: [f32; 2], parts: &[Part<'_>], source: &mut dyn Source) {
        let page = self.w.alloc();
        let contents = self.w.alloc();
        let resources = self.w.alloc();
        let mut used = paint::Used::default();
        let mut body = Vec::new();
        for part in parts {
            let raster_scale = (self.raster_dpi / 72.0 * part.scale).max(0.1);
            let mut painter =
                paint::Painter::new(self, source, part, size, resources, raster_scale, &mut used);
            body.extend_from_slice(&painter.page(part.draws));
        }
        self.w.stream(contents, |_| {}, &body);
        self.w.obj(resources, |out| used.write(out));
        let [w, h] = size;
        let pages_at = self.pages_at;
        self.w.obj(page, |out| {
            out.extend_from_slice(b"<</Type/Page/Parent ");
            pages_at.put(out);
            out.extend_from_slice(b"/MediaBox[0 0 ");
            object::num(out, w);
            out.push(b' ');
            object::num(out, h);
            out.extend_from_slice(b"]/Resources ");
            resources.put(out);
            out.extend_from_slice(b"/Contents ");
            contents.put(out);
            out.extend_from_slice(b"/Group<</S/Transparency/CS/DeviceRGB/I true>>>>");
        });
        self.pages.push(page);
    }

    /// The finished file.
    pub fn finish(mut self) -> Vec<u8> {
        let fonts = std::mem::take(&mut self.fonts);
        fonts.write(&mut self.w);
        let pages = std::mem::take(&mut self.pages);
        self.w.obj(self.pages_at, |out| {
            out.extend_from_slice(b"<</Type/Pages/Kids[");
            for page in &pages {
                page.put(out);
                out.push(b' ');
            }
            let _ = write!(out, "]/Count {}>>", pages.len());
        });
        let catalog = self.w.alloc();
        let pages_at = self.pages_at;
        let lang = self.info.lang.clone();
        self.w.obj(catalog, |out| {
            out.extend_from_slice(b"<</Type/Catalog/Pages ");
            pages_at.put(out);
            if let Some(lang) = &lang {
                out.extend_from_slice(b"/Lang");
                text(out, lang);
            }
            out.extend_from_slice(b"/ViewerPreferences<</DisplayDocTitle true>>>>");
        });
        let info = self.w.alloc();
        let fields = [
            ("Title", self.info.title.as_deref()),
            ("Author", self.info.author.as_deref()),
            ("Subject", self.info.subject.as_deref()),
            ("Keywords", self.info.keywords.as_deref()),
            ("Creator", self.info.creator.as_deref()),
            ("Producer", Some("Viso")),
        ];
        self.w.obj(info, |out| {
            out.extend_from_slice(b"<<");
            for (key, value) in fields {
                if let Some(value) = value {
                    let _ = write!(out, "/{key}");
                    text(out, value);
                }
            }
            out.extend_from_slice(b">>");
        });
        self.w.finish(catalog, info)
    }
}

#[cfg(test)]
mod tests;
