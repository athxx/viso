//! Per-type compact stores for the retained scene (§8, §8.5).
//!
//! Each primitive kind lives in its own dense store — solid quads, images,
//! glyph runs, vector paths, meshes — plus the identity-separated
//! transform/brush/clip stores a later stage bumps independently (§8.5). The
//! stores are SoA/AoS `Vec`s of plain data, never `Vec<Box<dyn …>>`: the whole
//! point is that lowering a frame walks contiguous memory, not a pointer chase.
//!
//! Storage is **cleared, not freed** across frames. `begin_frame` resets every
//! length to zero but keeps the backing capacity, so a steady-state scene of
//! the same shape reuses last frame's allocations and the counting-allocator
//! bench stays flat (§28). A slot is positionally assigned: the Nth quad in the
//! primitive stream is entry N of the quad store, frame after frame, which is
//! the identity the ingest diff (F3.2) relies on.
//!
//! F3.1 uses these as a *shadow*: the immediate walk stays authoritative and
//! the stores are rebuilt each frame from the same primitives, then re-lowered
//! and checked byte-identical to the immediate scratch. The compact shape and
//! the retessellation cache below are what F3.2/F3.3 then make load-bearing.

use crate::primitive::{
    GlyphInstance, GlyphInstanceData, ImageInstance, MeshVertex, Path, QuadInstance,
};

use super::ids::{BrushId, ClipId, GeometryId, ImageId, MeshId, PathId, PrimitiveId, TransformId};

/// A retained solid/bordered quad: the resolved GPU instance plus the identity
/// handles it separates into (§8.5). F3.1 stores the instance directly; F3.2
/// splits mutations across the transform/brush planes via those handles.
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

/// A retained image draw: its lowered instance and the texture it samples.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageEntry {
    /// The lowered instance in world space.
    pub instance: ImageInstance,
    /// The sampled texture (resource plane).
    pub texture: viso_gpu::TextureId,
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

/// Dense store of solid/bordered quads (§8). AoS: one `QuadEntry` per quad, in
/// paint order, addressed by [`crate::scene::ids::PrimitiveId`] via the
/// paint-order record.
#[derive(Debug, Default)]
pub struct SolidQuadStore {
    entries: Vec<QuadEntry>,
}

impl SolidQuadStore {
    /// Clear for a new frame, keeping capacity.
    pub fn begin_frame(&mut self) {
        self.entries.clear();
    }

    /// Positionally append a quad, returning its dense slot handle.
    pub fn push(&mut self, instance: QuadInstance) -> GeometryId {
        let index = self.entries.len() as u32;
        self.entries.push(QuadEntry {
            instance,
            transform: TransformId::new(index),
            brush: BrushId::new(index),
        });
        GeometryId::new(index)
    }

    /// The entry at `id`, or `None` if the slot is out of range.
    pub fn get(&self, id: GeometryId) -> Option<&QuadEntry> {
        self.entries.get(id.index() as usize)
    }

    /// Number of live entries this frame.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store holds no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Dense store of image draws (§8).
#[derive(Debug, Default)]
pub struct ImageStore {
    entries: Vec<ImageEntry>,
}

impl ImageStore {
    /// Clear for a new frame, keeping capacity.
    pub fn begin_frame(&mut self) {
        self.entries.clear();
    }

    /// Positionally append an image, returning its dense slot handle.
    pub fn push(&mut self, instance: ImageInstance, texture: viso_gpu::TextureId) -> ImageId {
        let index = self.entries.len() as u32;
        self.entries.push(ImageEntry { instance, texture });
        ImageId::new(index)
    }

    /// The entry at `id`, or `None` if out of range.
    pub fn get(&self, id: ImageId) -> Option<&ImageEntry> {
        self.entries.get(id.index() as usize)
    }

    /// Number of live entries this frame.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store holds no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Dense store of glyph runs (§8). The runs' glyph instances are packed into
/// one shared `instances` vector (SoA-friendly: one contiguous upload), with
/// each run carrying its `[start, start+count)` range.
#[derive(Debug, Default)]
pub struct GlyphRunStore {
    runs: Vec<GlyphRunEntry>,
    /// All runs' glyph instances, back to back in paint order.
    instances: Vec<GlyphInstance>,
}

impl GlyphRunStore {
    /// Clear for a new frame, keeping capacity.
    pub fn begin_frame(&mut self) {
        self.runs.clear();
        self.instances.clear();
    }

    /// Append a run: push each glyph's lowered instance, then record the range.
    /// `color` is the run color the instance carries. Returns the run's slot.
    pub fn push_run(
        &mut self,
        glyphs: impl Iterator<Item = GlyphInstance>,
        atlas: viso_gpu::TextureId,
    ) -> u32 {
        let start = self.instances.len() as u32;
        self.instances.extend(glyphs);
        let count = self.instances.len() as u32 - start;
        let run = self.runs.len() as u32;
        self.runs.push(GlyphRunEntry {
            start,
            count,
            atlas,
        });
        run
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

/// A retained vector path with its cached tessellation (§8). The tessellation
/// is keyed by the path's geometry and a quality bucket; a transform- or
/// paint-only change reuses the cached vertices/indices rather than re-running
/// `Path::tessellate`.
#[derive(Debug, Clone)]
pub struct PathEntry {
    /// The path outline + paint that produced the cached tessellation. Held so
    /// the diff can detect a geometry change (F3.2) and invalidate the cache.
    pub path: Path,
    /// The quality bucket the tessellation was cached at (F3.1 uses a single
    /// bucket; higher-quality buckets land with device-scale-aware quality).
    pub quality: u16,
    /// Cached fill+stroke vertices, in the path's own space (origin not yet
    /// subtracted). Absolute-indexed by `indices`, base-zero within this entry.
    pub vertices: Vec<MeshVertex>,
    /// Cached triangle-list indices into `vertices` (base-zero).
    pub indices: Vec<u32>,
}

/// Dense store of vector paths with an in-line retessellation cache (§8). A
/// path whose geometry and quality bucket are unchanged reuses its cached
/// `vertices`/`indices`; `Path::tessellate` runs only on a cache miss.
#[derive(Debug, Default)]
pub struct VectorPathStore {
    entries: Vec<PathEntry>,
}

impl VectorPathStore {
    /// Clear for a new frame, keeping capacity.
    ///
    /// F3.1 rebuilds the shadow store each frame, so this clears; F3.2 retains
    /// entries across frames and only re-tessellates on a geometry change,
    /// which is where the cache earns its keep.
    pub fn begin_frame(&mut self) {
        self.entries.clear();
    }

    /// Append a path, tessellating it into the cache at the default quality
    /// bucket. Returns its dense slot handle.
    pub fn push(&mut self, path: &Path) -> PathId {
        let index = self.entries.len() as u32;
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        path.tessellate(&mut vertices, &mut indices);
        self.entries.push(PathEntry {
            path: path.clone(),
            quality: DEFAULT_QUALITY,
            vertices,
            indices,
        });
        PathId::new(index)
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

/// The single tessellation quality bucket F3.1 uses. Device-scale-aware
/// higher-quality buckets are added when path quality is wired to DPI.
pub const DEFAULT_QUALITY: u16 = 0;

/// A caller-supplied triangle mesh, stored as its vertices/indices (no
/// tessellation — a mesh is already triangulated, §8).
#[derive(Debug, Clone)]
pub struct MeshEntry {
    /// The mesh vertices, in the mesh's own space (base-zero indices).
    pub vertices: Vec<MeshVertex>,
    /// Triangle-list indices into `vertices` (base-zero).
    pub indices: Vec<u32>,
}

/// Dense store of caller-supplied meshes (§8).
#[derive(Debug, Default)]
pub struct MeshStore {
    entries: Vec<MeshEntry>,
}

impl MeshStore {
    /// Clear for a new frame, keeping capacity.
    pub fn begin_frame(&mut self) {
        self.entries.clear();
    }

    /// Append a mesh, returning its dense slot handle.
    pub fn push(&mut self, vertices: &[MeshVertex], indices: &[u32]) -> MeshId {
        let index = self.entries.len() as u32;
        self.entries.push(MeshEntry {
            vertices: vertices.to_vec(),
            indices: indices.to_vec(),
        });
        MeshId::new(index)
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

/// Dense store of effective clips referenced by primitives (§8).
#[derive(Debug, Default)]
pub struct ClipStore {
    entries: Vec<ClipEntry>,
}

impl ClipStore {
    /// Clear for a new frame, keeping capacity.
    pub fn begin_frame(&mut self) {
        self.entries.clear();
    }

    /// Append a clip, returning its slot handle.
    pub fn push(&mut self, rect: Option<crate::primitive::Rect>) -> ClipId {
        let index = self.entries.len() as u32;
        self.entries.push(ClipEntry { rect });
        ClipId::new(index)
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

/// A retained affine transform (§8.5). F3.1 carries the world-space origin an
/// emit subtracts; the identity separation lets a pure move bump this store's
/// plane alone in F3.2.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransformEntry {
    /// The world-space origin subtracted from an emit's geometry (a translation
    /// only, for now — the offscreen-layer origin).
    pub origin: [f32; 2],
}

/// Dense store of transforms (§8.5).
#[derive(Debug, Default)]
pub struct TransformStore {
    entries: Vec<TransformEntry>,
}

impl TransformStore {
    /// Clear for a new frame, keeping capacity.
    pub fn begin_frame(&mut self) {
        self.entries.clear();
    }

    /// Append a transform, returning its slot handle.
    pub fn push(&mut self, origin: [f32; 2]) -> TransformId {
        let index = self.entries.len() as u32;
        self.entries.push(TransformEntry { origin });
        TransformId::new(index)
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

/// A retained brush — a resolved fill/stroke paint (§8.5). F3.1 does not need
/// to split paint out of the instance yet; the store exists so the diff can
/// bump paint independently, and so the freeze pins its shape.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrushEntry {
    /// Straight linear RGBA the brush paints.
    pub color: [f32; 4],
}

/// Dense store of brushes (§8.5).
#[derive(Debug, Default)]
pub struct BrushStore {
    entries: Vec<BrushEntry>,
}

impl BrushStore {
    /// Clear for a new frame, keeping capacity.
    pub fn begin_frame(&mut self) {
        self.entries.clear();
    }

    /// Append a brush, returning its slot handle.
    pub fn push(&mut self, color: [f32; 4]) -> BrushId {
        let index = self.entries.len() as u32;
        self.entries.push(BrushEntry { color });
        BrushId::new(index)
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
    /// Slot in the [`ImageStore`].
    Image(ImageId),
    /// Run slot in the [`GlyphRunStore`].
    GlyphRun(u32),
    /// Slot in the [`VectorPathStore`].
    Path(PathId),
    /// Slot in the [`MeshStore`].
    Mesh(MeshId),
    /// A composite draw emitted when a translucent layer closes (an image draw
    /// whose instance is built by the immediate walk's `close_offscreen`). F3.1
    /// records the resolved instance inline so re-derivation reproduces it.
    Composite(ImageInstance),
}

impl std::fmt::Display for StoreRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreRef::Quad(_) => write!(f, "quad"),
            StoreRef::Image(_) => write!(f, "image"),
            StoreRef::GlyphRun(_) => write!(f, "glyph-run"),
            StoreRef::Path(_) => write!(f, "path"),
            StoreRef::Mesh(_) => write!(f, "mesh"),
            StoreRef::Composite(_) => write!(f, "composite"),
        }
    }
}

/// A primitive id is a plain positional index this stage; kept as a helper so
/// the paint-order record and the diff agree on the mapping.
pub fn primitive_id(order_index: usize) -> PrimitiveId {
    PrimitiveId::new(order_index as u32)
}
