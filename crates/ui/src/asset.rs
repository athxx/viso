//! The assets a node store's nodes show, by [`ResourceId`].
//!
//! `viso-ui` decodes nothing and owns no GPU device, so a resource passes
//! through three states here, each filled by the tier that can:
//!
//! 1. **Offered**: a view mounting a node that shows an asset offers the
//!    asset's encoded bytes ([`AssetTable::offer`]).
//! 2. **Decoded**: the facade decodes what is offered — once per resource
//!    revision for the whole app — and provides the payload
//!    ([`NodeStore::provide_asset`](crate::component::NodeStore::provide_asset)).
//!    A vector payload draws from here; its nodes resolve at once.
//! 3. **Resident**: for a raster, the facade uploads the pixels into the
//!    window's renderer and attaches the texture
//!    ([`NodeStore::attach_texture`](crate::component::NodeStore::attach_texture)),
//!    which resolves the nodes and hands back the texture it replaced, for the
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
use viso_render::asset::{RasterImage, ResourceId, ResourceRevision, VectorScene};

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
    pub(crate) bytes: Option<Arc<[u8]>>,
    /// The newest payload provided, and its revision.
    pub(crate) payload: Option<(ResourceRevision, AssetPayload)>,
    /// A raster's texture in this store's window, the revision it was uploaded
    /// from and that raster's size; it draws until the newest payload's
    /// replaces it.
    pub(crate) texture: Option<(ResourceRevision, TextureId, [f32; 2])>,
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
    pub fn offer(&mut self, id: ResourceId, revision: ResourceRevision, bytes: Arc<[u8]>) {
        let entry = self.entries.entry(id).or_insert_with(|| AssetEntry {
            offered: revision,
            bytes: None,
            payload: None,
            texture: None,
        });
        let newer = entry.payload.as_ref().is_none_or(|(r, _)| revision > *r)
            && (entry.bytes.is_none() || revision > entry.offered);
        if newer {
            entry.offered = revision;
            entry.bytes = Some(bytes);
            self.revision += 1;
        }
    }

    /// Every resource whose newest bytes are not decoded yet: its id, revision
    /// and bytes.
    pub fn undecoded(&self) -> impl Iterator<Item = (ResourceId, ResourceRevision, &Arc<[u8]>)> {
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

    /// Every texture the table holds, to release when the window's renderer
    /// goes away or is rebuilt; the table forgets them.
    pub fn take_textures(&mut self) -> Vec<TextureId> {
        let textures: Vec<TextureId> = self
            .entries
            .values_mut()
            .filter_map(|e| e.texture.take().map(|(_, t, _)| t))
            .collect();
        if !textures.is_empty() {
            self.revision += 1;
        }
        textures
    }

    /// The payload of `id`, and its revision.
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
    /// decoded from; `false` when the table already holds a newer one. The
    /// previous texture stays until the new payload's is attached, so the old
    /// pixels draw until the new ones are resident.
    pub(crate) fn provide(
        &mut self,
        id: ResourceId,
        revision: ResourceRevision,
        payload: AssetPayload,
    ) -> bool {
        let entry = self.entries.entry(id).or_insert_with(|| AssetEntry {
            offered: revision,
            bytes: None,
            payload: None,
            texture: None,
        });
        if entry.payload.as_ref().is_some_and(|(r, _)| *r >= revision) {
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

    /// Records `texture` as `id`'s at `revision`; the texture it replaces, or
    /// `texture` itself when the table has moved past `revision` (the caller
    /// releases whichever comes back).
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
