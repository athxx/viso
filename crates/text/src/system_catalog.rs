//! The async system-font catalog: the font-picker workload, with a residency
//! lifetime kept strictly separate from the working text cache.
//!
//! A font picker enumerates every installed system face — hundreds on a desktop,
//! and each preview shapes a sample string ("The quick brown fox", or a CJK
//! specimen) in a face the document may never use again. If those one-shot
//! preview faces were admitted into the working [`FontCache`](crate::FontCache),
//! a single scroll through the picker would churn the hot working set: the faces
//! the open document is actually rendering would be pushed toward eviction by
//! faces the user is merely glancing at. Byte-budget SLRU already resists that at
//! the eviction level, but the stronger guarantee this spec asks for (section 18)
//! is that the picker scan has an **independent cache lifetime** — it must not
//! even *reach* the working cache.
//!
//! This catalog is that separate lifetime. Enumeration produces compact
//! descriptors only ([`CatalogFace`]), never parsed bytes — enumerating three
//! thousand faces parses nothing, exactly as the manifest does. A preview admits
//! a face into the catalog's own small, self-bounded preview ring, evicted FIFO
//! within the catalog's own budget. When the picker closes, the whole catalog
//! (descriptors and preview ring) is dropped in one move; nothing lingers in, and
//! nothing was ever taken from, the working cache.
//!
//! # Async shape
//!
//! Enumeration is a cold, potentially slow platform operation (a directory scan,
//! a CoreText / DirectWrite / fontconfig query). The facade performs it off the
//! main thread and delivers the descriptors; this crate owns the catalog state
//! and the preview-residency policy, not the platform enumeration. Preview
//! shaping is scheduled at [`Priority::CatalogPreview`](crate::text_work::Priority::CatalogPreview),
//! the lowest class, so a picker can never starve a visible glyph or an edit.
//!
//! # What is *not* here
//!
//! On a runtime that withholds system fonts (WASM / Canvas, section 16.1) the
//! catalog simply enumerates nothing — there is no implicit browser local-font
//! enumeration and no bundled default. An empty catalog is the correct, total
//! answer, exactly as [`NoSystemFonts`](crate::system_fonts::NoSystemFonts) is
//! for resolution.

use std::collections::VecDeque;

use crate::FontFaceId;
use crate::font_request::{FontRole, FontSlant, FontWeight, FontWidth};

/// A compact descriptor for one enumerated system face.
///
/// This is metadata only — a family name for display, its style attributes, and
/// a color-capability flag. It holds no bytes and no parsed face: enumerating
/// thousands of faces costs only these descriptors, and a face's bytes are read
/// (into the catalog's preview ring, never the working cache) only when the user
/// actually previews it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogFace {
    /// Family name, for display in the picker UI.
    pub family: String,
    /// Weight of this face.
    pub weight: FontWeight,
    /// Width / stretch of this face.
    pub width: FontWidth,
    /// Slant of this face.
    pub slant: FontSlant,
    /// The role this face plausibly serves, for grouping in the picker.
    pub role: FontRole,
    /// Whether this face carries a color glyph table.
    pub color: bool,
}

/// One face resident in the catalog's own preview ring.
#[derive(Debug)]
struct PreviewEntry {
    face: FontFaceId,
    /// Estimated resident cost of the previewed face, in bytes.
    cost_bytes: u64,
}

/// The async system-font catalog: enumerated descriptors plus a self-bounded
/// preview residency whose lifetime is independent of the working text cache.
///
/// The catalog is a cold-path structure — it exists only while a picker is open
/// and is dropped whole when the picker closes. Its preview ring is deliberately
/// tiny: the picker needs only the handful of faces currently on screen rendered
/// as specimens, not the whole enumerated set resident at once.
#[derive(Debug)]
pub struct SystemFontCatalog {
    /// Enumerated face descriptors (metadata only; no bytes).
    faces: Vec<CatalogFace>,
    /// Faces currently rendered as previews, in admission order (FIFO eviction).
    preview: VecDeque<PreviewEntry>,
    /// Total resident cost of the preview ring.
    preview_bytes: u64,
    /// Byte budget for the preview ring — the catalog's *own* budget, entirely
    /// separate from the working [`FontCache`](crate::FontCache) budget.
    preview_budget_bytes: u64,
}

impl Default for SystemFontCatalog {
    fn default() -> Self {
        // A small default preview budget: a picker needs only the specimens
        // currently visible resident, not the whole enumerated set. The facade
        // may tune it per device.
        Self::with_preview_budget(256 << 10)
    }
}

impl SystemFontCatalog {
    /// An empty catalog with the given preview-ring byte budget.
    pub fn with_preview_budget(preview_budget_bytes: u64) -> Self {
        Self {
            faces: Vec::new(),
            preview: VecDeque::new(),
            preview_bytes: 0,
            preview_budget_bytes,
        }
    }

    /// Receive the enumerated face descriptors from the facade's async scan.
    ///
    /// Enumeration is a metadata-only operation: this stores descriptors and
    /// parses no bytes, so a three-thousand-face enumeration is cheap. Called
    /// once when the picker opens (or again to refresh); it replaces the prior
    /// enumeration and clears any stale previews.
    pub fn set_enumeration(&mut self, faces: Vec<CatalogFace>) {
        self.faces = faces;
        self.preview.clear();
        self.preview_bytes = 0;
    }

    /// The enumerated face descriptors, for the picker to display.
    pub fn faces(&self) -> &[CatalogFace] {
        &self.faces
    }

    /// The number of enumerated faces.
    pub fn len(&self) -> usize {
        self.faces.len()
    }

    /// Whether the catalog enumerated no faces (an empty catalog is the correct
    /// answer on a system-font-less runtime, section 16.1).
    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    /// Admit one previewed face into the catalog's own preview ring, evicting the
    /// oldest previews (FIFO) while the ring exceeds its own budget.
    ///
    /// This is the whole point of the catalog: a preview lands *here*, in the
    /// catalog's independent-lifetime residency, and never in the working
    /// [`FontCache`](crate::FontCache). A picker scanning every enumerated face
    /// therefore cannot displace the working set — the working cache is never
    /// even consulted. Re-previewing an already-resident face is a no-op (it
    /// stays where it is; the picker is merely re-rendering a visible specimen).
    pub fn preview(&mut self, face: FontFaceId, cost_bytes: u64) {
        if self.preview.iter().any(|e| e.face == face) {
            return;
        }
        self.preview.push_back(PreviewEntry { face, cost_bytes });
        self.preview_bytes += cost_bytes;
        self.enforce_preview_budget();
    }

    /// Evict the oldest previews until the ring is within its own budget. A
    /// single preview larger than the whole budget is allowed to stay resident
    /// (evicting it would leave the ring unable to hold anything), so the ring is
    /// never emptied below one entry by budget pressure alone.
    fn enforce_preview_budget(&mut self) {
        while self.preview_bytes > self.preview_budget_bytes && self.preview.len() > 1 {
            if let Some(evicted) = self.preview.pop_front() {
                self.preview_bytes -= evicted.cost_bytes;
            }
        }
    }

    /// Whether a face is currently resident in the preview ring.
    pub fn is_previewing(&self, face: FontFaceId) -> bool {
        self.preview.iter().any(|e| e.face == face)
    }

    /// The number of faces resident in the preview ring.
    pub fn preview_len(&self) -> usize {
        self.preview.len()
    }

    /// The total resident cost of the preview ring, for counters and tests.
    pub fn preview_bytes(&self) -> u64 {
        self.preview_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FontCache;

    fn catalog_face(family: &str) -> CatalogFace {
        CatalogFace {
            family: family.to_owned(),
            weight: FontWeight::REGULAR,
            width: FontWidth::NORMAL,
            slant: FontSlant::Normal,
            role: FontRole::Ui,
            color: false,
        }
    }

    #[test]
    fn enumeration_is_metadata_only_and_holds_no_faces_resident() {
        // Enumerating a large installed-font set stores descriptors only; nothing
        // is resident in the preview ring until something is actually previewed.
        let mut catalog = SystemFontCatalog::default();
        let faces: Vec<_> = (0..3000)
            .map(|n| catalog_face(&format!("Family {n}")))
            .collect();
        catalog.set_enumeration(faces);
        assert_eq!(catalog.len(), 3000);
        assert_eq!(catalog.preview_len(), 0);
        assert_eq!(catalog.preview_bytes(), 0);
    }

    #[test]
    fn a_picker_scan_does_not_pollute_the_working_cache() {
        // The acceptance assertion: a picker scanning every enumerated face must
        // not touch the working FontCache. Set up a working cache holding a hot
        // document face, capture its state, then run a full picker scan through
        // the catalog and assert the working cache is byte-for-byte unchanged.
        let mut work = FontCache::with_budget(500);
        let hot = FontFaceId(1000);
        // Promote the document's face to the working set across epochs.
        work.admit(hot, 100);
        work.advance_epoch();
        work.admit(hot, 100);
        work.advance_epoch();
        work.admit(hot, 100);
        work.advance_epoch();
        let working_len_before = work.len();
        let working_bytes_before = work.total_bytes();
        assert!(work.contains(hot));

        // Open the picker and scan-preview 500 one-shot faces through the catalog.
        let mut catalog = SystemFontCatalog::with_preview_budget(4 << 10);
        catalog.set_enumeration((0..500).map(|n| catalog_face(&format!("F{n}"))).collect());
        for n in 0..500 {
            // Each preview goes into the catalog's own ring — never the working
            // cache. The working cache is not even a parameter here.
            catalog.preview(FontFaceId(n), 1 << 10);
        }

        // The working cache never saw the scan: same faces, same bytes, and the
        // hot document face is still resident.
        assert_eq!(work.len(), working_len_before);
        assert_eq!(work.total_bytes(), working_bytes_before);
        assert!(work.contains(hot));
    }

    #[test]
    fn preview_ring_is_bounded_by_its_own_budget() {
        // The preview ring evicts oldest-first under its own budget, so a long
        // scan keeps only a bounded set of specimens resident.
        let mut catalog = SystemFontCatalog::with_preview_budget(4 << 10);
        for n in 0..100 {
            catalog.preview(FontFaceId(n), 1 << 10);
        }
        // Four 1 KiB specimens fit the 4 KiB budget; the ring never grows beyond.
        assert!(catalog.preview_bytes() <= 4 << 10);
        assert!(catalog.preview_len() <= 4);
        // The most recently previewed faces are the ones still resident.
        assert!(catalog.is_previewing(FontFaceId(99)));
        assert!(!catalog.is_previewing(FontFaceId(0)));
    }

    #[test]
    fn re_previewing_a_resident_face_is_a_no_op() {
        // Re-rendering a specimen already on screen must not grow the ring or
        // double-count its bytes.
        let mut catalog = SystemFontCatalog::with_preview_budget(64 << 10);
        catalog.preview(FontFaceId(1), 1 << 10);
        let bytes_after_first = catalog.preview_bytes();
        let len_after_first = catalog.preview_len();
        catalog.preview(FontFaceId(1), 1 << 10);
        assert_eq!(catalog.preview_bytes(), bytes_after_first);
        assert_eq!(catalog.preview_len(), len_after_first);
    }

    #[test]
    fn a_single_oversized_preview_stays_resident() {
        // A specimen larger than the whole ring budget must not evict itself into
        // an empty ring — the picker still needs to show it.
        let mut catalog = SystemFontCatalog::with_preview_budget(4 << 10);
        catalog.preview(FontFaceId(1), 64 << 10);
        assert_eq!(catalog.preview_len(), 1);
        assert!(catalog.is_previewing(FontFaceId(1)));
    }

    #[test]
    fn refreshing_enumeration_clears_stale_previews() {
        // Re-enumerating (picker reopened / font set changed) drops the prior
        // preview residency; nothing from the old scan lingers.
        let mut catalog = SystemFontCatalog::default();
        catalog.set_enumeration(vec![catalog_face("A")]);
        catalog.preview(FontFaceId(1), 1 << 10);
        assert_eq!(catalog.preview_len(), 1);
        catalog.set_enumeration(vec![catalog_face("B")]);
        assert_eq!(catalog.preview_len(), 0);
        assert_eq!(catalog.preview_bytes(), 0);
    }

    #[test]
    fn a_system_font_less_runtime_enumerates_nothing() {
        // On a runtime that withholds system fonts (section 16.1) the catalog is
        // simply empty — no implicit local-font enumeration, no bundled default.
        let catalog = SystemFontCatalog::default();
        assert!(catalog.is_empty());
        assert_eq!(catalog.len(), 0);
    }
}
