//! A persistent, grow-only device buffer with per-slot dirty upload (§9.1).
//!
//! One pool backs one instance family that shares a pipeline and stride (the
//! quad / image / glyph instance streams, and the mesh vertex / index streams).
//! The buffer is created once and lives for the renderer's lifetime; it grows
//! only when a frame's instance count first exceeds every previous frame's, and
//! growth retires the old buffer through the F1 deferred-destruction path rather
//! than leaking it.
//!
//! # Why a shadow diff
//!
//! The renderer lowers the retained scene into a dense per-family array in draw
//! order every frame. Between two frames where nothing moved, that array is
//! byte-identical; a hover that repaints one primitive changes exactly one
//! element. The pool keeps a CPU shadow of what currently sits in the device
//! buffer and, on [`sync`](InstancePool::sync), compares the freshly lowered
//! array against it element by element. Only the runs that differ are uploaded,
//! each as one contiguous [`write_buffer`]:
//!
//! - an unchanged frame issues **zero** uploads,
//! - a one-slot change issues **one** minimal upload of that slot's bytes,
//! - a full re-fill (first frame, or after a grow) issues one upload of the lot.
//!
//! This is the §9.1 contract: a local paint change costs a local upload, never
//! a full-scene re-upload. The dirty-run merging here is deliberately minimal
//! (adjacent changed slots coalesce into one run); the dedicated dirty-range
//! coalescer (§9.3) refines the merge with a gap threshold in a later step.
//!
//! # Draw-order slots
//!
//! Slot `i` is element `i` of the lowered array — its draw-order position within
//! the family. Because the sole primitive producer re-emits the whole tree in
//! stable order every frame, an unchanged primitive keeps the same slot frame to
//! frame, so its shadow entry matches and it is never re-uploaded. The pool owns
//! *where each element's bytes live persistently*; the renderer's segments still
//! govern draw sequencing, and a segment's contiguous instance range maps
//! directly to a contiguous slot range in this buffer.
//!
//! [`write_buffer`]: viso_gpu::GpuBackend::write_buffer

use std::mem::size_of;

use viso_gpu::{BufferDesc, BufferId, BufferUsage, GpuBackend};

/// A long-lived device buffer for one instance family, uploaded per changed
/// slot against a CPU shadow.
///
/// `T` is the family's element type: a `#[derive(GpuPod)]` instance struct (the
/// quad / image / glyph streams and the mesh vertex stream), or `u32` for the
/// mesh index stream. The bound is `Copy + 'static` POD — see
/// [`gpu_pod_bytes`]'s `SAFETY` — with `PartialEq` used to detect the slots that
/// changed since last frame.
pub struct InstancePool<T> {
    /// The device buffer backing this family, or `None` until the first element
    /// is uploaded (a renderer that never draws this family touches no GPU
    /// memory). Recreated — and the old one retired — only when the buffer must
    /// grow.
    buffer: Option<BufferId>,
    /// Capacity of `buffer` in elements (its high-water mark). Zero when
    /// `buffer` is `None`.
    capacity: usize,
    /// A CPU mirror of the device buffer's live prefix: `shadow[i]` is the value
    /// currently stored in device slot `i`. Its length is the number of slots
    /// uploaded last frame. Retained across frames so a steady frame allocates
    /// nothing.
    shadow: Vec<T>,
    /// How this family's buffer is used (`INSTANCE`, `VERTEX`, or `INDEX`), OR-ed
    /// with `CPU_WRITE`. Fixed at construction.
    usage: BufferUsage,
    /// A debug label for the device buffer (GPU tooling; ignored by headless).
    label: &'static str,
}

impl<T: Copy + PartialEq + 'static> InstancePool<T> {
    /// Byte size of one element.
    const STRIDE: usize = size_of::<T>();

    /// A new, empty pool for a family used as `usage` (the base usage such as
    /// [`BufferUsage::INSTANCE`]; `CPU_WRITE` is added automatically). No device
    /// buffer is created until the first non-empty [`sync`](Self::sync).
    pub fn new(usage: BufferUsage, label: &'static str) -> Self {
        Self {
            buffer: None,
            capacity: 0,
            shadow: Vec::new(),
            usage: usage | BufferUsage::CPU_WRITE,
            label,
        }
    }

    /// The device buffer backing this family, if one has been created.
    ///
    /// `None` before the first non-empty [`sync`](Self::sync). Callers encode
    /// draws against this handle; slot `i` sits at byte offset `i * stride`.
    pub fn buffer(&self) -> Option<BufferId> {
        self.buffer
    }

    /// Capacity of the device buffer in elements (its high-water mark).
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Number of slots currently live in the device buffer (last frame's count).
    pub fn len(&self) -> usize {
        self.shadow.len()
    }

    /// Whether the pool holds no live slots.
    pub fn is_empty(&self) -> bool {
        self.shadow.is_empty()
    }

    /// Reconcile the device buffer with `instances`, the family's freshly
    /// lowered draw-order array, uploading only the slots that changed.
    ///
    /// Returns the number of `write_buffer` calls issued this frame (0 when the
    /// array is byte-identical to last frame) — used by counters and the
    /// steady-state test to prove a local change stays a local upload.
    ///
    /// The buffer grows (cold path, one buffer create + one retire of the old
    /// buffer) when `instances.len()` first exceeds the capacity; a grow forces
    /// a single full upload of the new contents. Otherwise the array is diffed
    /// against the shadow and each maximal run of changed slots is uploaded as
    /// one contiguous range.
    pub fn sync<B: GpuBackend>(&mut self, backend: &mut B, instances: &[T]) -> usize {
        if instances.is_empty() {
            // Nothing to draw this frame. The device buffer and shadow are
            // retained (no shrink); the renderer simply draws no slots. Truncate
            // the shadow so a later frame that re-adds slots diffs against an
            // empty prefix and uploads them.
            self.shadow.clear();
            return 0;
        }

        if instances.len() > self.capacity {
            self.grow(backend, instances.len());
            self.upload_run(backend, 0, instances);
            self.shadow.clear();
            self.shadow.extend_from_slice(instances);
            return 1;
        }

        // Within capacity: diff element by element against the shadow, uploading
        // each maximal run of changed slots. Slots beyond the shadow's current
        // length are new this frame and always count as changed.
        let mut writes = 0;
        let mut i = 0;
        while i < instances.len() {
            let changed = i >= self.shadow.len() || self.shadow[i] != instances[i];
            if !changed {
                i += 1;
                continue;
            }
            // Extend the run while slots keep differing.
            let start = i;
            while i < instances.len() && (i >= self.shadow.len() || self.shadow[i] != instances[i])
            {
                i += 1;
            }
            self.upload_run(backend, start, &instances[start..i]);
            writes += 1;
        }

        // The shadow now mirrors exactly `instances`.
        self.shadow.clear();
        self.shadow.extend_from_slice(instances);
        writes
    }

    /// Grow the device buffer to hold at least `needed` elements, retiring the
    /// old buffer through the backend's deferred-destruction path.
    ///
    /// Cold path only (a frame whose instance count first exceeds the high-water
    /// mark). Capacity rounds up to a power of two so growth is amortized.
    #[cold]
    fn grow<B: GpuBackend>(&mut self, backend: &mut B, needed: usize) {
        let new_cap = needed.next_power_of_two();
        if let Some(old) = self.buffer.take() {
            // Deferred/epoch-reclaimed: the old buffer may still be read by an
            // in-flight frame, so the backend retires it and frees it once its
            // epoch passes — never leaked, never freed early.
            backend.destroy_buffer(old);
        }
        self.buffer = Some(backend.create_buffer(&BufferDesc {
            size: new_cap * Self::STRIDE,
            usage: self.usage,
            label: self.label,
        }));
        self.capacity = new_cap;
        // The freshly created buffer holds no valid slots; force the caller's
        // full re-upload by dropping the shadow.
        self.shadow.clear();
    }

    /// Upload `run` (a contiguous slice of elements) starting at slot `start`.
    fn upload_run<B: GpuBackend>(&self, backend: &mut B, start: usize, run: &[T]) {
        let buffer = self.buffer.expect("buffer created before upload");
        backend.write_buffer(buffer, start * Self::STRIDE, gpu_pod_bytes(run));
    }
}

/// View a slice of POD elements as the raw bytes to upload.
fn gpu_pod_bytes<T: Copy + 'static>(elems: &[T]) -> &[u8] {
    // SAFETY: every `T` a pool is instantiated with is a plain-old-data element
    // whose in-memory bytes are exactly its device-buffer representation — the
    // four instance structs are `#[derive(GpuPod)]` (`#[repr(C)]`, POD scalars,
    // no padding surprises) and the index stream is `u32`. Reinterpreting the
    // element slice as bytes reads initialized memory only; the returned view
    // lives as long as the borrow of `elems` and is read-only.
    unsafe {
        core::slice::from_raw_parts(elems.as_ptr() as *const u8, core::mem::size_of_val(elems))
    }
}
