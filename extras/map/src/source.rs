//! Where tiles come from: a [`TileSource`] answers a request for one tile of
//! the XYZ pyramid with its encoded bytes, later, and may be told to drop a
//! request the view no longer needs.
//!
//! The bytes are whatever the source's layers read — a Mapbox Vector Tile
//! (gzipped or not), a PNG, JPEG or WebP raster, or an encoded elevation
//! tile. `Ok(None)` means the pyramid has no tile there (an empty ocean
//! square, a region outside an extract): the view draws the background and
//! never asks again. An error is retried on a later camera change.
//!
//! Any closure from a [`TileId`] to a future of the bytes (a [`Reply`], an
//! `async` block, another crate's reply type) is a source, so a reader of
//! PMTiles, MBTiles or HTTP plugs in with one line; [`MemorySource`] serves
//! tiles held in memory, for tests and bundled data.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;

pub use viso_services::{Completer, Reply, ServiceError, ServiceResult, reply};

/// A tile of the XYZ (slippy map) pyramid: zoom `z`, column `x` from the
/// antimeridian eastward, row `y` from the north edge southward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileId {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileId {
    /// The tile `(z, x, y)`.
    pub const fn new(z: u8, x: u32, y: u32) -> Self {
        Self { z, x, y }
    }

    /// The tiles across one row (and down one column) at zoom `z`.
    pub const fn span(z: u8) -> u32 {
        1 << z
    }

    /// Whether the tile lies inside its zoom's pyramid.
    pub fn is_valid(self) -> bool {
        self.z < 32 && self.x < Self::span(self.z) && self.y < Self::span(self.z)
    }

    /// The tile one zoom up that holds this one; `None` at zoom zero.
    pub fn parent(self) -> Option<Self> {
        (self.z > 0).then(|| Self::new(self.z - 1, self.x / 2, self.y / 2))
    }

    /// The tile `levels` zooms up that holds this one (itself at zero).
    pub fn ancestor(self, levels: u8) -> Self {
        let levels = levels.min(self.z);
        Self::new(self.z - levels, self.x >> levels, self.y >> levels)
    }

    /// The four tiles one zoom down that this one holds, row by row.
    pub fn children(self) -> [Self; 4] {
        let (z, x, y) = (self.z + 1, self.x * 2, self.y * 2);
        [
            Self::new(z, x, y),
            Self::new(z, x + 1, y),
            Self::new(z, x, y + 1),
            Self::new(z, x + 1, y + 1),
        ]
    }

    /// Whether `other` is this tile or lies inside it.
    pub fn contains(self, other: Self) -> bool {
        other.z >= self.z && other.ancestor(other.z - self.z) == self
    }
}

impl std::fmt::Display for TileId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}/{}", self.z, self.x, self.y)
    }
}

/// A tile's encoded bytes, or `None` where the pyramid has no tile.
/// Dropping it drops the request.
pub type TileReply = Pin<Box<dyn Future<Output = ServiceResult<Option<Vec<u8>>>>>>;

/// Something that answers tile requests: a file reader, a network client, a
/// generator, a table in memory.
///
/// Requests come from the loop thread, nearest the view's center first; a
/// source may answer from any thread. When the camera moves on before an
/// answer arrives, the view drops its reply and calls [`cancel`]
/// (`TileSource::cancel`), so a source that queues work can skip it.
pub trait TileSource: 'static {
    /// Begin fetching `tile`.
    fn request(&self, tile: TileId) -> TileReply;

    /// The view no longer wants `tile`. A hint: the reply is already
    /// dropped, and answering it anyway is harmless.
    fn cancel(&self, tile: TileId) {
        let _ = tile;
    }
}

impl<F, R> TileSource for F
where
    F: Fn(TileId) -> R + 'static,
    R: Future<Output = ServiceResult<Option<Vec<u8>>>> + 'static,
{
    fn request(&self, tile: TileId) -> TileReply {
        Box::pin(self(tile))
    }
}

impl<S: TileSource + ?Sized> TileSource for Rc<S> {
    fn request(&self, tile: TileId) -> TileReply {
        (**self).request(tile)
    }

    fn cancel(&self, tile: TileId) {
        (**self).cancel(tile);
    }
}

/// Tiles held in memory, answered at once.
#[derive(Debug, Clone, Default)]
pub struct MemorySource {
    tiles: HashMap<TileId, Arc<[u8]>>,
}

impl MemorySource {
    /// A source with no tiles.
    pub fn new() -> Self {
        Self::default()
    }

    /// Hold `bytes` as `tile`, replacing what it held.
    pub fn insert(&mut self, tile: TileId, bytes: impl Into<Arc<[u8]>>) {
        self.tiles.insert(tile, bytes.into());
    }

    /// The source with `bytes` held as `tile`.
    pub fn with(mut self, tile: TileId, bytes: impl Into<Arc<[u8]>>) -> Self {
        self.insert(tile, bytes);
        self
    }

    /// The tiles held.
    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    /// Whether no tile is held.
    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }
}

impl TileSource for MemorySource {
    fn request(&self, tile: TileId) -> TileReply {
        Box::pin(Reply::ready(Ok(self
            .tiles
            .get(&tile)
            .map(|bytes| bytes.to_vec()))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tile_knows_its_family() {
        let t = TileId::new(3, 5, 2);
        assert_eq!(t.parent(), Some(TileId::new(2, 2, 1)));
        assert_eq!(t.ancestor(3), TileId::new(0, 0, 0));
        assert_eq!(t.ancestor(9), TileId::new(0, 0, 0));
        assert!(t.children().iter().all(|c| t.contains(*c)));
        assert!(!TileId::new(3, 4, 2).contains(t));
        assert!(t.is_valid() && !TileId::new(1, 2, 0).is_valid());
        assert_eq!(TileId::new(0, 0, 0).parent(), None);
        assert_eq!(t.to_string(), "3/5/2");
    }

    /// The answer, if it is there at once.
    fn now(mut reply: TileReply) -> Option<ServiceResult<Option<Vec<u8>>>> {
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        match reply.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(r) => Some(r),
            std::task::Poll::Pending => None,
        }
    }

    #[test]
    fn closures_and_memory_answer_requests() {
        let tile = TileId::new(1, 1, 0);
        let memory = MemorySource::new().with(tile, vec![1u8, 2, 3]);
        assert_eq!(now(memory.request(tile)), Some(Ok(Some(vec![1, 2, 3]))));
        assert_eq!(now(memory.request(TileId::new(0, 0, 0))), Some(Ok(None)));

        let closure = |t: TileId| Reply::ready(Ok((t.z == 1).then(|| vec![t.x as u8])));
        let source: Rc<dyn TileSource> = Rc::new(closure);
        assert_eq!(now(source.request(tile)), Some(Ok(Some(vec![1]))));
        source.cancel(tile);
        let later = |_: TileId| async { Ok(None) };
        assert_eq!(now(later.request(tile)), Some(Ok(None)));
    }
}
