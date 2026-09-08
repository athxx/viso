//! Text shaping: map a string to positioned glyph ids via rustybuzz, resolving
//! coverage across the [`FontStore`]'s fallback chain and mixed-direction text
//! via the Unicode Bidirectional Algorithm.
//!
//! Shaping is coverage-authoritative: a run is shaped with the chain's primary
//! face, and any glyph the shaper leaves as `.notdef` (id 0) is reshaped, over
//! the byte range it covers, against the next face in the chain — recursively,
//! until the chain is exhausted (the leftover `.notdef`s stay, rendering the
//! primary face's missing-glyph box). This mirrors the fallback model the
//! `FontStore` chain was built for: the cmap probe ([`FontFace::has_char`]) only
//! orders candidates; the shaper decides what a face can actually render.
//!
//! Direction: a cheap scan takes the left-to-right fast path when the text has
//! no possibly-RTL character (the common case: ASCII / Latin / CJK / Thai /
//! emoji), skipping BiDi's classification pass entirely. Otherwise the text is
//! segmented into visual runs by [`unicode_bidi`] and each run is shaped in its
//! resolved direction, appended in visual order.
//!
//! Advances and offsets are returned in **em units** so the caller applies pixel
//! size later; each glyph also carries the [`FontId`] it was shaped with, so
//! layout and rasterization address the right face.

use crate::FontId;
use crate::font::{FontFace, FontStore};

/// One shaped glyph: the face it resolved to, its id in that face, the source
/// cluster (byte offset into the input), and pen advance / positioning offset in
/// em units (em of the resolving face).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapedGlyph {
    /// The chain face this glyph was shaped with — layout/raster address it.
    pub font: FontId,
    pub id: u16,
    pub cluster: u32,
    pub advance_em: f32,
    pub offset_x_em: f32,
    pub offset_y_em: f32,
}

/// Shape `text` against `store`'s fallback chain, resolving coverage across the
/// chain and mixed direction via BiDi. Returns glyphs in final visual order,
/// each tagged with the [`FontId`] that rendered it. An empty chain (no font
/// loaded) shapes to no glyphs.
pub fn shape(store: &FontStore, text: &str) -> Vec<ShapedGlyph> {
    let chain = store.chain();
    if chain.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    if is_definitely_ltr(text) {
        // Fast path: no possibly-RTL character, so shape one LTR run and skip
        // the BiDi classification/allocation entirely.
        shape_run(store, chain, text, 0, text.len(), Direction::Ltr, &mut out);
    } else {
        // At least one possibly-RTL character: resolve embedding levels and
        // segment into visual runs, shaping each in its resolved direction.
        let bidi = unicode_bidi::ParagraphBidiInfo::new(text, Some(unicode_bidi::Level::ltr()));
        if bidi.is_pure_ltr {
            shape_run(store, chain, text, 0, text.len(), Direction::Ltr, &mut out);
        } else {
            let (levels, runs) = bidi.visual_runs(0..text.len());
            for run in &runs {
                let dir = if levels[run.start].is_rtl() {
                    Direction::Rtl
                } else {
                    Direction::Ltr
                };
                shape_run(store, chain, text, run.start, run.end, dir, &mut out);
            }
        }
    }
    out
}

/// Shaping direction for one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Ltr,
    Rtl,
}

impl From<Direction> for rustybuzz::Direction {
    fn from(d: Direction) -> Self {
        match d {
            Direction::Ltr => rustybuzz::Direction::LeftToRight,
            Direction::Rtl => rustybuzz::Direction::RightToLeft,
        }
    }
}

/// Shape the byte range `[start, end)` of `text` with `chain[0]`, then recurse
/// over `chain[1..]` for every `.notdef` run, appending glyphs to `out` in the
/// visual order the shaper produced.
fn shape_run(
    store: &FontStore,
    chain: &[FontId],
    text: &str,
    start: usize,
    end: usize,
    dir: Direction,
    out: &mut Vec<ShapedGlyph>,
) {
    let Some((&font, remaining)) = chain.split_first() else {
        return;
    };
    let face = store.face(font);
    let glyphs = shape_step(face, font, text, start, end, dir);

    // Group runs of glyphs that share a cluster; the shaper emits clusters
    // monotonically in the shaping direction, so a group is one indivisible
    // shaping unit and coverage is decided per group.
    let groups: Vec<&[ShapedGlyph]> = group_by_cluster(&glyphs);

    let mut i = 0;
    while i < groups.len() {
        let missing = groups[i].iter().any(|g| g.id == 0);
        if missing && !remaining.is_empty() {
            // Extend across every adjacent still-missing group so the whole span
            // reshapes against the remaining chain in one recursive call.
            let run_start = i;
            while i < groups.len() && groups[i].iter().any(|g| g.id == 0) {
                i += 1;
            }
            let run_end = i;
            // The logical byte range this missing run covers. The "logically
            // next" cluster after a group sits to the right in LTR and to the
            // left in RTL; at the visual edge the range runs to the shaped span
            // boundary (`end` for LTR, the group before the run for RTL).
            let (lo, hi) = match dir {
                Direction::Ltr => {
                    let lo = groups[run_start][0].cluster as usize;
                    let hi = groups
                        .get(run_end)
                        .map_or(end, |next| next[0].cluster as usize);
                    (lo, hi)
                }
                Direction::Rtl => {
                    let lo = groups[run_end - 1][0].cluster as usize;
                    let hi = if run_start == 0 {
                        end
                    } else {
                        // The group logically before this run (visually after,
                        // in RTL) ends where its cluster begins.
                        groups[run_start - 1][0].cluster as usize
                    };
                    (lo, hi)
                }
            };
            shape_run(store, remaining, text, lo, hi, dir, out);
        } else {
            out.extend_from_slice(groups[i]);
            i += 1;
        }
    }
}

/// Shape one `[start, end)` slice with a single `face`, tagging every glyph with
/// `font` and rebasing its cluster to the absolute byte offset in the full text.
fn shape_step(
    face: &FontFace,
    font: FontId,
    text: &str,
    start: usize,
    end: usize,
    dir: Direction,
) -> Vec<ShapedGlyph> {
    let rb = face.rb();
    let upem = face.units_per_em;

    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(&text[start..end]);
    buffer.set_direction(dir.into());

    let output = rustybuzz::shape(&rb, &[], buffer);
    let infos = output.glyph_infos();
    let positions = output.glyph_positions();

    infos
        .iter()
        .zip(positions.iter())
        .map(|(info, pos)| ShapedGlyph {
            font,
            id: info.glyph_id as u16,
            // rustybuzz reports clusters relative to the slice; rebase to the
            // absolute byte offset so coverage ranges index the full `text`.
            cluster: start as u32 + info.cluster,
            advance_em: pos.x_advance as f32 / upem,
            offset_x_em: pos.x_offset as f32 / upem,
            offset_y_em: pos.y_offset as f32 / upem,
        })
        .collect()
}

/// Slice `glyphs` into maximal runs sharing a cluster (one shaping unit).
fn group_by_cluster(glyphs: &[ShapedGlyph]) -> Vec<&[ShapedGlyph]> {
    let mut groups = Vec::new();
    let mut i = 0;
    while i < glyphs.len() {
        let cluster = glyphs[i].cluster;
        let mut j = i + 1;
        while j < glyphs.len() && glyphs[j].cluster == cluster {
            j += 1;
        }
        groups.push(&glyphs[i..j]);
        i = j;
    }
    groups
}

/// Whether `text` is guaranteed to contain no right-to-left character, so the
/// LTR fast path is correct. A conservative scan: any code point in a
/// strong-RTL block (Hebrew, Arabic, Syriac, Thaana, N'Ko, Samaritan, and the
/// Arabic/Hebrew presentation-form ranges) forces the full BiDi path.
fn is_definitely_ltr(text: &str) -> bool {
    !text.chars().any(is_maybe_rtl)
}

/// Whether `c` falls in a block that can resolve to right-to-left. Errs toward
/// `true` only for ranges that actually carry RTL characters, so the fast path
/// stays available for the overwhelmingly common LTR text.
fn is_maybe_rtl(c: char) -> bool {
    matches!(c as u32,
        0x0590..=0x05FF   // Hebrew
        | 0x0600..=0x06FF // Arabic
        | 0x0700..=0x074F // Syriac
        | 0x0750..=0x077F // Arabic Supplement
        | 0x0780..=0x07BF // Thaana
        | 0x07C0..=0x07FF // N'Ko
        | 0x0800..=0x083F // Samaritan
        | 0x08A0..=0x08FF // Arabic Extended-A
        | 0xFB1D..=0xFB4F // Hebrew presentation forms
        | 0xFB50..=0xFDFF // Arabic presentation forms-A
        | 0xFE70..=0xFEFF // Arabic presentation forms-B
    )
}
