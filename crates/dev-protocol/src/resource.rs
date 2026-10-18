//! The resource patch (`Viso_Hot_Reload.md` §22): the assets a revision
//! replaces, and the blobs that carry their bytes.
//!
//! An asset can be far larger than a frame, so its bytes travel apart from the
//! patch naming it: the host sends them as [`BlobChunk`]s, in order, ahead of
//! the patch, and the runtime's [`Blobs`] assembles each by its content hash.
//! The patch's [`ResourceAsset`]s then claim them, each by its path, hash and
//! length, so a blob that went missing, was cut short or holds other bytes
//! refuses the revision instead of showing something else. What no patch
//! claims is dropped with the next patch. A dev snapshot travels the same
//! way, in both directions.

use std::collections::HashMap;
use std::sync::Arc;

use viso_ende::{Decoder, Encoder};
use viso_schema::ResourceId;

use crate::wire::{
    MAX_FILES, MAX_PATH, WireError, content_hash, malformed, read_count, read_list, read_string,
    read_u32, write_string,
};

/// The largest asset a patch carries.
pub const MAX_ASSET: usize = 64 << 20;

/// The most bytes one chunk carries.
pub const BLOB_CHUNK: usize = 1 << 20;

/// The most bytes the blobs waiting for their patch hold together.
pub const MAX_STAGED: usize = 256 << 20;

/// The `resource` section of a patch: each asset it replaces.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResourcePatch {
    pub assets: Vec<ResourceAsset>,
}

/// An asset a patch replaces, and the blob carrying its new bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceAsset {
    /// Its path under `assets/`, normalized: the resource it names.
    pub path: String,
    /// The [`content_hash`] of its bytes.
    pub hash: u64,
    pub len: u32,
}

impl ResourceAsset {
    pub fn id(&self) -> ResourceId {
        ResourceId::of(&self.path)
    }
}

/// A blob, by its content hash and length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlobRef {
    /// The [`content_hash`] of its bytes.
    pub hash: u64,
    pub len: u32,
}

impl BlobRef {
    pub fn of(bytes: &[u8]) -> BlobRef {
        BlobRef {
            hash: content_hash(bytes),
            len: bytes.len() as u32,
        }
    }

    pub(super) fn write(self, enc: &mut Encoder) {
        enc.write_u64(self.hash);
        enc.write_varint(u64::from(self.len));
    }

    pub(super) fn read(dec: &mut Decoder<'_>) -> Result<BlobRef, WireError> {
        let blob = BlobRef {
            hash: dec.read_u64()?,
            len: read_u32(dec)?,
        };
        if blob.len as usize > MAX_STAGED {
            return Err(WireError::Bound { what: "blob" });
        }
        Ok(blob)
    }
}

/// A run of a blob's bytes, at `offset` of its `total`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobChunk {
    /// The [`content_hash`] of the whole blob.
    pub hash: u64,
    pub total: u32,
    pub offset: u32,
    pub bytes: Vec<u8>,
}

/// `bytes` as the chunks that carry them, in order; one empty chunk for no
/// bytes.
pub fn blob_chunks(bytes: &[u8]) -> impl Iterator<Item = BlobChunk> + '_ {
    let hash = content_hash(bytes);
    let total = bytes.len() as u32;
    let count = bytes.len().div_ceil(BLOB_CHUNK).max(1);
    (0..count).map(move |at| {
        let start = at * BLOB_CHUNK;
        let end = (start + BLOB_CHUNK).min(bytes.len());
        BlobChunk {
            hash,
            total,
            offset: start as u32,
            bytes: bytes[start..end].to_vec(),
        }
    })
}

/// The blobs the host sent ahead of the next patch, assembled by hash.
#[derive(Debug, Default)]
pub struct Blobs {
    blobs: HashMap<u64, Blob>,
    /// The bytes the blobs hold together.
    staged: usize,
}

#[derive(Debug)]
enum Blob {
    /// Its total length and the bytes so far.
    Filling(usize, Vec<u8>),
    /// Claimed whole, for every asset of the patch with the same bytes.
    Whole(Arc<[u8]>),
}

impl Blobs {
    /// Adds `chunk` to its blob. A chunk out of order, past its blob's length
    /// or over [`MAX_STAGED`] drops the blob, so the patch claiming it fails.
    pub fn add(&mut self, chunk: BlobChunk) {
        let total = chunk.total as usize;
        let offset = chunk.offset as usize;
        if offset == 0 && !self.blobs.contains_key(&chunk.hash) {
            let bytes = Vec::with_capacity(total.min(BLOB_CHUNK));
            self.blobs.insert(chunk.hash, Blob::Filling(total, bytes));
        }
        let fits = match self.blobs.get(&chunk.hash) {
            Some(Blob::Filling(expected, bytes)) => {
                *expected == total
                    && bytes.len() == offset
                    && offset + chunk.bytes.len() <= total
                    && self.staged + chunk.bytes.len() <= MAX_STAGED
            }
            Some(Blob::Whole(_)) => false,
            None => return,
        };
        if !fits {
            self.drop_blob(chunk.hash);
            return;
        }
        if let Some(Blob::Filling(_, bytes)) = self.blobs.get_mut(&chunk.hash) {
            bytes.extend_from_slice(&chunk.bytes);
            self.staged += chunk.bytes.len();
        }
    }

    /// The bytes of `asset`: a whole blob of its length whose content hashes
    /// to its hash.
    pub fn claim(&mut self, asset: &ResourceAsset) -> Option<Arc<[u8]>> {
        self.take(BlobRef {
            hash: asset.hash,
            len: asset.len,
        })
    }

    /// The bytes `blob` names, whole and hashing to its hash.
    pub fn take(&mut self, blob: BlobRef) -> Option<Arc<[u8]>> {
        let staged = self.blobs.get_mut(&blob.hash)?;
        if let Blob::Filling(total, bytes) = staged {
            if bytes.len() != *total || content_hash(bytes) != blob.hash {
                self.drop_blob(blob.hash);
                return None;
            }
            *staged = Blob::Whole(Arc::from(std::mem::take(bytes)));
        }
        match staged {
            Blob::Whole(bytes) if bytes.len() == blob.len as usize => Some(Arc::clone(bytes)),
            _ => None,
        }
    }

    /// Drops `blob`, its bytes read: the same bytes sent again assemble
    /// anew.
    pub fn release(&mut self, blob: BlobRef) {
        self.drop_blob(blob.hash);
    }

    fn drop_blob(&mut self, hash: u64) {
        if let Some(blob) = self.blobs.remove(&hash) {
            self.staged -= match blob {
                Blob::Filling(_, bytes) => bytes.len(),
                Blob::Whole(bytes) => bytes.len(),
            };
        }
    }

    /// Drops every blob, claimed or not: the patch they came for is staged.
    pub fn clear(&mut self) {
        self.blobs.clear();
        self.staged = 0;
    }
}

pub(super) fn write_resources(enc: &mut Encoder, resources: &ResourcePatch) {
    enc.write_varint(resources.assets.len() as u64);
    for asset in &resources.assets {
        write_string(enc, &asset.path);
        enc.write_u64(asset.hash);
        enc.write_varint(u64::from(asset.len));
    }
}

pub(super) fn read_resources(dec: &mut Decoder<'_>) -> Result<ResourcePatch, WireError> {
    let assets = read_list(dec, MAX_FILES, "patched assets", |dec| {
        let asset = ResourceAsset {
            path: read_string(dec, MAX_PATH, "asset path")?,
            hash: dec.read_u64()?,
            len: read_u32(dec)?,
        };
        if asset.len as usize > MAX_ASSET {
            return Err(WireError::Bound { what: "asset" });
        }
        Ok(asset)
    })?;
    Ok(ResourcePatch { assets })
}

pub(super) fn write_chunk(enc: &mut Encoder, chunk: &BlobChunk) {
    enc.write_u64(chunk.hash);
    enc.write_varint(u64::from(chunk.total));
    enc.write_varint(u64::from(chunk.offset));
    enc.write_varint(chunk.bytes.len() as u64);
    enc.write_raw(&chunk.bytes);
}

pub(super) fn read_chunk(dec: &mut Decoder<'_>) -> Result<BlobChunk, WireError> {
    let hash = dec.read_u64()?;
    let total = read_u32(dec)?;
    let offset = read_u32(dec)?;
    if total as usize > MAX_STAGED {
        return Err(WireError::Bound { what: "blob" });
    }
    let len = read_count(dec, BLOB_CHUNK, "blob chunk")?;
    if offset as usize + len > total as usize {
        return Err(malformed(dec));
    }
    let bytes = dec.read_raw(len)?.to_vec();
    Ok(BlobChunk {
        hash,
        total,
        offset,
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(bytes: &[u8]) -> ResourceAsset {
        ResourceAsset {
            path: "photos/day.png".into(),
            hash: content_hash(bytes),
            len: bytes.len() as u32,
        }
    }

    #[test]
    fn a_blob_split_into_chunks_is_claimed_whole() {
        let bytes: Vec<u8> = (0..BLOB_CHUNK * 2 + 17).map(|i| i as u8).collect();
        let chunks: Vec<BlobChunk> = blob_chunks(&bytes).collect();
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.bytes.len() <= BLOB_CHUNK));
        let mut blobs = Blobs::default();
        for chunk in chunks {
            blobs.add(chunk);
        }
        let claimed = blobs.claim(&asset(&bytes)).expect("whole");
        assert_eq!(*claimed, *bytes);
        let again = blobs
            .claim(&asset(&bytes))
            .expect("an asset of the same bytes");
        assert!(Arc::ptr_eq(&claimed, &again), "shared, not copied");
        blobs.clear();
        assert_eq!(blobs.staged, 0);
        assert_eq!(blobs.claim(&asset(&bytes)), None, "cleared with the patch");

        let mut blobs = Blobs::default();
        blobs.add(blob_chunks(b"").next().unwrap());
        assert_eq!(blobs.claim(&asset(b"")).as_deref(), Some(&[][..]));
    }

    #[test]
    fn a_blob_cut_short_out_of_order_or_of_other_bytes_is_not_claimed() {
        let bytes: Vec<u8> = (0..BLOB_CHUNK + 5).map(|i| (i * 7) as u8).collect();
        let chunks: Vec<BlobChunk> = blob_chunks(&bytes).collect();

        let mut blobs = Blobs::default();
        blobs.add(chunks[0].clone());
        assert_eq!(blobs.claim(&asset(&bytes)), None, "cut short");

        let mut blobs = Blobs::default();
        blobs.add(chunks[1].clone());
        blobs.add(chunks[0].clone());
        blobs.add(chunks[1].clone());
        assert_eq!(blobs.claim(&asset(&bytes)).as_deref(), Some(&bytes[..]));

        let mut blobs = Blobs::default();
        blobs.add(chunks[0].clone());
        blobs.add(chunks[0].clone());
        assert_eq!(blobs.claim(&asset(&bytes)), None, "a repeated chunk");
        assert_eq!(blobs.staged, 0);

        let mut blobs = Blobs::default();
        let mut forged = chunks.clone();
        forged[1].bytes[0] ^= 1;
        for chunk in forged {
            blobs.add(chunk);
        }
        assert_eq!(blobs.claim(&asset(&bytes)), None, "other bytes");
    }
}
