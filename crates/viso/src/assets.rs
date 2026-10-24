//! The app's assets, shared by its windows: each resource's bytes decoded
//! once, on a worker thread, for every window showing it, and each window's
//! rasters uploaded into its own device.
//!
//! A window's store offers the bytes of what its nodes show
//! ([`AssetTable`](viso_ui::AssetTable)); every frame, before layout, the app
//! serves the windows whose tables moved: a payload it already decoded is
//! provided at once, anything else goes to the worker, which wakes the loop
//! when it is done, and its result is provided to every window on the next
//! frame. The app keeps decoded payloads only weakly — the tables hold them,
//! and a table lets go of a raster's pixels once its texture is attached — so
//! a resident image costs its texture and nothing on the CPU. An image no node
//! shows any more stays resident for one that shows it again, within the
//! images budget: past it, the least recently shown is let go of. A payload that
//! does not decode is reported once and never retried at that revision.
//!
//! An image the build converted to its target's texel format is read, not
//! decoded: an embedded one is handed to the device where it lies in the
//! binary. One in a format the device does not sample — an image converted
//! for another GPU, or a reload's — decodes on the worker to RGBA8 instead,
//! the formats every device samples.
//!
//! Textures live on the window's device, which keeps them across a renderer
//! rebuild and a device loss; a replaced one is released through the
//! renderer, whose device destroys it once no frame in flight draws it.
//!
//! A hot reload [replaces](Assets::replace) a resource with bytes the build
//! did not embed: the app keeps the newest bytes of each, so a table offering
//! older ones — a window or mount opened later — or none at all is served
//! those.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Weak};
use std::thread;

use viso_gpu::{GpuBackend, SampledFormats, TextureDesc};
use viso_platform::LoopWaker;
use viso_render::Renderer;
use viso_render::asset::{AssetError, RasterImage, ResourceId, ResourceRevision, VectorScene};
use viso_ui::{AssetBytes, AssetPayload, NodeStore};

/// What the app's assets cost, counted since it started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AssetCounters {
    /// Payloads decoded.
    pub(crate) decodes: u64,
    /// Payloads that did not decode.
    pub(crate) decode_failures: u64,
    /// Rasters uploaded into a window's device.
    pub(crate) uploads: u64,
    /// The bytes those uploads wrote.
    pub(crate) upload_bytes: u64,
    /// Images no node showed, let go of to keep within the budget.
    pub(crate) evictions: u64,
}

/// A decoded payload the app remembers while some table holds it.
enum Held {
    Raster(Weak<RasterImage>),
    Vector(Weak<VectorScene>),
}

impl Held {
    fn of(payload: &AssetPayload) -> Held {
        match payload {
            AssetPayload::Raster(image) => Held::Raster(Arc::downgrade(image)),
            AssetPayload::Vector(scene) => Held::Vector(Arc::downgrade(scene)),
        }
    }

    fn upgrade(&self) -> Option<AssetPayload> {
        match self {
            Held::Raster(image) => image.upgrade().map(AssetPayload::Raster),
            Held::Vector(scene) => scene.upgrade().map(AssetPayload::Vector),
        }
    }
}

/// A payload to decode.
struct Job {
    id: ResourceId,
    revision: ResourceRevision,
    input: Input,
    /// The formats the devices it is uploaded into sample.
    sampled: SampledFormats,
}

/// What a job decodes.
#[derive(Clone)]
enum Input {
    Bytes(AssetBytes),
    /// The bytes of a file, read first.
    File(Arc<Path>),
}

/// The most of a file an asset reads: past it the file does not load.
const MAX_FILE: u64 = 1 << 30;

impl Input {
    /// Its payload, or why it has none.
    fn decode(&self, sampled: SampledFormats) -> Result<AssetPayload, AssetError> {
        match self {
            Input::Bytes(bytes) => decode(bytes, sampled),
            Input::File(path) => decode(&read(path)?, sampled),
        }
    }
}

/// The bytes of the file at `path`, at most [`MAX_FILE`] of them.
fn read(path: &Path) -> Result<AssetBytes, AssetError> {
    use std::io::Read;
    let unread = |e: std::io::Error| AssetError::Decode(format!("`{}`: {e}", path.display()));
    let file = std::fs::File::open(path).map_err(unread)?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(unread)?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(AssetError::Decode(format!(
            "`{}` is larger than {MAX_FILE} bytes",
            path.display()
        )));
    }
    Ok(AssetBytes::Shared(bytes.into()))
}

/// A decoded payload, or why it did not decode.
struct Done {
    id: ResourceId,
    revision: ResourceRevision,
    result: Result<AssetPayload, AssetError>,
}

/// The main thread's slice for decoding where no worker thread runs: a
/// quarter of a 60Hz frame.
const INLINE_SLICE: std::time::Duration = std::time::Duration::from_millis(4);

/// The decode thread.
struct Worker {
    jobs: Sender<Job>,
    done: Receiver<Done>,
}

impl Worker {
    /// A worker that wakes the loop with `waker` after each result; `None`
    /// when no thread can be spawned.
    fn spawn(waker: LoopWaker) -> Option<Worker> {
        let (jobs, inbox) = mpsc::channel::<Job>();
        let (outbox, done) = mpsc::channel();
        thread::Builder::new()
            .name("viso-assets".into())
            .spawn(move || {
                for job in inbox {
                    let done = Done {
                        id: job.id,
                        revision: job.revision,
                        result: job.input.decode(job.sampled),
                    };
                    if outbox.send(done).is_err() {
                        break;
                    }
                    waker.wake();
                }
            })
            .ok()?;
        Some(Worker { jobs, done })
    }
}

/// `bytes` decoded by the format they are in, a raster in a format outside
/// `sampled` decoded on to RGBA8.
///
/// A build whose images were all converted links no decoder for what it
/// never ships: no raster decoder (`viso_converted_rasters`), no SVG
/// parser (`viso_converted_svg`); such a file is reported as not decoding.
pub(crate) fn decode(
    bytes: &AssetBytes,
    sampled: SampledFormats,
) -> Result<AssetPayload, AssetError> {
    use viso_render::asset::AssetFormat;
    let raster = |image| Ok(AssetPayload::Raster(Arc::new(sampleable(image, sampled)?)));
    if viso_image::texture::is_texture(bytes) {
        return raster(viso_image::texture::read(bytes)?);
    }
    match AssetFormat::sniff(bytes).map(AssetFormat::kind) {
        #[cfg(not(viso_converted_rasters))]
        Some(viso_render::asset::AssetKind::Raster) => {
            raster(viso_render::asset::decode_raster_asset(bytes)?)
        }
        #[cfg(not(viso_converted_svg))]
        Some(viso_render::asset::AssetKind::Vector) => viso_render::svg::parse_svg(bytes)
            .map(|scene| AssetPayload::Vector(Arc::new(scene.into_vector())))
            .map_err(|error| AssetError::Decode(error.to_string())),
        #[cfg(any(viso_converted_rasters, viso_converted_svg))]
        Some(_) => Err(AssetError::Decode(
            "the build converted every image of this kind, and this one is not".to_owned(),
        )),
        None => Err(AssetError::Format),
    }
}

/// `image`, or its texels decoded to RGBA8 when `sampled` lacks its format.
fn sampleable(image: RasterImage, sampled: SampledFormats) -> Result<RasterImage, AssetError> {
    if sampled.contains(image.format.sampled_class()) {
        return Ok(image);
    }
    image
        .to_rgba8()
        .ok_or_else(|| AssetError::Decode(format!("{image:?} does not hold its texels")))
}

/// The app's assets.
#[derive(Default)]
pub(crate) struct Assets {
    /// The newest payload decoded of each resource, while a table holds it.
    decoded: HashMap<ResourceId, (ResourceRevision, Held)>,
    /// The revision of each resource the worker is decoding.
    decoding: HashMap<ResourceId, ResourceRevision>,
    /// The newest revision of each resource that did not decode.
    failed: HashMap<ResourceId, ResourceRevision>,
    /// The newest bytes of each resource a hot reload replaced.
    latest: HashMap<ResourceId, (ResourceRevision, AssetBytes)>,
    worker: Option<Worker>,
    /// Whether no worker thread could be spawned: decoding runs inline.
    inline: bool,
    /// The results received and not yet provided.
    arrived: Vec<Done>,
    /// Reused per window: the resources to serve.
    scratch: Vec<(ResourceId, ResourceRevision, Input)>,
    rasters: Vec<(ResourceId, ResourceRevision, Arc<RasterImage>)>,
    /// The formats every device served so far samples; every format until
    /// one is.
    sampled: Option<SampledFormats>,
    counters: AssetCounters,
}

impl Assets {
    pub(crate) fn counters(&self) -> AssetCounters {
        self.counters
    }

    /// Counts `n` images a window let go of.
    pub(crate) fn evicted(&mut self, n: usize) {
        self.counters.evictions += n as u64;
    }

    /// Takes the worker's results; `true` when any arrived, for the caller to
    /// frame the windows.
    pub(crate) fn receive(&mut self) -> bool {
        let before = self.arrived.len();
        while let Some(worker) = &self.worker {
            match worker.done.try_recv() {
                Ok(done) => self.arrived.push(done),
                Err(TryRecvError::Empty) => break,
                // The worker died: decode inline from now on.
                Err(TryRecvError::Disconnected) => {
                    self.worker = None;
                    self.inline = true;
                    self.decoding.clear();
                }
            }
        }
        self.arrived.len() > before
    }

    /// Provides the results that arrived to every store of `stores` that
    /// knows the resource.
    pub(crate) fn provide<'s>(&mut self, stores: impl Iterator<Item = &'s mut NodeStore>) {
        self.receive();
        if self.arrived.is_empty() {
            return;
        }
        let arrived = std::mem::take(&mut self.arrived);
        let mut payloads = Vec::with_capacity(arrived.len());
        for done in arrived {
            if self.decoding.get(&done.id) == Some(&done.revision) {
                self.decoding.remove(&done.id);
            }
            if let Some(payload) = self.settle(done) {
                payloads.push(payload);
            }
        }
        for store in stores {
            for (id, revision, payload) in &payloads {
                if store.assets().knows(*id) {
                    store.provide_asset(*id, *revision, payload.clone());
                }
            }
        }
    }

    /// Records a result: the payload to provide, or the failure reported.
    fn settle(&mut self, done: Done) -> Option<(ResourceId, ResourceRevision, AssetPayload)> {
        match done.result {
            Ok(payload) => {
                self.counters.decodes += 1;
                let newer = self
                    .decoded
                    .get(&done.id)
                    .is_none_or(|(r, held)| done.revision > *r || held.upgrade().is_none());
                if newer {
                    self.decoded
                        .insert(done.id, (done.revision, Held::of(&payload)));
                }
                Some((done.id, done.revision, payload))
            }
            Err(error) => {
                self.counters.decode_failures += 1;
                let failed = self.failed.entry(done.id).or_insert(done.revision);
                *failed = (*failed).max(done.revision);
                eprintln!(
                    "[viso] asset {:016x} (revision {}) does not decode: {error}",
                    done.id.0, done.revision.0
                );
                None
            }
        }
    }

    /// Replaces resource `id` with `bytes` at `revision`, decoded to
    /// `payload` already: the app keeps the bytes for whatever shows the
    /// resource later, and every store of `stores` showing it now takes the
    /// payload.
    #[cfg(feature = "hot-reload")]
    pub(crate) fn replace<'s>(
        &mut self,
        id: ResourceId,
        revision: ResourceRevision,
        bytes: AssetBytes,
        payload: AssetPayload,
        stores: impl Iterator<Item = &'s mut NodeStore>,
    ) {
        self.latest.insert(id, (revision, bytes));
        self.decoded.insert(id, (revision, Held::of(&payload)));
        for store in stores {
            if store.assets().knows(id) {
                store.provide_asset(id, revision, payload.clone());
            }
        }
    }

    /// Serves `store`, whose table moved: offers the newest bytes the app
    /// holds of what it shows, provides what the app decoded, sends the rest to
    /// the worker (`waker` wakes the loop when it is done), and uploads the
    /// rasters not resident into `gpu`, releasing what they replace.
    ///
    /// With no worker the decodes run here, in an idle slice
    /// ([`INLINE_SLICE`], at least one decode); `true` when some wait for the
    /// next frame's.
    pub(crate) fn serve<B: GpuBackend>(
        &mut self,
        store: &mut NodeStore,
        gpu: Option<(&mut B, &mut Renderer)>,
        waker: &dyn Fn() -> LoopWaker,
    ) -> bool {
        let start = viso_platform::Instant::now();
        let mut deferred = false;
        if let Some((backend, _)) = &gpu {
            let caps = backend.caps().sampled;
            self.sampled = Some(self.sampled.map_or(caps, |s| s & caps));
        }
        let sampled = self.sampled.unwrap_or(SampledFormats::all());
        let mut todo = std::mem::take(&mut self.scratch);
        if !self.latest.is_empty() {
            // What shows with no bytes, or with older bytes than a reload's.
            let newer = |id: ResourceId, than: Option<ResourceRevision>| {
                let (revision, bytes) = self.latest.get(&id)?;
                than.is_none_or(|than| *revision > than)
                    .then(|| (id, *revision, Input::Bytes(bytes.clone())))
            };
            let table = store.assets();
            todo.extend(table.wanted().filter_map(|id| newer(id, None)));
            todo.extend(
                table
                    .undecoded()
                    .filter_map(|(id, revision, _)| newer(id, Some(revision))),
            );
            for (id, revision, input) in todo.drain(..) {
                if let Input::Bytes(bytes) = input {
                    store.assets_mut().offer(id, revision, bytes);
                }
            }
        }
        let table = store.assets();
        todo.extend(
            table
                .undecoded()
                .map(|(id, revision, bytes)| (id, revision, Input::Bytes(bytes.clone()))),
        );
        todo.extend(
            table
                .unread()
                .map(|(id, revision, path)| (id, revision, Input::File(Arc::clone(path)))),
        );
        for (id, revision, input) in todo.drain(..) {
            if let Some((decoded, held)) = self.decoded.get(&id)
                && *decoded >= revision
                && let Some(payload) = held.upgrade()
            {
                store.provide_asset(id, *decoded, payload);
                continue;
            }
            let pending = |at: Option<&ResourceRevision>| at.is_some_and(|r| *r >= revision);
            if pending(self.failed.get(&id)) || pending(self.decoding.get(&id)) {
                continue;
            }
            if self.worker.is_none() && !self.inline {
                self.worker = (crate::footprint::workers() > 0)
                    .then(|| Worker::spawn(waker()))
                    .flatten();
                self.inline = self.worker.is_none();
            }
            if self.inline && start.elapsed() >= INLINE_SLICE {
                deferred = true;
                continue;
            }
            let job = Job {
                id,
                revision,
                input: input.clone(),
                sampled,
            };
            match &self.worker {
                Some(worker) if worker.jobs.send(job).is_ok() => {
                    self.decoding.insert(id, revision);
                }
                _ => {
                    let done = Done {
                        id,
                        revision,
                        result: input.decode(sampled),
                    };
                    if let Some((id, revision, payload)) = self.settle(done) {
                        store.provide_asset(id, revision, payload);
                    }
                }
            }
        }
        self.scratch = todo;
        if let Some((backend, renderer)) = gpu {
            self.upload(store, backend, renderer);
        }
        deferred
    }

    /// Uploads the rasters of `store` not resident into `backend`, releasing
    /// what they replace through `renderer`.
    pub(crate) fn upload<B: GpuBackend>(
        &mut self,
        store: &mut NodeStore,
        backend: &mut B,
        renderer: &mut Renderer,
    ) {
        self.rasters.clear();
        self.rasters.extend(
            store
                .assets()
                .unresident()
                .map(|(id, revision, image)| (id, revision, Arc::clone(image))),
        );
        let sampled = backend.caps().sampled;
        for (id, revision, image) in self.rasters.drain(..) {
            // Decoded before this device was served: one lacking its format
            // takes it as RGBA8, decoded here once.
            let fallback;
            let image = if sampled.contains(image.format.sampled_class()) {
                &*image
            } else {
                match sampleable((*image).clone(), sampled) {
                    Ok(decoded) => {
                        fallback = decoded;
                        &fallback
                    }
                    Err(error) => {
                        self.counters.decode_failures += 1;
                        eprintln!(
                            "[viso] asset {:016x} (revision {}) does not decode: {error}",
                            id.0, revision.0
                        );
                        continue;
                    }
                }
            };
            // An image holds sRGB-encoded color: its texture decodes a sample.
            let desc = TextureDesc {
                width: image.width,
                height: image.height,
                format: image.format.srgb().unwrap_or(image.format),
                render_target: false,
                label: "asset",
            };
            // An embedded converted image is created from where it lies; a
            // device sampling it in place never copies it.
            let texture = match image.texels.as_static() {
                Some(texels) => backend.create_texture_static(&desc, texels),
                None => {
                    let texture = backend.create_texture(&desc);
                    backend.write_texture(texture, 0, 0, image.width, image.height, &image.texels);
                    texture
                }
            };
            self.counters.uploads += 1;
            self.counters.upload_bytes += image.texels.len() as u64;
            if let Some(released) = store.attach_texture(id, revision, texture) {
                renderer.release_texture(backend, released);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use viso_gpu::{CpuRaster, RawWindowHandle};
    use viso_ui::{AssetDraw, AssetFit, BoxStyle, BuildCx, Content, LeafStyle, NodeId, Size};

    const JPEG: &[u8] = include_bytes!("../../image/src/fixtures/two_tone.jpg");
    const SVG: &[u8] =
        b"<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 20\"><rect width=\"10\" height=\"20\" fill=\"#f00\"/></svg>";

    struct Gpu {
        backend: CpuRaster,
        renderer: Renderer,
    }

    impl Gpu {
        fn new() -> Gpu {
            let mut backend = CpuRaster::new();
            let surface = backend.create_surface(RawWindowHandle::Headless, 64, 64);
            let format = backend.surface_format(surface);
            let renderer = Renderer::new(&mut backend, format);
            Gpu { backend, renderer }
        }
    }

    /// A store with one leaf per resource of `shows`, each showing it.
    fn store(shows: &[(ResourceId, &'static [u8])]) -> (NodeStore, Vec<NodeId>) {
        let mut store = NodeStore::new();
        let mut nodes = Vec::new();
        {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(Default::default(), |cx| {
                for _ in shows {
                    nodes.push(
                        cx.leaf(LeafStyle {
                            size: Size::fixed(40.0, 40.0),
                            style: BoxStyle::NONE,
                        })
                        .id(),
                    );
                }
            });
        }
        for (&(id, bytes), &node) in shows.iter().zip(&nodes) {
            store
                .assets_mut()
                .offer(id, ResourceRevision::default(), AssetBytes::Static(bytes));
            store.set_asset(node, id, AssetFit::Contain, None);
        }
        (store, nodes)
    }

    /// Serves `store` until the worker has nothing in flight.
    fn serve(assets: &mut Assets, store: &mut NodeStore, gpu: &mut Gpu) {
        for _ in 0..2000 {
            assets.provide(std::iter::once(&mut *store));
            assets.serve(
                store,
                Some((&mut gpu.backend, &mut gpu.renderer)),
                &LoopWaker::inert,
            );
            if assets.decoding.is_empty() && assets.arrived.is_empty() {
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("the worker never finished");
    }

    fn drawn(store: &NodeStore, node: NodeId) -> (AssetDraw, [f32; 2]) {
        match store.content_payload(node) {
            Some(Content::Asset { draw, natural, .. }) => (draw.clone(), [natural.x, natural.y]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn assets_decode_once_upload_per_window_and_hold_no_pixels_once_resident() {
        let (photo, icon) = (ResourceId::of("photo.jpg"), ResourceId::of("icon.svg"));
        let mut assets = Assets::default();
        let mut gpu = Gpu::new();
        let (mut first, nodes) = store(&[(photo, JPEG), (icon, SVG)]);
        serve(&mut assets, &mut first, &mut gpu);

        let (draw, size) = drawn(&first, nodes[0]);
        assert!(matches!(draw, AssetDraw::Raster(_)), "{draw:?}");
        assert_eq!(size, [8.0, 4.0]);
        let (draw, size) = drawn(&first, nodes[1]);
        assert!(matches!(draw, AssetDraw::Vector(_)), "{draw:?}");
        assert_eq!(size, [10.0, 20.0]);
        assert_eq!(
            assets.counters(),
            AssetCounters {
                decodes: 2,
                decode_failures: 0,
                uploads: 1,
                upload_bytes: 8 * 4 * 4,
                evictions: 0,
            }
        );
        assert!(
            first.assets().payload(photo).is_none(),
            "the texture holds the pixels"
        );
        assert_eq!(first.assets().undecoded().count(), 0);

        // A second window: the scene it shares, the raster it decodes again,
        // since no table held its pixels.
        let mut second_gpu = Gpu::new();
        let (mut second, nodes) = store(&[(icon, SVG), (photo, JPEG)]);
        serve(&mut assets, &mut second, &mut second_gpu);
        assert!(matches!(drawn(&second, nodes[0]).0, AssetDraw::Vector(_)));
        assert!(matches!(drawn(&second, nodes[1]).0, AssetDraw::Raster(_)));
        assert_eq!(assets.counters().decodes, 3);
        assert_eq!(assets.counters().uploads, 2);

        // A frame that moved nothing serves nothing.
        let textures = gpu.backend.texture_count();
        serve(&mut assets, &mut first, &mut gpu);
        assert_eq!(gpu.backend.texture_count(), textures);
        assert_eq!(assets.counters().uploads, 2);
    }

    #[cfg(feature = "hot-reload")]
    #[test]
    fn a_replaced_raster_repaints_its_nodes_and_serves_later_tables_the_newest() {
        use viso_ui::DirtyClass;
        let (photo, icon) = (ResourceId::of("photo.jpg"), ResourceId::of("icon.svg"));
        let mut assets = Assets::default();
        let mut gpu = Gpu::new();
        let (mut first, nodes) = store(&[(photo, JPEG), (icon, SVG)]);
        serve(&mut assets, &mut first, &mut gpu);
        let AssetDraw::Raster(old) = drawn(&first, nodes[0]).0 else {
            panic!("resident");
        };
        first.clear_dirty();

        // The same size, other pixels: only the photo's node repaints, with a
        // new texture.
        let next = ResourceRevision(2);
        let image = RasterImage::rgba8(8, 4, vec![0x80; 8 * 4 * 4]);
        let bytes = AssetBytes::Shared(Arc::from(JPEG));
        assets.replace(
            photo,
            next,
            bytes.clone(),
            AssetPayload::Raster(Arc::new(image)),
            std::iter::once(&mut first),
        );
        assets.upload(&mut first, &mut gpu.backend, &mut gpu.renderer);
        let AssetDraw::Raster(new) = drawn(&first, nodes[0]).0 else {
            panic!("resident");
        };
        assert_ne!(new, old);
        assert_eq!(first.dirty(nodes[0]), DirtyClass::PAINT);
        assert!(first.dirty(nodes[1]).is_empty(), "the icon is untouched");
        assert_eq!(
            assets.counters().decodes,
            2,
            "the reload's payload is not decoded again"
        );

        // A table offering the build's bytes is served the newest instead.
        let (mut later, _) = store(&[(photo, JPEG)]);
        assets.serve(
            &mut later,
            Some((&mut gpu.backend, &mut gpu.renderer)),
            &LoopWaker::inert,
        );
        assert_eq!(
            later
                .assets()
                .undecoded()
                .next()
                .map(|(_, r, b)| (r, b.clone())),
            Some((next, bytes)),
            "the newest bytes, to decode"
        );
    }

    /// A converted image: the photo as RGB565, 8×4 texels laid out at 4×2.
    fn converted() -> &'static [u8] {
        static BYTES: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
        BYTES.get_or_init(|| {
            let decoded = viso_render::asset::decode_raster(JPEG).expect("decodes");
            let plan = viso_image::convert::Plan {
                sampler: viso_image::convert::Sampler::Cpu { rgb565: true },
                quality: viso_image::convert::Quality::High,
                format: None,
                surface: None,
                effort: viso_image::codec::Effort::Fast,
            };
            let texels = decoded.rgba8_texels().expect("texels");
            let image = plan.convert(&texels, decoded.width, decoded.height, [4.0, 2.0]);
            viso_image::texture::write(&image)
        })
    }

    #[test]
    fn a_converted_image_borrows_its_texels_and_a_device_lacking_its_format_gets_rgba8() {
        let bytes = AssetBytes::Static(converted());
        let Ok(AssetPayload::Raster(image)) = decode(&bytes, SampledFormats::all()) else {
            panic!("a raster");
        };
        assert_eq!(image.format, viso_gpu::TextureFormat::Rgb565Unorm);
        assert_eq!(image.size(), [4.0, 2.0], "lays out at its natural size");
        let texels = image.texels.as_static().expect("borrowed");
        assert_eq!(texels.as_ptr(), converted()[32..].as_ptr());

        let Ok(AssetPayload::Raster(fallback)) = decode(&bytes, SampledFormats::empty()) else {
            panic!("a raster");
        };
        assert_eq!(fallback.format, viso_gpu::TextureFormat::Rgba8Unorm);
        assert_eq!(
            (fallback.width, fallback.height, fallback.size()),
            (8, 4, [4.0, 2.0])
        );
        assert_eq!(fallback.to_rgba8(), image.to_rgba8(), "the same texels");

        // The CPU raster samples RGB565: uploaded from where it lies.
        let photo = ResourceId::of("photo.jpg");
        let mut assets = Assets::default();
        let mut gpu = Gpu::new();
        let (mut store, nodes) = store(&[(photo, converted())]);
        serve(&mut assets, &mut store, &mut gpu);
        assert!(matches!(
            drawn(&store, nodes[0]),
            (AssetDraw::Raster(_), [4.0, 2.0])
        ));
        assert_eq!(assets.counters().upload_bytes, 8 * 4 * 2);
    }

    #[test]
    fn a_new_revision_replaces_the_texture_and_a_bad_one_is_reported_once() {
        let photo = ResourceId::of("photo.jpg");
        let mut assets = Assets::default();
        let mut gpu = Gpu::new();
        let (mut store, nodes) = store(&[(photo, JPEG)]);
        serve(&mut assets, &mut store, &mut gpu);
        let AssetDraw::Raster(old) = drawn(&store, nodes[0]).0 else {
            panic!("resident");
        };

        let next = ResourceRevision(1);
        store
            .assets_mut()
            .offer(photo, next, AssetBytes::Static(JPEG));
        serve(&mut assets, &mut store, &mut gpu);
        let AssetDraw::Raster(new) = drawn(&store, nodes[0]).0 else {
            panic!("resident");
        };
        assert_ne!(new, old);
        assert_eq!(store.assets().texture(photo), Some((next, new)));

        let bad = ResourceRevision(2);
        store.assets_mut().offer(
            photo,
            bad,
            AssetBytes::Static(b"\x89PNG\r\n\x1a\nnot a png"),
        );
        serve(&mut assets, &mut store, &mut gpu);
        serve(&mut assets, &mut store, &mut gpu);
        assert_eq!(assets.counters().decode_failures, 1, "never retried");
        assert_eq!(
            drawn(&store, nodes[0]).0,
            AssetDraw::Raster(new),
            "the last good draws"
        );
    }

    #[test]
    fn a_file_reads_and_decodes_off_the_frame_path_and_bytes_given_at_run_time_decode_too() {
        let dir = std::env::temp_dir().join(format!("viso-assets-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("photo.jpg");
        std::fs::write(&path, JPEG).unwrap();
        let photo = viso_ui::Asset::file(&path);
        let given = viso_ui::Asset::bytes(SVG);
        assert_ne!(given.id, viso_ui::Asset::bytes(SVG).id, "each is its own");
        let missing = viso_ui::Asset::file(dir.join("missing.png"));

        let mut store = NodeStore::new();
        let mut nodes = Vec::new();
        {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(Default::default(), |cx| {
                for asset in [&photo, &given, &missing] {
                    let leaf = cx.leaf(LeafStyle {
                        size: Size::fixed(40.0, 40.0),
                        style: BoxStyle::NONE,
                    });
                    cx.asset(leaf, asset, AssetFit::Contain, None);
                    nodes.push(leaf.id());
                }
            });
        }
        assert_eq!(store.assets().unread().count(), 2, "nothing read at build");
        let (mut assets, mut gpu) = (Assets::default(), Gpu::new());
        serve(&mut assets, &mut store, &mut gpu);
        let (draw, size) = drawn(&store, nodes[0]);
        assert!(matches!(draw, AssetDraw::Raster(_)), "{draw:?}");
        assert_eq!(size, [8.0, 4.0]);
        assert!(matches!(drawn(&store, nodes[1]).0, AssetDraw::Vector(_)));
        assert_eq!(drawn(&store, nodes[2]).0, AssetDraw::Nothing);
        assert_eq!(assets.counters().decode_failures, 1, "the missing file");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
