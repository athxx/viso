//! Text as Type 3 fonts: each face's glyphs, as they are first used, become
//! the procedures of fonts of up to 256 codes, with a map from each code
//! back to the text it shows so the page reads, searches and copies as text.

use std::collections::HashMap;
use std::io::Write;

use crate::content::Content;
use crate::object::{Ref, Writer, num};
use crate::{Glyph, GlyphShape};

/// Codes one font holds.
const CODES: usize = 256;

/// One glyph a font draws.
struct Coded {
    advance: f32,
    bbox: [f32; 4],
    /// The glyph's procedure: its advance, box and outline.
    proc: Vec<u8>,
    /// The text it shows, for the code-to-text map.
    text: String,
}

/// One Type 3 font: up to [`CODES`] glyphs of a face.
struct Font {
    at: Ref,
    units_per_em: f32,
    /// The face's reach above and below the baseline, in font units.
    ascender: f32,
    descender: f32,
    glyphs: Vec<Coded>,
}

/// Every font of the document, by face.
#[derive(Default)]
pub(crate) struct Fonts {
    fonts: Vec<Font>,
    /// Each face's fonts, the last the one taking new glyphs.
    faces: HashMap<u32, Vec<usize>>,
    /// Where each glyph drew: its font and code; `None` for one that has
    /// nothing to draw.
    coded: HashMap<(u32, u16), Option<(usize, u8)>>,
}

impl Fonts {
    /// The font and code drawing glyph `glyph` of face `face`, coding it
    /// with `make`'s glyph on first use; `None` where it has no shape.
    pub(crate) fn code(
        &mut self,
        w: &mut Writer,
        face: u32,
        glyph: u16,
        text: &str,
        make: impl FnOnce() -> Option<Glyph>,
    ) -> Option<(Ref, u8)> {
        if let Some(&coded) = self.coded.get(&(face, glyph)) {
            if let Some((font, code)) = coded {
                let entry = &mut self.fonts[font].glyphs[usize::from(code)];
                if entry.text.is_empty() && !text.is_empty() {
                    entry.text = text.to_owned();
                }
                return Some((self.fonts[font].at, code));
            }
            return None;
        }
        let made = make().and_then(|g| procedure(&g).map(|c| (g, c)));
        let Some((made, (advance, bbox, proc))) = made else {
            self.coded.insert((face, glyph), None);
            return None;
        };
        let fonts = self.faces.entry(face).or_default();
        let font = match fonts.last() {
            Some(&at)
                if self.fonts[at].glyphs.len() < CODES
                    && self.fonts[at].units_per_em == made.units_per_em =>
            {
                at
            }
            _ => {
                self.fonts.push(Font {
                    at: w.alloc(),
                    units_per_em: made.units_per_em,
                    ascender: made.ascender,
                    descender: made.descender,
                    glyphs: Vec::new(),
                });
                fonts.push(self.fonts.len() - 1);
                self.fonts.len() - 1
            }
        };
        let glyphs = &mut self.fonts[font].glyphs;
        let code = glyphs.len() as u8;
        glyphs.push(Coded {
            advance,
            bbox,
            proc,
            text: text.to_owned(),
        });
        self.coded.insert((face, glyph), Some((font, code)));
        Some((self.fonts[font].at, code))
    }

    /// How far glyph `glyph` of face `face` advances the pen, in ems; `None`
    /// before it is coded or where it draws nothing.
    pub(crate) fn advance(&self, face: u32, glyph: u16) -> Option<f32> {
        let (font, code) = (*self.coded.get(&(face, glyph))?)?;
        let font = &self.fonts[font];
        Some(font.glyphs[usize::from(code)].advance / font.units_per_em)
    }

    /// Writes every font.
    pub(crate) fn write(self, w: &mut Writer) {
        for font in self.fonts {
            write_font(w, font);
        }
    }
}

/// A glyph's advance, box and procedure, in font units; `None` for a glyph
/// with nothing to draw and no advance.
fn procedure(glyph: &Glyph) -> Option<(f32, [f32; 4], Vec<u8>)> {
    let mut c = Content::default();
    let mut bbox = [0.0f32; 4];
    match &glyph.shape {
        GlyphShape::Outline(cmds) => {
            bbox = bounds(cmds);
            c.op(
                &[glyph.advance, 0.0, bbox[0], bbox[1], bbox[2], bbox[3]],
                "d1",
            );
            c.path(cmds);
            c.op(&[], "f");
        }
        GlyphShape::Mask {
            width,
            height,
            coverage,
            origin,
            unit,
        } => {
            let (w, h) = (*width as f32 * unit, *height as f32 * unit);
            bbox = [origin[0], origin[1], origin[0] + w, origin[1] + h];
            c.op(
                &[glyph.advance, 0.0, bbox[0], bbox[1], bbox[2], bbox[3]],
                "d1",
            );
            c.save();
            c.transform([w, 0.0, 0.0, h, origin[0], origin[1]]);
            let _ = writeln!(
                c.buf,
                "BI /W {width} /H {height} /IM true /BPC 1 /D [1 0] ID"
            );
            // One bit a pixel, set where the coverage is at least half.
            let row = (*width as usize).div_ceil(8);
            let mut bits = vec![0u8; row * *height as usize];
            for y in 0..*height as usize {
                for x in 0..*width as usize {
                    if coverage[y * *width as usize + x] >= 128 {
                        bits[y * row + x / 8] |= 0x80 >> (x % 8);
                    }
                }
            }
            c.raw(&bits);
            c.raw(b"\nEI\n");
            c.restore();
        }
        GlyphShape::Empty => {
            c.op(&[glyph.advance, 0.0, 0.0, 0.0, 0.0, 0.0], "d1");
        }
    }
    Some((glyph.advance, bbox, c.buf))
}

/// The box of the outline `cmds`, its control points included.
fn bounds(cmds: &[viso_render::PathCmd]) -> [f32; 4] {
    use viso_render::PathCmd;
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    let mut add = |p: viso_render::Point| {
        b[0] = b[0].min(p.x);
        b[1] = b[1].min(p.y);
        b[2] = b[2].max(p.x);
        b[3] = b[3].max(p.y);
    };
    for cmd in cmds {
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
    if b[0] > b[2] { [0.0; 4] } else { b }
}

fn write_font(w: &mut Writer, font: Font) {
    let procs: Vec<Ref> = font
        .glyphs
        .iter()
        .map(|g| {
            let at = w.alloc();
            w.stream(at, |_| {}, &g.proc);
            at
        })
        .collect();
    let unicode = w.alloc();
    w.stream(unicode, |_| {}, &to_unicode(&font.glyphs));
    let mut bbox = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for g in &font.glyphs {
        if g.bbox[0] < g.bbox[2] {
            bbox = [
                bbox[0].min(g.bbox[0]),
                bbox[1].min(g.bbox[1]),
                bbox[2].max(g.bbox[2]),
                bbox[3].max(g.bbox[3]),
            ];
        }
    }
    if bbox[0] > bbox[2] {
        bbox = [0.0; 4];
    }
    let last = font.glyphs.len().saturating_sub(1);
    let descriptor = w.alloc();
    w.obj(font.at, |out| {
        out.extend_from_slice(b"<</Type/Font/Subtype/Type3/FontBBox[");
        for v in bbox {
            num(out, v);
            out.push(b' ');
        }
        let unit = 1.0 / font.units_per_em;
        out.extend_from_slice(b"]/FontMatrix[");
        for v in [unit, 0.0, 0.0, unit, 0.0, 0.0] {
            num_precise(out, v);
            out.push(b' ');
        }
        out.extend_from_slice(b"]/CharProcs<<");
        for (code, proc) in procs.iter().enumerate() {
            let _ = write!(out, "/g{code} ");
            proc.put(out);
        }
        out.extend_from_slice(b">>/Encoding<</Type/Encoding/Differences[0");
        for code in 0..font.glyphs.len() {
            let _ = write!(out, "/g{code}");
        }
        let _ = write!(out, "]>>/FirstChar 0/LastChar {last}/Widths[");
        for g in &font.glyphs {
            num(out, g.advance);
            out.push(b' ');
        }
        out.extend_from_slice(b"]/Resources<<>>/ToUnicode ");
        unicode.put(out);
        out.extend_from_slice(b"/FontDescriptor ");
        descriptor.put(out);
        out.extend_from_slice(b">>");
    });
    w.obj(descriptor, |out| {
        let _ = write!(
            out,
            "<</Type/FontDescriptor/FontName/V{}/Flags 4/ItalicAngle 0/StemV 0/FontBBox[",
            font.at.0
        );
        for v in bbox {
            num(out, v);
            out.push(b' ');
        }
        out.extend_from_slice(b"]/Ascent ");
        num(out, font.ascender);
        out.extend_from_slice(b"/Descent ");
        num(out, font.descender);
        out.extend_from_slice(b"/CapHeight ");
        num(out, font.ascender);
        out.extend_from_slice(b">>");
    });
}

/// A number written with enough places for a reciprocal of units per em.
fn num_precise(out: &mut Vec<u8>, v: f32) {
    let _ = write!(out, "{v:.9}");
    while out.last() == Some(&b'0') {
        out.pop();
    }
    if out.last() == Some(&b'.') {
        out.pop();
    }
}

/// The map from each code of a font to the text its glyph shows.
fn to_unicode(glyphs: &[Coded]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(
        b"/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
1 begincodespacerange\n<00> <FF>\nendcodespacerange\n",
    );
    let mapped: Vec<(usize, &str)> = glyphs
        .iter()
        .enumerate()
        .filter(|(_, g)| !g.text.is_empty())
        .map(|(code, g)| (code, g.text.as_str()))
        .collect();
    for block in mapped.chunks(100) {
        let _ = writeln!(out, "{} beginbfchar", block.len());
        for &(code, s) in block {
            let _ = write!(out, "<{code:02X}> <");
            for unit in s.encode_utf16() {
                let _ = write!(out, "{unit:04X}");
            }
            out.extend_from_slice(b">\n");
        }
        out.extend_from_slice(b"endbfchar\n");
    }
    out.extend_from_slice(b"endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    out
}
