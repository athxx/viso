//! Codepoint coverage: does a face render a given codepoint / cluster / run,
//! answered without shaping and without re-parsing the face.
//!
//! Coverage is the predicate [`crate::fallback`] walks the fallback chain with.
//! It reads the face's character map; it does not shape or rasterize.
//!
//! # The accelerator, not a probe
//!
//! [`face_covers`] is the cold one-shot probe: it parses the face, asks, and
//! throws the parse away. That is the right shape for a single question and the
//! wrong shape for a fallback walk, where the same faces are asked about every
//! cluster of every run — N faces would mean N face parses per cluster.
//!
//! [`Coverage`] is the accelerator. The first question about a face derives a
//! compact coverage set from its `cmap` once; every later question is a bounded
//! lookup against that set, so a fallback walk over N faces is N set tests and
//! zero face parses.
//!
//! # Storage: pages, ranges, bitsets — never a resident 1.1M-scalar table
//!
//! A set is a sparse directory over 512-scalar pages. A page the `cmap` never
//! touches is absent and costs nothing, which is most of Unicode for most faces.
//! A page that is present is stored as whichever is smaller: a page-local
//! inclusive range list (4 bytes a range — the natural shape of a `cmap`, whose
//! coverage comes in runs), or a 64-byte bitset once the ranges outnumber what
//! the bitset costs. A page that is wholly covered is a tag with no payload.
//! A Latin face lands in a few hundred bytes; a full CJK face in tens of
//! kilobytes, against the ~137 KB a flat plane-0 bitset would cost and the
//! 1.1M-entry table the runtime specification forbids as a resident layout.
//!
//! # A candidate filter, never the final answer
//!
//! Codepoint coverage answers "does this face have a glyph for these scalars?".
//! For a complex cluster — an emoji ZWJ sequence, a skin-tone or
//! variation-selector sequence, a regional-indicator flag, an Indic
//! conjunct — that is a *candidate filter* only: a face can map every scalar
//! individually and still shape the cluster into `.notdef`s or a wrong visual.
//! The authoritative answer is the shaper's, read back from
//! [`crate::shaping::ShapedRun::has_coverage_miss`]; coverage only decides which
//! faces are worth shaping with. Callers must not treat a `true` here as a
//! decision to render.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use unicode_segmentation::UnicodeSegmentation;

use crate::FontFaceId;
use crate::inspect::{self, BudgetState, CacheKey, MissLedger};

/// Whether an sfnt face covers every scalar in `text`.
///
/// The cold one-shot probe: it parses `sfnt`, checks the `cmap`, and drops the
/// parse. Use it when a face is asked about once. For repeated questions —
/// fallback planning, per-cluster face selection — use [`Coverage`], which
/// derives the answer from a set built once per face.
pub fn face_covers(sfnt: &[u8], index: u32, text: &str) -> bool {
    ttf_parser::Face::parse(sfnt, index)
        .ok()
        .is_some_and(|face| text.chars().all(|ch| face.glyph_index(ch).is_some()))
}

/// Scalars per page. 512 keeps a page's bitset at 64 bytes and a page-local
/// offset inside a `u16`, and matches the granularity `cmap` coverage actually
/// arrives in (script blocks are page-aligned far more often than not).
const PAGE_BITS: u32 = 9;
const PAGE_SIZE: u32 = 1 << PAGE_BITS;
const PAGE_MASK: u32 = PAGE_SIZE - 1;
/// 64-bit words in one page's bitset.
const PAGE_WORDS: usize = (PAGE_SIZE / 64) as usize;
/// Above this many page-local ranges a bitset is the smaller encoding: a range
/// is two `u16`, a bitset is `PAGE_WORDS` words.
const MAX_PAGE_RANGES: usize = PAGE_WORDS * 8 / 4;

/// How one present page stores its covered scalars.
#[derive(Debug, Clone, Copy)]
enum Page {
    /// Every scalar in the page is covered; no payload.
    Full,
    /// `count` page-local inclusive ranges, sorted, starting at `first` in the
    /// set's shared range storage.
    Ranges { first: u32, count: u16 },
    /// [`PAGE_WORDS`] words starting at `first` in the set's shared bit storage.
    Bits { first: u32 },
}

/// One face's cmap-derived coverage, in the compact page form described in the
/// module documentation.
///
/// Page ids live in their own array so the binary search that locates a page
/// touches only the ids, not the payload descriptors beside them.
#[derive(Debug, Default)]
struct CoverageSet {
    /// Present page indices, ascending. `codepoint >> PAGE_BITS` fits a `u16`
    /// for every Unicode scalar (0x110000 >> 9 = 2176 pages).
    page_ids: Vec<u16>,
    /// Payload descriptor for `page_ids[i]`.
    pages: Vec<Page>,
    /// Every [`Page::Ranges`] page's ranges, concatenated.
    ranges: Vec<(u16, u16)>,
    /// Every [`Page::Bits`] page's words, concatenated.
    bits: Vec<u64>,
}

impl CoverageSet {
    /// Derive coverage from an sfnt, or an empty set if the bytes do not parse.
    ///
    /// The `cmap` subtables enumerate *candidate* codepoints — a format-4 range
    /// may contain entries that map to glyph 0 — so each candidate is confirmed
    /// through [`ttf_parser::Face::glyph_index`], the same predicate
    /// [`face_covers`] uses. The set is therefore exactly as accurate as a
    /// per-codepoint probe, at one parse instead of one per question.
    fn from_sfnt(sfnt: &[u8], index: u32) -> Self {
        let Ok(face) = ttf_parser::Face::parse(sfnt, index) else {
            return Self::default();
        };

        // Enumerate the cmap rather than scanning Unicode: the walk is bounded by
        // what the face actually maps, never by the 1.1M scalar space.
        let mut covered: Vec<u32> = Vec::new();
        if let Some(cmap) = face.tables().cmap {
            for subtable in cmap.subtables {
                if subtable.is_unicode() {
                    subtable.codepoints(|cp| covered.push(cp));
                }
            }
        }
        covered.sort_unstable();
        covered.dedup();
        covered.retain(|&cp| char::from_u32(cp).is_some_and(|ch| face.glyph_index(ch).is_some()));
        Self::from_sorted_scalars(&covered)
    }

    /// Pack an ascending, deduplicated scalar list into the page form, choosing
    /// per page whichever of `Full` / ranges / bitset is smallest.
    fn from_sorted_scalars(covered: &[u32]) -> Self {
        let mut set = Self::default();
        let mut runs: Vec<(u16, u16)> = Vec::new();
        let mut i = 0;
        while i < covered.len() {
            let page = covered[i] >> PAGE_BITS;
            let start = i;
            while i < covered.len() && covered[i] >> PAGE_BITS == page {
                i += 1;
            }
            let page_scalars = &covered[start..i];

            runs.clear();
            for &cp in page_scalars {
                let local = (cp & PAGE_MASK) as u16;
                match runs.last_mut() {
                    Some(last) if last.1 + 1 == local => last.1 = local,
                    _ => runs.push((local, local)),
                }
            }

            let payload = if runs.len() == 1 && runs[0] == (0, PAGE_MASK as u16) {
                Page::Full
            } else if runs.len() <= MAX_PAGE_RANGES {
                let first = set.ranges.len() as u32;
                set.ranges.extend_from_slice(&runs);
                Page::Ranges {
                    first,
                    count: runs.len() as u16,
                }
            } else {
                let first = set.bits.len() as u32;
                set.bits.extend(std::iter::repeat_n(0u64, PAGE_WORDS));
                for &cp in page_scalars {
                    let local = cp & PAGE_MASK;
                    set.bits[first as usize + (local >> 6) as usize] |= 1u64 << (local & 63);
                }
                Page::Bits { first }
            };
            set.page_ids.push(page as u16);
            set.pages.push(payload);
        }

        set.page_ids.shrink_to_fit();
        set.pages.shrink_to_fit();
        set.ranges.shrink_to_fit();
        set.bits.shrink_to_fit();
        set
    }

    /// Whether the face has a glyph for `ch`.
    fn covers(&self, ch: char) -> bool {
        let cp = ch as u32;
        let page = (cp >> PAGE_BITS) as u16;
        let Ok(at) = self.page_ids.binary_search(&page) else {
            return false;
        };
        let local = (cp & PAGE_MASK) as u16;
        match self.pages[at] {
            Page::Full => true,
            Page::Ranges { first, count } => {
                let from = first as usize;
                self.ranges[from..from + count as usize]
                    .iter()
                    .any(|&(lo, hi)| local >= lo && local <= hi)
            }
            Page::Bits { first } => {
                let word = self.bits[first as usize + (local >> 6) as usize];
                word >> (local & 63) & 1 == 1
            }
        }
    }

    /// Resident bytes of this set, for the Face Cache's budget accounting.
    fn bytes(&self) -> usize {
        size_of::<Self>()
            + self.page_ids.capacity() * size_of::<u16>()
            + self.pages.capacity() * size_of::<Page>()
            + self.ranges.capacity() * size_of::<(u16, u16)>()
            + self.bits.capacity() * size_of::<u64>()
    }
}

/// The coverage accelerator: lazily built, per-face coverage sets.
///
/// One instance belongs to whatever owns the faces (the fallback planner, the
/// shaping front end). A set is built on the first question about a face and
/// dropped with the face through [`Coverage::forget`] / [`Coverage::clear`], so
/// no coverage outlives its owner. [`Coverage::bytes`] is the cost to charge
/// against the Face Cache's budget.
#[derive(Debug, Default)]
pub struct Coverage {
    sets: HashMap<FontFaceId, CoverageSet>,
    /// Resident bytes across all sets, kept incrementally so the Face Cache can
    /// read the cost without walking the map.
    bytes: usize,
    /// Counter: face parses performed to build sets — one per face, ever. A
    /// steady-state fallback walk must not move this.
    face_parses: u64,
    /// Sets built and dropped, with why. Empty unless [`inspect::ENABLED`].
    ledger: MissLedger,
}

impl Coverage {
    /// An empty accelerator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the face covers `ch`, building its set on first use.
    pub fn face_covers_scalar(
        &mut self,
        face: FontFaceId,
        sfnt: &[u8],
        index: u32,
        ch: char,
    ) -> bool {
        self.set_for(face, sfnt, index).covers(ch)
    }

    /// Whether the face covers every scalar in `text`, building its set on first
    /// use.
    ///
    /// A candidate filter, not a rendering decision — see the module
    /// documentation on complex clusters.
    pub fn face_covers(&mut self, face: FontFaceId, sfnt: &[u8], index: u32, text: &str) -> bool {
        let set = self.set_for(face, sfnt, index);
        text.chars().all(|ch| set.covers(ch))
    }

    /// Bytes of `text`, from its start, the face covers, measured over extended
    /// grapheme clusters so the boundary never falls inside one.
    ///
    /// A cluster counts as covered only when the face has a glyph for *every*
    /// scalar in it; the first cluster with any missing scalar stops the
    /// mapping. This keeps emoji ZWJ / skin-tone / variation-selector / flag
    /// sequences atomic: a face covering the base emoji but not the joiner
    /// contributes zero for that cluster, so the whole cluster is re-planned onto
    /// one face rather than split across several. Zero means no leading cluster
    /// is covered.
    pub fn mapped_len(&mut self, face: FontFaceId, sfnt: &[u8], index: u32, text: &str) -> usize {
        let set = self.set_for(face, sfnt, index);
        let mut mapped = 0;
        for (offset, cluster) in text.grapheme_indices(true) {
            if cluster.chars().all(|ch| set.covers(ch)) {
                mapped = offset + cluster.len();
            } else {
                break;
            }
        }
        mapped
    }

    /// Whether an *already built* set covers the codepoint.
    ///
    /// A face with no set — never queried, or already forgotten — is a clean
    /// `false`, never a panic: this is the read-only form for callers that hold
    /// no face bytes, and the absence of a set is not evidence of coverage.
    pub fn covers(&self, face: FontFaceId, codepoint: char) -> bool {
        self.sets
            .get(&face)
            .is_some_and(|set| set.covers(codepoint))
    }

    /// Drop the face's coverage, releasing its bytes.
    pub fn forget(&mut self, face: FontFaceId) {
        if let Some(set) = self.sets.remove(&face) {
            self.bytes -= set.bytes();
            if inspect::ENABLED {
                self.ledger
                    .evicted(CacheKey::Coverage(face), Some(CacheKey::Face(face)));
            }
        }
    }

    /// Drop every set — the font source changed, so every derived answer is
    /// stale.
    pub fn clear(&mut self) {
        if inspect::ENABLED {
            for &face in self.sets.keys() {
                self.ledger.evicted(CacheKey::Coverage(face), None);
            }
        }
        self.sets.clear();
        self.bytes = 0;
    }

    /// Sets built and dropped, with why.
    pub fn ledger(&self) -> &MissLedger {
        &self.ledger
    }

    /// Resident bytes of all coverage sets, to charge against the Face Cache.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Resident bytes of one face's set; zero when it has none.
    pub fn face_bytes(&self, face: FontFaceId) -> usize {
        self.sets.get(&face).map_or(0, CoverageSet::bytes)
    }

    /// Faces with a built set.
    pub fn face_count(&self) -> usize {
        self.sets.len()
    }

    /// Face parses performed to build sets: one per face, ever. Zero growth
    /// across a warm fallback walk is the property this counter exists to pin.
    pub fn face_parses(&self) -> u64 {
        self.face_parses
    }

    /// The face's set, built from `sfnt` on first use (the specification's lazy
    /// build). Bytes that do not parse yield an empty set, which is cached like
    /// any other so a broken face is parsed once rather than on every question.
    fn set_for(&mut self, face: FontFaceId, sfnt: &[u8], index: u32) -> &CoverageSet {
        let budget = BudgetState {
            resident_bytes: self.bytes as u64,
            budget_bytes: None,
            entries: self.sets.len() as u64,
        };
        match self.sets.entry(face) {
            Entry::Occupied(slot) => slot.into_mut(),
            Entry::Vacant(slot) => {
                if inspect::ENABLED {
                    self.ledger.missed(CacheKey::Coverage(face), budget);
                }
                let set = CoverageSet::from_sfnt(sfnt, index);
                self.face_parses += 1;
                self.bytes += set.bytes();
                slot.insert(set)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A subset of DejaVu Sans: covers Latin, misses CJK / emoji / Cyrillic.
    const DEJAVU: &[u8] = include_bytes!("../tests/fixtures/DejaVuSans-subset.ttf");

    fn face() -> ttf_parser::Face<'static> {
        ttf_parser::Face::parse(DEJAVU, 0).expect("fixture parses")
    }

    fn id(n: u32) -> FontFaceId {
        FontFaceId(n)
    }

    /// The accelerator's answer must equal the per-codepoint probe's answer for
    /// every scalar the face's cmap names — the set is a faster spelling of
    /// `glyph_index`, not an approximation of it.
    #[test]
    fn coverage_agrees_with_glyph_index_over_the_whole_cmap() {
        let face = face();
        let set = CoverageSet::from_sfnt(DEJAVU, 0);

        let mut candidates: Vec<u32> = Vec::new();
        for subtable in face.tables().cmap.expect("fixture has a cmap").subtables {
            if subtable.is_unicode() {
                subtable.codepoints(|cp| candidates.push(cp));
            }
        }
        candidates.sort_unstable();
        candidates.dedup();
        assert!(candidates.len() > 50, "the fixture maps a real alphabet");

        let mut covered = 0;
        for cp in candidates {
            let Some(ch) = char::from_u32(cp) else {
                continue;
            };
            let expected = face.glyph_index(ch).is_some();
            assert_eq!(set.covers(ch), expected, "U+{cp:04X}");
            covered += usize::from(expected);
        }
        assert!(covered > 50, "the fixture genuinely covers Latin");
    }

    /// Scalars outside the cmap are absent pages: a miss, not a false positive,
    /// and not a panic at the top of the scalar space either.
    #[test]
    fn scalars_outside_the_cmap_are_a_miss() {
        let set = CoverageSet::from_sfnt(DEJAVU, 0);
        for ch in ['\u{4F60}', '\u{1F469}', '\u{10FFFF}', '\u{E000}'] {
            assert!(!set.covers(ch), "{ch:?} is outside the fixture's cmap");
        }
    }

    /// The set must stay far below a resident scalar table. The bound is
    /// deliberately loose — the point is the order of magnitude, not a byte
    /// count that would break on a fixture change.
    #[test]
    fn a_latin_face_costs_kilobytes_not_a_scalar_table() {
        let set = CoverageSet::from_sfnt(DEJAVU, 0);
        assert!(
            set.bytes() < 4096,
            "a Latin face's coverage is small: {} bytes",
            set.bytes()
        );
        // A flat plane-0 bitset alone would be 8 KiB per page * 2176 pages of
        // addressable space; the sparse directory is what keeps this honest.
        assert!(!set.page_ids.is_empty(), "the fixture has present pages");
    }

    /// Dense pages must take the bitset encoding and sparse pages the range
    /// encoding, and both must answer identically to the scalars they were built
    /// from. Built from a synthetic codepoint set rather than a face so both arms
    /// are reachable regardless of the fixture's shape.
    #[test]
    fn dense_pages_use_bitsets_and_sparse_pages_use_ranges() {
        // Page 0: two runs -> ranges. Page 1: every other scalar -> far more
        // runs than a bitset costs. Page 3: wholly covered -> Full.
        let mut covered: Vec<u32> = Vec::new();
        covered.extend(0x00..0x10);
        covered.extend(0x20..0x30);
        covered.extend((PAGE_SIZE..PAGE_SIZE * 2).step_by(2));
        covered.extend(PAGE_SIZE * 3..PAGE_SIZE * 4);

        let set = CoverageSet::from_sorted_scalars(&covered);
        assert_eq!(set.page_ids, [0, 1, 3]);
        assert!(matches!(set.pages[0], Page::Ranges { count: 2, .. }));
        assert!(matches!(set.pages[1], Page::Bits { .. }));
        assert!(matches!(set.pages[2], Page::Full));

        for cp in 0..PAGE_SIZE * 5 {
            let ch = char::from_u32(cp).expect("low scalars are all characters");
            assert_eq!(set.covers(ch), covered.contains(&cp), "U+{cp:04X}");
        }
    }

    /// The lazy-build contract: the first question about a face parses it, every
    /// later question — thousands of them — parses nothing.
    #[test]
    fn a_face_is_parsed_once_however_often_it_is_asked() {
        let mut coverage = Coverage::new();
        assert_eq!(coverage.face_parses(), 0, "nothing is built eagerly");

        assert!(coverage.face_covers(id(1), DEJAVU, 0, "A"));
        assert_eq!(coverage.face_parses(), 1);

        for ch in "AVWxnaeiou".chars().cycle().take(2000) {
            coverage.face_covers_scalar(id(1), DEJAVU, 0, ch);
        }
        assert_eq!(coverage.face_parses(), 1, "no reparse on the warm path");
        assert_eq!(coverage.face_count(), 1);
    }

    /// Distinct faces get distinct sets, each parsed once.
    #[test]
    fn each_face_is_built_separately() {
        let mut coverage = Coverage::new();
        coverage.face_covers(id(1), DEJAVU, 0, "A");
        coverage.face_covers(id(2), DEJAVU, 0, "A");
        coverage.face_covers(id(1), DEJAVU, 0, "V");
        assert_eq!(coverage.face_parses(), 2);
        assert_eq!(coverage.face_count(), 2);
    }

    /// A face with no set answers `false` rather than panicking or guessing —
    /// the read-only form for callers holding no bytes.
    #[test]
    fn an_unregistered_face_is_a_clean_false() {
        let mut coverage = Coverage::new();
        assert!(!coverage.covers(id(7), 'A'), "never queried");

        coverage.face_covers(id(7), DEJAVU, 0, "A");
        assert!(coverage.covers(id(7), 'A'), "built, and Latin is covered");

        coverage.forget(id(7));
        assert!(!coverage.covers(id(7), 'A'), "forgotten with its face");
    }

    /// Bytes are charged when a set is built and released when the face is
    /// dropped: the Face Cache can hold coverage to a budget only if the cost
    /// moves in both directions.
    #[test]
    fn bytes_are_charged_on_build_and_released_on_drop() {
        let mut coverage = Coverage::new();
        assert_eq!(coverage.bytes(), 0);

        coverage.face_covers(id(1), DEJAVU, 0, "A");
        let one = coverage.bytes();
        assert!(one > 0, "a built set has a cost");

        coverage.face_covers(id(2), DEJAVU, 0, "A");
        assert_eq!(coverage.bytes(), one * 2, "each face is charged");

        coverage.forget(id(2));
        assert_eq!(coverage.bytes(), one, "dropping the face released its cost");
        coverage.clear();
        assert_eq!(coverage.bytes(), 0);
        assert_eq!(coverage.face_count(), 0);
    }

    /// A face whose bytes do not parse covers nothing, and is parsed once rather
    /// than on every question.
    #[test]
    fn unparseable_bytes_cover_nothing_and_are_parsed_once() {
        let mut coverage = Coverage::new();
        assert!(!coverage.face_covers(id(1), b"not a font", 0, "A"));
        assert!(!coverage.face_covers(id(1), b"not a font", 0, "A"));
        assert_eq!(coverage.face_parses(), 1);
        assert_eq!(coverage.mapped_len(id(1), b"not a font", 0, "A"), 0);
    }

    /// Cluster-atomic mapping: the boundary lands on a grapheme edge, so an
    /// emoji ZWJ sequence the face cannot cover falls through whole instead of
    /// being split into base / joiner / trailing scalar.
    #[test]
    fn mapped_len_stops_on_a_cluster_boundary() {
        let mut coverage = Coverage::new();
        let run = "A\u{1F469}\u{200D}\u{1F4BB}";
        assert_eq!(coverage.mapped_len(id(1), DEJAVU, 0, run), "A".len());

        // A covered base plus a combining mark the fixture lacks is one cluster,
        // and it is rejected whole rather than half-mapped.
        assert_eq!(
            coverage.mapped_len(id(1), DEJAVU, 0, "e\u{0061}\u{0301}"),
            "e".len()
        );

        // Fully covered text maps entirely: atomicity does not shrink coverage.
        assert_eq!(coverage.mapped_len(id(1), DEJAVU, 0, "Aenx"), 4);
    }

    /// Coverage is a candidate filter. Every scalar of this ZWJ sequence is in
    /// the fixture's cmap, so the filter passes it — and that says nothing about
    /// whether the face shapes the sequence as one glyph. The authoritative
    /// answer belongs to the shaper; a filter pass is permission to try, not a
    /// decision to render.
    #[test]
    fn a_filter_pass_is_not_a_shaping_decision() {
        let mut coverage = Coverage::new();
        // "a" ZWJ "b" with scalars the fixture covers, if it covers the joiner.
        let joined = "a\u{200D}b";
        let all_mapped = face().glyph_index('\u{200D}').is_some();
        assert_eq!(
            coverage.face_covers(id(1), DEJAVU, 0, joined),
            all_mapped,
            "the filter reports scalar coverage and nothing more"
        );
        // Either way the filter is per-scalar: it cannot and does not claim the
        // sequence renders as a ligated cluster.
        assert!(coverage.covers(id(1), 'a') && coverage.covers(id(1), 'b'));
    }

    /// The cold probe and the accelerator must never disagree.
    #[test]
    fn the_cold_probe_and_the_accelerator_agree() {
        let mut coverage = Coverage::new();
        for text in ["A", "AVn", "\u{4F60}", "A\u{4F60}", "", "e\u{0301}"] {
            assert_eq!(
                coverage.face_covers(id(1), DEJAVU, 0, text),
                face_covers(DEJAVU, 0, text),
                "{text:?}"
            );
        }
    }
}
