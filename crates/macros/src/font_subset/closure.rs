//! The glyph closure of a subset: every glyph shaping or drawing a kept code
//! point can reach.

use ttf_parser::GlyphId;
use ttf_parser::gsub::SubstitutionSubtable;
use ttf_parser::opentype_layout::{Coverage, LayoutTable};

use super::sfnt::read_u16;

/// A set of glyph ids below the face's glyph count.
pub(super) struct GlyphSet {
    held: Vec<bool>,
    len: usize,
}

impl GlyphSet {
    pub fn new(glyphs: u16) -> Self {
        Self {
            held: vec![false; usize::from(glyphs)],
            len: 0,
        }
    }

    /// Add `glyph`; ids past the glyph count are ignored.
    pub fn insert(&mut self, glyph: u16) -> bool {
        match self.held.get_mut(usize::from(glyph)) {
            Some(held) if !*held => {
                *held = true;
                self.len += 1;
                true
            }
            _ => false,
        }
    }

    pub fn contains(&self, glyph: u16) -> bool {
        self.held.get(usize::from(glyph)).copied().unwrap_or(false)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn iter(&self) -> impl Iterator<Item = u16> + '_ {
        self.held
            .iter()
            .enumerate()
            .filter(|&(_, &held)| held)
            .map(|(glyph, _)| glyph as u16)
    }
}

/// The glyphs `coverage` lists, each with its coverage index.
fn covered(coverage: Coverage<'_>) -> Vec<(u16, u16)> {
    match coverage {
        Coverage::Format1 { glyphs } => glyphs
            .into_iter()
            .enumerate()
            .map(|(index, glyph)| (glyph.0, index as u16))
            .collect(),
        Coverage::Format2 { records } => records
            .into_iter()
            .flat_map(|record| {
                (record.start.0..=record.end.0)
                    .map(move |glyph| (glyph, record.value.wrapping_add(glyph - record.start.0)))
            })
            .collect(),
    }
}

/// Close `glyphs` under every substitution of `gsub` whose input it holds.
///
/// Every lookup takes part, whether a feature lists it directly or a
/// contextual lookup calls it, and contexts are not checked: the closure may
/// keep a glyph no shaping of the kept text reaches, never drop one it does.
pub(super) fn close_over_gsub(gsub: &LayoutTable<'_>, glyphs: &mut GlyphSet) {
    loop {
        let before = glyphs.len();
        for lookup in gsub.lookups {
            for subtable in lookup.subtables.into_iter::<SubstitutionSubtable>() {
                substitute(&subtable, glyphs);
            }
        }
        if glyphs.len() == before {
            return;
        }
    }
}

fn substitute(subtable: &SubstitutionSubtable<'_>, glyphs: &mut GlyphSet) {
    use ttf_parser::gsub::SingleSubstitution;
    match subtable {
        SubstitutionSubtable::Single(single) => {
            for (glyph, index) in covered(single.coverage()) {
                if !glyphs.contains(glyph) {
                    continue;
                }
                let output = match single {
                    SingleSubstitution::Format1 { delta, .. } => {
                        Some(GlyphId(glyph.wrapping_add(*delta as u16)))
                    }
                    SingleSubstitution::Format2 { substitutes, .. } => substitutes.get(index),
                };
                if let Some(output) = output {
                    glyphs.insert(output.0);
                }
            }
        }
        SubstitutionSubtable::Multiple(multiple) => {
            for (glyph, index) in covered(multiple.coverage) {
                if glyphs.contains(glyph)
                    && let Some(sequence) = multiple.sequences.get(index)
                {
                    for output in sequence.substitutes {
                        glyphs.insert(output.0);
                    }
                }
            }
        }
        SubstitutionSubtable::Alternate(alternate) => {
            for (glyph, index) in covered(alternate.coverage) {
                if glyphs.contains(glyph)
                    && let Some(set) = alternate.alternate_sets.get(index)
                {
                    for output in set.alternates {
                        glyphs.insert(output.0);
                    }
                }
            }
        }
        SubstitutionSubtable::Ligature(ligature) => {
            for (glyph, index) in covered(ligature.coverage) {
                if !glyphs.contains(glyph) {
                    continue;
                }
                let Some(set) = ligature.ligature_sets.get(index) else {
                    continue;
                };
                for candidate in set {
                    if candidate
                        .components
                        .into_iter()
                        .all(|component| glyphs.contains(component.0))
                    {
                        glyphs.insert(candidate.glyph.0);
                    }
                }
            }
        }
        SubstitutionSubtable::ReverseChainSingle(reverse) => {
            for (glyph, index) in covered(reverse.coverage) {
                if glyphs.contains(glyph)
                    && let Some(output) = reverse.substitutes.get(index)
                {
                    glyphs.insert(output.0);
                }
            }
        }
        // Contextual lookups substitute only through the lookups they call,
        // which the closure visits on their own.
        SubstitutionSubtable::Context(_) | SubstitutionSubtable::ChainContext(_) => {}
    }
}

const ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
const WE_HAVE_A_SCALE: u16 = 0x0008;
const MORE_COMPONENTS: u16 = 0x0020;
const WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
const WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;

/// The glyphs composite glyph `outline` (a `glyf` entry) references; empty
/// for a simple or empty glyph.
pub(super) fn components(outline: &[u8]) -> Vec<u16> {
    let mut found = Vec::new();
    let Some(contours) = read_u16(outline, 0) else {
        return found;
    };
    if (contours as i16) >= 0 {
        return found;
    }
    let mut at = 10;
    while let (Some(flags), Some(glyph)) = (read_u16(outline, at), read_u16(outline, at + 2)) {
        found.push(glyph);
        at += 4;
        at += if flags & ARG_1_AND_2_ARE_WORDS != 0 {
            4
        } else {
            2
        };
        if flags & WE_HAVE_A_SCALE != 0 {
            at += 2;
        } else if flags & WE_HAVE_AN_X_AND_Y_SCALE != 0 {
            at += 4;
        } else if flags & WE_HAVE_A_TWO_BY_TWO != 0 {
            at += 8;
        }
        if flags & MORE_COMPONENTS == 0 {
            break;
        }
    }
    found
}

/// Close `glyphs` under composite references; `outline` returns a glyph's
/// `glyf` entry.
pub(super) fn close_over_components<'a>(glyphs: &mut GlyphSet, outline: impl Fn(u16) -> &'a [u8]) {
    let mut pending: Vec<u16> = glyphs.iter().collect();
    while let Some(glyph) = pending.pop() {
        for component in components(outline(glyph)) {
            if glyphs.insert(component) {
                pending.push(component);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn components_walk_every_record_and_its_arguments() {
        let mut outline = vec![0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0];
        // Word arguments with a scale, then byte arguments with a 2x2.
        outline.extend_from_slice(
            &(ARG_1_AND_2_ARE_WORDS | WE_HAVE_A_SCALE | MORE_COMPONENTS).to_be_bytes(),
        );
        outline.extend_from_slice(&7u16.to_be_bytes());
        outline.extend_from_slice(&[0; 6]);
        outline.extend_from_slice(&WE_HAVE_A_TWO_BY_TWO.to_be_bytes());
        outline.extend_from_slice(&9u16.to_be_bytes());
        outline.extend_from_slice(&[0; 10]);
        assert_eq!(components(&outline), [7, 9]);
        assert!(components(&[0, 1, 0, 0]).is_empty());
        assert!(components(&[]).is_empty());
    }

    #[test]
    fn a_glyph_set_counts_each_glyph_once() {
        let mut set = GlyphSet::new(4);
        assert!(set.insert(2));
        assert!(!set.insert(2));
        assert!(!set.insert(9));
        assert_eq!(set.len(), 1);
        assert_eq!(set.iter().collect::<Vec<_>>(), [2]);
    }
}
