//! The render chunk (§9.6): one contiguous draw the planner emitted, and the
//! unit of incremental rebuild.
//!
//! A [`RenderChunk`] is the maximal run of paint-order-adjacent primitives that
//! share a [`BatchKey`](super::planner::BatchKey) and clip — everything the
//! encoder needs to issue one draw command, plus the paint-order span and
//! revision it was built from. When a later frame changes one primitive, only
//! the chunk covering it is rebuilt; chunks whose key and inputs did not move
//! keep their batch structure untouched.

use super::planner::{BatchFamily, BatchKey};
use crate::primitive::Rect;

/// A render chunk's index into the frame's chunk list — the stable handle
/// architecture section 62 names for `RenderChunkId -> ranges`.
///
/// `RenderChunkId(i)` addresses the `i`-th [`RenderChunk`] the planner emitted,
/// in submission order. It addresses the same contiguous runs as
/// [`BatchId`](crate::BatchId) — one chunk per emitted draw — but the chunk it
/// names uniquely carries the paint-order span an [`InspectBatch`](crate::InspectBatch)
/// lacks, so a consumer can map a changed paint-order position back to the one
/// chunk that must be rebuilt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderChunkId(pub u32);

/// One contiguous draw: the paint-order span it covers, the geometry range it
/// draws, and the packed state that produced it.
///
/// `geometry` is a half-open `(start, count)` in the family buffer selected by
/// `key.family()` — instances for quad/image/glyph families, indices for the
/// mesh family (the same unit convention the instance pools and
/// [`FrameStats`](crate::FrameStats) use). `order` is the half-open span of
/// paint-order positions the chunk absorbed, so a consumer can map a changed
/// primitive back to the one chunk that must be rebuilt.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderChunk {
    /// The packed GPU-state key every primitive in this chunk shares.
    pub key: BatchKey,
    /// The pipeline family this chunk draws through (a decoded view of `key`,
    /// kept inline so the encoder need not unpack the family every command).
    pub family: BatchFamily,
    /// The effective clip rect, or `None` for an unclipped chunk. Held
    /// structurally because a rect is not packed into `key`.
    pub clip: Option<Rect>,
    /// The half-open geometry range `(start, count)` this chunk draws, in the
    /// family buffer `key.family()` selects: instances for quad/image/glyph,
    /// **indices** for mesh.
    pub geometry: (u32, u32),
    /// The half-open span `[start, end)` of paint-order positions this chunk
    /// absorbed. A local change touching position `p` rebuilds the one chunk
    /// whose span contains `p`.
    pub order: (u32, u32),
}

impl RenderChunk {
    /// Open a new chunk at paint-order position `order_start`, drawing `count`
    /// geometry units starting at `geom_start` in its family buffer.
    pub fn open(
        key: BatchKey,
        family: BatchFamily,
        clip: Option<Rect>,
        geom_start: u32,
        count: u32,
        order_start: u32,
    ) -> RenderChunk {
        RenderChunk {
            key,
            family,
            clip,
            geometry: (geom_start, count),
            order: (order_start, order_start + 1),
        }
    }

    /// Absorb an adjacent primitive of `count` more geometry units at the next
    /// paint-order position: grow the geometry range and extend the order span
    /// by one. The caller has already checked the primitive
    /// [`joins`](super::planner::joins) this chunk.
    pub fn absorb(&mut self, count: u32) {
        self.geometry.1 += count;
        self.order.1 += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::super::planner::BatchTarget;
    use super::*;

    #[test]
    fn open_starts_a_one_wide_order_span() {
        let key = BatchKey::pack(BatchFamily::Quad, BatchTarget::Main, None);
        let chunk = RenderChunk::open(key, BatchFamily::Quad, None, 0, 1, 0);
        assert_eq!(chunk.geometry, (0, 1));
        assert_eq!(chunk.order, (0, 1));
    }

    #[test]
    fn absorb_grows_geometry_and_order() {
        let key = BatchKey::pack(BatchFamily::Quad, BatchTarget::Main, None);
        let mut chunk = RenderChunk::open(key, BatchFamily::Quad, None, 4, 1, 2);
        chunk.absorb(1);
        chunk.absorb(1);
        assert_eq!(chunk.geometry, (4, 3));
        assert_eq!(chunk.order, (2, 5));
    }

    #[test]
    fn absorb_sums_mesh_index_counts() {
        // Mesh chunks count indices, so absorbing runs of several indices sums
        // them, while each absorb advances the order span by exactly one.
        let key = BatchKey::pack(BatchFamily::Mesh, BatchTarget::Main, None);
        let mut chunk = RenderChunk::open(key, BatchFamily::Mesh, None, 0, 6, 0);
        chunk.absorb(6);
        assert_eq!(chunk.geometry, (0, 12));
        assert_eq!(chunk.order, (0, 2));
    }
}
