//! The frame-local transient render-target pool and its lifetime planner (§16.4).
//!
//! Effects that cannot be drawn in place — group opacity, content blur, and every
//! later offscreen effect — need a render target for part of a frame and nothing
//! afterwards. Creating one texture per effect and destroying it at frame end is
//! the obvious implementation and the wrong one: it churns device allocations on
//! every frame, and a scene with twenty shadows holds twenty targets alive when
//! two would do.
//!
//! This module is the alternative. A frame *declares* the targets it wants as
//! **virtual** targets — an extent, a format, a usage, a sample count, and the
//! slots of the frame's pass timeline where the target is first written and last
//! read. Once every pass is recorded, [`TransientTargets::assign`] resolves the
//! virtuals onto **physical** textures:
//!
//! 1. **Compatibility.** A virtual can only land on a physical with the same
//!    [`TargetKey`]: same format, same usage, same sample count, same size class.
//!    Extents are bucketed by [`size_class`] rather than matched exactly, so a
//!    401×97 target and a 400×96 one share a physical instead of minting two.
//! 2. **Lifetime.** Two virtuals share a physical only when their live intervals
//!    are disjoint: the resident's last read must fall strictly before the
//!    newcomer's first write. Strictly, so a pass can never alias its own source
//!    onto its own target.
//! 3. **Persistence.** Physicals outlive the frame. A steady-state frame that
//!    re-declares the same shapes claims the same textures and allocates nothing;
//!    a physical that goes unclaimed for [`TRANSIENT_TARGET_IDLE_FRAMES`]
//!    consecutive frames is retired, so a one-off huge effect does not pin its
//!    memory forever.
//!
//! Because bucketing is a pure function of the requested extent, a target's
//! *physical* extent is known the moment it is declared — only the `TextureId`
//! and `BindGroupId` are resolved late. Callers therefore size their geometry and
//! uv sub-rects at record time: a virtual carries both its `used` extent (what the
//! frame actually draws into) and its physical extent (what the texture measures),
//! and the used region is always the top-left corner of the physical one.

use viso_gpu::{
    BindGroupDesc, BindGroupId, Binding, GpuBackend, SamplerId, TextureDesc, TextureFormat,
    TextureId,
};

/// Frames a physical target may go unclaimed before it is retired.
///
/// Long enough that a target used by an occasionally-visible effect (a hover
/// shadow, a menu that opens once a second) survives between appearances; short
/// enough that a transiently huge target does not pin its bytes indefinitely.
pub const TRANSIENT_TARGET_IDLE_FRAMES: u32 = 60;

/// The timeline slot of the frame's final surface pass, used as a virtual's last
/// read when the surface composites it. Resolved to the real slot index in
/// [`TransientTargets::assign`], which is the only place that knows how long the
/// timeline turned out to be.
pub const SURFACE_SLOT: u32 = u32::MAX;

/// How a transient target is used by the GPU, part of its [`TargetKey`].
///
/// A bitset rather than an enum: a target is typically both written as a color
/// attachment and sampled afterwards, and the two capabilities are independent.
/// Two targets alias only when their usage sets are identical, so a sampled-only
/// target can never land on a physical that was never created as a render target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TargetUsage(u8);

impl TargetUsage {
    /// The target is drawn into by a render pass.
    pub const RENDER_TARGET: Self = Self(1 << 0);
    /// The target is read by a later draw through a sampler.
    pub const SAMPLED: Self = Self(1 << 1);
    /// Written by a pass and sampled afterwards — every offscreen layer and blur
    /// scratch target Viso allocates today.
    pub const COLOR_ATTACHMENT: Self = Self(0b11);

    /// Whether this usage set includes being drawn into.
    #[inline]
    pub const fn is_render_target(self) -> bool {
        self.0 & Self::RENDER_TARGET.0 != 0
    }

    /// Whether this usage set includes being sampled.
    #[inline]
    pub const fn is_sampled(self) -> bool {
        self.0 & Self::SAMPLED.0 != 0
    }
}

/// Round `n` up to its size class: the bucket a transient target's extent is
/// keyed by, so near-identical extents share one physical texture.
///
/// The step is a sixteenth of the enclosing power of two (never below 16 px), so
/// the class is within ~6.25% of the request for large extents and never wastes
/// more than 15 px on a small one. Bucketing is what makes reuse work across
/// frames: a panel that resizes by a pixel while being dragged keeps claiming the
/// same texture instead of minting one per frame.
///
/// Examples: `10 → 16`, `40 → 48`, `80 → 80`, `200 → 208`, `1000 → 1024`,
/// `1080 → 1152`, `1920 → 1920`.
#[inline]
pub fn size_class(n: u32) -> u32 {
    let n = n.max(1);
    // A pathological extent has no enclosing power of two; it is its own class.
    let Some(pot) = n.checked_next_power_of_two() else {
        return n;
    };
    let step = (pot >> 4).max(16);
    n.div_ceil(step).saturating_mul(step)
}

/// The compatibility key two transient targets must share to alias onto one
/// physical texture: pixel format, usage set, sample count, and size class.
///
/// Every dimension is load-bearing. Format and sample count change the texture's
/// memory layout outright. Usage changes what the device is allowed to do with
/// it. Size class is what turns "reuse" from an exact-extent coincidence into the
/// common case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetKey {
    /// The texel format both the writer and the reader agree on.
    pub format: TextureFormat,
    /// The capabilities the texture was created with.
    pub usage: TargetUsage,
    /// Samples per texel. `1` for every target Viso allocates today; keyed so a
    /// multisampled target can never alias a single-sampled one.
    pub samples: u32,
    /// Width size class in physical pixels ([`size_class`]).
    pub width: u32,
    /// Height size class in physical pixels ([`size_class`]).
    pub height: u32,
}

impl TargetKey {
    /// Bytes one physical texture of this key occupies.
    #[inline]
    pub fn bytes(&self) -> usize {
        self.width as usize
            * self.height as usize
            * self.format.bytes_per_texel()
            * self.samples as usize
    }
}

/// A request for a transient target: the extent the frame actually draws into,
/// plus the compatibility dimensions. The extent is bucketed into a [`TargetKey`]
/// by [`TransientTargets::declare`]; the unbucketed request is kept as the
/// target's `used` extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetDesc {
    /// Width the frame draws into, before bucketing.
    pub width: u32,
    /// Height the frame draws into, before bucketing.
    pub height: u32,
    /// Texel format.
    pub format: TextureFormat,
    /// Usage set.
    pub usage: TargetUsage,
    /// Samples per texel (`1` for a plain target).
    pub samples: u32,
    /// Debug label for the physical texture, if one has to be created.
    pub label: &'static str,
}

/// A handle to a target declared this frame. Opaque and `Copy`; valid until the
/// next [`TransientTargets::begin_frame`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetId(u32);

impl TargetId {
    /// This target's declaration index within the frame, dense from zero.
    ///
    /// Exposed so the render graph can key its per-target side tables by plain
    /// index instead of hashing handles; it is not a device resource id, and it
    /// is only meaningful for the frame that declared it.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// A target the frame declared, before (and after) it is bound to a physical.
#[derive(Debug, Clone, Copy)]
struct VirtualTarget {
    /// Compatibility key (bucketed extent).
    key: TargetKey,
    /// The extent the frame draws into, always the physical texture's top-left
    /// corner. Never larger than the key's size class.
    used: [u32; 2],
    /// Timeline slot of the pass that writes this target.
    first_write: u32,
    /// Timeline slot of the last pass that reads it, or [`SURFACE_SLOT`] when the
    /// surface pass is the reader.
    last_read: u32,
    /// The physical texture assigned in [`TransientTargets::assign`].
    texture: Option<TextureId>,
    /// The sampling bind group of `texture`.
    bind_group: Option<BindGroupId>,
}

/// A pooled device texture that virtual targets are assigned onto. Persists
/// across frames; retired after [`TRANSIENT_TARGET_IDLE_FRAMES`] idle frames.
struct PhysicalTarget {
    /// The key every virtual assigned to it shares.
    key: TargetKey,
    /// The device texture.
    texture: TextureId,
    /// Its sampling bind group (texture + the renderer's shared clamp sampler).
    bind_group: BindGroupId,
    /// The first timeline slot at which this physical is free again this frame:
    /// one past the resident virtual's last read. `0` means untouched so far.
    free_at: u32,
    /// Whether any virtual claimed it this frame.
    claimed: bool,
    /// Consecutive frames it went unclaimed.
    idle_frames: u32,
}

/// Per-frame accounting for the transient pool, surfaced through `FrameStats`
/// (§30) so the §31 budget gate can be written against real numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TransientStats {
    /// The largest number of bytes of transient target memory live at once this
    /// frame: the peak of the lifetime sweep, not the sum over passes. This is
    /// the number an occupancy budget has to bound.
    pub peak_bytes: usize,
    /// Bytes held by the whole pool after this frame's assignment, including
    /// physicals no virtual claimed (they are still resident until retired).
    pub pool_bytes: usize,
    /// Physical textures the pool holds after this frame.
    pub targets: usize,
    /// Physical textures created during this frame's assignment. Zero at steady
    /// state — the exit criterion for "never one texture per effect".
    pub allocations: u32,
}

/// The frame-local transient render-target pool (§16.4): declare virtual targets
/// while recording passes, [`assign`](Self::assign) them onto pooled physicals
/// once the frame's pass timeline is known, then read back concrete handles.
pub struct TransientTargets {
    /// Pooled device textures, persistent across frames.
    physicals: Vec<PhysicalTarget>,
    /// This frame's declarations, in increasing `first_write` order.
    virtuals: Vec<VirtualTarget>,
    /// Scratch for the peak-bytes lifetime sweep, reused each frame.
    deltas: Vec<i64>,
    /// This frame's accounting, filled by `assign`.
    stats: TransientStats,
}

impl Default for TransientTargets {
    fn default() -> Self {
        Self::new()
    }
}

impl TransientTargets {
    /// An empty pool.
    pub fn new() -> Self {
        TransientTargets {
            physicals: Vec::with_capacity(8),
            virtuals: Vec::with_capacity(8),
            deltas: Vec::with_capacity(16),
            stats: TransientStats::default(),
        }
    }

    /// Drop the previous frame's declarations. Physical textures are kept — that
    /// is the whole point of the pool — and so is every buffer's capacity, so a
    /// steady frame allocates nothing here either.
    pub fn begin_frame(&mut self) {
        self.virtuals.clear();
        self.stats = TransientStats::default();
    }

    /// Declare a target written by the pass at timeline slot `first_write`.
    ///
    /// Returns a handle whose physical extent is already final (bucketing is
    /// pure), so the caller can size geometry and uv sub-rects immediately. The
    /// target's last read defaults to its own write — call
    /// [`read_at`](Self::read_at) for every pass that samples it.
    ///
    /// # Panics
    /// Panics in debug builds if declarations are not in non-decreasing
    /// `first_write` order, which the interval-reuse sweep relies on.
    pub fn declare(&mut self, desc: TargetDesc, first_write: u32) -> TargetId {
        debug_assert!(
            self.virtuals
                .last()
                .is_none_or(|v| v.first_write <= first_write),
            "transient targets must be declared in pass-timeline order"
        );
        let key = TargetKey {
            format: desc.format,
            usage: desc.usage,
            samples: desc.samples.max(1),
            width: size_class(desc.width),
            height: size_class(desc.height),
        };
        let id = TargetId(self.virtuals.len() as u32);
        self.virtuals.push(VirtualTarget {
            key,
            used: [desc.width.max(1), desc.height.max(1)],
            first_write,
            last_read: first_write,
            texture: None,
            bind_group: None,
        });
        // The label only matters when `assign` has to mint a texture; keeping it
        // on the virtual would grow the hot struct for a cold-path string, so the
        // pool labels by role instead (see `assign`).
        let _ = desc.label;
        id
    }

    /// Extend `id`'s lifetime to cover a read by the pass at timeline slot `slot`
    /// (or by the surface pass, [`SURFACE_SLOT`]).
    pub fn read_at(&mut self, id: TargetId, slot: u32) {
        let v = &mut self.virtuals[id.0 as usize];
        v.last_read = v.last_read.max(slot);
    }

    /// Bind every declared virtual onto a physical texture, creating physicals
    /// only where lifetime and compatibility leave no reusable one, and retire
    /// physicals that have been idle too long.
    ///
    /// `timeline_len` is the number of offscreen/blur passes this frame, so the
    /// surface pass sits at slot `timeline_len` — that is what [`SURFACE_SLOT`]
    /// resolves to. `sampler` is the shared clamp sampler the pool pairs with each
    /// texture to make it samplable.
    pub fn assign<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        sampler: SamplerId,
        timeline_len: u32,
    ) {
        let surface = timeline_len;
        for p in &mut self.physicals {
            p.free_at = 0;
            p.claimed = false;
        }

        let mut allocations = 0u32;
        for i in 0..self.virtuals.len() {
            let key = self.virtuals[i].key;
            let first_write = self.virtuals[i].first_write;
            let last_read = self.resolved_last_read(i, surface);

            // Reuse the first compatible physical whose resident died strictly
            // before this target is written; `free_at` is one past that death, so
            // `free_at <= first_write` is exactly "no overlap".
            let slot = match self
                .physicals
                .iter()
                .position(|p| p.key == key && p.free_at <= first_write)
            {
                Some(slot) => slot,
                None => {
                    let texture = backend.create_texture(&TextureDesc {
                        width: key.width,
                        height: key.height,
                        format: key.format,
                        render_target: key.usage.is_render_target(),
                        label: "transient-target",
                    });
                    let bind_group = backend.create_bind_group(&BindGroupDesc {
                        label: "transient-target",
                        bindings: vec![Binding::Texture(texture), Binding::Sampler(sampler)],
                    });
                    allocations += 1;
                    self.physicals.push(PhysicalTarget {
                        key,
                        texture,
                        bind_group,
                        free_at: 0,
                        claimed: false,
                        idle_frames: 0,
                    });
                    self.physicals.len() - 1
                }
            };

            let p = &mut self.physicals[slot];
            p.free_at = last_read.saturating_add(1);
            p.claimed = true;
            p.idle_frames = 0;
            let (texture, bind_group) = (p.texture, p.bind_group);
            let v = &mut self.virtuals[i];
            v.texture = Some(texture);
            v.bind_group = Some(bind_group);
        }

        self.stats.peak_bytes = self.peak_bytes(surface);
        self.stats.allocations = allocations;
        self.retire(backend);
        self.stats.pool_bytes = self.physicals.iter().map(|p| p.key.bytes()).sum();
        self.stats.targets = self.physicals.len();
    }

    /// The device texture assigned to `id`.
    ///
    /// # Panics
    /// Panics if called before [`assign`](Self::assign).
    pub fn texture(&self, id: TargetId) -> TextureId {
        self.virtuals[id.0 as usize]
            .texture
            .expect("transient targets assigned before their textures are read")
    }

    /// The sampling bind group of the texture assigned to `id`.
    ///
    /// # Panics
    /// Panics if called before [`assign`](Self::assign).
    pub fn bind_group(&self, id: TargetId) -> BindGroupId {
        self.virtuals[id.0 as usize]
            .bind_group
            .expect("transient targets assigned before their bind groups are read")
    }

    /// The physical extent of `id`'s texture — its size class, which is what a
    /// pass viewport and a sampler's uv normalization must use. Known as soon as
    /// the target is declared.
    pub fn phys_extent(&self, id: TargetId) -> [u32; 2] {
        let v = &self.virtuals[id.0 as usize];
        [v.key.width, v.key.height]
    }

    /// The extent the frame draws into, anchored at the physical texture's
    /// top-left. Always `<=` [`phys_extent`](Self::phys_extent).
    pub fn used_extent(&self, id: TargetId) -> [u32; 2] {
        self.virtuals[id.0 as usize].used
    }

    /// This frame's accounting.
    pub fn stats(&self) -> TransientStats {
        self.stats
    }

    /// Virtual `i`'s last read as a concrete timeline slot.
    fn resolved_last_read(&self, i: usize, surface: u32) -> u32 {
        let v = &self.virtuals[i];
        if v.last_read == SURFACE_SLOT {
            surface
        } else {
            v.last_read
        }
    }

    /// Peak live transient bytes across the frame's timeline: a difference sweep
    /// over `[first_write, last_read]` intervals, prefix-summed for the maximum.
    ///
    /// This counts each *virtual* once, so two virtuals aliasing one physical add
    /// up only when their lifetimes actually overlap — which, by construction,
    /// they never do. The result is therefore the honest high-water mark of
    /// simultaneously-needed target memory, the quantity a budget bounds.
    fn peak_bytes(&mut self, surface: u32) -> usize {
        let slots = surface as usize + 2;
        self.deltas.clear();
        self.deltas.resize(slots, 0);
        for i in 0..self.virtuals.len() {
            let bytes = self.virtuals[i].key.bytes() as i64;
            let first = (self.virtuals[i].first_write as usize).min(slots - 1);
            let last = (self.resolved_last_read(i, surface) as usize).min(slots - 2);
            self.deltas[first] += bytes;
            self.deltas[last + 1] -= bytes;
        }
        let mut live = 0i64;
        let mut peak = 0i64;
        for d in &self.deltas {
            live += d;
            peak = peak.max(live);
        }
        peak as usize
    }

    /// Retire physicals that have gone unclaimed for too many consecutive frames.
    ///
    /// Safe to do here: a retired physical is by definition not referenced by any
    /// of this frame's virtuals, and the backend defers destruction until the GPU
    /// has finished every frame that could still be reading it.
    fn retire<B: GpuBackend>(&mut self, backend: &mut B) {
        let mut i = 0;
        while i < self.physicals.len() {
            if self.physicals[i].claimed {
                i += 1;
                continue;
            }
            self.physicals[i].idle_frames += 1;
            if self.physicals[i].idle_frames > TRANSIENT_TARGET_IDLE_FRAMES {
                let p = self.physicals.swap_remove(i);
                backend.destroy_bind_group(p.bind_group);
                backend.destroy_texture(p.texture);
            } else {
                i += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_gpu::{HeadlessRaster, SamplerDesc};

    /// Size classes are a pure, monotonic rounding-up of the request with a
    /// bounded relative slack, so the class is a stable reuse key rather than a
    /// per-frame accident.
    #[test]
    fn size_classes_round_up_with_bounded_slack() {
        assert_eq!(size_class(0), 16, "an empty extent still needs a texel");
        assert_eq!(size_class(1), 16);
        assert_eq!(size_class(10), 16);
        assert_eq!(size_class(16), 16, "an exact class is itself");
        assert_eq!(size_class(40), 48);
        assert_eq!(size_class(80), 80);
        assert_eq!(size_class(1000), 1024);
        assert_eq!(size_class(1080), 1152);
        assert_eq!(size_class(1920), 1920);

        // Monotonic, never shrinking, and never wasting more than one step.
        let mut prev = 0;
        for n in 1..4096u32 {
            let c = size_class(n);
            assert!(c >= n, "{n} classes up to {c}");
            assert!(c >= prev, "class is monotonic at {n}");
            prev = c;
            let step = (n.next_power_of_two() >> 4).max(16);
            assert!(c - n < step, "{n} wastes less than one step of {step}");
        }
    }

    /// Usage is a set, and the two capabilities are independent: a
    /// render-target-only usage is not sampled, and vice versa.
    #[test]
    fn usage_sets_are_distinct() {
        assert!(TargetUsage::COLOR_ATTACHMENT.is_render_target());
        assert!(TargetUsage::COLOR_ATTACHMENT.is_sampled());
        assert!(TargetUsage::RENDER_TARGET.is_render_target());
        assert!(!TargetUsage::RENDER_TARGET.is_sampled());
        assert!(TargetUsage::SAMPLED.is_sampled());
        assert!(!TargetUsage::SAMPLED.is_render_target());
        assert_ne!(TargetUsage::RENDER_TARGET, TargetUsage::COLOR_ATTACHMENT);
    }

    /// Key compatibility rejects a mismatch on every dimension, so a virtual can
    /// never land on a physically incompatible texture.
    #[test]
    fn keys_differ_on_every_dimension() {
        let base = TargetKey {
            format: TextureFormat::Bgra8Unorm,
            usage: TargetUsage::COLOR_ATTACHMENT,
            samples: 1,
            width: 64,
            height: 64,
        };
        assert_ne!(
            base,
            TargetKey {
                format: TextureFormat::Rgba8Unorm,
                ..base
            }
        );
        assert_ne!(
            base,
            TargetKey {
                usage: TargetUsage::SAMPLED,
                ..base
            }
        );
        assert_ne!(base, TargetKey { samples: 4, ..base });
        assert_ne!(base, TargetKey { width: 80, ..base });
        assert_ne!(base, TargetKey { height: 80, ..base });
        assert_eq!(base.bytes(), 64 * 64 * 4);
    }

    /// A pool plus the sampler every target is paired with, as the renderer holds
    /// them.
    fn pool() -> (HeadlessRaster, SamplerId, TransientTargets) {
        let mut gpu = HeadlessRaster::new();
        let sampler = gpu.create_sampler(&SamplerDesc::LINEAR_CLAMP);
        (gpu, sampler, TransientTargets::new())
    }

    fn desc(width: u32, height: u32) -> TargetDesc {
        TargetDesc {
            width,
            height,
            format: TextureFormat::Bgra8Unorm,
            usage: TargetUsage::COLOR_ATTACHMENT,
            samples: 1,
            label: "test",
        }
    }

    /// Two same-key targets whose lifetimes do not overlap share one physical
    /// texture — the whole point of the planner (§16.4). A ping-pong chain of four
    /// passes needs two textures, not four, and the peak is two live at once.
    #[test]
    fn disjoint_lifetimes_alias_one_texture() {
        let (mut gpu, sampler, mut pool) = pool();
        // a → b → c → d, each read only by its successor: a dies at slot 1, so c
        // (written at 2) lands back on a's texture, and d lands back on b's.
        let ids: Vec<TargetId> = (0..4)
            .map(|slot| pool.declare(desc(40, 40), slot))
            .collect();
        for (i, id) in ids.iter().enumerate().take(3) {
            pool.read_at(*id, i as u32 + 1);
        }
        pool.read_at(ids[3], SURFACE_SLOT);
        pool.assign(&mut gpu, sampler, 4);

        let stats = pool.stats();
        assert_eq!(stats.targets, 2, "four passes ping-pong over two textures");
        assert_eq!(stats.allocations, 2);
        assert_eq!(pool.texture(ids[0]), pool.texture(ids[2]));
        assert_eq!(pool.texture(ids[1]), pool.texture(ids[3]));
        assert_ne!(pool.texture(ids[0]), pool.texture(ids[1]));

        // Peak counts each virtual once over its own live interval: at most two of
        // the four are live simultaneously.
        assert_eq!(stats.peak_bytes, 2 * 48 * 48 * 4);
        assert_eq!(stats.pool_bytes, 2 * 48 * 48 * 4);
    }

    /// A target still alive when the next one is written cannot alias it — a blur
    /// pass must never read and write the same texture.
    #[test]
    fn overlapping_lifetimes_never_alias() {
        let (mut gpu, sampler, mut pool) = pool();
        let a = pool.declare(desc(40, 40), 0);
        let b = pool.declare(desc(40, 40), 1);
        // `a` is read by the surface pass, so it outlives every intermediate.
        pool.read_at(a, SURFACE_SLOT);
        pool.read_at(b, SURFACE_SLOT);
        pool.assign(&mut gpu, sampler, 2);

        assert_ne!(pool.texture(a), pool.texture(b));
        assert_eq!(pool.stats().targets, 2);
        assert_eq!(pool.stats().peak_bytes, 2 * 48 * 48 * 4);
    }

    /// Incompatible keys never share, even with disjoint lifetimes: a different
    /// size class or format is a different physical texture.
    #[test]
    fn incompatible_keys_never_share() {
        let (mut gpu, sampler, mut pool) = pool();
        let small = pool.declare(desc(40, 40), 0);
        let large = pool.declare(desc(200, 40), 1);
        let other_format = pool.declare(
            TargetDesc {
                format: TextureFormat::Rgba8Unorm,
                ..desc(40, 40)
            },
            2,
        );
        pool.read_at(small, 1);
        pool.read_at(large, 2);
        pool.read_at(other_format, SURFACE_SLOT);
        pool.assign(&mut gpu, sampler, 3);

        assert_eq!(pool.stats().targets, 3);
        assert_eq!(pool.phys_extent(small), [48, 48]);
        assert_eq!(pool.used_extent(small), [40, 40]);
        assert_eq!(pool.phys_extent(large), [208, 48]);
        assert_ne!(pool.texture(small), pool.texture(other_format));
    }

    /// A steady frame allocates nothing: the second identical frame claims the same
    /// physicals, so no `create_texture` happens and the pool neither grows nor
    /// shrinks. That is the "never one texture per effect" contract.
    #[test]
    fn steady_frames_allocate_nothing() {
        let (mut gpu, sampler, mut pool) = pool();
        for frame in 0..4 {
            pool.begin_frame();
            let a = pool.declare(desc(40, 40), 0);
            let b = pool.declare(desc(40, 40), 1);
            pool.read_at(a, 1);
            pool.read_at(b, SURFACE_SLOT);
            pool.assign(&mut gpu, sampler, 2);
            let stats = pool.stats();
            assert_eq!(stats.targets, 2);
            assert_eq!(
                stats.allocations,
                if frame == 0 { 2 } else { 0 },
                "only the first frame mints textures"
            );
        }
    }

    /// A physical that stops being claimed is held for a bounded number of frames
    /// (so a brief topology change does not thrash) and then destroyed, instead of
    /// leaking for the process's lifetime.
    #[test]
    fn unclaimed_physicals_retire_after_an_idle_window() {
        let (mut gpu, sampler, mut pool) = pool();
        pool.begin_frame();
        let a = pool.declare(desc(40, 40), 0);
        let b = pool.declare(desc(40, 40), 1);
        pool.read_at(a, 1);
        pool.read_at(b, SURFACE_SLOT);
        pool.assign(&mut gpu, sampler, 2);
        assert_eq!(pool.stats().targets, 2);

        // From here on only one target is needed per frame.
        for _ in 0..TRANSIENT_TARGET_IDLE_FRAMES {
            pool.begin_frame();
            let only = pool.declare(desc(40, 40), 0);
            pool.read_at(only, SURFACE_SLOT);
            pool.assign(&mut gpu, sampler, 1);
            assert_eq!(
                pool.stats().targets,
                2,
                "the spare is kept through the idle window"
            );
        }
        pool.begin_frame();
        let only = pool.declare(desc(40, 40), 0);
        pool.read_at(only, SURFACE_SLOT);
        pool.assign(&mut gpu, sampler, 1);
        assert_eq!(pool.stats().targets, 1, "then the spare is released");
        assert_eq!(pool.stats().pool_bytes, 48 * 48 * 4);
    }
}
