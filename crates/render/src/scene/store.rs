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
    AnalyticEllipseInstance, AnalyticRRectInstance, GlyphInstance, GlyphInstanceData,
    ImageInstance, MeshVertex, Path, QuadInstance,
};

use super::ids::{BrushId, ClipId, GeometryId, ImageId, MeshId, PathId, PrimitiveId, TransformId};

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
    /// `uv_size` → geometry; `color` (tint) → paint; `texture` → resource.
    pub fn ingest(
        &mut self,
        instance: ImageInstance,
        texture: viso_gpu::TextureId,
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
                resource: prev.texture != texture,
                appended: false,
            };
            if dirty.any() {
                self.entries[index] = ImageEntry { instance, texture };
            }
            dirty
        } else {
            self.entries.push(ImageEntry { instance, texture });
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

/// A retained vector path with its cached tessellation (§8). The tessellation
/// is keyed by the path's geometry and a quality bucket; a transform- or
/// paint-only change reuses the cached vertices/indices rather than re-running
/// `Path::tessellate`.
#[derive(Debug, Clone)]
pub struct PathEntry {
    /// The path outline + paint that produced the cached tessellation. Held so
    /// the diff can detect a geometry/paint change and invalidate the cache.
    pub path: Path,
    /// The quality bucket the tessellation was cached at (a single bucket for
    /// now; higher-quality buckets land with device-scale-aware quality).
    pub quality: u16,
    /// Cached fill+stroke vertices, in the path's own space (origin not yet
    /// subtracted). Absolute-indexed by `indices`, base-zero within this entry.
    pub vertices: Vec<MeshVertex>,
    /// Cached triangle-list indices into `vertices` (base-zero).
    pub indices: Vec<u32>,
}

/// Dense store of vector paths with an in-line retessellation cache (§8). A
/// path whose geometry and quality bucket are unchanged reuses its cached
/// `vertices`/`indices`; `Path::tessellate` runs only on a geometry change
/// (color is baked into the tessellated vertices, so a recolor re-tessellates
/// too, but reports the paint plane).
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

    /// Diff a path against the retained entry at the cursor. Fully equal → cache
    /// hit, no re-tessellation, no bump. `cmds` differ → geometry + re-tessellate.
    /// Same `cmds` but fill/stroke differ → paint + re-tessellate (color is
    /// baked into `MeshVertex`). Cold growth appends + tessellates.
    pub fn ingest(&mut self, path: &Path) -> (PathId, DirtyPlanes) {
        let index = self.cursor;
        let dirty = if index < self.entries.len() {
            let prev = &self.entries[index];
            let geometry = prev.path.cmds != path.cmds;
            let paint =
                !geometry && (prev.path.fill != path.fill || prev.path.stroke != path.stroke);
            if geometry || paint {
                let mut vertices = Vec::new();
                let mut indices = Vec::new();
                path.tessellate(&mut vertices, &mut indices);
                self.entries[index] = PathEntry {
                    path: path.clone(),
                    quality: DEFAULT_QUALITY,
                    vertices,
                    indices,
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
            let mut vertices = Vec::new();
            let mut indices = Vec::new();
            path.tessellate(&mut vertices, &mut indices);
            self.entries.push(PathEntry {
                path: path.clone(),
                quality: DEFAULT_QUALITY,
                vertices,
                indices,
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

/// The single tessellation quality bucket in use. Device-scale-aware
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

/// A retained brush — a resolved fill/stroke paint (§8.5). The store exists so
/// the diff can bump paint independently, and so the freeze pins its shape.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrushEntry {
    /// Straight linear RGBA the brush paints.
    pub color: [f32; 4],
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

    /// Diff a brush against the retained entry at the cursor. A differing color
    /// bumps the paint plane; cold growth appends.
    pub fn ingest(&mut self, color: [f32; 4]) -> (BrushId, bool) {
        let index = self.cursor;
        let changed = if index < self.entries.len() {
            let changed = self.entries[index].color != color;
            if changed {
                self.entries[index] = BrushEntry { color };
            }
            changed
        } else {
            self.entries.push(BrushEntry { color });
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
    /// Slot in the [`ImageStore`].
    Image(ImageId),
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
            StoreRef::Image(_) => write!(f, "image"),
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
