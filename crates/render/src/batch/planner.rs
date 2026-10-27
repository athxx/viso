//! Order-safe batch planning: fold a paint-ordered stream of primitives
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
//! (batch identity is a packed key, never a string). Two adjacent
//! primitives join their draw exactly when their keys are equal *and* their
//! structural clip and pass target match.
//!
//! # Order safety
//!
//! Paint order is a correctness contract. Adjacent primitives with equal keys
//! join one run ([`joins`]); [`plan`] then folds those runs into draws, and a
//! run may join a draw *earlier* than the one before it only when moving it
//! there cannot change a pixel: every draw it passes over lies in the same
//! space, covers a known region, and does not overlap the run. Two draws that
//! cover disjoint pixels commute, so the emitted draws still replay the scene
//! exactly. A run joins only a draw whose instances end where its own begin,
//! so a draw stays one contiguous range of its family buffer. The walk back
//! stops at the first draw it cannot pass, and after [`REACH`] draws.
//!
//! # Mergeability
//!
//! Families that bind no per-draw resource — quads, the analytic shapes,
//! shadows, meshes — grow a run on their key alone. Families that sample one
//! texture — images, both glyph-run lanes, gradients — grow one too, because
//! the texture's bind group is part of the key; a user program's boxes merge
//! on the program. Only the composite families (color transform, advanced
//! blend, material) are unmergeable: each carries per-draw state beyond its
//! bind group, so it neither extends the previous draw nor accepts the next
//! even when two adjacent ones would pack the same key
//! ([`BatchFamily::mergeable`]).

use crate::primitive::Rect;
use crate::transform::Space;
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
    /// One run of MTSDF glyphs, drawn instanced from the *same* shared glyph
    /// buffer, binding its field atlas's `bind_group`.
    ///
    /// Its own family rather than a [`GlyphRun`](BatchFamily::GlyphRun) variant
    /// because the two lanes decode differently in the fragment — exact coverage
    /// versus a multi-channel distance — so they are two pipelines, and a
    /// pipeline switch is exactly what a family tag exists to separate. The
    /// instance ABI is shared, so a paragraph that mixes the lanes still fills
    /// one buffer.
    MtsdfRun,
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
    /// One fused run of per-pixel color effects, drawn instanced from the shared
    /// color-transform buffer and binding the source texture's `bind_group`: an
    /// affine color matrix plus an optional gamma, applied to what an offscreen
    /// layer already rendered.
    ColorTransform,
    /// One isolated advanced-blend composite, drawn instanced from the shared
    /// advanced-blend buffer and binding *two* textures in one `bind_group` — the
    /// isolated layer and the bounded snapshot of what is behind it. The fragment
    /// evaluates a blend the fixed-function stage cannot express, which is why it
    /// is its own family rather than a variant of [`Image`](BatchFamily::Image):
    /// the common `SrcOver` pipeline stays untouched.
    AdvancedBlend,
    /// One frosted material surface, drawn instanced from the shared material
    /// buffer and binding the blurred backdrop's `bind_group`. Its own family
    /// rather than an [`Image`](BatchFamily::Image) variant because the fragment
    /// tints, grains and masks in one pass.
    Material,
    /// Boxes painted by one user program, drawn instanced from that program's
    /// own buffer. The program's slot is part of the key
    /// ([`BatchKey::with_program`]), so only draws of one program merge.
    Program,
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
            BatchFamily::ColorTransform => 10,
            BatchFamily::AdvancedBlend => 11,
            BatchFamily::Material => 12,
            BatchFamily::MtsdfRun => 13,
            BatchFamily::Program => 14,
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
            10 => Some(BatchFamily::ColorTransform),
            11 => Some(BatchFamily::AdvancedBlend),
            12 => Some(BatchFamily::Material),
            13 => Some(BatchFamily::MtsdfRun),
            14 => Some(BatchFamily::Program),
            _ => None,
        }
    }

    /// Whether draws of this family can grow by absorbing an adjacent primitive
    /// of the same key.
    ///
    /// Two groups qualify. The families that bind no per-draw resource — quads,
    /// the analytic shapes, meshes — merge on their key alone. The families that
    /// sample one texture — images, glyph runs, gradients — merge too, because the
    /// texture is *part of* the key: an equal key means an equal bind group, so a
    /// run of them is one resource and one draw (both text lanes included — a
    /// field atlas batches exactly like a coverage atlas). That is the binding fallback
    /// doing its work: atlas the content, and bind-group batching collapses a screen of
    /// icons or a page of text into a draw per atlas, which is the draw count a
    /// resource table would have reached with backend support this RHI does not
    /// need.
    ///
    /// A user program's boxes merge on the program: one pipeline, one buffer.
    ///
    /// The composite families stand alone regardless: a color transform, an
    /// advanced blend, and a material surface each carry per-draw state beyond
    /// their bind group (a fused matrix, a destination snapshot, a backdrop), so
    /// two of them are not one draw even when their keys agree.
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
                | BatchFamily::Image
                | BatchFamily::GlyphRun
                | BatchFamily::MtsdfRun
                | BatchFamily::Gradient
                | BatchFamily::Program
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

/// A packed key over the GPU state a single draw command fixes.
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
/// | 50..64  | 14    | program slot         | the [`BatchFamily::Program`] |
/// |         |       |                      | draw's slot, or `0`       |
///
/// The reserved fields carry the single class this renderer uses today; they
/// exist in the layout so that adding blend modes, MSAA, or a depth pass later
/// packs into an existing field without moving the frozen ones (the full field set is
/// the batch key's identity).
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
    const PROGRAM_SHIFT: u64 = 50;
    const PROGRAM_MASK: u64 = 0x3fff; // 14 bits

    /// The most program slots a key tells apart.
    pub const PROGRAM_SLOTS: u32 = Self::PROGRAM_MASK as u32 + 1;

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

    /// The key of a [`BatchFamily::Program`] draw of the program in `slot`.
    ///
    /// Panics in debug when `slot` is not below [`PROGRAM_SLOTS`](Self::PROGRAM_SLOTS),
    /// which the renderer refuses to install into.
    pub fn with_program(self, slot: u32) -> BatchKey {
        debug_assert!(slot < Self::PROGRAM_SLOTS, "program slot exceeds its field");
        let cleared = self.0 & !(Self::PROGRAM_MASK << Self::PROGRAM_SHIFT);
        BatchKey(cleared | ((u64::from(slot) & Self::PROGRAM_MASK) << Self::PROGRAM_SHIFT))
    }

    /// The program-slot field: a program draw's slot, `0` for every other
    /// family.
    pub fn program_field(self) -> u64 {
        (self.0 >> Self::PROGRAM_SHIFT) & Self::PROGRAM_MASK
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
    /// The transform root and moving clips the primitive follows: draws that
    /// move differently cannot share one.
    pub space: Space,
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
/// structural clips and spaces match — the adjacency-only, order-preserving
/// policy.
pub fn joins(prev: &BatchItem, next: &BatchItem) -> bool {
    prev.mergeable
        && next.mergeable
        && prev.key == next.key
        && prev.clip == next.clip
        && prev.space == next.space
}

/// How many draws [`plan`] walks back past to find one a run can join.
pub const REACH: usize = 32;

/// One run of adjacent, joined primitives handed to [`plan`]: what it binds,
/// where its instances lie in its family buffer, and the region it covers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Run {
    /// The batch item the run's primitives share.
    pub item: BatchItem,
    /// The first instance (or index) of the run in its family buffer.
    pub start: u32,
    /// How many instances (or indices) the run draws.
    pub count: u32,
    /// The region its pixels may touch, in its pass's space, or `None` when
    /// not known. A draw passed over and a run moved past it must both have one.
    pub cover: Option<Rect>,
}

impl Run {
    /// The region the run touches once its clip cuts it.
    fn touched(&self) -> Option<Rect> {
        let cover = self.cover?;
        Some(match self.item.clip {
            Some(clip) => cover.intersect(clip),
            None => cover,
        })
    }

    /// Whether `self` and `other` draw into the same pass.
    fn same_pass(&self, other: &Run) -> bool {
        self.item.key.target_field() == other.item.key.target_field()
            && self.item.key.target_class_field() == other.item.key.target_class_field()
    }
}

/// One draw [`plan`] emits: the run that opened it, grown by the runs it took
/// in. Its `start`/`count` cover every instance of those runs, and its
/// `cover` their union.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Draw {
    /// The index of the run that opened the draw.
    pub first: u32,
    /// The draw as one run: its item, instance range and cover.
    pub run: Run,
}

/// Fold `runs`, in paint order, into `draws`: `of[i]` is the draw run `i`
/// lands in. Returns how many runs joined a draw other than the last.
///
/// A run extends the last draw when the two [`joins`] and its instances follow
/// the draw's. Otherwise it walks back: past each draw of another pass, and
/// past each draw of its own pass that lies in its space and covers a known
/// region apart from its own, until a draw it joins whose instances it
/// follows. A draw it can neither pass nor join ends the walk, and the run
/// opens a draw of its own. Program runs never move: their instances are laid
/// out by the run that precedes them.
pub fn plan(runs: &[Run], draws: &mut Vec<Draw>, of: &mut Vec<u32>) -> u32 {
    draws.clear();
    of.clear();
    let mut moved = 0;
    for (i, run) in runs.iter().enumerate() {
        let at = match draws.last() {
            Some(last) if extends(&last.run, run) => Some(draws.len() - 1),
            Some(_) if run.item.key.family() != BatchFamily::Program => {
                let found = hoist(draws, run);
                moved += u32::from(found.is_some());
                found
            }
            _ => None,
        };
        match at {
            Some(d) => {
                let draw = &mut draws[d].run;
                draw.count += run.count;
                draw.cover = match (draw.cover, run.cover) {
                    (Some(a), Some(b)) => Some(a.union(b)),
                    _ => None,
                };
                of.push(d as u32);
            }
            None => {
                of.push(draws.len() as u32);
                draws.push(Draw {
                    first: i as u32,
                    run: *run,
                });
            }
        }
    }
    moved
}

/// Whether `run` can grow `draw`: the two join and `run`'s instances begin
/// where the draw's end.
fn extends(draw: &Run, run: &Run) -> bool {
    joins(&draw.item, &run.item) && draw.start + draw.count == run.start
}

/// The earlier draw `run` may join without changing a pixel, if any.
fn hoist(draws: &[Draw], run: &Run) -> Option<usize> {
    let touched = run.touched()?;
    for (d, draw) in draws.iter().enumerate().rev().take(REACH) {
        let draw = &draw.run;
        if !draw.same_pass(run) {
            continue;
        }
        if extends(draw, run) {
            return Some(d);
        }
        let apart = draw.item.space == run.item.space
            && draw.touched().is_some_and(|t| !t.intersects(touched));
        if !apart {
            return None;
        }
    }
    None
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
    fn a_program_key_carries_its_slot() {
        let key = BatchKey::pack(BatchFamily::Program, BatchTarget::Offscreen(2), None);
        let a = key.with_program(5);
        assert_eq!(a.family(), BatchFamily::Program);
        assert_eq!(a.target_field(), 3);
        assert_eq!(a.program_field(), 5);
        assert_eq!(
            a.with_program(9).program_field(),
            9,
            "a slot replaces the slot"
        );
        assert_ne!(a, key.with_program(6));
        assert_eq!(
            key.with_program(BatchKey::PROGRAM_SLOTS - 1)
                .program_field(),
            u64::from(BatchKey::PROGRAM_SLOTS - 1)
        );
        assert_eq!(key.program_field(), 0);
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
            BatchFamily::ColorTransform,
            BatchFamily::AdvancedBlend,
            BatchFamily::Program,
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
            space: Space::STATIC,
            mergeable: true,
        };
        assert!(joins(&quad, &quad));
    }

    #[test]
    fn unmergeable_families_never_join() {
        let img = BatchItem {
            key: BatchKey::pack(BatchFamily::Image, BatchTarget::Main, Some(bg(1))),
            clip: None,
            space: Space::STATIC,
            mergeable: false,
        };
        // Two byte-identical images still never merge (each binds its own draw).
        assert!(!joins(&img, &img));
        // A fused color op samples one specific source texture, so it stands alone
        // for the same reason — the fusion happens in the matrix, not in the batch.
        assert!(!BatchFamily::ColorTransform.mergeable());
        // An isolated advanced blend binds one specific (source, destination) pair
        // and writes with `Replace`, so two adjacent ones must never share a draw.
        assert!(!BatchFamily::AdvancedBlend.mergeable());
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
            space: Space::STATIC,
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
            space: Space::STATIC,
            mergeable: true,
        };
        assert!(!joins(&a, &b));
    }

    #[test]
    fn a_different_space_splits_a_mergeable_run() {
        let key = BatchKey::pack(BatchFamily::Quad, BatchTarget::Main, None);
        let a = BatchItem {
            key,
            clip: None,
            space: Space::STATIC,
            mergeable: true,
        };
        let b = BatchItem {
            space: Space {
                slot: 0,
                chain: a.space.chain,
            },
            ..a
        };
        assert!(joins(&a, &a));
        assert!(!joins(&a, &b));
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    /// A run of `family` binding `resource`, drawing `count` instances from
    /// `start` over `cover`.
    fn run(family: BatchFamily, resource: u32, start: u32, count: u32, cover: Option<Rect>) -> Run {
        Run {
            item: BatchItem {
                key: BatchKey::pack(family, BatchTarget::Main, Some(bg(resource))),
                clip: None,
                space: Space::STATIC,
                mergeable: family.mergeable(),
            },
            start,
            count,
            cover,
        }
    }

    fn planned(runs: &[Run]) -> (Vec<Draw>, Vec<u32>, u32) {
        let (mut draws, mut of) = (Vec::new(), Vec::new());
        let moved = plan(runs, &mut draws, &mut of);
        (draws, of, moved)
    }

    /// Cards in a column, each a background quad with a label over it: the
    /// paint order alternates quad, glyphs, quad, glyphs.
    fn cards(n: u32) -> Vec<Run> {
        (0..n)
            .flat_map(|i| {
                let y = i as f32 * 30.0;
                [
                    run(BatchFamily::Quad, 0, i, 1, Some(rect(0.0, y, 100.0, 28.0))),
                    run(
                        BatchFamily::GlyphRun,
                        1,
                        i * 5,
                        5,
                        Some(rect(4.0, y + 4.0, 50.0, 20.0)),
                    ),
                ]
            })
            .collect()
    }

    #[test]
    fn text_over_sibling_cards_folds_into_one_quad_draw_and_one_glyph_draw() {
        let (draws, of, moved) = planned(&cards(6));
        assert_eq!(draws.len(), 2);
        assert_eq!((draws[0].run.start, draws[0].run.count), (0, 6));
        assert_eq!((draws[1].run.start, draws[1].run.count), (0, 30));
        assert_eq!(draws[0].run.item.key.family(), BatchFamily::Quad);
        assert_eq!(of, [0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1]);
        assert_eq!(moved, 5, "each later card's quad moved past the labels");
        assert_eq!(draws[0].run.cover, Some(rect(0.0, 0.0, 100.0, 178.0)));
    }

    #[test]
    fn a_run_over_a_draw_it_would_pass_keeps_its_place() {
        // The second quad covers the first label: it must draw after it.
        let mut runs = cards(2);
        runs[2].cover = Some(rect(0.0, 20.0, 100.0, 28.0));
        let (draws, of, moved) = planned(&runs);
        assert_eq!(moved, 0);
        assert_eq!(of, [0, 1, 2, 3]);
        assert_eq!(draws.len(), 4);
    }

    #[test]
    fn a_draw_of_unknown_extent_is_never_passed() {
        let mut runs = cards(2);
        runs[1].cover = None;
        assert_eq!(planned(&runs).0.len(), 4, "the label's extent is unknown");
        let mut runs = cards(2);
        runs[2].cover = None;
        assert_eq!(planned(&runs).0.len(), 4, "the moving quad's is");
    }

    #[test]
    fn a_draw_in_another_space_is_never_passed() {
        // A label under a transform root may move anywhere without a new plan.
        let mut runs = cards(2);
        runs[1].item.space = Space {
            slot: 3,
            chain: Space::STATIC.chain,
        };
        assert_eq!(planned(&runs).0.len(), 4);
    }

    #[test]
    fn a_draw_into_another_pass_is_no_barrier() {
        let mut runs = cards(2);
        runs[1].item.key = BatchKey::pack(
            BatchFamily::GlyphRun,
            BatchTarget::Offscreen(0),
            Some(bg(1)),
        );
        runs[1].cover = Some(rect(0.0, 0.0, 400.0, 400.0));
        let (draws, of, _) = planned(&runs);
        assert_eq!(of[2], 0, "the second quad passes a draw of another pass");
        assert_eq!(draws.len(), 3);
    }

    #[test]
    fn a_run_joins_only_a_draw_whose_instances_it_follows() {
        // The second quad's instance is not next to the first's in the buffer.
        let mut runs = cards(2);
        runs[2].start = 7;
        let (draws, _, moved) = planned(&runs);
        assert_eq!(moved, 0);
        assert_eq!(draws.len(), 4, "and the second label must follow its quad");
    }

    #[test]
    fn a_clip_cuts_the_region_a_draw_touches() {
        // The label's glyphs reach into the next card, but its clip keeps them
        // on its own.
        let mut runs = cards(2);
        runs[1].cover = Some(rect(4.0, 4.0, 50.0, 40.0));
        assert_eq!(planned(&runs).0.len(), 4);
        runs[1].item.clip = Some(rect(0.0, 0.0, 100.0, 28.0));
        runs[3].item.clip = Some(rect(0.0, 30.0, 100.0, 28.0));
        let (draws, of, _) = planned(&runs);
        assert_eq!(of[2], 0, "the clipped label no longer reaches the quad");
        assert_eq!(draws.len(), 3, "labels under different clips stay apart");
    }

    #[test]
    fn program_runs_keep_their_place() {
        let program = |start: u32, y: f32| Run {
            item: BatchItem {
                key: BatchKey::pack(BatchFamily::Program, BatchTarget::Main, None).with_program(1),
                clip: None,
                space: Space::STATIC,
                mergeable: true,
            },
            start,
            count: 1,
            cover: Some(rect(0.0, y, 10.0, 10.0)),
        };
        let runs = [
            program(0, 0.0),
            run(
                BatchFamily::Quad,
                0,
                0,
                1,
                Some(rect(50.0, 0.0, 10.0, 10.0)),
            ),
            program(1, 20.0),
        ];
        assert_eq!(planned(&runs).0.len(), 3);
    }

    #[test]
    fn the_walk_back_stops_after_its_reach() {
        // A quad, then more disjoint images than the walk passes, then a quad.
        let mut runs = vec![run(
            BatchFamily::Quad,
            0,
            0,
            1,
            Some(rect(0.0, 0.0, 4.0, 4.0)),
        )];
        for i in 0..REACH as u32 {
            let x = 10.0 + i as f32 * 10.0;
            runs.push(run(
                BatchFamily::Image,
                2 + i,
                i,
                1,
                Some(rect(x, 0.0, 4.0, 4.0)),
            ));
        }
        runs.push(run(
            BatchFamily::Quad,
            0,
            1,
            1,
            Some(rect(0.0, 10.0, 4.0, 4.0)),
        ));
        let (draws, _, moved) = planned(&runs);
        assert_eq!(moved, 0);
        assert_eq!(draws.len(), REACH + 2);
        runs.remove(1);
        let (draws, _, moved) = planned(&runs);
        assert_eq!(moved, 1);
        assert_eq!(draws.len(), REACH);
    }
}
