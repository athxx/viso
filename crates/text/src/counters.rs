//! [`TextCounters`] — strippable observability for the text subsystem's
//! per-frame work. The facade reads these to confirm steady-state repaint does
//! no reshaping and to expose text cost in a `VISO_FRAME_TRACE` dump.
//!
//! Four counts capture the subsystem's cost centers: paragraph-cache reshapes
//! (shape + place misses), the re-linebreak subset of those (wrapped misses that
//! did line-breaking), glyphs freshly rasterized into an atlas (not deduplicated
//! hits), and bytes uploaded from dirty atlas bands. They never gate correctness
//! and never ride a hot inner loop beyond a single increment per event.
//!
//! The fields are [`Cell`]s so a shared `&TextCounters` can be bumped while the
//! owning [`TextSystem`](crate::TextSystem) holds other fields borrowed during
//! `prepare` (the split-borrow layout/atlas walk).

use std::cell::Cell;

/// Per-window counts of the text subsystem's work. Reset at a frame or
/// benchmark-iteration boundary so each count reflects one measured window.
#[derive(Debug, Default)]
pub struct TextCounters {
    /// Paragraph-cache misses that ran `layout` (shape + line-break + place). A
    /// steady-state repaint of unchanged text holds this at zero.
    reshapes: Cell<u64>,
    /// The re-linebreak subset of `reshapes`: misses where soft wrapping was
    /// active (`max_width` was `Some`), so line-breaking work actually ran.
    relinebreaks: Cell<u64>,
    /// Glyphs newly rasterized into an atlas this window — an atlas miss that
    /// packed a fresh entry, not a dedup hit on an already-packed glyph.
    rasters: Cell<u64>,
    /// Bytes uploaded from dirty atlas bands this window (SDF and color atlases
    /// combined), recorded at the facade's texture-write sites.
    atlas_upload_bytes: Cell<u64>,
}

impl TextCounters {
    /// Paragraph reshapes since the last reset.
    #[inline]
    pub fn reshapes(&self) -> u64 {
        self.reshapes.get()
    }
    /// Re-linebreaks (wrapped reshapes) since the last reset.
    #[inline]
    pub fn relinebreaks(&self) -> u64 {
        self.relinebreaks.get()
    }
    /// Fresh glyph rasterizations since the last reset.
    #[inline]
    pub fn rasters(&self) -> u64 {
        self.rasters.get()
    }
    /// Atlas upload bytes since the last reset.
    #[inline]
    pub fn atlas_upload_bytes(&self) -> u64 {
        self.atlas_upload_bytes.get()
    }

    /// Zero every counter — call at frame or benchmark-iteration boundaries so a
    /// count reflects one measured window.
    pub fn reset(&self) {
        self.reshapes.set(0);
        self.relinebreaks.set(0);
        self.rasters.set(0);
        self.atlas_upload_bytes.set(0);
    }

    /// Record a paragraph reshape. `wrapped` is whether soft wrapping was active
    /// (a `Some(max_width)` request), which additionally counts a re-linebreak.
    #[inline]
    pub fn record_reshape(&self, wrapped: bool) {
        self.reshapes.set(self.reshapes.get() + 1);
        if wrapped {
            self.relinebreaks.set(self.relinebreaks.get() + 1);
        }
    }

    /// Record a freshly rasterized glyph (an atlas miss that packed a new entry).
    #[inline]
    pub fn record_raster(&self) {
        self.rasters.set(self.rasters.get() + 1);
    }

    /// Record `n` bytes uploaded from a dirty atlas band.
    #[inline]
    pub fn record_atlas_upload(&self, n: u64) {
        self.atlas_upload_bytes
            .set(self.atlas_upload_bytes.get() + n);
    }
}
