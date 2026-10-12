//! The assets a node store's nodes show, by [`ResourceId`].
//!
//! `viso-ui` decodes nothing and owns no GPU device, so a resource passes
//! through three states here, each filled by the tier that can:
//!
//! 1. **Offered**: a view mounting a node that shows an asset offers the
//!    asset's encoded bytes ([`AssetTable::offer`]), or, holding none, asks
//!    for it ([`AssetTable::want`]) for the facade to offer the bytes the app
//!    holds.
//! 2. **Decoded**: the facade decodes what is offered — once per resource
//!    revision for the whole app — and provides the payload
//!    ([`NodeStore::provide_asset`](crate::component::NodeStore::provide_asset)).
//!    A vector payload draws from here; its nodes resolve at once.
//! 3. **Resident**: for a raster, the facade uploads the pixels into the
//!    window's renderer and attaches the texture
//!    ([`NodeStore::attach_texture`](crate::component::NodeStore::attach_texture)),
//!    which resolves the nodes, drops the table's hold on the pixels (the
//!    texture holds them now) and hands back the texture it replaced, for the
//!    facade to release once no frame in flight draws it.
//!
//! A node's [`Content::Asset`](crate::content::Content::Asset) carries what it
//! draws, so paint reads no table. Providing a new payload rewrites only the
//! nodes showing that resource: `PAINT` when the natural size holds, `MEASURE |
//! LAYOUT | PAINT` when it moves. Every change bumps the table's
//! [`revision`](AssetTable::revision); the facade compares it against the one it
//! last served and does nothing on a frame it did not move.

use std::collections::HashMap;
use std::sync::Arc;

use viso_render::TextureId;
use viso_render::asset::{AssetKind, RasterImage, ResourceId, ResourceRevision, VectorScene};

/// What a node showing an asset draws.
#[derive(Debug, Clone, PartialEq)]
pub enum AssetDraw {
    /// Nothing yet: the asset is not decoded, or its raster not resident.
    Nothing,
    Raster(TextureId),
    Vector(Arc<VectorScene>),
}

/// How an asset fills its node's box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AssetFit {
    /// Scaled to fit inside the box, keeping its aspect, centered.
    #[default]
    Contain,
    /// Scaled to cover the box, keeping its aspect, centered and cropped.
    Cover,
    /// Stretched to the box.
    Fill,
}

/// An asset's encoded bytes: embedded in the binary, or loaded and shared.
#[derive(Clone)]
pub enum AssetBytes {
    Static(&'static [u8]),
    Shared(Arc<[u8]>),
}

impl std::ops::Deref for AssetBytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            AssetBytes::Static(bytes) => bytes,
            AssetBytes::Shared(bytes) => bytes,
        }
    }
}

impl PartialEq for AssetBytes {
    fn eq(&self, other: &AssetBytes) -> bool {
        **self == **other
    }
}

impl Eq for AssetBytes {}

impl std::fmt::Debug for AssetBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AssetBytes({} bytes)", self.len())
    }
}

/// An asset a Rust view shows, as `asset!("…")` embeds it: its resource, how
/// it draws and its encoded bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub id: ResourceId,
    pub kind: AssetKind,
    pub bytes: AssetBytes,
}

/// A decoded asset.
#[derive(Debug, Clone)]
pub enum AssetPayload {
    Raster(Arc<RasterImage>),
    Vector(Arc<VectorScene>),
}

impl AssetPayload {
    /// Its natural size in pixels.
    pub fn size(&self) -> [f32; 2] {
        match self {
            AssetPayload::Raster(image) => image.size(),
            AssetPayload::Vector(scene) => scene.size,
        }
    }
}

/// One resource of the table.
#[derive(Debug, Clone)]
pub(crate) struct AssetEntry {
    /// The revision of the newest bytes offered.
    pub(crate) offered: ResourceRevision,
    /// Those bytes, until a payload of their revision is provided.
    pub(crate) bytes: Option<AssetBytes>,
    /// The newest payload provided, and its revision: a vector's for as long
    /// as it draws, a raster's until its texture is attached.
    pub(crate) payload: Option<(ResourceRevision, AssetPayload)>,
    /// A raster's texture in this store's window, the revision it was uploaded
    /// from and that raster's size; it draws until the newest payload's
    /// replaces it.
    pub(crate) texture: Option<(ResourceRevision, TextureId, [f32; 2])>,
}

impl AssetEntry {
    fn new(offered: ResourceRevision) -> AssetEntry {
        AssetEntry {
            offered,
            bytes: None,
            payload: None,
            texture: None,
        }
    }

    /// The newest revision decoded: provided, or resident.
    fn decoded(&self) -> Option<ResourceRevision> {
        let payload = self.payload.as_ref().map(|(r, _)| *r);
        let texture = self.texture.map(|(r, ..)| r);
        payload.max(texture)
    }
}

/// The assets of one node store.
#[derive(Debug, Default)]
pub struct AssetTable {
    entries: HashMap<ResourceId, AssetEntry>,
    revision: u64,
}

impl AssetTable {
    /// Offers the encoded bytes of `id` at `revision`, to decode. Bytes no newer
    /// than what the table holds are ignored, so every node mounting the same
    /// asset offers it for free.
    pub fn offer(&mut self, id: ResourceId, revision: ResourceRevision, bytes: AssetBytes) {
        let entry = self
            .entries
            .entry(id)
            .or_insert_with(|| AssetEntry::new(revision));
        let newer = entry.decoded().is_none_or(|r| revision > r)
            && (entry.bytes.is_none() || revision > entry.offered);
        if newer {
            entry.offered = revision;
            entry.bytes = Some(bytes);
            self.revision += 1;
        }
    }

    /// Records that a node shows `id` while no bytes of it are at hand, for
    /// the facade to offer what the app holds.
    pub fn want(&mut self, id: ResourceId) {
        if let std::collections::hash_map::Entry::Vacant(entry) = self.entries.entry(id) {
            entry.insert(AssetEntry::new(ResourceRevision::default()));
            self.revision += 1;
        }
    }

    /// Every resource a node shows that the table holds nothing of: no bytes,
    /// no payload, no texture.
    pub fn wanted(&self) -> impl Iterator<Item = ResourceId> + '_ {
        self.entries.iter().filter_map(|(id, e)| {
            (e.bytes.is_none() && e.payload.is_none() && e.texture.is_none()).then_some(*id)
        })
    }

    /// Every resource whose newest bytes are not decoded yet: its id, revision
    /// and bytes.
    pub fn undecoded(&self) -> impl Iterator<Item = (ResourceId, ResourceRevision, &AssetBytes)> {
        self.entries
            .iter()
            .filter_map(|(id, e)| Some((*id, e.offered, e.bytes.as_ref()?)))
    }

    /// Every raster with no texture in this window yet: its id, revision and
    /// pixels.
    pub fn unresident(
        &self,
    ) -> impl Iterator<Item = (ResourceId, ResourceRevision, &Arc<RasterImage>)> {
        self.entries.iter().filter_map(|(id, e)| match &e.payload {
            Some((revision, AssetPayload::Raster(image)))
                if e.texture.is_none_or(|(r, ..)| r != *revision) =>
            {
                Some((*id, *revision, image))
            }
            _ => None,
        })
    }

    /// Whether the table holds `id` in any state.
    pub fn knows(&self, id: ResourceId) -> bool {
        self.entries.contains_key(&id)
    }

    /// The payload of `id` not yet resident, and its revision.
    pub fn payload(&self, id: ResourceId) -> Option<(ResourceRevision, &AssetPayload)> {
        let (revision, payload) = self.entries.get(&id)?.payload.as_ref()?;
        Some((*revision, payload))
    }

    /// The texture of `id` in this window, and the revision it was uploaded
    /// from.
    pub fn texture(&self, id: ResourceId) -> Option<(ResourceRevision, TextureId)> {
        let (revision, texture, _) = self.entries.get(&id)?.texture?;
        Some((revision, texture))
    }

    /// Moves with every change of what the table holds.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// How many resources the table knows.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table knows no resource.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// What a node showing `id` draws now, and its natural size: a vector
    /// from its payload; a raster from its resident texture (the old pixels
    /// until the newest are uploaded), or nothing but its size before any is.
    pub(crate) fn resolve(&self, id: ResourceId) -> (AssetDraw, [f32; 2]) {
        let Some(entry) = self.entries.get(&id) else {
            return (AssetDraw::Nothing, [0.0; 2]);
        };
        match (&entry.payload, entry.texture) {
            (Some((_, AssetPayload::Vector(scene))), _) => {
                (AssetDraw::Vector(Arc::clone(scene)), scene.size)
            }
            (_, Some((_, texture, size))) => (AssetDraw::Raster(texture), size),
            (Some((_, payload)), None) => (AssetDraw::Nothing, payload.size()),
            (None, None) => (AssetDraw::Nothing, [0.0; 2]),
        }
    }

    /// Stores `payload` as `id`'s at `revision`, dropping the bytes it was
    /// decoded from; `false` when the table already decoded one as new. The
    /// previous texture stays until the new payload's is attached, so the old
    /// pixels draw until the new ones are resident.
    pub(crate) fn provide(
        &mut self,
        id: ResourceId,
        revision: ResourceRevision,
        payload: AssetPayload,
    ) -> bool {
        let entry = self
            .entries
            .entry(id)
            .or_insert_with(|| AssetEntry::new(revision));
        if entry.decoded().is_some_and(|r| r >= revision) {
            return false;
        }
        if entry.offered <= revision {
            entry.bytes = None;
            entry.offered = revision;
        }
        entry.payload = Some((revision, payload));
        self.revision += 1;
        true
    }

    /// Records `texture` as `id`'s at `revision` and lets go of the pixels it
    /// was uploaded from; the texture it replaces, or `texture` itself when
    /// the table has moved past `revision` (the caller releases whichever comes
    /// back).
    pub(crate) fn attach(
        &mut self,
        id: ResourceId,
        revision: ResourceRevision,
        texture: TextureId,
    ) -> Result<Option<TextureId>, TextureId> {
        let Some(entry) = self.entries.get_mut(&id) else {
            return Err(texture);
        };
        let size = match &entry.payload {
            Some((r, AssetPayload::Raster(image))) if *r == revision => image.size(),
            _ => return Err(texture),
        };
        let old = entry.texture.replace((revision, texture, size));
        entry.payload = None;
        self.revision += 1;
        Ok(old.map(|(_, t, _)| t))
    }

    pub(crate) fn clear(&mut self) {
        if !self.entries.is_empty() {
            self.revision += 1;
        }
        self.entries.clear();
    }
}
