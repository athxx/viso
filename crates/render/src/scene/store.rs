//! Per-type compact stores for the retained scene (§8, §8.5).
//!
//! Each primitive kind lives in its own dense store — solid quads, images,
//! glyph runs, vector paths, meshes — plus the identity-separated
//! transform/brush/clip stores a later stage bumps independently (§8.5). The
//! stores are SoA/AoS `Vec`s of plain data, never `Vec<Box<dyn …>>`: the whole
//! point is that lowering a frame walks contiguous memory, not a pointer chase.
//!
//! Storage **persists across frames** (§8.4, §9.1). `begin_frame` resets a
//! per-store *cursor* to zero without touching the entries; the ingest walk
//! then re-visits each slot in paint order (`ingest_*`), diffing the incoming
//! primitive against the retained entry at that positional slot. A slot is
//! positionally assigned — the Nth quad in the primitive stream is entry N of
//! the quad store, frame after frame, stable because the sole producer
//! re-emits the whole tree in stable pre-order — so an unchanged scene mutates
//! nothing and re-uses last frame's allocations, keeping the counting-allocator
//! bench flat (§28). Growth past the retained length appends (cold); a shrink
//! is trimmed by `finish_frame` at the cursor.
//!
//! Each `ingest_*` returns which revision planes the field-wise comparison
//! moved ([`DirtyPlanes`]) so the caller ([`super::ingest`]) bumps only the
//! planes a change actually touched (§8.4); an unchanged slot bumps nothing.
//! The store owns the comparison because it owns the entry layout; the field →
//! plane mapping is the primitive semantics [`super::ingest`] documents.
//!
//! Re-lowering a frame reads `get(id)`/`glyphs(run)` back in paint order, which
//! is byte-identical to what an immediate walk of the same primitives would
//! produce (the diff either left the retained value in place, when equal, or
//! overwrote it with the freshly lowered one, when changed).

use crate::primitive::{
    AnalyticCapsuleInstance, AnalyticEllipseInstance, AnalyticLineInstance, AnalyticRRectInstance,
    GlyphInstance, GlyphInstanceData, GradientInstance, ImageInstance, MeshVertex, Path,
    PathGeometry, QuadInstance, Rgba, ShadowInstance,
};

use super::ids::{
    BrushId, ClipChainId, ClipId, GeometryId, ImageId, MeshId, PathId, PrimitiveId, TransformId,
};

/// A retained solid/bordered quad: the resolved GPU instance plus the identity
/// handles it separates into (§8.5). The instance is held whole; the diff
/// splits its fields across the transform/geometry/paint planes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuadEntry {
    /// The lowered instance, in world space (origin not yet subtracted — the
    /// per-emit origin lives in the paint-order record so one geometry can be
    /// re-lowered under different layer origins).
    pub instance: QuadInstance,
    /// Transform identity — bumped alone by a pure move (§8.5).
    pub transform: TransformId,
    /// Brush identity — bumped alone by a recolor (§8.5).
    pub brush: BrushId,
}

/// A retained image draw: its lowered instance, the texture it samples, and the
/// sampler descriptor selecting filter/address (both resource-plane data — a
/// change of either rebinds the draw's bind group without touching geometry).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageEntry {
    /// The lowered instance in world space.
    pub instance: ImageInstance,
    /// The sampled texture (resource plane).
    pub texture: viso_gpu::TextureId,
    /// How the texture is sampled (resource plane). The renderer interns this to
    /// a shared `SamplerId`; it is not GPU instance data.
    pub sampler: viso_gpu::SamplerDesc,
}

/// A retained glyph run: a contiguous range of glyph instances in the run
/// store's shared instance vector, plus the atlas they sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphRunEntry {
    /// First glyph instance index into [`GlyphRunStore::instances`].
    pub start: u32,
    /// Number of glyph instances in this run.
    pub count: u32,
    /// The A8 coverage atlas the run samples (resource plane).
    pub atlas: viso_gpu::TextureId,
}

/// Which revision planes an ingested primitive moved at its slot (§8.4).
/// Returned by every `ingest_*`; the caller ([`super::ingest`]) bumps exactly
/// the planes flagged here and no others. An all-`false` result means the slot
/// was byte-identical — zero store mutation, zero bump.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DirtyPlanes {
    /// A shape/size field moved: quad size/radius/border width, image/glyph uv,
    /// path commands, mesh positions/indices.
    pub geometry: bool,
    /// A paint field moved: fill/border/tint/vertex color.
    pub paint: bool,
    /// A translation field moved: the world-space position.
    pub transform: bool,
    /// A bound resource changed: image/glyph atlas texture.
    pub resource: bool,
    /// The slot did not exist last frame — a cold structural append. The caller
    /// treats it as a whole-primitive add (bumps geometry + the structural
    /// generation), so the individual field flags stay `false` here.
    pub appended: bool,
}

impl DirtyPlanes {
    /// A fresh cold-append classification.
    const APPENDED: Self = Self {
        geometry: false,
        paint: false,
        transform: false,
        resource: false,
        appended: true,
    };

    /// Whether any plane moved (including a cold append).
    pub fn any(&self) -> bool {
        self.geometry || self.paint || self.transform || self.resource || self.appended
    }
}

/// Dense store of solid/bordered quads (§8). AoS: one `QuadEntry` per quad, in
/// paint order, addressed by [`PrimitiveId`] via the paint-order record.
/// Entries persist across frames; a cursor walks them.
#[derive(Debug, Default)]
pub struct SolidQuadStore {
    entries: Vec<QuadEntry>,
    cursor: usize,
}

impl SolidQuadStore {
    /// Reset the cursor for a new frame, keeping entries to diff against.
    pub fn begin_frame(&mut self) {
        self.cursor = 0;
    }

    /// Trim entries the frame's walk did not revisit (the scene shrank).
    /// Returns whether a trim happened.
    pub fn finish_frame(&mut self) -> bool {
        if self.cursor < self.entries.len() {
            self.entries.truncate(self.cursor);
            true
        } else {
            false
        }
    }

    /// Diff `instance` against the retained quad at the cursor and advance. If
    /// the slot is new (cold growth) the quad is appended. Returns the slot and
    /// the planes the field-wise comparison moved: `rect_pos` → transform,
    /// `rect_size`/`radius`/`border_width` → geometry, `color`/`border_color` →
    /// paint. An unchanged slot mutates nothing.
    pub fn ingest(&mut self, instance: QuadInstance) -> (GeometryId, DirtyPlanes) {
        let index = self.cursor;
        let dirty = if index < self.entries.len() {
            let prev = &self.entries[index].instance;
            let dirty = DirtyPlanes {
                transform: prev.rect_pos != instance.rect_pos,
                geometry: prev.rect_size != instance.rect_size
                    || prev.radius != instance.radius
                    || prev.border_width != instance.border_width,
                paint: prev.color != instance.color || prev.border_color != instance.border_color,
                resource: false,
                appended: false,
            };
            if dirty.any() {
                self.entries[index].instance = instance;
            }
            dirty
        } else {
            let idx = index as u32;
            self.entries.push(QuadEntry {
                instance,
                transform: TransformId::new(idx),
                brush: BrushId::new(idx),
            });
            DirtyPlanes::APPENDED
        };
        self.cursor += 1;
        (GeometryId::new(index as u32), dirty)
    }

    /// The entry at `id`, or `None` if the slot is out of range.
    pub fn get(&self, id: GeometryId) -> Option<&QuadEntry> {
        self.entries.get(id.index() as usize)
    }

    /// Number of live entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store holds no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A retained analytic rounded rectangle: the resolved GPU instance plus the
/// identity handles it separates into (§8.5), mirroring [`QuadEntry`] but for
/// the per-corner-radius family.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnalyticRRectEntry {
    /// The lowered instance, in world space (origin not yet subtracted).
    pub instance: AnalyticRRectInstance,
    /// Transform identity — bumped alone by a pure move (§8.5).
    pub transform: TransformId,
    /// Brush identity — bumped alone by a recolor (§8.5).
    pub brush: BrushId,
}

/// Dense store of analytic rounded rectangles (§8). Same AoS/cursor shape as
/// [`SolidQuadStore`]; the diff splits `rect_pos` → transform,
/// `rect_size`/`radius`/`border_width` → geometry, `color`/`border_color` →
/// paint.
#[derive(Debug, Default)]
pub struct AnalyticRRectStore {
    entries: Vec<AnalyticRRectEntry>,
    cursor: usize,
}

impl AnalyticRRectStore {
    /// Reset the cursor for a new frame, keeping entries to diff against.
    pub fn begin_frame(&mut self) {
        self.cursor = 0;
    }

    /// Trim entries the frame's walk did not revisit. Returns whether a trim
    /// happened.
    pub fn finish_frame(&mut self) -> bool {
        if self.cursor < self.entries.len() {
            self.entries.truncate(self.cursor);
            true
        } else {
            false
        }
    }

    /// Diff `instance` against the retained rrect at the cursor and advance,
    /// appending on cold growth. Returns the slot and the planes the field-wise
    /// comparison moved.
    pub fn ingest(&mut self, instance: AnalyticRRectInstance) -> (GeometryId, DirtyPlanes) {
        let index = self.cursor;
        let dirty = if index < self.entries.len() {
            let prev = &self.entries[index].instance;
            let dirty = DirtyPlanes {
                transform: prev.rect_pos != instance.rect_pos,
                geometry: prev.rect_size != instance.rect_size
                    || prev.radius != instance.radius
                    || prev.border_width != instance.border_width,
                paint: prev.color != instance.color || prev.border_color != instance.border_color,
                resource: false,
                appended: false,
            };
            if dirty.any() {
                self.entries[index].instance = instance;
            }
            dirty
        } else {
            let idx = index as u32;
            self.entries.push(AnalyticRRectEntry {
                instance,
                transform: TransformId::new(idx),
                brush: BrushId::new(idx),
            });
            DirtyPlanes::APPENDED
        };
        self.cursor += 1;
        (GeometryId::new(index as u32), dirty)
    }

    /// The entry at `id`, or `None` if the slot is out of range.
    pub fn get(&self, id: GeometryId) -> Option<&AnalyticRRectEntry> {
        self.entries.get(id.index() as usize)
    }

    /// Number of live entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store holds no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A retained analytic ellipse: the resolved GPU instance plus its identity
/// handles (§8.5). Like [`AnalyticRRectEntry`] without a per-corner radius —
/// the ellipse radii are derived from `rect_size` in the shader.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnalyticEllipseEntry {
    /// The lowered instance, in world space (origin not yet subtracted).
    pub instance: AnalyticEllipseInstance,
    /// Transform identity — bumped alone by a pure move (§8.5).
    pub transform: TransformId,
    /// Brush identity — bumped alone by a recolor (§8.5).
    pub brush: BrushId,
}

/// Dense store of analytic ellipses (§8). Same AoS/cursor shape as
/// [`SolidQuadStore`]; the diff splits `rect_pos` → transform,
/// `rect_size`/`border_width` → geometry, `color`/`border_color` → paint.
#[derive(Debug, Default)]
pub struct AnalyticEllipseStore {
    entries: Vec<AnalyticEllipseEntry>,
    cursor: usize,
}

impl AnalyticEllipseStore {
    /// Reset the cursor for a new frame, keeping entries to diff against.
    pub fn begin_frame(&mut self) {
        self.cursor = 0;
    }

    /// Trim entries the frame's walk did not revisit. Returns whether a trim
    /// happened.
    pub fn finish_frame(&mut self) -> bool {
        if self.cursor < self.entries.len() {
            self.entries.truncate(self.cursor);
            true
        } else {
            false
        }
    }

    /// Diff `instance` against the retained ellipse at the cursor and advance,
    /// appending on cold growth. Returns the slot and the moved planes.
    pub fn ingest(&mut self, instance: AnalyticEllipseInstance) -> (GeometryId, DirtyPlanes) {
        let index = self.cursor;
        let dirty = if index < self.entries.len() {
            let prev = &self.entries[index].instance;
            let dirty = DirtyPlanes {
                transform: prev.rect_pos != instance.rect_pos,
                geometry: prev.rect_size != instance.rect_size
                    || prev.border_width != instance.border_width,
                paint: prev.color != instance.color || prev.border_color != instance.border_color,
                resource: false,
                appended: false,
            };
            if dirty.any() {
                self.entries[index].instance = instance;
            }
            dirty
        } else {
            let idx = index as u32;
            self.entries.push(AnalyticEllipseEntry {
                instance,
                transform: TransformId::new(idx),
                brush: BrushId::new(idx),
            });
            DirtyPlanes::APPENDED
        };
        self.cursor += 1;
        (GeometryId::new(index as u32), dirty)
    }

    /// The entry at `id`, or `None` if the slot is out of range.
    pub fn get(&self, id: GeometryId) -> Option<&AnalyticEllipseEntry> {
        self.entries.get(id.index() as usize)
    }

    /// Number of live entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store holds no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A retained analytic capsule: the resolved GPU instance plus its identity
/// handles (§8.5). Byte-identical to [`AnalyticEllipseEntry`] — the corner
/// radius is derived from `rect_size` in the shader.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnalyticCapsuleEntry {
    /// The lowered instance, in world space (origin not yet subtracted).
    pub instance: AnalyticCapsuleInstance,
    /// Transform identity — bumped alone by a pure move (§8.5).
    pub transform: TransformId,
    /// Brush identity — bumped alone by a recolor (§8.5).
    pub brush: BrushId,
}

/// Dense store of analytic capsules (§8). Same AoS/cursor shape as
/// [`AnalyticEllipseStore`]; the diff splits `rect_pos` → transform,
/// `rect_size`/`border_width` → geometry, `color`/`border_color` → paint.
#[derive(Debug, Default)]
pub struct AnalyticCapsuleStore {
    entries: Vec<AnalyticCapsuleEntry>,
    cursor: usize,
}

impl AnalyticCapsuleStore {
    /// Reset the cursor for a new frame, keeping entries to diff against.
    pub fn begin_frame(&mut self) {
        self.cursor = 0;
    }

    /// Trim entries the frame's walk did not revisit. Returns whether a trim
    /// happened.
    pub fn finish_frame(&mut self) -> bool {
        if self.cursor < self.entries.len() {
            self.entries.truncate(self.cursor);
            true
        } else {
            false
        }
    }

    /// Diff `instance` against the retained capsule at the cursor and advance,
    /// appending on cold growth. Returns the slot and the moved planes.
    pub fn ingest(&mut self, instance: AnalyticCapsuleInstance) -> (GeometryId, DirtyPlanes) {
        let index = self.cursor;
        let dirty = if index < self.entries.len() {
            let prev = &self.entries[index].instance;
            let dirty = DirtyPlanes {
                transform: prev.rect_pos != instance.rect_pos,
                geometry: prev.rect_size != instance.rect_size
                    || prev.border_width != instance.border_width,
                paint: prev.color != instance.color || prev.border_color != instance.border_color,
                resource: false,
                appended: false,
            };
            if dirty.any() {
                self.entries[index].instance = instance;
            }
            dirty
        } else {
            let idx = index as u32;
            self.entries.push(AnalyticCapsuleEntry {
                instance,
                transform: TransformId::new(idx),
                brush: BrushId::new(idx),
            });
            DirtyPlanes::APPENDED
        };
        self.cursor += 1;
        (GeometryId::new(index as u32), dirty)
    }

    /// The entry at `id`, or `None` if the slot is out of range.
    pub fn get(&self, id: GeometryId) -> Option<&AnalyticCapsuleEntry> {
        self.entries.get(id.index() as usize)
    }

    /// Number of live entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store holds no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A retained analytic line: the resolved GPU instance plus its identity handles
/// (§8.5).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnalyticLineEntry {
    /// The lowered instance, in world space (origin not yet subtracted).
    pub instance: AnalyticLineInstance,
    /// Transform identity — bumped alone by a pure move (§8.5).
    pub transform: TransformId,
    /// Brush identity — bumped alone by a recolor (§8.5).
    pub brush: BrushId,
}

/// Dense store of analytic lines (§8). Same AoS/cursor shape as the other
/// analytic stores; the diff splits `p0`/`p1` → transform,
/// `width`/`cap`/`join`/`miter_limit`/`border_width` → geometry,
/// `color`/`border_color` → paint.
#[derive(Debug, Default)]
pub struct AnalyticLineStore {
    entries: Vec<AnalyticLineEntry>,
    cursor: usize,
}

impl AnalyticLineStore {
    /// Reset the cursor for a new frame, keeping entries to diff against.
    pub fn begin_frame(&mut self) {
        self.cursor = 0;
    }

    /// Trim entries the frame's walk did not revisit. Returns whether a trim
    /// happened.
    pub fn finish_frame(&mut self) -> bool {
        if self.cursor < self.entries.len() {
            self.entries.truncate(self.cursor);
            true
        } else {
            false
        }
    }

    /// Diff `instance` against the retained line at the cursor and advance,
    /// appending on cold growth. Returns the slot and the moved planes.
    pub fn ingest(&mut self, instance: AnalyticLineInstance) -> (GeometryId, DirtyPlanes) {
        let index = self.cursor;
        let dirty = if index < self.entries.len() {
            let prev = &self.entries[index].instance;
            let dirty = DirtyPlanes {
                transform: prev.p0 != instance.p0 || prev.p1 != instance.p1,
                geometry: prev.width != instance.width
                    || prev.cap != instance.cap
                    || prev.join != instance.join
                    || prev.miter_limit != instance.miter_limit
                    || prev.border_width != instance.border_width,
                paint: prev.color != instance.color || prev.border_color != instance.border_color,
                resource: false,
                appended: false,
            };
            if dirty.any() {
                self.entries[index].instance = instance;
            }
            dirty
        } else {
            let idx = index as u32;
            self.entries.push(AnalyticLineEntry {
                instance,
                transform: TransformId::new(idx),
                brush: BrushId::new(idx),
            });
            DirtyPlanes::APPENDED
        };
        self.cursor += 1;
        (GeometryId::new(index as u32), dirty)
    }

    /// The entry at `id`, or `None` if the slot is out of range.
    pub fn get(&self, id: GeometryId) -> Option<&AnalyticLineEntry> {
        self.entries.get(id.index() as usize)
    }

    /// Number of live entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store holds no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A retained analytic shadow: the resolved GPU instance plus its identity
/// handles (§8.5).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnalyticShadowEntry {
    /// The lowered instance, in world space (origin not yet subtracted).
    pub instance: ShadowInstance,
    /// Transform identity — bumped alone by a pure move (§8.5).
    pub transform: TransformId,
    /// Brush identity — bumped alone by a recolor (§8.5).
    pub brush: BrushId,
}

/// Dense store of analytic shadows (§8). Same AoS/cursor shape as the other
/// analytic stores; the diff splits `rect_pos`/`offset` → transform,
/// `rect_size`/`radius`/`sigma`/`spread`/`shape` → geometry, `color` → paint.
#[derive(Debug, Default)]
pub struct AnalyticShadowStore {
    entries: Vec<AnalyticShadowEntry>,
    cursor: usize,
}

impl AnalyticShadowStore {
    /// Reset the cursor for a new frame, keeping entries to diff against.
    pub fn begin_frame(&mut self) {
        self.cursor = 0;
    }

    /// Trim entries the frame's walk did not revisit. Returns whether a trim
    /// happened.
    pub fn finish_frame(&mut self) -> bool {
        if self.cursor < self.entries.len() {
            self.entries.truncate(self.cursor);
            true
        } else {
            false
        }
    }

    /// Diff `instance` against the retained shadow at the cursor and advance,
    /// appending on cold growth. Returns the slot and the moved planes.
    pub fn ingest(&mut self, instance: ShadowInstance) -> (GeometryId, DirtyPlanes) {
        let index = self.cursor;
        let dirty = if index < self.entries.len() {
            let prev = &self.entries[index].instance;
            let dirty = DirtyPlanes {
                transform: prev.rect_pos != instance.rect_pos || prev.offset != instance.offset,
                geometry: prev.rect_size != instance.rect_size
                    || prev.radius != instance.radius
                    || prev.sigma != instance.sigma
                    || prev.spread != instance.spread
                    || prev.shape != instance.shape,
                paint: prev.color != instance.color,
                resource: false,
                appended: false,
            };
            if dirty.any() {
                self.entries[index].instance = instance;
            }
            dirty
        } else {
            let idx = index as u32;
            self.entries.push(AnalyticShadowEntry {
                instance,
                transform: TransformId::new(idx),
                brush: BrushId::new(idx),
            });
            DirtyPlanes::APPENDED
        };
        self.cursor += 1;
        (GeometryId::new(index as u32), dirty)
    }

    /// The entry at `id`, or `None` if the slot is out of range.
    pub fn get(&self, id: GeometryId) -> Option<&AnalyticShadowEntry> {
        self.entries.get(id.index() as usize)
    }

    /// Number of live entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store holds no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A retained gradient draw: its lowered instance and the LUT texture the
/// renderer binds for it (§8.5). The instance is finalized during lowering — the
/// renderer resolves `lut_v`/`use_lut` against its LUT atlas before ingest — so
/// the entry holds the whole [`GradientInstance`] plus the atlas `texture` that
/// backs the ramp (the resource plane, like [`ImageEntry`]). A two-stop inline
/// gradient still carries the atlas handle so a later switch to the LUT path is
/// a plain resource diff, not a structural change.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GradientEntry {
    /// The lowered instance, in world space (origin not yet subtracted).
    pub instance: GradientInstance,
    /// The 1D LUT atlas texture the ramp is baked into (bound per draw, §16.1).
    pub texture: viso_gpu::TextureId,
}

/// Dense store of gradient fills (§8). Same AoS/cursor shape as the analytic
/// stores, plus the per-draw LUT texture like [`ImageStore`]; the diff splits
/// `rect_pos` → transform, `rect_size`/`kind`/`extend`/`p0`/`p1`/`lut_v`/
/// `use_lut` → geometry, `color0`/`color1` → paint, `texture` → resource. A
/// static gradient resolves to the same instance and LUT row every frame (the
/// row is content-addressed and stable), so a steady scene diffs to nothing.
#[derive(Debug, Default)]
pub struct GradientStore {
    entries: Vec<GradientEntry>,
    cursor: usize,
}

impl GradientStore {
    /// Reset the cursor for a new frame, keeping entries to diff against.
    pub fn begin_frame(&mut self) {
        self.cursor = 0;
    }

    /// Trim entries the frame's walk did not revisit. Returns whether a trim
    /// happened.
    pub fn finish_frame(&mut self) -> bool {
        if self.cursor < self.entries.len() {
            self.entries.truncate(self.cursor);
            true
        } else {
            false
        }
    }

    /// Diff `instance`/`texture` against the retained gradient at the cursor and
    /// advance, appending on cold growth. Returns the slot and the moved planes.
    pub fn ingest(
        &mut self,
        instance: GradientInstance,
        texture: viso_gpu::TextureId,
    ) -> (GeometryId, DirtyPlanes) {
        let index = self.cursor;
        let dirty = if index < self.entries.len() {
            let prev = &self.entries[index];
            let pi = &prev.instance;
            let dirty = DirtyPlanes {
                transform: pi.rect_pos != instance.rect_pos,
                geometry: pi.rect_size != instance.rect_size
                    || pi.kind != instance.kind
                    || pi.extend != instance.extend
                    || pi.p0 != instance.p0
                    || pi.p1 != instance.p1
                    || pi.lut_v != instance.lut_v
                    || pi.use_lut != instance.use_lut,
                paint: pi.color0 != instance.color0 || pi.color1 != instance.color1,
                resource: prev.texture != texture,
                appended: false,
            };
            if dirty.any() {
                self.entries[index] = GradientEntry { instance, texture };
            }
            dirty
        } else {
            self.entries.push(GradientEntry { instance, texture });
            DirtyPlanes::APPENDED
        };
        self.cursor += 1;
        (GeometryId::new(index as u32), dirty)
    }

    /// The entry at `id`, or `None` if the slot is out of range.
    pub fn get(&self, id: GeometryId) -> Option<&GradientEntry> {
        self.entries.get(id.index() as usize)
    }

    /// Number of live entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store holds no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Dense store of image draws (§8). Entries persist across frames.
#[derive(Debug, Default)]
pub struct ImageStore {
    entries: Vec<ImageEntry>,
    cursor: usize,
}

impl ImageStore {
    /// Reset the cursor for a new frame, keeping entries.
    pub fn begin_frame(&mut self) {
        self.cursor = 0;
    }

    /// Trim entries the frame did not revisit. Returns whether a trim happened.
    pub fn finish_frame(&mut self) -> bool {
        if self.cursor < self.entries.len() {
            self.entries.truncate(self.cursor);
            true
        } else {
            false
        }
    }

    /// Diff an image against the retained entry at the cursor and advance,
    /// appending on cold growth. `rect_pos` → transform; `rect_size`/`uv_pos`/
    /// `uv_size` → geometry; `color` (tint) → paint; `texture`/`sampler` →
    /// resource (a sampler change rebinds the bind group, like a texture swap).
    pub fn ingest(
        &mut self,
        instance: ImageInstance,
        texture: viso_gpu::TextureId,
        sampler: viso_gpu::SamplerDesc,
    ) -> (ImageId, DirtyPlanes) {
        let index = self.cursor;
        let dirty = if index < self.entries.len() {
            let prev = &self.entries[index];
            let pi = &prev.instance;
            let dirty = DirtyPlanes {
                transform: pi.rect_pos != instance.rect_pos,
                geometry: pi.rect_size != instance.rect_size
                    || pi.uv_pos != instance.uv_pos
                    || pi.uv_size != instance.uv_size,
                paint: pi.color != instance.color,
                resource: prev.texture != texture || prev.sampler != sampler,
                appended: false,
            };
            if dirty.any() {
                self.entries[index] = ImageEntry {
                    instance,
                    texture,
                    sampler,
                };
            }
            dirty
        } else {
            self.entries.push(ImageEntry {
                instance,
                texture,
                sampler,
            });
            DirtyPlanes::APPENDED
        };
        self.cursor += 1;
        (ImageId::new(index as u32), dirty)
    }

    /// The entry at `id`, or `None` if out of range.
    pub fn get(&self, id: ImageId) -> Option<&ImageEntry> {
        self.entries.get(id.index() as usize)
    }

    /// Number of live entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store holds no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Dense store of glyph runs (§8). The runs' glyph instances are packed into
/// one shared `instances` vector each frame (SoA-friendly: one contiguous
/// upload). The previous frame's packed instances are kept in `prev` (buffers
/// swapped at `begin_frame`) so a run's glyphs can be diffed position/uv/color
/// against last frame without a per-run stable slot; the run entries themselves
/// persist and are diffed for the atlas (resource plane).
#[derive(Debug, Default)]
pub struct GlyphRunStore {
    runs: Vec<GlyphRunEntry>,
    /// This frame's glyph instances, back to back in paint order.
    instances: Vec<GlyphInstance>,
    /// Last frame's packed instances, addressed by the retained run entries'
    /// `[start, start+count)` ranges. Swapped with `instances` each frame.
    prev: Vec<GlyphInstance>,
    cursor: usize,
}

impl GlyphRunStore {
    /// Start a new frame: swap the packed instance buffer with last frame's (so
    /// `prev` holds the ranges the retained run entries still point at), clear
    /// the now-current buffer, and reset the cursor. Run entries persist.
    pub fn begin_frame(&mut self) {
        std::mem::swap(&mut self.instances, &mut self.prev);
        self.instances.clear();
        self.cursor = 0;
    }

    /// Trim run entries the frame did not revisit. Returns whether a trim ran.
    pub fn finish_frame(&mut self) -> bool {
        if self.cursor < self.runs.len() {
            self.runs.truncate(self.cursor);
            true
        } else {
            false
        }
    }

    /// Ingest a run: pack its glyph instances into this frame's buffer, then
    /// diff against the retained run entry at the cursor. A run whose atlas and
    /// every packed glyph instance match last frame bumps nothing; a differing
    /// `rect_pos`/`rect_size`/`uv_*` bumps geometry, a differing `color` bumps
    /// paint, a differing `atlas` bumps resource. Glyph count change → geometry.
    pub fn ingest_run(
        &mut self,
        glyphs: impl Iterator<Item = GlyphInstance>,
        atlas: viso_gpu::TextureId,
    ) -> (u32, DirtyPlanes) {
        let start = self.instances.len() as u32;
        self.instances.extend(glyphs);
        let count = self.instances.len() as u32 - start;
        let now = &self.instances[start as usize..(start + count) as usize];
        let index = self.cursor;

        let dirty = if index < self.runs.len() {
            let prev_entry = self.runs[index];
            let s = prev_entry.start as usize;
            let e = s + prev_entry.count as usize;
            let prev_glyphs = self.prev.get(s..e).unwrap_or(&[]);
            let count_changed = prev_entry.count != count;
            let mut geometry = count_changed;
            let mut paint = false;
            if !count_changed {
                for (a, b) in prev_glyphs.iter().zip(now) {
                    if a.rect_pos != b.rect_pos
                        || a.rect_size != b.rect_size
                        || a.uv_pos != b.uv_pos
                        || a.uv_size != b.uv_size
                    {
                        geometry = true;
                    }
                    if a.color != b.color {
                        paint = true;
                    }
                }
            }
            self.runs[index] = GlyphRunEntry {
                start,
                count,
                atlas,
            };
            DirtyPlanes {
                geometry,
                paint,
                transform: false,
                resource: prev_entry.atlas != atlas,
                appended: false,
            }
        } else {
            self.runs.push(GlyphRunEntry {
                start,
                count,
                atlas,
            });
            DirtyPlanes::APPENDED
        };
        self.cursor += 1;
        (index as u32, dirty)
    }

    /// The run entry at slot `run`, or `None` if out of range.
    pub fn run(&self, run: u32) -> Option<&GlyphRunEntry> {
        self.runs.get(run as usize)
    }

    /// The glyph instances of a run, or `&[]` if the range is out of bounds.
    pub fn glyphs(&self, run: &GlyphRunEntry) -> &[GlyphInstance] {
        let start = run.start as usize;
        let end = start + run.count as usize;
        self.instances.get(start..end).unwrap_or(&[])
    }

    /// Number of runs this frame.
    pub fn len(&self) -> usize {
        self.runs.len()
    }

    /// Whether the store holds no runs.
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }
}

/// The color/opacity a path is painted with — the per-primitive paint, held
/// separately from the retained [`PathGeometry`] so a recolor never
/// re-tessellates (§13.4). `fill`/`stroke_color` are straight-linear RGBA
/// matching the source `Path`; `opacity` is a whole-path multiplier (1.0 today,
/// reserved for a group-opacity plane).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathPaint {
    /// Fill color, or `None` for an unfilled path.
    pub fill: Option<Rgba>,
    /// Stroke color, or `None` for an unstroked path.
    pub stroke_color: Option<Rgba>,
    /// Whole-path opacity multiplier applied at lowering.
    pub opacity: f32,
}

impl PathPaint {
    /// The paint carried by a source path (fill/stroke color; opacity 1.0).
    fn from_path(path: &Path) -> PathPaint {
        PathPaint {
            fill: path.fill,
            stroke_color: path.stroke.map(|s| s.color),
            opacity: 1.0,
        }
    }
}

/// The placement of a retained [`PathGeometry`] relative to the local space it
/// was tessellated in — the per-primitive transform, held separately so a
/// move/zoom that leaves the shape intact never re-tessellates (§13.4). This
/// round carries translate + uniform scale; non-uniform scale / rotation route
/// through a geometry rebuild.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathTransform {
    /// Translation from the tessellated space to the path's current space.
    pub offset: [f32; 2],
    /// Uniform scale about the tessellated origin (1.0 = as-tessellated).
    pub scale: f32,
}

impl PathTransform {
    /// The identity placement (geometry used exactly as tessellated).
    const IDENTITY: Self = Self {
        offset: [0.0, 0.0],
        scale: 1.0,
    };
}

/// The cache key for a retained tessellation: a translation-invariant structural
/// fingerprint of the outline (see [`Path::geometry_fingerprint`]) paired with
/// the quality bucket it was tessellated at. Two paths with the same key share a
/// tessellation; they differ only by a [`PathTransform`] and/or [`PathPaint`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GeometryKey {
    /// Structural fingerprint of the outline, invariant under translation.
    pub fingerprint: u64,
    /// The quality bucket the geometry was tessellated at.
    pub quality_bucket: u16,
}

/// A retained vector path split into a cached colorless tessellation plus the
/// cheap per-primitive paint/transform layered on at lowering (§13.4). The
/// geometry is keyed by [`GeometryKey`]; a transform- or paint-only change
/// reuses `geometry` untouched rather than re-running the tessellator.
#[derive(Debug, Clone)]
pub struct PathEntry {
    /// The cache key of `geometry` (structural fingerprint + quality bucket).
    pub geom_key: GeometryKey,
    /// The retained colorless tessellation, in its own local space.
    pub geometry: PathGeometry,
    /// The color/opacity to paint `geometry` with (applied at lowering).
    pub paint: PathPaint,
    /// The placement of `geometry` in the path's current space (applied at
    /// lowering).
    pub xform: PathTransform,
    /// The full source outline, retained so the next frame's diff can classify a
    /// change as geometry / transform-only / paint-only.
    pub path: Path,
}

/// Dense store of vector paths with an in-line retessellation cache (§13.4). A
/// path whose structural fingerprint and quality bucket are unchanged reuses its
/// cached [`PathGeometry`]; the tessellator runs only on a geometry change. A
/// pure translation/uniform-scale updates [`PathTransform`] and a recolor updates
/// [`PathPaint`], both without touching the geometry.
#[derive(Debug, Default)]
pub struct VectorPathStore {
    entries: Vec<PathEntry>,
    cursor: usize,
}

impl VectorPathStore {
    /// Reset the cursor for a new frame, keeping entries (and their cached
    /// tessellations) to diff against.
    pub fn begin_frame(&mut self) {
        self.cursor = 0;
    }

    /// Trim entries the frame did not revisit. Returns whether a trim ran.
    pub fn finish_frame(&mut self) -> bool {
        if self.cursor < self.entries.len() {
            self.entries.truncate(self.cursor);
            true
        } else {
            false
        }
    }

    /// Diff a path against the retained entry at the cursor, classifying the
    /// change into independent planes (§13.4):
    ///
    /// - **geometry** — the structural fingerprint or quality bucket changed:
    ///   re-tessellate into a fresh [`PathGeometry`]. This is the only path that
    ///   runs the tessellator (and the only one the `path_tessellations` counter
    ///   counts).
    /// - **transform-only** — the outline is the previous one shifted by a pure
    ///   translation: reuse the cached geometry, update [`PathTransform`].
    /// - **paint-only** — only fill/stroke color moved: reuse the cached
    ///   geometry, update [`PathPaint`].
    ///
    /// A fully equal path is a cache hit (no planes). Cold growth appends and
    /// tessellates once.
    pub fn ingest(&mut self, path: &Path, device_scale: f32) -> (PathId, DirtyPlanes) {
        let index = self.cursor;
        let dirty = if index < self.entries.len() {
            let prev = &self.entries[index];
            // The quality bucket is chosen relative to the bucket this entry is
            // already cached at, so the boundary hysteresis holds across the
            // scale hovering near a step (§13.4).
            let bucket = quality_bucket(device_scale, prev.geom_key.quality_bucket);
            let fingerprint = path.geometry_fingerprint();
            let geometry =
                fingerprint != prev.geom_key.fingerprint || bucket != prev.geom_key.quality_bucket;
            if geometry {
                // The shape (or its quality) changed: re-tessellate at the
                // current bucket. The new geometry defines a fresh local space,
                // so the transform resets to identity and paint is re-read from
                // the source.
                let geo = path.tessellate_geometry_at(bucket);
                self.entries[index] = PathEntry {
                    geom_key: GeometryKey {
                        fingerprint,
                        quality_bucket: bucket,
                    },
                    geometry: geo,
                    paint: PathPaint::from_path(path),
                    xform: PathTransform::IDENTITY,
                    path: path.clone(),
                };
                DirtyPlanes {
                    geometry: true,
                    ..DirtyPlanes::default()
                }
            } else {
                // Same shape: classify the cheap deltas. A pure translation
                // folds into the transform (relative to the previous placement);
                // color deltas fold into paint. Both reuse the cached geometry.
                let translation = path.translation_from(&prev.path);
                let paint = PathPaint::from_path(path);
                let transform_moved = matches!(translation, Some(d) if d != [0.0, 0.0]);
                let paint_moved = paint != prev.paint;
                if transform_moved {
                    let d = translation.expect("translation present");
                    let prev_xform = prev.xform;
                    self.entries[index].xform = PathTransform {
                        offset: [prev_xform.offset[0] + d[0], prev_xform.offset[1] + d[1]],
                        scale: prev_xform.scale,
                    };
                }
                if paint_moved {
                    self.entries[index].paint = paint;
                }
                // Retain the current source outline for the next frame's diff.
                self.entries[index].path = path.clone();
                DirtyPlanes {
                    transform: transform_moved,
                    paint: paint_moved,
                    ..DirtyPlanes::default()
                }
            }
        } else {
            // Cold append: no cached bucket to hold, so the bucket is chosen
            // from the identity (bucket 0) baseline for this device scale.
            let bucket = quality_bucket(device_scale, DEFAULT_QUALITY);
            let geo = path.tessellate_geometry_at(bucket);
            self.entries.push(PathEntry {
                geom_key: GeometryKey {
                    fingerprint: path.geometry_fingerprint(),
                    quality_bucket: bucket,
                },
                geometry: geo,
                paint: PathPaint::from_path(path),
                xform: PathTransform::IDENTITY,
                path: path.clone(),
            });
            DirtyPlanes::APPENDED
        };
        self.cursor += 1;
        (PathId::new(index as u32), dirty)
    }

    /// The entry at `id`, or `None` if out of range.
    pub fn get(&self, id: PathId) -> Option<&PathEntry> {
        self.entries.get(id.index() as usize)
    }

    /// Number of paths this frame.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store holds no paths.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The tessellation quality bucket for the identity device scale (1.0). A path
/// ingested with no scale information tessellates at this bucket.
pub const DEFAULT_QUALITY: u16 = 0;

/// The device scales at which the tessellation quality bucket steps up, in
/// ascending order. A path drawn at a higher device scale needs a finer
/// flatten tolerance to stay smooth, so it rebuilds into a higher bucket once
/// its scale crosses the next boundary.
///
/// Boundaries are the *enter* thresholds: bucket `i` (0-based) covers roughly
/// `[QUALITY_STEPS[i-1], QUALITY_STEPS[i])`. Bucket 0 is the identity scale.
const QUALITY_STEPS: [f32; 3] = [1.5, 2.5, 4.0];

/// Hysteresis band around each bucket boundary, as a fraction of the boundary
/// scale. A bucket only steps *up* once the scale exceeds `boundary * (1 + H)`
/// and only steps *down* once it falls below `boundary * (1 - H)`; between those
/// the previous bucket is held. This stops a scale hovering on a boundary (e.g.
/// a pinch-zoom settling near 2.0×) from re-tessellating every frame (§13.4).
const QUALITY_HYSTERESIS: f32 = 0.1;

/// The quality bucket a path should tessellate at for `device_scale`, given the
/// bucket it is currently cached at (`prev`).
///
/// Without hysteresis a scale parked on a boundary would flip buckets — and thus
/// re-tessellate — on tiny jitter. Instead each boundary is a band: crossing
/// upward requires `scale > boundary * (1 + H)`, crossing downward requires
/// `scale < boundary * (1 - H)`, and inside the band the previous bucket wins.
/// The result is monotone in `device_scale` and stable across small wobble.
pub fn quality_bucket(device_scale: f32, prev: u16) -> u16 {
    let scale = device_scale.max(0.0);
    let prev = prev as usize;
    // Walk the boundaries; a boundary is "crossed" only outside its hysteresis
    // band, so whether we are moving up or down decides which edge applies.
    let mut bucket = 0usize;
    for (i, &boundary) in QUALITY_STEPS.iter().enumerate() {
        let target = i + 1;
        let crossed = if target <= prev {
            // At or below where we already are: hold this step unless the scale
            // has dropped below the band's lower edge.
            scale >= boundary * (1.0 - QUALITY_HYSTERESIS)
        } else {
            // Above where we are: only step up past the band's upper edge.
            scale >= boundary * (1.0 + QUALITY_HYSTERESIS)
        };
        if crossed {
            bucket = target;
        } else {
            break;
        }
    }
    bucket as u16
}

/// A caller-supplied triangle mesh, stored as its vertices/indices (no
/// tessellation — a mesh is already triangulated, §8).
#[derive(Debug, Clone)]
pub struct MeshEntry {
    /// The mesh vertices, in the mesh's own space (base-zero indices).
    pub vertices: Vec<MeshVertex>,
    /// Triangle-list indices into `vertices` (base-zero).
    pub indices: Vec<u32>,
}

/// Dense store of caller-supplied meshes (§8). Entries persist across frames.
#[derive(Debug, Default)]
pub struct MeshStore {
    entries: Vec<MeshEntry>,
    cursor: usize,
}

impl MeshStore {
    /// Reset the cursor for a new frame, keeping entries.
    pub fn begin_frame(&mut self) {
        self.cursor = 0;
    }

    /// Trim entries the frame did not revisit. Returns whether a trim ran.
    pub fn finish_frame(&mut self) -> bool {
        if self.cursor < self.entries.len() {
            self.entries.truncate(self.cursor);
            true
        } else {
            false
        }
    }

    /// Diff a mesh against the retained entry at the cursor. Index or vertex
    /// position/edge change → geometry; a pure vertex-color change → paint;
    /// equal → no-op. Cold growth appends.
    pub fn ingest(&mut self, vertices: &[MeshVertex], indices: &[u32]) -> (MeshId, DirtyPlanes) {
        let index = self.cursor;
        let dirty = if index < self.entries.len() {
            let prev = &self.entries[index];
            let structural = prev.indices != indices || prev.vertices.len() != vertices.len();
            let mut geometry = structural;
            let mut paint = false;
            if !structural {
                for (a, b) in prev.vertices.iter().zip(vertices) {
                    if a.pos != b.pos || a.edge != b.edge {
                        geometry = true;
                    }
                    if a.color != b.color {
                        paint = true;
                    }
                }
            }
            if geometry || paint {
                self.entries[index] = MeshEntry {
                    vertices: vertices.to_vec(),
                    indices: indices.to_vec(),
                };
            }
            DirtyPlanes {
                geometry,
                paint,
                transform: false,
                resource: false,
                appended: false,
            }
        } else {
            self.entries.push(MeshEntry {
                vertices: vertices.to_vec(),
                indices: indices.to_vec(),
            });
            DirtyPlanes::APPENDED
        };
        self.cursor += 1;
        (MeshId::new(index as u32), dirty)
    }

    /// The entry at `id`, or `None` if out of range.
    pub fn get(&self, id: MeshId) -> Option<&MeshEntry> {
        self.entries.get(id.index() as usize)
    }

    /// Number of meshes this frame.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store holds no meshes.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A retained clip rect (§8). Identity-separated so a clip change bumps the
/// clip plane alone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipEntry {
    /// The effective clip rect in world space, or `None` for unclipped.
    pub rect: Option<crate::primitive::Rect>,
}

/// Dense store of effective clips referenced by primitives (§8). Entries
/// persist across frames.
#[derive(Debug, Default)]
pub struct ClipStore {
    entries: Vec<ClipEntry>,
    cursor: usize,
}

impl ClipStore {
    /// Reset the cursor for a new frame, keeping entries.
    pub fn begin_frame(&mut self) {
        self.cursor = 0;
    }

    /// Trim entries the frame did not revisit. Returns whether a trim ran.
    pub fn finish_frame(&mut self) -> bool {
        if self.cursor < self.entries.len() {
            self.entries.truncate(self.cursor);
            true
        } else {
            false
        }
    }

    /// Diff a clip against the retained entry at the cursor. A differing rect
    /// bumps the clip plane; cold growth appends. Returns the slot and whether
    /// the clip changed (carried in `DirtyPlanes::geometry`, the clip store's
    /// single plane at this layer — [`super::ingest`] maps it to the clip
    /// revision).
    pub fn ingest(&mut self, rect: Option<crate::primitive::Rect>) -> (ClipId, bool) {
        let index = self.cursor;
        let changed = if index < self.entries.len() {
            let changed = self.entries[index].rect != rect;
            if changed {
                self.entries[index] = ClipEntry { rect };
            }
            changed
        } else {
            self.entries.push(ClipEntry { rect });
            true
        };
        self.cursor += 1;
        (ClipId::new(index as u32), changed)
    }

    /// The entry at `id`, or `None` if out of range.
    pub fn get(&self, id: ClipId) -> Option<&ClipEntry> {
        self.entries.get(id.index() as usize)
    }

    /// Number of clips this frame.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store holds no clips.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The cache key of a *complex* (path/mask) clip in a resolved chain (§14.2).
///
/// A rect-only chain resolves to a pure [`ClipChainDescriptor::rect`] and carries
/// no mask key. A chain whose tail is an arbitrary-path clip must be re-rastered
/// into a coverage mask only when one of the inputs that determines the mask's
/// pixels moves; this key is exactly that set, so an unchanged key means the
/// retained mask is still valid and no re-raster is owed. The mask's *pixels*
/// are a function of these fields and nothing else:
///
/// - `geometry_revision` — the path arena's revision the mask was built against;
///   a reparse/edit bumps it and invalidates the mask.
/// - `transform_bucket` — the quantized effective transform (a mask built at one
///   rotation/skew bucket cannot be reused at another; a pure translation folds
///   into the ROI, not the bucket).
/// - `device_scale_q` — the device pixel scale, quantized, since the mask is
///   rasterized in device pixels.
/// - `fill_rule` — nonzero vs even-odd changes which pixels the path covers.
/// - `composition` — how this clip composes with the enclosing chain (intersect
///   is the default; a difference/xor composition covers different pixels).
///
/// Every field is an integer or an already-quantized bucket so the key is
/// `Eq`/`Hash`: comparison is exact, never a float tolerance on the hot path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClipMaskKey {
    /// The path/geometry revision the mask was rasterized against.
    pub geometry_revision: u64,
    /// The quantized effective transform bucket the mask was built under.
    pub transform_bucket: u64,
    /// The device pixel scale, quantized to an integer bucket.
    pub device_scale_q: u32,
    /// The fill rule the coverage was computed with.
    pub fill_rule: ClipFillRule,
    /// How this clip composes with the enclosing chain.
    pub composition: ClipComposition,
}

/// The fill rule a path clip's coverage is computed under (§14.2 mask key).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClipFillRule {
    /// Nonzero winding.
    NonZero,
    /// Even-odd winding.
    EvenOdd,
}

/// How a clip in a resolved chain composes with the clip enclosing it
/// (§14.2 mask key). Intersection is the default; the others are part of the key
/// because they select different covered pixels for the same geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClipComposition {
    /// The child is clipped to the intersection of itself and its parent — the
    /// ordinary nested-clip case.
    Intersect,
    /// The child clips to the region of its parent *outside* this shape.
    Difference,
    /// Symmetric difference.
    Xor,
}

/// A resolved clip chain: the pre-intersected axis-aligned box plus, for a
/// complex tail, the mask key that says whether the retained coverage is still
/// valid (§14.2).
///
/// This is the "pre-resolved clip descriptor" a [`ClipChainId`] names. Resolving
/// a stack of nested clips folds all the axis-aligned rects into a single
/// [`rect`](Self::rect) **once**; every clipped primitive under the chain reads
/// that one box instead of walking the clip stack per primitive. When the chain
/// is rect-only, `mask` is `None` and the descriptor is complete — there is no
/// path to reparse and no mask to raster. When a complex clip is present, `mask`
/// carries its [`ClipMaskKey`]; an unchanged key across frames means the retained
/// mask stands and is not re-rastered.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipChainDescriptor {
    /// The intersection of every axis-aligned rect in the chain, in world space.
    /// A zero-area rect here is an [empty clip](Self::is_empty): the whole subtree
    /// under this chain draws nothing (§14.3).
    pub rect: crate::primitive::Rect,
    /// The complex tail's mask key, or `None` for a rect-only chain.
    pub mask: Option<ClipMaskKey>,
}

impl ClipChainDescriptor {
    /// Whether this chain clips everything away: its folded rect has zero area,
    /// so no primitive under it can be visible. The renderer uses this to reject
    /// the whole subtree immediately rather than emit draws that the scissor
    /// would discard pixel by pixel (§14.3).
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.rect.w <= 0.0 || self.rect.h <= 0.0
    }
}

/// One retained resolved-chain entry: its descriptor and the identity of the
/// input stack it was resolved from (§14.2).
///
/// `input` is the resolution key — the sequence of clip stores/keys the chain was
/// built from, hashed to a compact fingerprint. It is what lets an unchanged
/// chain skip re-resolution: if the incoming stack fingerprints to the same
/// `input`, the retained `descriptor` is returned untouched — no rect refold, no
/// mask re-raster, no per-primitive tree walk.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipChainEntry {
    /// The fingerprint of the input clip stack this chain was resolved from.
    pub input: u64,
    /// The resolved descriptor.
    pub descriptor: ClipChainDescriptor,
}

/// Dense store of resolved clip chains (§14.2), populating [`ClipChainId`].
///
/// Mirrors [`ClipStore`]'s retained cursor/diff/truncate discipline: the Nth
/// chain resolved this frame owns the Nth slot, frame after frame. The store's
/// job is to resolve a nested clip stack into one [`ClipChainDescriptor`] exactly
/// once per change: [`resolve`](Self::resolve) compares the incoming stack's
/// fingerprint against the retained entry and, when it matches, returns the
/// cached descriptor without folding anything. A changed fingerprint refolds and
/// reports the change so the caller can bump the clip plane.
#[derive(Debug, Default)]
pub struct ClipChainStore {
    entries: Vec<ClipChainEntry>,
    cursor: usize,
}

impl ClipChainStore {
    /// Reset the cursor for a new frame, keeping entries to diff against.
    pub fn begin_frame(&mut self) {
        self.cursor = 0;
    }

    /// Trim chains the frame did not revisit. Returns whether a trim ran.
    pub fn finish_frame(&mut self) -> bool {
        if self.cursor < self.entries.len() {
            self.entries.truncate(self.cursor);
            true
        } else {
            false
        }
    }

    /// Resolve a nested clip stack into a retained [`ClipChainDescriptor`].
    ///
    /// `rects` is the ordered stack of axis-aligned clip rects (outermost first);
    /// they are pre-intersected into one box. `mask` is the complex tail's key,
    /// or `None`. `input` fingerprints the stack's identity (geometry revisions,
    /// rect identities) so an unchanged stack is recognised without refolding.
    ///
    /// When the retained entry at the cursor carries the same `input`, its
    /// descriptor is returned unchanged — the fold and any mask raster are
    /// skipped. Otherwise the rects are folded (starting from
    /// [`Rect::INFINITE`](crate::primitive::Rect::INFINITE), the intersection
    /// identity), the entry is (re)written, and the change is reported. Returns
    /// the slot, its descriptor, and whether the chain changed.
    pub fn resolve(
        &mut self,
        rects: &[crate::primitive::Rect],
        mask: Option<ClipMaskKey>,
        input: u64,
    ) -> (ClipChainId, ClipChainDescriptor, bool) {
        let index = self.cursor;
        // A retained chain with the same input fingerprint is still valid: hand
        // back the cached descriptor without re-folding or re-rastering.
        if index < self.entries.len() && self.entries[index].input == input {
            self.cursor += 1;
            let entry = self.entries[index];
            return (ClipChainId::new(index as u32), entry.descriptor, false);
        }

        // Cold path: fold the axis-aligned stack once. `INFINITE` is the
        // intersection identity, so an empty stack resolves to unbounded.
        let mut rect = crate::primitive::Rect::INFINITE;
        for r in rects {
            rect = rect.intersect(*r);
        }
        let descriptor = ClipChainDescriptor { rect, mask };
        let entry = ClipChainEntry { input, descriptor };
        if index < self.entries.len() {
            self.entries[index] = entry;
        } else {
            self.entries.push(entry);
        }
        self.cursor += 1;
        (ClipChainId::new(index as u32), descriptor, true)
    }

    /// The resolved descriptor at `id`, or `None` if out of range.
    pub fn get(&self, id: ClipChainId) -> Option<&ClipChainDescriptor> {
        self.entries.get(id.index() as usize).map(|e| &e.descriptor)
    }

    /// Number of resolved chains this frame.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store holds no chains.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A retained affine transform (§8.5). Carries the world-space origin an emit
/// subtracts; the identity separation lets a pure move bump this store's plane
/// alone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransformEntry {
    /// The world-space origin subtracted from an emit's geometry (a translation
    /// only, for now — the offscreen-layer origin).
    pub origin: [f32; 2],
}

/// Dense store of transforms (§8.5). Entries persist across frames.
#[derive(Debug, Default)]
pub struct TransformStore {
    entries: Vec<TransformEntry>,
    cursor: usize,
}

impl TransformStore {
    /// Reset the cursor for a new frame, keeping entries.
    pub fn begin_frame(&mut self) {
        self.cursor = 0;
    }

    /// Trim entries the frame did not revisit. Returns whether a trim ran.
    pub fn finish_frame(&mut self) -> bool {
        if self.cursor < self.entries.len() {
            self.entries.truncate(self.cursor);
            true
        } else {
            false
        }
    }

    /// Diff a transform against the retained entry at the cursor. A differing
    /// origin bumps the transform plane; cold growth appends.
    pub fn ingest(&mut self, origin: [f32; 2]) -> (TransformId, bool) {
        let index = self.cursor;
        let changed = if index < self.entries.len() {
            let changed = self.entries[index].origin != origin;
            if changed {
                self.entries[index] = TransformEntry { origin };
            }
            changed
        } else {
            self.entries.push(TransformEntry { origin });
            true
        };
        self.cursor += 1;
        (TransformId::new(index as u32), changed)
    }

    /// The entry at `id`, or `None` if out of range.
    pub fn get(&self, id: TransformId) -> Option<&TransformEntry> {
        self.entries.get(id.index() as usize)
    }

    /// Number of transforms this frame.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store holds no transforms.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The paint model a brush resolves to (§12 Brush model). A fill/stroke is one
/// of these; the store diffs them by value so a recolor bumps the paint plane
/// alone.
///
/// `Solid` is the wired steady-state path: its color is baked inline into each
/// primitive's instance at lowering, and the brush store's entry mirrors it so
/// the paint diff sees the change. The gradient variants name the fill's slot in
/// the [`GradientStore`] (the gradient's lowered geometry + LUT live there),
/// keeping the brush a pure paint identity: moving or recoloring the gradient
/// still diffs its brush entry. [`Brush::ImagePattern`] (§12.4) and
/// [`Brush::ShaderBrush`] are declared for the model but not yet lowered; their
/// [`BrushStore::ingest`] path is an explicit `todo!`, never a silent no-op.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Brush {
    /// A flat straight-linear RGBA fill — baked inline into the instance.
    Solid([f32; 4]),
    /// A linear gradient, resolved into the [`GradientStore`] slot `id`.
    LinearGradient(GeometryId),
    /// A radial gradient, resolved into the [`GradientStore`] slot `id`.
    RadialGradient(GeometryId),
    /// A sweep gradient, resolved into the [`GradientStore`] slot `id`.
    SweepGradient(GeometryId),
    /// A tiled/stretched image fill (§12.4). Declared for the model; lowering is
    /// deferred (D2.2).
    ImagePattern(ImageId),
    /// A user shader fill. Declared for the model; lowering is deferred.
    ShaderBrush,
}

/// A retained brush — a resolved fill/stroke paint (§8.5). The store exists so
/// the diff can bump paint independently, and so the freeze pins its shape.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrushEntry {
    /// The resolved paint this brush applies.
    pub brush: Brush,
}

/// Dense store of brushes (§8.5). Entries persist across frames.
#[derive(Debug, Default)]
pub struct BrushStore {
    entries: Vec<BrushEntry>,
    cursor: usize,
}

impl BrushStore {
    /// Reset the cursor for a new frame, keeping entries.
    pub fn begin_frame(&mut self) {
        self.cursor = 0;
    }

    /// Trim entries the frame did not revisit. Returns whether a trim ran.
    pub fn finish_frame(&mut self) -> bool {
        if self.cursor < self.entries.len() {
            self.entries.truncate(self.cursor);
            true
        } else {
            false
        }
    }

    /// Diff a brush against the retained entry at the cursor. A differing brush
    /// (bit-exact for the solid color) bumps the paint plane; cold growth
    /// appends. The deferred [`Brush::ImagePattern`]/[`Brush::ShaderBrush`]
    /// paths are rejected explicitly rather than stored as no-ops.
    pub fn ingest(&mut self, brush: Brush) -> (BrushId, bool) {
        match brush {
            Brush::ImagePattern(_) => {
                todo!("ImagePattern brush lowering is deferred to D2.2 (§12.4)")
            }
            Brush::ShaderBrush => todo!("ShaderBrush lowering is not implemented"),
            _ => {}
        }
        let index = self.cursor;
        let changed = if index < self.entries.len() {
            let changed = self.entries[index].brush != brush;
            if changed {
                self.entries[index] = BrushEntry { brush };
            }
            changed
        } else {
            self.entries.push(BrushEntry { brush });
            true
        };
        self.cursor += 1;
        (BrushId::new(index as u32), changed)
    }

    /// The entry at `id`, or `None` if out of range.
    pub fn get(&self, id: BrushId) -> Option<&BrushEntry> {
        self.entries.get(id.index() as usize)
    }

    /// Number of brushes this frame.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store holds no brushes.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Convenience: build a glyph instance iterator's worth of instances from a run
/// draw's glyphs and color, matching `GlyphRunDraw::instance` exactly. Kept here
/// so the store's glyph lowering stays paired with its consumers.
pub fn glyph_instances<'a>(
    glyphs: &'a [GlyphInstanceData],
    color: [f32; 4],
) -> impl Iterator<Item = GlyphInstance> + 'a {
    glyphs.iter().map(move |g| GlyphInstance {
        rect_pos: [g.rect.x, g.rect.y],
        rect_size: [g.rect.w, g.rect.h],
        uv_pos: [g.uv.x, g.uv.y],
        uv_size: [g.uv.w, g.uv.h],
        color,
    })
}

/// Reference to a primitive's dense slot, tagged by kind. The paint-order
/// record (`scene::mod`) holds one of these per emitted primitive so
/// re-derivation can pull the right store entry in paint order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StoreRef {
    /// Slot in the [`SolidQuadStore`].
    Quad(GeometryId),
    /// Slot in the [`AnalyticRRectStore`].
    AnalyticRRect(GeometryId),
    /// Slot in the [`AnalyticEllipseStore`].
    AnalyticEllipse(GeometryId),
    /// Slot in the [`AnalyticCapsuleStore`].
    AnalyticCapsule(GeometryId),
    /// Slot in the [`AnalyticLineStore`].
    AnalyticLine(GeometryId),
    /// Slot in the [`ImageStore`].
    Image(ImageId),
    /// Slot in the [`GradientStore`].
    Gradient(GeometryId),
    /// Slot in the [`AnalyticShadowStore`].
    AnalyticShadow(GeometryId),
    /// Run slot in the [`GlyphRunStore`].
    GlyphRun(u32),
    /// Slot in the [`VectorPathStore`].
    Path(PathId),
    /// Slot in the [`MeshStore`].
    Mesh(MeshId),
    /// A composite draw emitted when a translucent layer closes: an image draw
    /// sampling offscreen pass `pass`, whose resolved instance is recorded inline
    /// so lowering reproduces it. The pass index resolves the sampling bind group
    /// from the renderer's live offscreen passes at lowering time.
    Composite {
        instance: ImageInstance,
        pass: usize,
    },
}

impl std::fmt::Display for StoreRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreRef::Quad(_) => write!(f, "quad"),
            StoreRef::AnalyticRRect(_) => write!(f, "analytic-rrect"),
            StoreRef::AnalyticEllipse(_) => write!(f, "analytic-ellipse"),
            StoreRef::AnalyticCapsule(_) => write!(f, "analytic-capsule"),
            StoreRef::AnalyticLine(_) => write!(f, "analytic-line"),
            StoreRef::Image(_) => write!(f, "image"),
            StoreRef::Gradient(_) => write!(f, "gradient"),
            StoreRef::AnalyticShadow(_) => write!(f, "analytic-shadow"),
            StoreRef::GlyphRun(_) => write!(f, "glyph-run"),
            StoreRef::Path(_) => write!(f, "path"),
            StoreRef::Mesh(_) => write!(f, "mesh"),
            StoreRef::Composite { .. } => write!(f, "composite"),
        }
    }
}

/// A primitive id is a plain positional index this stage; kept as a helper so
/// the paint-order record and the diff agree on the mapping.
pub fn primitive_id(order_index: usize) -> PrimitiveId {
    PrimitiveId::new(order_index as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitive::{PathCmd, Point, Rgba};

    /// A filled path with one curved edge, so a finer quality bucket flattens
    /// it into more vertices (the observable effect of a bucket change).
    fn curved_path() -> Path {
        Path {
            cmds: vec![
                PathCmd::MoveTo(Point::new(0.0, 0.0)),
                PathCmd::CubicTo(
                    Point::new(40.0, 80.0),
                    Point::new(80.0, -40.0),
                    Point::new(120.0, 40.0),
                ),
                PathCmd::LineTo(Point::new(120.0, 0.0)),
                PathCmd::Close,
            ],
            fill: Some(Rgba::new(1.0, 0.0, 0.0, 1.0)),
            stroke: None,
            shadow: None,
        }
    }

    #[test]
    fn quality_bucket_is_monotone_in_scale() {
        // Deep inside each band (past the upper hysteresis edge) the bucket is
        // fixed regardless of history, and rises with scale.
        assert_eq!(quality_bucket(1.0, 0), 0);
        assert_eq!(quality_bucket(2.0, 0), 1);
        assert_eq!(quality_bucket(3.0, 0), 2);
        assert_eq!(quality_bucket(5.0, 0), 3);
    }

    #[test]
    fn quality_bucket_holds_inside_the_hysteresis_band() {
        // The first boundary is 1.5; its band is [1.35, 1.65]. A scale in the
        // band keeps whichever bucket we came from — no flip either way.
        assert_eq!(quality_bucket(1.5, 0), 0, "coming from 0, hold 0 in-band");
        assert_eq!(quality_bucket(1.5, 1), 1, "coming from 1, hold 1 in-band");
        // Only outside the band does the bucket move.
        assert_eq!(quality_bucket(1.66, 0), 1, "past upper edge steps up");
        assert_eq!(quality_bucket(1.34, 1), 0, "below lower edge steps down");
    }

    #[test]
    fn quality_bucket_does_not_flap_across_a_boundary() {
        // Simulate a scale wobbling around the 1.5 boundary inside the band:
        // the bucket must not oscillate frame to frame.
        let mut bucket = 0u16;
        for &s in &[1.48, 1.52, 1.49, 1.51, 1.50, 1.47, 1.53] {
            bucket = quality_bucket(s, bucket);
            assert_eq!(bucket, 0, "in-band wobble at {s} must hold bucket 0");
        }
    }

    #[test]
    fn within_bucket_scale_change_reuses_geometry() {
        let path = curved_path();
        let mut store = VectorPathStore::default();
        store.begin_frame();
        let (_, d0) = store.ingest(&path, 1.0);
        assert!(d0.appended);
        let verts0 = store.entries[0].geometry.verts.len();

        // A scale change that stays inside bucket 0's band: no rebuild.
        store.begin_frame();
        let (_, d1) = store.ingest(&path, 1.3);
        assert!(!d1.geometry, "in-band scale change must not re-tessellate");
        assert_eq!(store.entries[0].geometry.verts.len(), verts0);
    }

    #[test]
    fn cross_threshold_scale_change_retessellates_finer() {
        let path = curved_path();
        let mut store = VectorPathStore::default();
        store.begin_frame();
        store.ingest(&path, 1.0);
        let coarse = store.entries[0].geometry.verts.len();
        assert_eq!(store.entries[0].geom_key.quality_bucket, 0);

        // Cross well past the first boundary: rebuild into a finer bucket with
        // strictly more flattened vertices.
        store.begin_frame();
        let (_, d) = store.ingest(&path, 3.0);
        assert!(d.geometry, "crossing a bucket boundary must re-tessellate");
        assert!(store.entries[0].geom_key.quality_bucket > 0);
        assert!(
            store.entries[0].geometry.verts.len() > coarse,
            "finer bucket must produce more vertices: {} vs {}",
            store.entries[0].geometry.verts.len(),
            coarse,
        );
    }

    #[test]
    fn hysteresis_band_does_not_retessellate_across_frames() {
        let path = curved_path();
        let mut store = VectorPathStore::default();
        store.begin_frame();
        store.ingest(&path, 1.0);
        let verts = store.entries[0].geometry.verts.len();

        // Wobble across the 1.5 boundary within the band over several frames.
        for &s in &[1.48, 1.52, 1.49, 1.51, 1.5] {
            store.begin_frame();
            let (_, d) = store.ingest(&path, s);
            assert!(!d.geometry, "in-band wobble at {s} must not re-tessellate");
        }
        assert_eq!(store.entries[0].geometry.verts.len(), verts);
        assert_eq!(store.entries[0].geom_key.quality_bucket, 0);
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> crate::primitive::Rect {
        crate::primitive::Rect { x, y, w, h }
    }

    /// Nested axis-aligned rects are pre-intersected into one box on resolve —
    /// the descriptor carries the fold, not the stack.
    #[test]
    fn chain_pre_intersects_nested_rects() {
        let mut store = ClipChainStore::default();
        store.begin_frame();
        let outer = rect(0.0, 0.0, 100.0, 100.0);
        let inner = rect(20.0, 30.0, 100.0, 100.0);
        let (_id, d, changed) = store.resolve(&[outer, inner], None, 1);
        assert!(changed, "first resolve is a cold build");
        // Intersection: origin maxed, far edge minned → (20,30)..(100,100).
        assert_eq!(d.rect, rect(20.0, 30.0, 80.0, 70.0));
        assert!(d.mask.is_none(), "rect-only chain carries no mask");
        assert!(!d.is_empty());
    }

    /// An empty stack resolves to the unbounded identity, not a zero rect.
    #[test]
    fn empty_stack_resolves_to_infinite() {
        let mut store = ClipChainStore::default();
        store.begin_frame();
        let (_id, d, _) = store.resolve(&[], None, 7);
        assert_eq!(d.rect, crate::primitive::Rect::INFINITE);
        assert!(!d.is_empty());
    }

    /// Disjoint rects fold to a zero-area box — an empty clip that rejects the
    /// whole subtree (§14.3).
    #[test]
    fn disjoint_rects_are_an_empty_clip() {
        let mut store = ClipChainStore::default();
        store.begin_frame();
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(50.0, 50.0, 10.0, 10.0);
        let (_id, d, _) = store.resolve(&[a, b], None, 1);
        assert!(d.is_empty(), "non-overlapping clips draw nothing");
    }

    /// An unchanged input fingerprint returns the retained descriptor without
    /// re-folding: the second frame reports no change (no path reparse / mask
    /// re-raster / tree walk owed).
    #[test]
    fn unchanged_chain_skips_re_resolution() {
        let mut store = ClipChainStore::default();
        let r = rect(0.0, 0.0, 40.0, 40.0);

        store.begin_frame();
        let (id0, _d0, changed0) = store.resolve(&[r], None, 42);
        assert!(changed0);

        store.begin_frame();
        let (id1, _d1, changed1) = store.resolve(&[r], None, 42);
        assert!(!changed1, "same fingerprint reuses the resolved chain");
        assert_eq!(id0.index(), id1.index(), "same positional slot");
    }

    /// A changed fingerprint at the same slot re-resolves and reports the change,
    /// so the caller bumps the clip plane.
    #[test]
    fn changed_fingerprint_re_resolves() {
        let mut store = ClipChainStore::default();

        store.begin_frame();
        let (_id, _d, _) = store.resolve(&[rect(0.0, 0.0, 40.0, 40.0)], None, 1);

        store.begin_frame();
        let (_id, d, changed) = store.resolve(&[rect(0.0, 0.0, 60.0, 60.0)], None, 2);
        assert!(changed, "a moved clip re-resolves");
        assert_eq!(d.rect, rect(0.0, 0.0, 60.0, 60.0));
    }

    /// A complex tail carries its mask key; a changed key (a geometry-revision
    /// bump) is a changed fingerprint and re-resolves.
    #[test]
    fn complex_chain_carries_and_rekeys_its_mask() {
        let mut store = ClipChainStore::default();
        let key = |rev| ClipMaskKey {
            geometry_revision: rev,
            transform_bucket: 0,
            device_scale_q: 2,
            fill_rule: ClipFillRule::NonZero,
            composition: ClipComposition::Intersect,
        };

        store.begin_frame();
        let (_id, d, _) = store.resolve(&[rect(0.0, 0.0, 40.0, 40.0)], Some(key(1)), 1);
        assert_eq!(d.mask, Some(key(1)));

        // Geometry unchanged → same fingerprint → mask stands.
        store.begin_frame();
        let (_id, _d, changed) = store.resolve(&[rect(0.0, 0.0, 40.0, 40.0)], Some(key(1)), 1);
        assert!(!changed, "unchanged geometry keeps the retained mask");

        // Geometry revision bumped → new fingerprint → re-raster owed.
        store.begin_frame();
        let (_id, d, changed) = store.resolve(&[rect(0.0, 0.0, 40.0, 40.0)], Some(key(2)), 2);
        assert!(changed, "a geometry-revision bump invalidates the mask");
        assert_eq!(d.mask, Some(key(2)));
    }

    /// Chains resolved past the cursor are trimmed when the frame revisits fewer,
    /// mirroring the clip store's shrink discipline.
    #[test]
    fn finish_frame_trims_unrevisited_chains() {
        let mut store = ClipChainStore::default();
        store.begin_frame();
        store.resolve(&[rect(0.0, 0.0, 10.0, 10.0)], None, 1);
        store.resolve(&[rect(0.0, 0.0, 20.0, 20.0)], None, 2);
        assert!(!store.finish_frame(), "all revisited → no trim");
        assert_eq!(store.len(), 2);

        store.begin_frame();
        store.resolve(&[rect(0.0, 0.0, 10.0, 10.0)], None, 1);
        assert!(store.finish_frame(), "one chain went unvisited → trimmed");
        assert_eq!(store.len(), 1);
    }
}
