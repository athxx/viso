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
//!    A model is resident the same way: the facade uploads its geometry into
//!    the window's renderer and attaches the [`SceneModel`]
//!    ([`NodeStore::attach_model`](crate::component::NodeStore::attach_model)),
//!    which the fillers of the scenes holding it draw.
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

use crate::node::NodeId;
use viso_render::TextureId;
use viso_render::asset::{AssetKind, RasterImage, ResourceId, ResourceRevision, VectorScene};
use viso_render::scene3d::SceneModel;
use viso_render::scene3d::model::ModelAsset;

pub use viso_schema::resource::AssetBytes;
use viso_schema::resource::AssetFormat;

/// What a node showing an asset draws.
#[derive(Debug, Clone, PartialEq)]
pub enum AssetDraw {
    /// Nothing yet: the asset is not decoded, or its raster not resident.
    Nothing,
    /// Nothing: its bytes could not be read or did not decode.
    Failed,
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
/// ([`Asset::bytes`]), read from a file ([`Asset::file`]) or fetched from a
/// URL ([`Asset::url`]).
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
    /// A URL, fetched through the app's image loader when a node first
    /// shows it.
    Url(Arc<str>),
}

/// Where the bytes of a resource no one offered are fetched from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetOrigin {
    File(Arc<Path>),
    Url(Arc<str>),
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
        Asset {
            id: ResourceId::of(&format!("file:{}", path.display())),
            kind: kind_by_extension(path.extension().and_then(|x| x.to_str())),
            source: AssetSource::File(path.into()),
        }
    }

    /// The image at `url`, fetched through the app's image loader and
    /// decoded off the frame path when a node first shows it; without a
    /// loader it fails. The URL is the resource: the same URL shows what was
    /// fetched of it while any table keeps it.
    pub fn url(url: impl Into<Arc<str>>) -> Asset {
        let url: Arc<str> = url.into();
        let path = url.split(['?', '#']).next().unwrap_or_default();
        let name = path.rsplit('/').next().unwrap_or_default();
        let extension = name.rsplit_once('.').map(|(_, x)| x);
        Asset {
            id: ResourceId::of(&format!("url:{url}")),
            kind: kind_by_extension(extension),
            source: AssetSource::Url(url),
        }
    }
}

/// The kind a file named with `extension` holds, a raster unless it names
/// another.
fn kind_by_extension(extension: Option<&str>) -> AssetKind {
    let is =
        |names: &[&str]| extension.is_some_and(|x| names.iter().any(|n| x.eq_ignore_ascii_case(n)));
    if is(&["svg"]) {
        AssetKind::Vector
    } else if is(&["mp4", "m4v", "mov", "webm", "mkv"]) {
        AssetKind::Video
    } else if is(&["glb", "gltf"]) {
        AssetKind::Model
    } else {
        AssetKind::Raster
    }
}

/// A decoded asset.
#[derive(Debug, Clone)]
pub enum AssetPayload {
    Raster(Arc<RasterImage>),
    Vector(Arc<VectorScene>),
    /// A 3D model, its geometry and images until it is resident.
    Model(Arc<ModelAsset>),
}

impl AssetPayload {
    /// Its natural size in pixels; a model has none.
    pub fn size(&self) -> [f32; 2] {
        match self {
            AssetPayload::Raster(image) => image.size(),
            AssetPayload::Vector(scene) => scene.size,
            AssetPayload::Model(_) => [0.0; 2],
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
    /// A model decoded, `resident` once uploaded into this window.
    Model { resident: bool },
    /// Its bytes could not be read or did not decode.
    Failed,
}

/// One resource of the table.
#[derive(Debug, Clone)]
pub(crate) struct AssetEntry {
    /// The revision of the newest bytes offered.
    pub(crate) offered: ResourceRevision,
    /// Those bytes, until a payload of their revision is provided.
    pub(crate) bytes: Option<AssetBytes>,
    /// Where its bytes are fetched from, when they are not at hand.
    pub(crate) origin: Option<AssetOrigin>,
    /// The newest revision that could not be read or did not decode.
    pub(crate) failed: Option<ResourceRevision>,
    /// The newest payload provided, and its revision: a vector's for as long
    /// as it draws, a raster's until its texture is attached.
    pub(crate) payload: Option<(ResourceRevision, AssetPayload)>,
    /// A raster's texture in this store's window; it draws until the newest
    /// payload's replaces it.
    pub(crate) texture: Option<Resident>,
    /// A model uploaded into this store's window, and the revision it was
    /// uploaded from.
    pub(crate) model: Option<(ResourceRevision, Arc<SceneModel>)>,
    /// The nodes showing it, a node once for each time it shows it.
    pub(crate) users: Vec<NodeId>,
    /// When it was last shown, on the table's clock.
    pub(crate) shown: u64,
    /// The natural size of a resource the facade provides itself — a
    /// video's pictures — rather than from bytes it decodes; `None` for any
    /// other.
    pub(crate) hosted: Option<[f32; 2]>,
}

/// A raster's texture, the revision it was uploaded from, that raster's size,
/// its texels and the bytes the texture takes.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Resident {
    pub(crate) revision: ResourceRevision,
    pub(crate) texture: TextureId,
    pub(crate) size: [f32; 2],
    pub(crate) texels: (u32, u32),
    pub(crate) bytes: u64,
}

impl AssetEntry {
    fn new(offered: ResourceRevision) -> AssetEntry {
        AssetEntry {
            offered,
            bytes: None,
            origin: None,
            failed: None,
            payload: None,
            texture: None,
            model: None,
            users: Vec::new(),
            shown: 0,
            hosted: None,
        }
    }

    /// The bytes it holds that a node showing it again draws from at once: its
    /// texture's, its model's, or its vector scene's.
    fn kept_bytes(&self) -> u64 {
        match (&self.texture, &self.model, &self.payload) {
            (Some(t), ..) => t.bytes,
            (None, Some((_, model)), _) => model.bytes(),
            (None, None, Some((_, AssetPayload::Vector(scene)))) => scene.bytes(),
            _ => 0,
        }
    }

    /// Whether it holds no bytes, no payload, no texture and no model, nor
    /// failed.
    fn holds_nothing(&self) -> bool {
        self.bytes.is_none()
            && self.payload.is_none()
            && self.texture.is_none()
            && self.model.is_none()
            && self.failed.is_none()
    }

    /// Whether its newest bytes failed and it has nothing older to draw.
    fn fails(&self) -> bool {
        self.failed.is_some_and(|f| f >= self.offered)
            && self.texture.is_none()
            && self.model.is_none()
            && self.payload.is_none()
    }

    /// The newest revision decoded: provided, or resident.
    fn decoded(&self) -> Option<ResourceRevision> {
        let payload = self.payload.as_ref().map(|(r, _)| *r);
        let texture = self.texture.map(|t| t.revision);
        let model = self.model.as_ref().map(|(r, _)| *r);
        payload.max(texture).max(model)
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
    /// Models let go of, for the facade to release from the window's
    /// renderer.
    retired: Vec<Arc<SceneModel>>,
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
            && entry.failed.is_none_or(|f| revision > f)
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

    /// Records that a node shows `id`, fetched from `origin`.
    pub fn locate(&mut self, id: ResourceId, origin: AssetOrigin) {
        let entry = self
            .entries
            .entry(id)
            .or_insert_with(|| AssetEntry::new(ResourceRevision::default()));
        if entry.origin.is_none() {
            entry.origin = Some(origin);
            self.revision += 1;
        }
    }

    /// Every resource a node shows that the table holds nothing of — no
    /// bytes, no payload, no texture — nor knows an origin of.
    pub fn wanted(&self) -> impl Iterator<Item = ResourceId> + '_ {
        self.entries.iter().filter_map(|(id, e)| {
            (e.origin.is_none() && e.hosted.is_none() && e.holds_nothing()).then_some(*id)
        })
    }

    /// Every resource to fetch from its origin: one the table holds nothing
    /// of yet, with its revision and the origin.
    pub fn unread(&self) -> impl Iterator<Item = (ResourceId, ResourceRevision, &AssetOrigin)> {
        self.entries.iter().filter_map(|(id, e)| match &e.origin {
            Some(origin) if e.holds_nothing() => Some((*id, e.offered, origin)),
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

    /// Every model with no geometry in this window yet: its id, revision and
    /// decoded asset.
    pub fn unresident_models(
        &self,
    ) -> impl Iterator<Item = (ResourceId, ResourceRevision, &Arc<ModelAsset>)> {
        self.entries.iter().filter_map(|(id, e)| match &e.payload {
            Some((revision, AssetPayload::Model(model)))
                if e.model.as_ref().is_none_or(|(r, _)| r != revision) =>
            {
                Some((*id, *revision, model))
            }
            _ => None,
        })
    }

    /// Model `id` resident in this window: the newest uploaded, until a newer
    /// one is.
    pub fn model(&self, id: ResourceId) -> Option<&Arc<SceneModel>> {
        Some(&self.entries.get(&id)?.model.as_ref()?.1)
    }

    /// The models let go of since the last call — replaced, swept, or the
    /// table cleared — for the caller to release from the window's renderer
    /// once no frame in flight draws them.
    pub fn take_retired_models(&mut self) -> std::vec::Drain<'_, Arc<SceneModel>> {
        self.retired.drain(..)
    }

    /// Every resource the table knows and how far along it is, ascending by
    /// id. Cold path, for an inspector.
    pub fn states(&self) -> Vec<(ResourceId, AssetState)> {
        let mut out: Vec<(ResourceId, AssetState)> = self
            .entries
            .iter()
            .map(|(id, e)| {
                let state = match (&e.texture, &e.payload, &e.bytes) {
                    _ if e.fails() => AssetState::Failed,
                    (Some(t), ..) => AssetState::Resident { size: t.size },
                    _ if e.model.is_some() => AssetState::Model { resident: true },
                    (None, Some((_, AssetPayload::Model(_))), _) => {
                        AssetState::Model { resident: false }
                    }
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

    /// The texels of `id`'s texture in this window, wide and high.
    pub fn texture_size(&self, id: ResourceId) -> Option<(u32, u32)> {
        Some(self.entries.get(&id)?.texture?.texels)
    }

    /// The bytes of the textures resident in this window, in the formats they
    /// are stored in.
    pub fn texture_bytes(&self) -> u64 {
        self.texture_bytes
    }

    /// Records `node` showing `id`, and that a node wants it when the table
    /// holds nothing of it.
    pub(crate) fn hold(&mut self, id: ResourceId, node: NodeId) {
        self.want(id);
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.users.push(node);
        }
    }

    /// Records `node` no longer showing `id`.
    pub(crate) fn release(&mut self, id: ResourceId, node: NodeId) {
        let Some(entry) = self.entries.get_mut(&id) else {
            return;
        };
        // The newest first: a node unmounting is most often the last mounted.
        if let Some(at) = entry.users.iter().rposition(|&n| n == node) {
            entry.users.swap_remove(at);
        }
        if entry.users.is_empty() {
            self.clock += 1;
            entry.shown = self.clock;
            self.unswept = true;
        }
    }

    /// How many nodes show `id`.
    pub fn users(&self, id: ResourceId) -> u32 {
        self.entries.get(&id).map_or(0, |e| e.users.len() as u32)
    }

    /// The nodes showing `id`, in no order; a node showing it twice — a
    /// brush sampling it twice — is there twice.
    pub fn nodes(&self, id: ResourceId) -> &[NodeId] {
        self.entries.get(&id).map_or(&[], |e| &e.users)
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
            // What the facade provides it lets go of itself.
            if !e.users.is_empty() || e.hosted.is_some() {
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
            if let Some((_, model)) = entry.model {
                self.retired.push(model);
            }
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

    /// Whether a resource a node shows is still on its way: its bytes being
    /// fetched or decoded, or its raster not yet uploaded.
    pub fn is_loading(&self) -> bool {
        self.entries.values().any(|e| {
            !e.users.is_empty()
                && e.hosted.is_none()
                && e.texture.is_none()
                && e.payload.is_none()
                && e.failed.is_none_or(|failed| failed < e.offered)
        })
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
        if entry.fails() {
            return (AssetDraw::Failed, [0.0; 2]);
        }
        if let Some(size) = entry.hosted {
            let draw = entry
                .texture
                .map_or(AssetDraw::Nothing, |t| AssetDraw::Raster(t.texture));
            return (draw, size);
        }
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
        self.unswept |= entry.users.is_empty();
        self.revision += 1;
        true
    }

    /// Records that `id`'s bytes at `revision` could not be read or did not
    /// decode, dropping them; `false` when the table holds newer bytes or a
    /// payload as new. A texture or scene of an older revision keeps drawing.
    pub(crate) fn fail(&mut self, id: ResourceId, revision: ResourceRevision) -> bool {
        let Some(entry) = self.entries.get_mut(&id) else {
            return false;
        };
        let stale = revision < entry.offered
            || entry.decoded().is_some_and(|r| r >= revision)
            || entry.failed.is_some_and(|f| f >= revision);
        if stale {
            return false;
        }
        entry.bytes = None;
        entry.failed = Some(revision);
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
        let (size, texels, bytes) = match &entry.payload {
            Some((r, AssetPayload::Raster(image))) if *r == revision => (
                image.size(),
                (image.width, image.height),
                image.texture_bytes(),
            ),
            _ => return Err(texture),
        };
        let old = entry.texture.replace(Resident {
            revision,
            texture,
            size,
            texels,
            bytes,
        });
        self.texture_bytes += bytes;
        if let Some(old) = old {
            self.texture_bytes -= old.bytes;
        }
        self.unswept |= entry.users.is_empty();
        entry.payload = None;
        self.revision += 1;
        Ok(old.map(|t| t.texture))
    }

    /// Records `model`, uploaded from `id`'s model at `revision`, and lets go
    /// of the geometry and images it was uploaded from; the model it
    /// replaces is retired. `Err` gives `model` back when the table has moved
    /// past `revision`.
    pub(crate) fn attach_model(
        &mut self,
        id: ResourceId,
        revision: ResourceRevision,
        model: SceneModel,
    ) -> Result<(), SceneModel> {
        let Some(entry) = self.entries.get_mut(&id) else {
            return Err(model);
        };
        if !matches!(&entry.payload, Some((r, AssetPayload::Model(_))) if *r == revision) {
            return Err(model);
        }
        if let Some((_, old)) = entry.model.replace((revision, Arc::new(model))) {
            self.retired.push(old);
        }
        self.unswept |= entry.users.is_empty();
        entry.payload = None;
        self.revision += 1;
        Ok(())
    }

    /// Records `id` as a resource the facade provides itself, of no size
    /// yet.
    pub(crate) fn host(&mut self, id: ResourceId) {
        let entry = self
            .entries
            .entry(id)
            .or_insert_with(|| AssetEntry::new(ResourceRevision::default()));
        if entry.hosted.is_none() {
            entry.hosted = Some([0.0; 2]);
            self.revision += 1;
        }
    }

    /// Records hosted `id`'s natural size; `false` when it is not hosted or
    /// already that size.
    pub(crate) fn size_hosted(&mut self, id: ResourceId, size: [f32; 2]) -> bool {
        let Some(entry) = self.entries.get_mut(&id) else {
            return false;
        };
        if entry.hosted.is_none_or(|s| s == size) {
            return false;
        }
        entry.hosted = Some(size);
        self.revision += 1;
        true
    }

    /// Records `texture`, of `texels`, as what hosted `id` shows. Returns the
    /// texture it replaces, or `texture` itself when `id` is not hosted.
    pub(crate) fn attach_hosted(
        &mut self,
        id: ResourceId,
        texture: TextureId,
        texels: (u32, u32),
    ) -> Option<TextureId> {
        let Some(entry) = self.entries.get_mut(&id) else {
            return Some(texture);
        };
        let Some(size) = entry.hosted else {
            return Some(texture);
        };
        let bytes = u64::from(texels.0) * u64::from(texels.1) * 4;
        let old = entry.texture.replace(Resident {
            revision: ResourceRevision::default(),
            texture,
            size,
            texels,
            bytes,
        });
        entry.failed = None;
        self.texture_bytes += bytes;
        if let Some(old) = old {
            self.texture_bytes -= old.bytes;
        }
        self.revision += 1;
        old.map(|t| t.texture)
    }

    /// Records that hosted `id` cannot show, dropping its texture, which comes
    /// back for the caller to release.
    pub(crate) fn fail_hosted(&mut self, id: ResourceId) -> Option<TextureId> {
        let entry = self.entries.get_mut(&id)?;
        entry.hosted?;
        entry.failed = Some(entry.offered);
        let old = entry.texture.take();
        if let Some(old) = old {
            self.texture_bytes -= old.bytes;
        }
        self.revision += 1;
        old.map(|t| t.texture)
    }

    /// Forgets hosted `id`, whose texture comes back for the caller to
    /// release.
    pub(crate) fn forget_hosted(&mut self, id: ResourceId) -> Option<TextureId> {
        self.entries.get(&id)?.hosted?;
        let entry = self.entries.remove(&id)?;
        self.revision += 1;
        let old = entry.texture?;
        self.texture_bytes -= old.bytes;
        Some(old.texture)
    }

    pub(crate) fn clear(&mut self) {
        if !self.entries.is_empty() {
            self.revision += 1;
        }
        self.retired.extend(
            self.entries
                .drain()
                .filter_map(|(_, e)| e.model.map(|(_, m)| m)),
        );
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
    fn shown(n: u32) -> (NodeStore, Vec<NodeId>, Vec<ResourceId>) {
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

    #[test]
    fn its_fallbacks_show_while_it_loads_and_once_it_fails_and_its_watcher_hears_each() {
        use crate::notice::{Notice, NoticeKinds, deliver_notices};
        use crate::state::StateStore;
        use crate::virtual_list::VirtualLists;
        use std::cell::RefCell;
        use std::rc::Rc;

        let mut store = NodeStore::new();
        let (mut picture, mut loading, mut failed) = (None, None, None);
        {
            let mut cx = BuildCx::new(&mut store);
            cx.stack(Default::default(), |cx| {
                let leaf = |cx: &mut BuildCx<'_>| {
                    cx.leaf(LeafStyle {
                        size: Size::fixed(4.0, 4.0),
                        style: BoxStyle::NONE,
                    })
                };
                picture = Some(leaf(cx));
                loading = Some(leaf(cx));
                failed = Some(leaf(cx));
            });
        }
        let (picture, loading, failed) = (picture.unwrap(), loading.unwrap(), failed.unwrap());
        let (good, bad) = (ResourceId::of("good.png"), ResourceId::of("bad.png"));
        let r = ResourceRevision(1);
        for id in [good, bad] {
            store.assets_mut().offer(id, r, AssetBytes::Static(b"png"));
        }
        store.set_asset(picture.id(), bad, AssetFit::Fill, None);
        store.set_asset_fallbacks(picture.id(), Some(loading.id()), Some(failed.id()));
        assert!(!store.hidden(loading.id()), "loading");
        assert!(store.hidden(failed.id()));

        let heard = Rc::new(RefCell::new(Vec::new()));
        let log = Rc::clone(&heard);
        store.watch(
            picture.id(),
            NoticeKinds::ASSET,
            Box::new(move |notice: &Notice, _: &mut StateStore| log.borrow_mut().push(*notice)),
        );
        let (lists, mut states) = (VirtualLists::new(), StateStore::new());
        assert_eq!(
            deliver_notices(&mut store, &lists, &mut states),
            0,
            "still loading"
        );

        // It does not decode: the error shows, once heard.
        store.fail_asset(bad, r);
        assert_eq!(store.assets().resolve(bad).0, AssetDraw::Failed);
        assert!(store.hidden(loading.id()));
        assert!(!store.hidden(failed.id()));
        deliver_notices(&mut store, &lists, &mut states);
        deliver_notices(&mut store, &lists, &mut states);
        assert_eq!(*heard.borrow(), [Notice::AssetFailed]);
        assert!(!store.assets_mut().fail(bad, r), "reported once");

        // Another source loads: loading shows again until it draws.
        store.set_asset(picture.id(), good, AssetFit::Fill, None);
        assert!(!store.hidden(loading.id()));
        assert!(store.hidden(failed.id()));
        deliver_notices(&mut store, &lists, &mut states);
        store.provide_asset(good, r, raster());
        store.attach_texture(good, r, TextureId::new(1));
        assert!(store.hidden(loading.id()) && store.hidden(failed.id()));
        deliver_notices(&mut store, &lists, &mut states);
        assert_eq!(
            *heard.borrow(),
            [
                Notice::AssetFailed,
                Notice::AssetLoaded {
                    size: crate::Vec2::new(4.0, 4.0)
                }
            ]
        );

        // A newer revision that fails keeps the picture it had.
        let r2 = ResourceRevision(2);
        store
            .assets_mut()
            .offer(good, r2, AssetBytes::Static(b"bad"));
        store.fail_asset(good, r2);
        assert_eq!(
            store.assets().resolve(good).0,
            AssetDraw::Raster(TextureId::new(1))
        );
    }
}
