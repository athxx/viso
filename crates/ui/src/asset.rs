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
//! draws, so paint reads no table.
//!
//! The table counts the nodes showing each resource — a brush sampling it
//! counts too. One no node shows any more stays decoded, its texture resident,
//! for a node that shows it again; [`sweep`](AssetTable::sweep) keeps what
//! those hold within a budget, letting go of the least recently shown first,
//! and forgets the ones holding nothing worth keeping. Providing a new payload rewrites only the
//! nodes showing that resource: `PAINT` when the natural size holds, `MEASURE |
//! LAYOUT | PAINT` when it moves. Every change bumps the table's
//! [`revision`](AssetTable::revision); the facade compares it against the one it
//! last served and does nothing on a frame it did not move.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use viso_render::TextureId;
use viso_render::asset::{AssetKind, RasterImage, ResourceId, ResourceRevision, VectorScene};

pub use viso_schema::resource::AssetBytes;
use viso_schema::resource::AssetFormat;

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

/// An asset a Rust view shows: its resource, how it draws and where its
/// encoded bytes come from — embedded with `asset!("…")`, given at run time
/// ([`Asset::bytes`]) or read from a file ([`Asset::file`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub id: ResourceId,
    pub kind: AssetKind,
    pub source: AssetSource,
}

/// Where an asset's encoded bytes come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetSource {
    /// At hand: embedded, or given at run time.
    Bytes(AssetBytes),
    /// A file, read off the frame path when a node first shows it.
    File(Arc<Path>),
}

/// The resources [`Asset::bytes`] numbers, apart from any a path names.
const GIVEN: u64 = 0xb17e_5000_0000_0000;

impl Asset {
    /// An image in `bytes`, any format an asset may be in. Each call is a
    /// resource of its own: keep the asset to show the image again without
    /// decoding it again.
    pub fn bytes(bytes: impl Into<Arc<[u8]>>) -> Asset {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let bytes = AssetBytes::Shared(bytes.into());
        let kind = AssetFormat::sniff(&bytes).map_or(AssetKind::Raster, AssetFormat::kind);
        Asset {
            id: ResourceId(GIVEN | NEXT.fetch_add(1, Ordering::Relaxed)),
            kind,
            source: AssetSource::Bytes(bytes),
        }
    }

    /// The image in the file at `path`, read and decoded off the frame path
    /// when a node first shows it. The path is the resource: the same path
    /// shows what was read of it while any table keeps it.
    pub fn file(path: impl Into<PathBuf>) -> Asset {
        let path: PathBuf = path.into();
        let svg = path
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("svg"));
        Asset {
            id: ResourceId::of(&format!("file:{}", path.display())),
            kind: if svg {
                AssetKind::Vector
            } else {
                AssetKind::Raster
            },
            source: AssetSource::File(path.into()),
        }
    }
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

/// How far along a resource of a table is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AssetState {
    /// A node shows it; no bytes of it are at hand.
    Wanted,
    /// Its encoded bytes wait to decode.
    Encoded { bytes: u64 },
    /// A raster decoded, not uploaded yet, of `size` pixels.
    Decoded { size: [f32; 2] },
    /// A raster resident as a texture of `size` pixels.
    Resident { size: [f32; 2] },
    /// A vector scene of `size`, drawn as paths.
    Vector { size: [f32; 2] },
}

/// One resource of the table.
#[derive(Debug, Clone)]
pub(crate) struct AssetEntry {
    /// The revision of the newest bytes offered.
    pub(crate) offered: ResourceRevision,
    /// Those bytes, until a payload of their revision is provided.
    pub(crate) bytes: Option<AssetBytes>,
    /// The file its bytes are read from, when they are not at hand.
    pub(crate) file: Option<Arc<Path>>,
    /// The newest payload provided, and its revision: a vector's for as long
    /// as it draws, a raster's until its texture is attached.
    pub(crate) payload: Option<(ResourceRevision, AssetPayload)>,
    /// A raster's texture in this store's window; it draws until the newest
    /// payload's replaces it.
    pub(crate) texture: Option<Resident>,
    /// How many nodes show it.
    pub(crate) users: u32,
    /// When it was last shown, on the table's clock.
    pub(crate) shown: u64,
}

/// A raster's texture, the revision it was uploaded from, that raster's size
/// and the bytes the texture takes.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Resident {
    pub(crate) revision: ResourceRevision,
    pub(crate) texture: TextureId,
    pub(crate) size: [f32; 2],
    pub(crate) bytes: u64,
}

impl AssetEntry {
    fn new(offered: ResourceRevision) -> AssetEntry {
        AssetEntry {
            offered,
            bytes: None,
            file: None,
            payload: None,
            texture: None,
            users: 0,
            shown: 0,
        }
    }

    /// The bytes it holds that a node showing it again draws from at once: its
    /// texture's, or its vector scene's.
    fn kept_bytes(&self) -> u64 {
        match (&self.texture, &self.payload) {
            (Some(t), _) => t.bytes,
            (None, Some((_, AssetPayload::Vector(scene)))) => scene.bytes(),
            _ => 0,
        }
    }

    /// Whether it holds no bytes, no payload and no texture.
    fn holds_nothing(&self) -> bool {
        self.bytes.is_none() && self.payload.is_none() && self.texture.is_none()
    }

    /// The newest revision decoded: provided, or resident.
    fn decoded(&self) -> Option<ResourceRevision> {
        let payload = self.payload.as_ref().map(|(r, _)| *r);
        let texture = self.texture.map(|t| t.revision);
        payload.max(texture)
    }
}

/// The assets of one node store.
#[derive(Debug, Default)]
pub struct AssetTable {
    entries: HashMap<ResourceId, AssetEntry>,
    revision: u64,
    /// The bytes of the textures attached, in the formats they are stored in.
    texture_bytes: u64,
    /// Ticks once each time a resource loses its last node.
    clock: u64,
    /// Whether a resource lost its last node, or one gained something to
    /// keep, since the last sweep.
    unswept: bool,
    /// The budget and texture bytes of the last sweep.
    swept: (u64, u64),
    /// Reused by a sweep: the resources no node shows, least recently shown
    /// first, and what each keeps.
    idle: Vec<(u64, ResourceId, u64)>,
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

    /// Records that a node shows `id`, read from the file at `path`.
    pub fn locate(&mut self, id: ResourceId, path: Arc<Path>) {
        let entry = self
            .entries
            .entry(id)
            .or_insert_with(|| AssetEntry::new(ResourceRevision::default()));
        if entry.file.is_none() {
            entry.file = Some(path);
            self.revision += 1;
        }
    }

    /// Every resource a node shows that the table holds nothing of — no
    /// bytes, no payload, no texture — nor knows a file of.
    pub fn wanted(&self) -> impl Iterator<Item = ResourceId> + '_ {
        self.entries
            .iter()
            .filter_map(|(id, e)| (e.file.is_none() && e.holds_nothing()).then_some(*id))
    }

    /// Every resource to read from its file: one the table holds nothing of
    /// yet, with its revision and the file.
    pub fn unread(&self) -> impl Iterator<Item = (ResourceId, ResourceRevision, &Arc<Path>)> {
        self.entries.iter().filter_map(|(id, e)| match &e.file {
            Some(path) if e.holds_nothing() => Some((*id, e.offered, path)),
            _ => None,
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
                if e.texture.is_none_or(|t| t.revision != *revision) =>
            {
                Some((*id, *revision, image))
            }
            _ => None,
        })
    }

    /// Every resource the table knows and how far along it is, ascending by
    /// id. Cold path, for an inspector.
    pub fn states(&self) -> Vec<(ResourceId, AssetState)> {
        let mut out: Vec<(ResourceId, AssetState)> = self
            .entries
            .iter()
            .map(|(id, e)| {
                let state = match (&e.texture, &e.payload, &e.bytes) {
                    (Some(t), ..) => AssetState::Resident { size: t.size },
                    (None, Some((_, AssetPayload::Vector(scene))), _) => {
                        AssetState::Vector { size: scene.size }
                    }
                    (None, Some((_, AssetPayload::Raster(image))), _) => {
                        AssetState::Decoded { size: image.size() }
                    }
                    (None, None, Some(bytes)) => AssetState::Encoded {
                        bytes: bytes.len() as u64,
                    },
                    (None, None, None) => AssetState::Wanted,
                };
                (*id, state)
            })
            .collect();
        out.sort_unstable_by_key(|(id, _)| id.0);
        out
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
        let t = self.entries.get(&id)?.texture?;
        Some((t.revision, t.texture))
    }

    /// The bytes of the textures resident in this window, in the formats they
    /// are stored in.
    pub fn texture_bytes(&self) -> u64 {
        self.texture_bytes
    }

    /// Counts a node showing `id`, recording that a node wants it when the
    /// table holds nothing of it.
    pub(crate) fn hold(&mut self, id: ResourceId) {
        self.want(id);
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.users += 1;
        }
    }

    /// Counts a node no longer showing `id`.
    pub(crate) fn release(&mut self, id: ResourceId) {
        let Some(entry) = self.entries.get_mut(&id) else {
            return;
        };
        entry.users = entry.users.saturating_sub(1);
        if entry.users == 0 {
            self.clock += 1;
            entry.shown = self.clock;
            self.unswept = true;
        }
    }

    /// How many nodes show `id`.
    pub fn users(&self, id: ResourceId) -> u32 {
        self.entries.get(&id).map_or(0, |e| e.users)
    }

    /// Whether a resource lost its last node, or one no node shows gained
    /// something to keep, since the last [`sweep`](Self::sweep).
    pub fn unswept(&self) -> bool {
        self.unswept
    }

    /// Forgets the resources no node shows that keep nothing, then lets go of
    /// those that do, least recently shown first, until the table's textures
    /// and the scenes no node shows take at most `budget` bytes; `release`
    /// takes each texture let go of. A table no node left and nothing grew in
    /// since the last
    /// sweep, at the same budget, is left as it is. Returns how many it let go
    /// of.
    pub fn sweep(&mut self, budget: u64, mut release: impl FnMut(TextureId)) -> usize {
        if !self.unswept && self.swept == (budget, self.texture_bytes) {
            return 0;
        }
        self.unswept = false;
        let mut idle = std::mem::take(&mut self.idle);
        let before = self.entries.len();
        let mut scenes = 0;
        self.entries.retain(|id, e| {
            if e.users > 0 {
                return true;
            }
            let kept = e.kept_bytes();
            if e.texture.is_none() {
                scenes += kept;
            }
            idle.push((e.shown, *id, kept));
            kept > 0
        });
        let mut evicted = before - self.entries.len();
        idle.retain(|&(_, _, kept)| kept > 0);
        idle.sort_unstable_by_key(|&(shown, ..)| shown);
        for &(_, id, kept) in &idle {
            if self.texture_bytes + scenes <= budget {
                break;
            }
            let Some(entry) = self.entries.remove(&id) else {
                continue;
            };
            match entry.texture {
                Some(t) => {
                    self.texture_bytes -= t.bytes;
                    release(t.texture);
                }
                None => scenes -= kept,
            }
            evicted += 1;
        }
        idle.clear();
        self.idle = idle;
        self.swept = (budget, self.texture_bytes);
        if evicted > 0 {
            self.revision += 1;
        }
        evicted
    }

    /// Lets go of everything no node shows (see [`sweep`](Self::sweep)).
    pub fn evict_unshown(&mut self, release: impl FnMut(TextureId)) -> usize {
        self.unswept = true;
        self.sweep(0, release)
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
            (_, Some(t)) => (AssetDraw::Raster(t.texture), t.size),
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
        self.unswept |= entry.users == 0;
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
        let (size, bytes) = match &entry.payload {
            Some((r, AssetPayload::Raster(image))) if *r == revision => {
                (image.size(), image.texture_bytes())
            }
            _ => return Err(texture),
        };
        let old = entry.texture.replace(Resident {
            revision,
            texture,
            size,
            bytes,
        });
        self.texture_bytes += bytes;
        if let Some(old) = old {
            self.texture_bytes -= old.bytes;
        }
        self.unswept |= entry.users == 0;
        entry.payload = None;
        self.revision += 1;
        Ok(old.map(|t| t.texture))
    }

    pub(crate) fn clear(&mut self) {
        if !self.entries.is_empty() {
            self.revision += 1;
        }
        self.entries.clear();
        self.texture_bytes = 0;
        self.unswept = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{BuildCx, NodeStore};
    use crate::content::Content;
    use crate::reactive::EffectStore;
    use crate::{BoxStyle, LeafStyle, Size};

    /// A 4 × 4 raster: 64 bytes resident.
    fn raster() -> AssetPayload {
        AssetPayload::Raster(Arc::new(RasterImage::rgba8(4, 4, vec![255; 64])))
    }

    /// A store of `n` leaves under one root, each showing its own resource,
    /// decoded and resident as texture `i`.
    fn shown(n: u32) -> (NodeStore, Vec<crate::node::NodeId>, Vec<ResourceId>) {
        let mut store = NodeStore::new();
        let mut leaves = Vec::new();
        {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(Default::default(), |cx| {
                for _ in 0..n {
                    let leaf = LeafStyle {
                        size: Size::fixed(4.0, 4.0),
                        style: BoxStyle::NONE,
                    };
                    leaves.push(cx.leaf(leaf).id());
                }
            });
        }
        let ids: Vec<ResourceId> = (0..n)
            .map(|i| ResourceId::of(&format!("photo{i}.png")))
            .collect();
        let r = ResourceRevision(1);
        for (i, (&leaf, &id)) in leaves.iter().zip(&ids).enumerate() {
            store.assets_mut().offer(id, r, AssetBytes::Static(b"png"));
            store.set_asset(leaf, id, AssetFit::Fill, None);
            store.provide_asset(id, r, raster());
            store.attach_texture(id, r, TextureId::new(i as u32));
        }
        (store, leaves, ids)
    }

    #[test]
    fn an_image_no_node_shows_stays_until_the_budget_wants_it_least_recent_first() {
        let (mut store, leaves, ids) = shown(3);
        assert_eq!(store.assets().users(ids[0]), 1);
        assert_eq!(store.assets().texture_bytes(), 3 * 64);
        let mut released = Vec::new();
        assert_eq!(
            store.assets_mut().sweep(0, |t| released.push(t)),
            0,
            "every image is shown"
        );

        // The second, then the first, stop showing.
        let (mut effects, mut scratch) = (EffectStore::default(), Vec::new());
        store.free_tree(leaves[1], &mut effects, &mut scratch);
        store.free_tree(leaves[0], &mut effects, &mut scratch);
        assert_eq!(store.assets().users(ids[1]), 0);
        assert!(store.assets().unswept());
        // Within the budget both stay.
        assert_eq!(store.assets_mut().sweep(3 * 64, |t| released.push(t)), 0);
        assert!(store.assets().knows(ids[1]));
        // The budget an image short: the least recently shown goes.
        assert_eq!(store.assets_mut().sweep(2 * 64, |t| released.push(t)), 1);
        assert_eq!(released, [TextureId::new(1)]);
        assert!(!store.assets().knows(ids[1]));
        assert!(store.assets().knows(ids[0]));
        assert_eq!(store.assets().texture_bytes(), 2 * 64);

        // Shown again, it draws at once from what was kept.
        store.set_asset(leaves[2], ids[0], AssetFit::Fill, None);
        assert_eq!(store.assets().users(ids[0]), 1);
        assert_eq!(store.assets().users(ids[2]), 0, "the node moved off it");
        match store.content_payload(leaves[2]) {
            Some(Content::Asset { draw, .. }) => {
                assert_eq!(*draw, AssetDraw::Raster(TextureId::new(0)));
            }
            other => panic!("{other:?}"),
        }
        // A trim lets go of everything no node shows.
        released.clear();
        assert_eq!(store.assets_mut().evict_unshown(|t| released.push(t)), 1);
        assert_eq!(released, [TextureId::new(2)]);
        assert_eq!(store.assets().texture_bytes(), 64);
    }

    #[test]
    fn a_resource_no_node_shows_that_keeps_nothing_is_forgotten() {
        let mut store = NodeStore::new();
        let leaf = {
            let mut cx = BuildCx::new(&mut store);
            let leaf = LeafStyle {
                size: Size::fixed(4.0, 4.0),
                style: BoxStyle::NONE,
            };
            cx.leaf(leaf).id()
        };
        let id = ResourceId::of("never.png");
        store
            .assets_mut()
            .offer(id, ResourceRevision(1), AssetBytes::Static(b"png"));
        store.set_asset(leaf, id, AssetFit::Fill, None);
        let (mut effects, mut scratch) = (EffectStore::default(), Vec::new());
        store.free_tree(leaf, &mut effects, &mut scratch);
        assert_eq!(store.assets_mut().sweep(u64::MAX, |_| {}), 1);
        assert!(!store.assets().knows(id), "its bytes are not kept");
    }
}
