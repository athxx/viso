//! The mask model and retained mask cache: resolve a requested mask into
//! a cached coverage realization keyed so a stable mask is built once and reused,
//! stored tight (R8, ROI-sized, page-allocated) rather than as a permanent
//! full-screen RGBA texture.
//!
//! A mask is a per-pixel coverage/luminance field a later draw multiplies through
//! — a clip-path coverage, a soft alpha mask, an image or path used as a stencil.
//! Four kinds are supported at the baseline: [`MaskKind::Alpha`] and
//! [`MaskKind::Luminance`] as the two composition modes, and
//! [`MaskKind::Image`] / [`MaskKind::Path`] as the two sources. Higher-level mask
//! boolean algebra is a later lane and does not change this baseline.
//!
//! Storage discipline is a performance contract, not a detail:
//!
//! - **R8 where possible.** Alpha, luminance, and path coverage are one channel;
//!   only an image mask that genuinely carries color needs [`Rgba8Unorm`]. The
//!   [`MaskFormat`] a request resolves to is R8 unless the source forces color.
//! - **Tight ROI.** A mask is sized to the region of interest (the clip path's
//!   bounds, the masked draw's bounds) — never the whole surface.
//! - **Tile / page allocation.** ROIs pack into a shared R8 page via the same
//!   max-rects [`RectPacker`](crate::rect_packer) the glyph and color atlases use,
//!   so many small masks share one texture instead of each owning a target.
//!
//! This is a cold-path resource cache: the key is computed once when a
//! mask is requested (the same place a clip's [`ClipTier`](crate::ClipTier) is
//! chosen), and the cache is a keyed retained map — a stable mask hits its retained
//! slot with no re-raster, and slots untouched for a frame are reclaimed. The hot
//! path never rebuilds a mask whose inputs are unchanged.

use std::collections::HashMap;

use viso_gpu::TextureFormat;

use crate::primitive::Rect;
use crate::rect_packer::RectPacker;
use crate::scene::store::ClipFillRule;

/// Which mask a request realizes. The first two are composition modes
/// (how the mask's texel is interpreted), the last two are sources (where the
/// coverage comes from). All four are baseline; boolean combinations are a later
/// lane that reuses this vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MaskKind {
    /// The mask texel is coverage/alpha directly: the masked draw's alpha is
    /// multiplied by the mask's stored value.
    Alpha,
    /// The mask texel is a color whose *luminance* becomes coverage — a luminance
    /// mask (the CSS `luminance` mask-mode). Stored one-channel when the source is
    /// already grey; a color source is reduced to luminance at raster time.
    Luminance,
    /// The coverage comes from an image sampled as the mask. Carries color only
    /// when the mask mode needs the image's channels (a luminance image mask
    /// reduces to R8); a plain alpha image mask keeps only the alpha channel.
    Image,
    /// The coverage comes from rasterizing a vector path — the arbitrary-clip and
    /// path-mask case. Always one-channel coverage.
    Path,
}

impl MaskKind {
    /// Whether this kind is intrinsically single-channel coverage regardless of
    /// its source. Alpha/Luminance/Path store coverage; only [`Image`](MaskKind::Image)
    /// can need color, and only when its source actually carries it.
    pub fn is_coverage_only(self) -> bool {
        !matches!(self, MaskKind::Image)
    }
}

/// The pixel format a mask is stored in. R8 is the default and the common case;
/// RGBA is used only when color information is genuinely needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MaskFormat {
    /// Single 8-bit coverage channel — the default for alpha/luminance/path masks
    /// and for image masks whose color is not needed. Backs [`TextureFormat::R8Unorm`].
    R8,
    /// Full 8-bit RGBA — used only for an image mask that must preserve color.
    /// Backs [`TextureFormat::Rgba8Unorm`].
    Rgba8,
}

impl MaskFormat {
    /// The GPU texture format this mask storage maps to.
    pub fn texture_format(self) -> TextureFormat {
        match self {
            MaskFormat::R8 => TextureFormat::R8Unorm,
            MaskFormat::Rgba8 => TextureFormat::Rgba8Unorm,
        }
    }

    /// Bytes per texel of this storage.
    pub fn bytes_per_texel(self) -> usize {
        self.texture_format().bytes_per_texel()
    }
}

/// A requested mask, before the cache decides how to store and whether to reuse
/// it. `kind` is the composition/source; `needs_color` says whether an image
/// source must keep its channels (ignored for coverage-only kinds); `roi` is the
/// tight region the mask covers, in texels (points × the device scale); `key` is the exact set of
/// inputs the mask's pixels are a function of (the cache key).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MaskRequest {
    /// The mask kind (composition mode / source).
    pub kind: MaskKind,
    /// Whether an image source must preserve color. For a coverage-only `kind`
    /// this is ignored — such a mask is always R8.
    pub needs_color: bool,
    /// The tight region of interest, in texels — the mask's extent.
    pub roi: Rect,
    /// The cache key: the inputs the mask's pixels depend on and nothing else.
    pub key: MaskKey,
}

impl MaskRequest {
    /// The storage format this request resolves to: R8 unless the kind can carry
    /// color *and* the source actually needs it. Alpha/Luminance/Path are always
    /// R8; an image mask is R8 unless `needs_color`.
    pub fn format(&self) -> MaskFormat {
        if self.kind.is_coverage_only() || !self.needs_color {
            MaskFormat::R8
        } else {
            MaskFormat::Rgba8
        }
    }
}

/// The cache key of a retained mask: the exact set of inputs the mask's
/// pixels are a function of, so an unchanged key means the retained realization
/// is still valid and no re-raster is owed.
///
/// Mirrors [`ClipMaskKey`](crate::ClipMaskKey)'s discipline — every field is an
/// integer or an already-quantized bucket, so the key is `Eq`/`Hash` and
/// comparison on the resolve path is exact, never a float tolerance. A mask built
/// at one transform/scale bucket cannot be reused at another; a pure translation
/// folds into the ROI, not the bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MaskKey {
    /// The mask kind — an alpha and a luminance mask of the same source are
    /// distinct coverage.
    pub kind: MaskKind,
    /// The source geometry/image revision the mask was rasterized against; an
    /// edit or reparse bumps it and invalidates the cached mask.
    pub source_revision: u64,
    /// The quantized effective transform bucket the mask was built under.
    pub transform_bucket: u64,
    /// The device pixel scale, quantized to an integer bucket — the mask is
    /// rasterized in device pixels.
    pub device_scale_q: u32,
    /// The fill rule a path source's coverage was computed with (irrelevant to
    /// image sources, which pin it to [`ClipFillRule::NonZero`]).
    pub fill_rule: ClipFillRule,
}

/// Where a retained mask lives in the cache: its page, its origin and texel
/// extent in that page, plus the format it was stored in. The renderer turns
/// this into the sampled sub-rect of the page's texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaskSlot {
    /// The stored format (R8 unless a color image mask).
    pub format: MaskFormat,
    /// The page, an index into [`MaskCache::page_edges`].
    pub page: u16,
    /// Texel origin of the ROI within the mask page.
    pub x: u32,
    /// Texel origin of the ROI within the mask page.
    pub y: u32,
    /// ROI width in texels.
    pub w: u32,
    /// ROI height in texels.
    pub h: u32,
}

/// The outcome of resolving a mask request against the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaskResolution {
    /// The page slot the mask occupies.
    pub slot: MaskSlot,
    /// Whether this resolve rasterized the mask (a cold build or a re-raster after
    /// a key change). `false` means a cache hit: the retained slot was reused with
    /// no raster, the stable-mask fast path. Advances `clip_mask_builds`
    /// when `true`.
    pub rasterized: bool,
}

/// One retained cache entry: its slot and the frame it was last touched, used to
/// reclaim masks a frame no longer references.
#[derive(Debug, Clone, Copy)]
struct MaskEntry {
    slot: MaskSlot,
    last_used: u64,
}

/// The retained mask cache: a keyed store of coverage realizations backed
/// by a shared page, so a stable mask is built once and reused.
///
/// The cache is keyed by [`MaskKey`] (not positional like the scene's cursor
/// stores) because a mask's identity is its inputs, not its paint-order slot — the
/// same clip path masked under the same transform is the same mask wherever it
/// appears. This is a cold-path resource cache: the lookup happens once per
/// masked draw when it is planned, not per pixel or per frame for an unchanged
/// mask.
///
/// Storage is a list of square pages, each allocated by the shared max-rects
/// [`RectPacker`](crate::rect_packer); ROIs pack in tight, first fit across
/// the pages. There are no pages until a mask needs one; when none has room a
/// page is added at twice the edge of the last (at least the ROI's, rounded
/// up to a power of two, between the cache's minimum and maximum edge), so
/// the pages hold about what the masks need and few are ever bound. A frame
/// calls
/// [`begin_frame`](Self::begin_frame), [`resolve`](Self::resolve)s each mask it
/// needs, then [`end_frame`](Self::end_frame) to reclaim entries no frame has
/// touched — a purely retained mask survives untouched-free across frames only
/// while it is still referenced.
#[derive(Debug)]
pub struct MaskCache {
    entries: HashMap<MaskKey, MaskEntry>,
    pages: Vec<Page>,
    min_edge: u32,
    max_edge: u32,
    frame: u64,
}

/// One page: its edge in texels and what of it is free.
#[derive(Debug)]
struct Page {
    edge: u32,
    packer: RectPacker,
}

impl MaskCache {
    /// An empty cache whose pages are `min_edge` to `max_edge` texels square;
    /// an ROI wider or taller than `max_edge` is declined.
    pub fn new(min_edge: u32, max_edge: u32) -> MaskCache {
        let max_edge = max_edge.max(1);
        MaskCache {
            entries: HashMap::new(),
            pages: Vec::new(),
            min_edge: min_edge.clamp(1, max_edge),
            max_edge,
            frame: 0,
        }
    }

    /// Pages added from now on are at least `edge` texels square (within the
    /// maximum); the pages there are stay as they are.
    pub fn set_min_edge(&mut self, edge: u32) {
        self.min_edge = edge.clamp(1, self.max_edge);
    }

    /// Each page's edge in texels, in page order.
    pub fn page_edges(&self) -> impl ExactSizeIterator<Item = u32> + '_ {
        self.pages.iter().map(|p| p.edge)
    }

    /// Number of retained masks currently cached.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache holds no masks.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Start a new frame. Masks resolved this frame are marked touched; those not
    /// touched by [`end_frame`](Self::end_frame) are reclaimed.
    pub fn begin_frame(&mut self) {
        self.frame += 1;
    }

    /// Resolve a mask request against the cache.
    ///
    /// On a hit — an entry with the same [`MaskKey`] exists — the retained slot is
    /// returned untouched and `rasterized` is `false`: the stable-mask fast path,
    /// no re-raster. On a miss, the ROI is page-allocated (tight, in the
    /// request's [`format`](MaskRequest::format)), an entry is recorded, and
    /// `rasterized` is `true` so the caller knows a build is owed and
    /// `clip_mask_builds` advances. A miss no page has room for adds a page.
    /// Returns `None` only when the ROI is empty or wider or taller than the
    /// largest page (the caller falls back to a per-frame mask target).
    pub fn resolve(&mut self, request: &MaskRequest) -> Option<MaskResolution> {
        let frame = self.frame;
        if let Some(entry) = self.entries.get_mut(&request.key) {
            entry.last_used = frame;
            return Some(MaskResolution {
                slot: entry.slot,
                rasterized: false,
            });
        }

        // Miss: allocate the tight ROI in the resolved format. Texel extent is the
        // ROI rounded out to whole pixels; an empty ROI carries no mask.
        let (w, h) = roi_texels(request.roi);
        if w == 0 || h == 0 {
            return None;
        }
        let (page, x, y) = place(&mut self.pages, self.min_edge, self.max_edge, w, h)?;
        let slot = MaskSlot {
            format: request.format(),
            page,
            x,
            y,
            w,
            h,
        };
        self.entries.insert(
            request.key,
            MaskEntry {
                slot,
                last_used: frame,
            },
        );
        Some(MaskResolution {
            slot,
            rasterized: true,
        })
    }

    /// Reclaim masks not touched this frame, repacking the pages from the
    /// survivors and releasing the trailing pages none of them landed in.
    /// Returns whether anything was evicted. A steady scene touches every
    /// retained mask each frame and evicts nothing, so the pages stay stable
    /// and no mask is rebuilt.
    pub fn end_frame(&mut self) -> bool {
        let frame = self.frame;
        let before = self.entries.len();
        self.entries.retain(|_, e| e.last_used == frame);
        if self.entries.len() == before {
            return false;
        }
        // The free-rect set no longer reflects the survivors: rebuild it by
        // re-packing every surviving ROI. Origins may move; the caller re-blits
        // survivors it still needs (they carry no persistent GPU-side identity
        // beyond the slot returned each frame).
        for page in &mut self.pages {
            page.packer.reset();
        }
        let mut survivors: Vec<(&MaskKey, &mut MaskEntry)> = self.entries.iter_mut().collect();
        // Deterministic repack order (largest first, then by key) keeps
        // placement stable frame to frame for an unchanged survivor set.
        survivors.sort_by_key(|(key, e)| {
            (
                std::cmp::Reverse(e.slot.w * e.slot.h),
                key.source_revision,
                key.transform_bucket,
                key.device_scale_q,
                key.kind as u8,
                key.fill_rule as u8,
            )
        });
        let mut lost = Vec::new();
        for (key, entry) in survivors {
            // Every survivor fit before; one that no longer packs (the order
            // changed) gets a new page, or failing that leaves the cache.
            match place(
                &mut self.pages,
                self.min_edge,
                self.max_edge,
                entry.slot.w,
                entry.slot.h,
            ) {
                Some((page, x, y)) => {
                    entry.slot.page = page;
                    entry.slot.x = x;
                    entry.slot.y = y;
                }
                None => lost.push(*key),
            }
        }
        for key in lost {
            self.entries.remove(&key);
        }
        let used = self
            .entries
            .values()
            .map(|e| usize::from(e.slot.page) + 1)
            .max()
            .unwrap_or(0);
        self.pages.truncate(used);
        true
    }
}

/// Packs a `w × h` ROI first fit across `pages`, adding a page when none has
/// room: twice the last page's edge, at least the ROI's rounded up to a power
/// of two, within `min_edge..=max_edge`. `None` when the ROI exceeds
/// `max_edge` or the page count its index can name.
fn place(
    pages: &mut Vec<Page>,
    min_edge: u32,
    max_edge: u32,
    w: u32,
    h: u32,
) -> Option<(u16, u32, u32)> {
    if w > max_edge || h > max_edge {
        return None;
    }
    for (index, page) in pages.iter_mut().enumerate() {
        if let Some((x, y)) = page.packer.allocate(w, h) {
            return Some((index as u16, x, y));
        }
    }
    let index = u16::try_from(pages.len()).ok()?;
    let grown = pages.last().map_or(min_edge, |p| p.edge.saturating_mul(2));
    let edge = w
        .max(h)
        .next_power_of_two()
        .max(grown)
        .clamp(min_edge, max_edge);
    let mut packer = RectPacker::new(edge);
    let (x, y) = packer.allocate(w, h)?;
    pages.push(Page { edge, packer });
    Some((index, x, y))
}

/// The whole-pixel texel extent of an ROI: width/height rounded out to cover any
/// fractional edge, clamped to zero for a degenerate rect.
fn roi_texels(roi: Rect) -> (u32, u32) {
    let w = roi.w.ceil();
    let h = roi.h.ceil();
    if w <= 0.0 || h <= 0.0 {
        (0, 0)
    } else {
        (w as u32, h as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(rev: u64) -> MaskKey {
        MaskKey {
            kind: MaskKind::Path,
            source_revision: rev,
            transform_bucket: 0,
            device_scale_q: 1,
            fill_rule: ClipFillRule::NonZero,
        }
    }

    fn roi(w: f32, h: f32) -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w,
            h,
        }
    }

    fn request(kind: MaskKind, needs_color: bool, rev: u64) -> MaskRequest {
        MaskRequest {
            kind,
            needs_color,
            roi: roi(32.0, 24.0),
            key: MaskKey { kind, ..key(rev) },
        }
    }

    /// Coverage-only kinds are always R8; an image mask is R8 unless it needs
    /// color, and only then RGBA.
    #[test]
    fn coverage_masks_are_r8_color_image_is_rgba() {
        assert_eq!(request(MaskKind::Alpha, true, 0).format(), MaskFormat::R8);
        assert_eq!(
            request(MaskKind::Luminance, true, 0).format(),
            MaskFormat::R8
        );
        assert_eq!(request(MaskKind::Path, true, 0).format(), MaskFormat::R8);
        assert_eq!(request(MaskKind::Image, false, 0).format(), MaskFormat::R8);
        assert_eq!(
            request(MaskKind::Image, true, 0).format(),
            MaskFormat::Rgba8
        );
        assert_eq!(MaskFormat::R8.bytes_per_texel(), 1);
        assert_eq!(MaskFormat::Rgba8.bytes_per_texel(), 4);
    }

    /// A cold resolve rasterizes and page-allocates the tight ROI; the slot's
    /// texel extent is the ROI rounded out, not the page size.
    #[test]
    fn cold_resolve_rasterizes_a_tight_roi() {
        let mut cache = MaskCache::new(256, 256);
        let r = cache.resolve(&request(MaskKind::Path, false, 1)).unwrap();
        assert!(r.rasterized);
        assert_eq!((r.slot.w, r.slot.h), (32, 24));
        assert_eq!(r.slot.format, MaskFormat::R8);
        assert_eq!(cache.len(), 1);
    }

    /// A stable mask — same key next frame — is a cache hit: the retained slot is
    /// reused and nothing is re-rastered.
    #[test]
    fn stable_mask_hits_without_re_raster() {
        let mut cache = MaskCache::new(256, 256);
        cache.begin_frame();
        let first = cache.resolve(&request(MaskKind::Path, false, 1)).unwrap();
        assert!(first.rasterized);

        cache.begin_frame();
        let again = cache.resolve(&request(MaskKind::Path, false, 1)).unwrap();
        assert!(!again.rasterized, "unchanged key must not re-raster");
        assert_eq!(again.slot, first.slot);
        assert_eq!(cache.len(), 1);
    }

    /// A changed source revision is a different mask: it misses and re-rasters
    /// into its own slot.
    #[test]
    fn changed_revision_re_rasters() {
        let mut cache = MaskCache::new(256, 256);
        cache.begin_frame();
        cache.resolve(&request(MaskKind::Path, false, 1)).unwrap();
        cache.begin_frame();
        let changed = cache.resolve(&request(MaskKind::Path, false, 2)).unwrap();
        assert!(changed.rasterized);
        // The touched-this-frame set now has one live mask (rev 2); rev 1 was not
        // touched this frame.
        cache.end_frame();
        assert_eq!(cache.len(), 1);
    }

    /// A mask untouched for a frame is reclaimed at `end_frame`; a mask touched
    /// every frame survives.
    #[test]
    fn untouched_masks_are_reclaimed() {
        let mut cache = MaskCache::new(256, 256);
        cache.begin_frame();
        cache.resolve(&request(MaskKind::Path, false, 1)).unwrap();
        cache.resolve(&request(MaskKind::Path, false, 2)).unwrap();
        assert_eq!(cache.len(), 2);
        assert!(!cache.end_frame(), "both touched this frame, none evicted");

        // Next frame touches only rev 1.
        cache.begin_frame();
        let kept = cache.resolve(&request(MaskKind::Path, false, 1)).unwrap();
        assert!(!kept.rasterized);
        assert!(cache.end_frame(), "rev 2 went untouched and is evicted");
        assert_eq!(cache.len(), 1);
    }

    /// An empty ROI carries no mask (no allocation, no entry).
    #[test]
    fn empty_roi_resolves_to_none() {
        let mut cache = MaskCache::new(256, 256);
        let mut req = request(MaskKind::Path, false, 1);
        req.roi = roi(0.0, 10.0);
        assert!(cache.resolve(&req).is_none());
        assert_eq!(cache.len(), 0);
    }

    /// An ROI that does not fit the page falls back to `None` rather than
    /// allocating past the page.
    #[test]
    fn oversize_roi_falls_back_to_none() {
        let mut cache = MaskCache::new(16, 16);
        let mut req = request(MaskKind::Path, false, 1);
        req.roi = roi(64.0, 64.0);
        assert!(cache.resolve(&req).is_none());
    }

    /// No page exists until a mask needs one, and the first is the smallest
    /// that holds it.
    #[test]
    fn pages_appear_with_the_first_mask() {
        let mut cache = MaskCache::new(64, 1024);
        assert_eq!(cache.page_edges().len(), 0);
        let r = cache.resolve(&request(MaskKind::Path, false, 1)).unwrap();
        assert_eq!(r.slot.page, 0);
        assert_eq!(cache.page_edges().collect::<Vec<_>>(), [64]);

        let mut wide = request(MaskKind::Path, false, 2);
        wide.roi = roi(300.0, 10.0);
        let r = cache.resolve(&wide).unwrap();
        assert_eq!(r.slot.page, 1);
        assert_eq!(cache.page_edges().collect::<Vec<_>>(), [64, 512]);
    }

    /// A raised minimum sizes the pages added after it.
    #[test]
    fn a_raised_minimum_sizes_the_next_page() {
        let mut cache = MaskCache::new(64, 2048);
        cache.set_min_edge(512);
        cache.resolve(&request(MaskKind::Path, false, 1)).unwrap();
        assert_eq!(cache.page_edges().collect::<Vec<_>>(), [512]);
        cache.set_min_edge(4096);
        let mut big = request(MaskKind::Path, false, 2);
        big.roi = roi(600.0, 600.0);
        cache.resolve(&big).unwrap();
        assert_eq!(cache.page_edges().collect::<Vec<_>>(), [512, 2048]);
    }

    /// A full page is followed by one twice its edge; masks fill earlier
    /// pages first.
    #[test]
    fn a_full_page_is_followed_by_a_larger_one() {
        let mut cache = MaskCache::new(32, 1024);
        let mut req = |rev| {
            let mut r = request(MaskKind::Path, false, rev);
            r.roi = roi(32.0, 32.0);
            cache.resolve(&r).unwrap().slot
        };
        assert_eq!(req(1).page, 0);
        assert_eq!(req(2).page, 1);
        assert_eq!(req(3).page, 1);
        assert_eq!(cache.page_edges().collect::<Vec<_>>(), [32, 64]);
    }

    /// Evicting the masks of a trailing page releases it.
    #[test]
    fn an_emptied_trailing_page_is_released() {
        let mut cache = MaskCache::new(32, 1024);
        cache.begin_frame();
        let mut small = request(MaskKind::Path, false, 1);
        small.roi = roi(16.0, 16.0);
        let mut big = request(MaskKind::Path, false, 2);
        big.roi = roi(100.0, 100.0);
        cache.resolve(&small).unwrap();
        assert_eq!(cache.resolve(&big).unwrap().slot.page, 1);
        assert_eq!(cache.page_edges().len(), 2);

        cache.begin_frame();
        cache.resolve(&small).unwrap();
        assert!(cache.end_frame());
        assert_eq!(cache.page_edges().collect::<Vec<_>>(), [32]);
    }

    /// An ROI larger than the largest page is declined.
    #[test]
    fn an_roi_past_the_largest_page_is_declined() {
        let mut cache = MaskCache::new(32, 128);
        let mut req = request(MaskKind::Path, false, 1);
        req.roi = roi(129.0, 8.0);
        assert!(cache.resolve(&req).is_none());
        assert_eq!(cache.page_edges().len(), 0);
    }

    /// A fractional ROI edge rounds out to whole texels so the mask covers every
    /// partially-covered pixel.
    #[test]
    fn fractional_roi_rounds_out() {
        assert_eq!(roi_texels(roi(31.2, 23.9)), (32, 24));
        assert_eq!(roi_texels(roi(0.0, 5.0)), (0, 0));
    }
}
