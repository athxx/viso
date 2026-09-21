//! Order-safe batch planning (§9.6): fold a paint-ordered stream of primitives
//! into the fewest contiguous draw commands that still preserve visual order.
//!
//! # The key, not the comparison
//!
//! A draw command can absorb the primitive that follows it only when the two
//! share every piece of GPU state a single command fixes: the pipeline family,
//! the resource table it binds, the blend mode, the sample count, the
//! color-target class, and the depth/stencil class. Rather than compare those
//! fields one by one at each merge site, the planner packs the state-fixing
//! dimensions into one integer [`BatchKey`] and merges on integer equality
//! (§16.2, §29 — batch identity is a packed key, never a string). Two adjacent
//! primitives join their draw exactly when their keys are equal *and* their
//! structural clip and pass target match.
//!
//! # Order safety
//!
//! Paint order is a correctness contract (§8.6): a primitive may only join the
//! draw **immediately before it**. The planner never reaches back past an
//! intervening primitive to group a non-adjacent one, so the emitted draws,
//! read front to back, replay the scene in exactly its submission order. The
//! goal is the *maximum contiguous run of compatible instances*, not the fewest
//! possible draws (§9.6): widening the window to reorder across an intervening
//! primitive is only sound for spans explicitly marked reorder-safe, which this
//! layer does not yet mint — so the safe, order-preserving adjacency merge is
//! the whole policy here.
//!
//! # Mergeability
//!
//! Only primitives whose geometry lives in a *shared* family buffer can grow a
//! run: quads (one quad pipeline, one instance buffer) and triangle meshes (one
//! index stream). An image or a glyph run binds its own texture/atlas and is one
//! instanced draw of its own; a composite samples one offscreen pass. Those are
//! marked unmergeable — they neither extend the previous draw nor accept the
//! next — so each is its own batch even when two adjacent ones would pack the
//! same key. This mirrors the family split the lowering walk produces.

use crate::primitive::Rect;
use viso_gpu::BindGroupId;

/// Which built-in pipeline family a primitive draws through — the coarsest
/// dimension of a [`BatchKey`], selecting both the pipeline and the family
/// buffer its geometry indexes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchFamily {
    /// Axis-aligned rounded quads, drawn instanced from the shared quad buffer.
    Quad,
    /// Analytic rounded rectangles (per-corner radius), drawn instanced from
    /// their own shared buffer.
    AnalyticRRect,
    /// Analytic ellipses, drawn instanced from their own shared buffer.
    AnalyticEllipse,
    /// Analytic capsules/stadiums, drawn instanced from their own shared buffer.
    AnalyticCapsule,
    /// Analytic line segments (cap/join/miter), drawn instanced from their own
    /// shared buffer.
    AnalyticLine,
    /// A single textured image, drawn instanced from the shared image buffer,
    /// binding its texture's `bind_group`.
    Image,
    /// One run of coverage glyphs, drawn instanced from the shared glyph buffer,
    /// binding its atlas's `bind_group`.
    GlyphRun,
    /// Triangle meshes (vector paths and raw meshes), drawn indexed from the
    /// shared mesh vertex/index buffers.
    Mesh,
    /// A single gradient fill over an axis-aligned rect, drawn instanced from the
    /// shared gradient buffer, binding its baked 1D LUT atlas's `bind_group`.
    Gradient,
    /// Analytic soft drop shadows (rounded box / ellipse / capsule), drawn
    /// instanced from their own shared buffer; a closed-form Gaussian coverage
    /// ramp, binding no texture.
    AnalyticShadow,
}

impl BatchFamily {
    /// The 4-bit family tag packed into a [`BatchKey`]. Stable across builds so a
    /// frozen key round-trips (see [`BatchKey`] field layout).
    const fn tag(self) -> u64 {
        match self {
            BatchFamily::Quad => 0,
            BatchFamily::Image => 1,
            BatchFamily::GlyphRun => 2,
            BatchFamily::Mesh => 3,
            BatchFamily::AnalyticRRect => 4,
            BatchFamily::AnalyticEllipse => 5,
            BatchFamily::AnalyticCapsule => 6,
            BatchFamily::AnalyticLine => 7,
            BatchFamily::Gradient => 8,
            BatchFamily::AnalyticShadow => 9,
        }
    }

    /// Recover the family from its 4-bit tag. `None` for an unassigned tag.
    const fn from_tag(tag: u64) -> Option<BatchFamily> {
        match tag {
            0 => Some(BatchFamily::Quad),
            1 => Some(BatchFamily::Image),
            2 => Some(BatchFamily::GlyphRun),
            3 => Some(BatchFamily::Mesh),
            4 => Some(BatchFamily::AnalyticRRect),
            5 => Some(BatchFamily::AnalyticEllipse),
            6 => Some(BatchFamily::AnalyticCapsule),
            7 => Some(BatchFamily::AnalyticLine),
            8 => Some(BatchFamily::Gradient),
            9 => Some(BatchFamily::AnalyticShadow),
            _ => None,
        }
    }

    /// Whether draws of this family can grow by absorbing an adjacent primitive
    /// of the same key. Quads, analytic rrects/ellipses, and meshes each share a
    /// family buffer and merge; images and glyph runs each bind their own
    /// resource and stand alone.
    pub const fn mergeable(self) -> bool {
        matches!(
            self,
            BatchFamily::Quad
                | BatchFamily::AnalyticRRect
                | BatchFamily::AnalyticEllipse
                | BatchFamily::AnalyticCapsule
                | BatchFamily::AnalyticLine
                | BatchFamily::AnalyticShadow
                | BatchFamily::Mesh
        )
    }
}

/// Which render pass a primitive draws into — the render-target dimension of a
/// [`BatchKey`]. The surface pass, each offscreen pass, and each backdrop
/// capture pass are distinct targets, so a draw never spans two passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchTarget {
    /// The final surface pass, composited onto the window.
    Main,
    /// The `i`-th offscreen render-to-texture pass.
    Offscreen(usize),
    /// The `i`-th backdrop capture pass: the content behind one (or one shared
    /// group of) backdrop layers, re-rendered into a tight ROI target.
    Backdrop(usize),
}

impl BatchTarget {
    /// The render-target *index* field value packed into a [`BatchKey`]: `0` for
    /// the surface, and `i + 1` for offscreen pass or backdrop capture `i`. The
    /// two pass kinds share the index field and are told apart by
    /// [`class`](Self::class), so a capture index never has to be squeezed into a
    /// sub-range of the offscreen pass numbering.
    const fn field(self) -> u64 {
        match self {
            BatchTarget::Main => 0,
            BatchTarget::Offscreen(i) | BatchTarget::Backdrop(i) => i as u64 + 1,
        }
    }

    /// The render-target *class* field value: `0` for the surface and offscreen
    /// passes, `1` for a backdrop capture pass.
    const fn class(self) -> u64 {
        match self {
            BatchTarget::Main | BatchTarget::Offscreen(_) => 0,
            BatchTarget::Backdrop(_) => 1,
        }
    }
}

/// A packed key over the GPU state a single draw command fixes (§9.6, §16.2).
///
/// Two adjacent primitives can share one draw only if their keys are equal (and
/// their structural clip matches — a clip is four floats, compared alongside the
/// key rather than packed into it). The key is a `u64` partitioned into
/// bit-fields, low to high:
///
/// | bits    | width | field                | today                     |
/// |---------|-------|----------------------|---------------------------|
/// | 0..4    | 4     | pipeline family      | [`BatchFamily`]           |
/// | 4..8    | 4     | blend class          | reserved, `0` (src-over)  |
/// | 8..10   | 2     | sample count class   | reserved, `0` (1×)        |
/// | 10..12  | 2     | color-target class   | reserved, `0` (BGRA8)     |
/// | 12..14  | 2     | depth/stencil class  | reserved, `0` (none)      |
/// | 14..24  | 10    | render target index  | [`BatchTarget`]           |
/// | 24..48  | 24    | resource table       | bind-group index, or `0`  |
/// | 48..50  | 2     | render target class  | surface/offscreen, or     |
/// |         |       |                      | backdrop capture          |
/// | 50..64  | 14    | reserved             | `0`                       |
///
/// The reserved fields carry the single class this renderer uses today; they
/// exist in the layout so that adding blend modes, MSAA, or a depth pass later
/// packs into an existing field without moving the frozen ones (§9.6 names the
/// full field set as the batch key's identity).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BatchKey(u64);

impl BatchKey {
    const FAMILY_SHIFT: u64 = 0;
    const FAMILY_MASK: u64 = 0b1111;
    const TARGET_SHIFT: u64 = 14;
    const TARGET_MASK: u64 = 0x3ff; // 10 bits
    const RESOURCE_SHIFT: u64 = 24;
    const RESOURCE_MASK: u64 = 0xff_ffff; // 24 bits
    const TARGET_CLASS_SHIFT: u64 = 48;
    const TARGET_CLASS_MASK: u64 = 0b11;

    /// Pack a key from the state a primitive fixes: its pipeline `family`, the
    /// pass `target` it draws into, and the `resource` bind group it binds (the
    /// texture/atlas for image/glyph families, `None` for quad/mesh). Reserved
    /// class fields take their sole current value (`0`).
    ///
    /// Panics in debug if `target`'s pass index or `resource`'s bind-group index
    /// overflows its field — both are far beyond any real frame's pass or resource
    /// count, so the panic marks a layout/pack contract violation, not a runtime
    /// input error.
    pub fn pack(
        family: BatchFamily,
        target: BatchTarget,
        resource: Option<BindGroupId>,
    ) -> BatchKey {
        let target_field = target.field();
        debug_assert!(
            target_field <= Self::TARGET_MASK,
            "pass index exceeds the render-target field width"
        );
        let resource_field = resource.map_or(0, |bg| bg.index as u64);
        debug_assert!(
            resource_field <= Self::RESOURCE_MASK,
            "bind-group index exceeds the resource-table field width"
        );
        BatchKey(
            (family.tag() << Self::FAMILY_SHIFT)
                | ((target_field & Self::TARGET_MASK) << Self::TARGET_SHIFT)
                | ((resource_field & Self::RESOURCE_MASK) << Self::RESOURCE_SHIFT)
                | ((target.class() & Self::TARGET_CLASS_MASK) << Self::TARGET_CLASS_SHIFT),
        )
    }

    /// The pipeline family this key selects.
    pub fn family(self) -> BatchFamily {
        let tag = (self.0 >> Self::FAMILY_SHIFT) & Self::FAMILY_MASK;
        BatchFamily::from_tag(tag).expect("a packed key always holds a valid family tag")
    }

    /// The render-target index field: `0` for the surface, `i + 1` for offscreen
    /// pass or backdrop capture `i`.
    pub fn target_field(self) -> u64 {
        (self.0 >> Self::TARGET_SHIFT) & Self::TARGET_MASK
    }

    /// The render-target class field: `0` for the surface and offscreen passes,
    /// `1` for a backdrop capture pass.
    pub fn target_class_field(self) -> u64 {
        (self.0 >> Self::TARGET_CLASS_SHIFT) & Self::TARGET_CLASS_MASK
    }

    /// The resource-table field: the bound bind-group index, or `0` for none.
    pub fn resource_field(self) -> u64 {
        (self.0 >> Self::RESOURCE_SHIFT) & Self::RESOURCE_MASK
    }

    /// The raw packed bits, for a frozen round-trip test and cold introspection.
    pub fn bits(self) -> u64 {
        self.0
    }
}

/// One primitive presented to the planner: the packed state key it fixes, the
/// structural clip it draws under (four floats, compared alongside the key), and
/// whether its family can grow a run.
///
/// The planner turns a paint-ordered stream of these, plus each primitive's
/// geometry `count`, into contiguous [`super::chunk::RenderChunk`] draws.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BatchItem {
    /// The packed GPU-state key this primitive fixes.
    pub key: BatchKey,
    /// The effective clip rect, or `None` for unclipped. A rect is not packed
    /// into the integer key; it is compared structurally next to it.
    pub clip: Option<Rect>,
    /// Whether this primitive's family can absorb — and be absorbed into — an
    /// adjacent primitive with an equal key and clip. Quad/mesh: `true`;
    /// image/glyph/composite: `false`.
    pub mergeable: bool,
}

/// Decide whether `next` joins the draw that `prev` is currently growing.
///
/// The one merge predicate every lowering and introspection site routes
/// through, so the segment boundaries the encoder emits, the batch dump, and the
/// primitive→batch map can never drift apart. A primitive joins the previous
/// draw only when both are mergeable, their packed keys are equal, and their
/// structural clips match — the adjacency-only, order-preserving policy (§8.6,
/// §9.6).
pub fn joins(prev: &BatchItem, next: &BatchItem) -> bool {
    prev.mergeable && next.mergeable && prev.key == next.key && prev.clip == next.clip
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_gpu::BindGroupId;

    fn bg(index: u32) -> BindGroupId {
        BindGroupId {
            index,
            generation: 1,
        }
    }

    #[test]
    fn family_round_trips_through_the_key() {
        for family in [
            BatchFamily::Quad,
            BatchFamily::Image,
            BatchFamily::GlyphRun,
            BatchFamily::Mesh,
            BatchFamily::AnalyticRRect,
            BatchFamily::AnalyticEllipse,
            BatchFamily::AnalyticCapsule,
            BatchFamily::AnalyticLine,
            BatchFamily::Gradient,
            BatchFamily::AnalyticShadow,
        ] {
            let key = BatchKey::pack(family, BatchTarget::Main, None);
            assert_eq!(key.family(), family);
        }
    }

    #[test]
    fn target_round_trips_through_the_key() {
        assert_eq!(
            BatchKey::pack(BatchFamily::Quad, BatchTarget::Main, None).target_field(),
            0
        );
        assert_eq!(
            BatchKey::pack(BatchFamily::Quad, BatchTarget::Offscreen(0), None).target_field(),
            1
        );
        assert_eq!(
            BatchKey::pack(BatchFamily::Quad, BatchTarget::Offscreen(6), None).target_field(),
            7
        );
        // A backdrop capture numbers its index in the same field as an offscreen
        // pass (so neither kind loses range) and is told apart by the class field,
        // which is what keeps the two from aliasing at equal indices.
        let backdrop = BatchKey::pack(BatchFamily::Quad, BatchTarget::Backdrop(0), None);
        assert_eq!(backdrop.target_field(), 1);
        assert_eq!(backdrop.target_class_field(), 1);
        assert_eq!(
            BatchKey::pack(BatchFamily::Quad, BatchTarget::Offscreen(0), None).target_class_field(),
            0
        );
        assert_ne!(
            backdrop,
            BatchKey::pack(BatchFamily::Quad, BatchTarget::Offscreen(0), None)
        );
        // The whole index range stays available to captures too.
        assert_eq!(
            BatchKey::pack(BatchFamily::Quad, BatchTarget::Backdrop(0x3fe), None).target_field(),
            0x3ff
        );
    }

    #[test]
    fn resource_round_trips_through_the_key() {
        let key = BatchKey::pack(BatchFamily::Image, BatchTarget::Main, Some(bg(42)));
        assert_eq!(key.resource_field(), 42);
        // No resource packs a zero resource field.
        let none = BatchKey::pack(BatchFamily::Quad, BatchTarget::Main, None);
        assert_eq!(none.resource_field(), 0);
    }

    #[test]
    fn distinct_dimensions_yield_distinct_keys() {
        let a = BatchKey::pack(BatchFamily::Quad, BatchTarget::Main, None);
        let b = BatchKey::pack(BatchFamily::Mesh, BatchTarget::Main, None);
        let c = BatchKey::pack(BatchFamily::Quad, BatchTarget::Offscreen(0), None);
        let d = BatchKey::pack(BatchFamily::Image, BatchTarget::Main, Some(bg(1)));
        let e = BatchKey::pack(BatchFamily::Image, BatchTarget::Main, Some(bg(2)));
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(d, e);
    }

    #[test]
    fn fields_do_not_alias_when_all_set() {
        // A key with every live field non-zero recovers each field independently:
        // no shift/mask overlap.
        let key = BatchKey::pack(
            BatchFamily::GlyphRun,
            BatchTarget::Offscreen(3),
            Some(bg(9)),
        );
        assert_eq!(key.family(), BatchFamily::GlyphRun);
        assert_eq!(key.target_field(), 4);
        assert_eq!(key.resource_field(), 9);
    }

    #[test]
    fn mergeable_families_join_on_equal_key_and_clip() {
        let quad = BatchItem {
            key: BatchKey::pack(BatchFamily::Quad, BatchTarget::Main, None),
            clip: None,
            mergeable: true,
        };
        assert!(joins(&quad, &quad));
    }

    #[test]
    fn unmergeable_families_never_join() {
        let img = BatchItem {
            key: BatchKey::pack(BatchFamily::Image, BatchTarget::Main, Some(bg(1))),
            clip: None,
            mergeable: false,
        };
        // Two byte-identical images still never merge (each binds its own draw).
        assert!(!joins(&img, &img));
    }

    #[test]
    fn different_clip_splits_a_mergeable_run() {
        let key = BatchKey::pack(BatchFamily::Quad, BatchTarget::Main, None);
        let a = BatchItem {
            key,
            clip: Some(Rect {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            }),
            mergeable: true,
        };
        let b = BatchItem {
            key,
            clip: Some(Rect {
                x: 5.0,
                y: 5.0,
                w: 10.0,
                h: 10.0,
            }),
            mergeable: true,
        };
        assert!(!joins(&a, &b));
    }
}
