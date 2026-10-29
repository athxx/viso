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
//!
//! A glTF model decodes on the worker the same way — a `.gltf` read from a
//! file reads the buffers and images it names beside it — and each window
//! uploads its geometry into its own renderer; the table lets go of the
//! decoded geometry and images once the model is resident.
//!
//! An image shown from a URL is fetched through the app's [`ImageLoader`]
//! on the loop's thread — the future wakes the loop, which polls it — and its
//! bytes decode as any other's. Without a loader it fails. On the web, whose
//! build links no AVIF decoder, the browser decodes an AVIF the same way.

use std::collections::HashMap;
use std::fmt::Display;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Weak};
use std::task::{Context, Poll, Wake, Waker};
use std::thread;
use std::time::Duration;

use viso_gpu::{GpuBackend, SampledFormats, TextureDesc};
use viso_platform::LoopWaker;
use viso_render::Renderer;
use viso_render::asset::{AssetError, RasterImage, ResourceId, ResourceRevision, VectorScene};
use viso_render::scene3d::model::ModelAsset;
use viso_ui::{AssetBytes, AssetOrigin, AssetPayload, NodeStore};

/// Fetches the bytes of an image an app shows from a URL
/// ([`Asset::url`](viso_ui::Asset::url), the DSL `Image`'s `url`), with
/// whatever client the app uses:
///
/// ```ignore
/// fn image_loader() -> Option<ImageLoader> {
///     let client = viso_net::Client::new();
///     Some(ImageLoader::new(move |url| {
///         let response = client.get(url).send();
///         async move { response.await?.bytes().await }
///     }))
/// }
/// ```
///
/// The futures run on the loop's thread and need no runtime of their own:
/// one wakes the loop when it is ready. A failed fetch shows the image's
/// error view, and is not retried while a table keeps the resource.
#[derive(Clone)]
pub struct ImageLoader(Rc<dyn Fn(&str) -> Fetch>);

/// One fetch in flight.
type Fetch = Pin<Box<dyn Future<Output = Result<AssetBytes, String>>>>;

impl ImageLoader {
    /// A loader answering each URL with the bytes `fetch`'s future yields.
    pub fn new<F, Fut, B, E>(fetch: F) -> ImageLoader
    where
        F: Fn(&str) -> Fut + 'static,
        Fut: Future<Output = Result<B, E>> + 'static,
        B: Into<Arc<[u8]>>,
        E: Display,
    {
        ImageLoader(Rc::new(move |url| {
            let fetched = fetch(url);
            Box::pin(async move {
                fetched
                    .await
                    .map(|bytes| AssetBytes::Shared(bytes.into()))
                    .map_err(|error| error.to_string())
            })
        }))
    }
}

impl std::fmt::Debug for ImageLoader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ImageLoader")
    }
}

/// A fetch or browser decode in flight, and the resource it is for.
struct Fetching {
    id: ResourceId,
    revision: ResourceRevision,
    work: Pin<Box<dyn Future<Output = Result<Fetched, String>>>>,
}

/// What a fetch brought: the bytes to decode, or the browser's decode.
enum Fetched {
    Bytes(AssetBytes),
    #[cfg_attr(
        not(all(target_arch = "wasm32", target_os = "unknown")),
        allow(dead_code)
    )]
    Decoded(Decoded),
}

/// Whether the browser decodes `bytes` rather than the build: an AVIF on
/// the web, whose build links no AV1 decoder.
fn browser_decodes(bytes: &[u8]) -> bool {
    cfg!(all(target_arch = "wasm32", target_os = "unknown"))
        && viso_render::asset::AssetFormat::sniff(bytes)
            == Some(viso_render::asset::AssetFormat::Avif)
}

/// The browser's decode of `bytes`, the first frame of a sequence.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn browser_decode(bytes: AssetBytes) -> Pin<Box<dyn Future<Output = Result<Fetched, String>>>> {
    Box::pin(async move {
        let image = viso_platform::browser::decode_image(&bytes, "image/avif").await?;
        let raster = RasterImage::from_straight_rgba8(image.width, image.height, &image.rgba)
            .map_err(|error| error.to_string())?;
        Ok(Fetched::Decoded((
            AssetPayload::Raster(Arc::new(raster)),
            None,
        )))
    })
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn browser_decode(_: AssetBytes) -> Pin<Box<dyn Future<Output = Result<Fetched, String>>>> {
    Box::pin(async { Err("no browser decodes here".to_owned()) })
}

/// Wakes the loop for a fetch that can make progress.
struct FetchWaker(LoopWaker);

impl Wake for FetchWaker {
    fn wake(self: Arc<Self>) {
        self.0.wake();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.wake();
    }
}

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
    Model(Weak<ModelAsset>),
}

impl Held {
    fn of(payload: &AssetPayload) -> Held {
        match payload {
            AssetPayload::Raster(image) => Held::Raster(Arc::downgrade(image)),
            AssetPayload::Vector(scene) => Held::Vector(Arc::downgrade(scene)),
            AssetPayload::Model(model) => Held::Model(Arc::downgrade(model)),
        }
    }

    fn upgrade(&self) -> Option<AssetPayload> {
        match self {
            Held::Raster(image) => image.upgrade().map(AssetPayload::Raster),
            Held::Vector(scene) => scene.upgrade().map(AssetPayload::Vector),
            Held::Model(model) => model.upgrade().map(AssetPayload::Model),
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
    /// The bytes at a URL, fetched first, on the loop's thread.
    Url(Arc<str>),
}

/// The most of a file an asset reads: past it the file does not load.
const MAX_FILE: u64 = 1 << 30;

impl Input {
    /// Its payload — an animation's first frame, with the bytes the rest
    /// play from — or why it has none.
    fn decode(&self, sampled: SampledFormats) -> Result<Decoded, AssetError> {
        let (bytes, dir) = match self {
            Input::Bytes(bytes) => (bytes.clone(), None),
            Input::File(path) => (read(path)?, path.parent()),
            Input::Url(url) => {
                return Err(AssetError::Decode(format!("`{url}` is not fetched yet")));
            }
        };
        let payload = decode(&bytes, sampled, dir)?;
        let animated = matches!(payload, AssetPayload::Raster(_)) && animates(&bytes);
        Ok((payload, animated.then_some(bytes)))
    }
}

/// A payload, and an animation's bytes.
type Decoded = (AssetPayload, Option<AssetBytes>);

/// Whether `bytes`, decoded to a raster, play more frames than the first.
#[cfg(not(viso_converted_rasters))]
fn animates(bytes: &AssetBytes) -> bool {
    !viso_image::texture::is_texture(bytes) && viso_image::is_animated(bytes)
}

/// A build that converted every raster ships no animation.
#[cfg(viso_converted_rasters)]
fn animates(_: &AssetBytes) -> bool {
    false
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
    result: Result<Decoded, AssetError>,
}

/// What one [`Assets::serve`] did on the main thread.
#[must_use]
pub(crate) struct Served {
    /// Some decodes wait for the next frame's slice.
    pub(crate) deferred: bool,
    /// The time decodes took here, with no worker.
    pub(crate) inline: Duration,
}

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

/// Gives `store` resource `id`'s payload at `revision`, or its failure.
fn give(
    store: &mut NodeStore,
    id: ResourceId,
    revision: ResourceRevision,
    payload: Option<AssetPayload>,
) {
    match payload {
        Some(payload) => store.provide_asset(id, revision, payload),
        None => store.fail_asset(id, revision),
    }
}

/// `bytes` decoded by the format they are in, a raster in a format outside
/// `sampled` decoded on to RGBA8.
///
/// A build whose images were all converted links no decoder for what it
/// never ships: no raster decoder (`viso_converted_rasters`), no SVG
/// parser (`viso_converted_svg`); such a file is reported as not decoding.
/// A model reads the files it names by relative URI from `dir`: those of a
/// model with no directory must be embedded in it.
pub(crate) fn decode(
    bytes: &AssetBytes,
    sampled: SampledFormats,
    dir: Option<&Path>,
) -> Result<AssetPayload, AssetError> {
    use viso_render::asset::{AssetFormat, AssetKind};
    let raster = |image| Ok(AssetPayload::Raster(Arc::new(sampleable(image, sampled)?)));
    if viso_image::texture::is_texture(bytes) {
        return raster(viso_image::texture::read(bytes)?);
    }
    match AssetFormat::sniff(bytes).map(|format| (format, format.kind())) {
        // A video plays through the platform's player, never decoded here.
        Some((format, AssetKind::Video)) => Err(AssetError::Kind(format)),
        #[cfg(not(viso_converted_rasters))]
        Some((_, AssetKind::Raster)) => raster(viso_render::asset::decode_raster_asset(bytes)?),
        #[cfg(not(viso_converted_svg))]
        Some((_, AssetKind::Vector)) => viso_render::svg::parse_svg(bytes)
            .map(|scene| AssetPayload::Vector(Arc::new(scene.into_vector())))
            .map_err(|error| AssetError::Decode(error.to_string())),
        Some((_, AssetKind::Model)) => decode_model(bytes, dir).map(AssetPayload::Model),
        #[cfg(any(viso_converted_rasters, viso_converted_svg))]
        Some(_) => Err(AssetError::Decode(
            "the build converted every image of this kind, and this one is not".to_owned(),
        )),
        None => Err(AssetError::Format),
    }
}

/// The glTF model in `bytes`, the buffers and images it names by relative
/// URI read from `dir` (each at most [`MAX_FILE`] bytes).
fn decode_model(bytes: &[u8], dir: Option<&Path>) -> Result<Arc<ModelAsset>, AssetError> {
    let mut resolve = |uri: &str| -> Option<Vec<u8>> {
        let bytes = read(&dir?.join(uri)).ok()?;
        Some(bytes.to_vec())
    };
    match ModelAsset::decode_linked(bytes, &mut resolve) {
        Some(model) => model
            .map(Arc::new)
            .map_err(|error| AssetError::Decode(error.to_string())),
        None => Err(AssetError::Decode(
            "nothing in this app draws a model, so none decodes".to_owned(),
        )),
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
    models: Vec<(ResourceId, ResourceRevision, Arc<ModelAsset>)>,
    /// The formats every device served so far samples; every format until
    /// one is.
    sampled: Option<SampledFormats>,
    /// The animated images, playing.
    pub(crate) playback: crate::playback::Playback,
    counters: AssetCounters,
    /// What fetches the images shown from URLs.
    loader: Option<ImageLoader>,
    fetching: Vec<Fetching>,
    /// Wakes the loop for the fetches in flight.
    fetch_waker: Option<Waker>,
}

impl Assets {
    /// The app's assets, the images shown from URLs fetched through
    /// `loader`.
    pub(crate) fn with_loader(loader: Option<ImageLoader>) -> Assets {
        Assets {
            loader,
            ..Assets::default()
        }
    }

    pub(crate) fn counters(&self) -> AssetCounters {
        self.counters
    }

    /// Counts `n` images a window let go of.
    pub(crate) fn evicted(&mut self, n: usize) {
        self.counters.evictions += n as u64;
    }

    /// Takes the worker's results, and decodes what the fetches in flight
    /// fetched; `true` when any result arrived, for the caller to frame the
    /// windows.
    pub(crate) fn receive(&mut self) -> bool {
        let before = self.arrived.len();
        self.poll_fetches();
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
            payloads.push(self.settle(done));
        }
        for store in stores {
            for (id, revision, payload) in &payloads {
                if store.assets().knows(*id) {
                    give(store, *id, *revision, payload.clone());
                }
            }
        }
    }

    /// Polls the fetches in flight, handing what each fetched to the worker,
    /// or decoding it here where none runs.
    fn poll_fetches(&mut self) {
        let Some(waker) = self.fetch_waker.clone() else {
            return;
        };
        let mut cx = Context::from_waker(&waker);
        let sampled = self.sampled.unwrap_or(SampledFormats::all());
        let mut i = 0;
        while i < self.fetching.len() {
            let Poll::Ready(fetched) = self.fetching[i].work.as_mut().poll(&mut cx) else {
                i += 1;
                continue;
            };
            let Fetching { id, revision, .. } = self.fetching.swap_remove(i);
            let input = match fetched {
                Ok(Fetched::Bytes(bytes)) if browser_decodes(&bytes) => {
                    self.fetching.push(Fetching {
                        id,
                        revision,
                        work: browser_decode(bytes),
                    });
                    waker.wake_by_ref();
                    continue;
                }
                Ok(Fetched::Bytes(bytes)) => Input::Bytes(bytes),
                Ok(Fetched::Decoded(decoded)) => {
                    self.arrived.push(Done {
                        id,
                        revision,
                        result: Ok(decoded),
                    });
                    continue;
                }
                Err(error) => {
                    self.arrived.push(Done {
                        id,
                        revision,
                        result: Err(AssetError::Decode(error)),
                    });
                    continue;
                }
            };
            let job = Job {
                id,
                revision,
                input,
                sampled,
            };
            let job = match &self.worker {
                Some(worker) => match worker.jobs.send(job) {
                    Ok(()) => continue,
                    Err(mpsc::SendError(job)) => job,
                },
                None => job,
            };
            self.arrived.push(Done {
                id,
                revision,
                result: job.input.decode(sampled),
            });
        }
    }

    /// Starts fetching resource `id` from `url` through the loader, or fails
    /// it at once without one.
    fn fetch(&mut self, id: ResourceId, revision: ResourceRevision, url: &str, waker: LoopWaker) {
        let Some(loader) = &self.loader else {
            self.decoding.insert(id, revision);
            self.arrived.push(Done {
                id,
                revision,
                result: Err(AssetError::Decode(format!(
                    "`{url}`: the app has no image loader (Application::image_loader)"
                ))),
            });
            return;
        };
        let fetch = (loader.0)(url);
        let work = Box::pin(async move { fetch.await.map(Fetched::Bytes) });
        self.start(id, revision, work, waker);
    }

    /// Runs `work` for resource `id` at `revision` on the loop's thread.
    fn start(
        &mut self,
        id: ResourceId,
        revision: ResourceRevision,
        work: Pin<Box<dyn Future<Output = Result<Fetched, String>>>>,
        waker: LoopWaker,
    ) {
        self.decoding.insert(id, revision);
        let waker = self
            .fetch_waker
            .get_or_insert_with(|| Waker::from(Arc::new(FetchWaker(waker))));
        // Work that is ready at once is taken on the next wake.
        waker.wake_by_ref();
        self.fetching.push(Fetching { id, revision, work });
    }

    /// Records a result: the payload to provide, or, reported, none.
    fn settle(&mut self, done: Done) -> (ResourceId, ResourceRevision, Option<AssetPayload>) {
        match done.result {
            Ok((payload, animation)) => {
                self.counters.decodes += 1;
                if let Some(bytes) = animation {
                    self.playback.animated(done.id, done.revision, bytes);
                }
                let newer = self
                    .decoded
                    .get(&done.id)
                    .is_none_or(|(r, held)| done.revision > *r || held.upgrade().is_none());
                if newer {
                    self.decoded
                        .insert(done.id, (done.revision, Held::of(&payload)));
                }
                (done.id, done.revision, Some(payload))
            }
            Err(error) => {
                self.counters.decode_failures += 1;
                let failed = self.failed.entry(done.id).or_insert(done.revision);
                *failed = (*failed).max(done.revision);
                eprintln!(
                    "[viso] asset {:016x} (revision {}) does not decode: {error}",
                    done.id.0, done.revision.0
                );
                (done.id, done.revision, None)
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
        if matches!(payload, AssetPayload::Raster(_)) && animates(&bytes) {
            self.playback.animated(id, revision, bytes.clone());
        }
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
    /// With no worker the decodes run here, within `slice` (at least one
    /// decode; none when it is `None`), and all of them in a headless build.
    /// What they took is
    /// [`Served::inline`]; [`Served::deferred`] when some wait for the next
    /// frame's slice.
    pub(crate) fn serve<B: GpuBackend>(
        &mut self,
        store: &mut NodeStore,
        gpu: Option<(&mut B, &mut Renderer)>,
        waker: &dyn Fn() -> LoopWaker,
        slice: Option<Duration>,
    ) -> Served {
        let mut inline = Duration::ZERO;
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
        todo.extend(table.unread().map(|(id, revision, origin)| {
            let input = match origin {
                AssetOrigin::File(path) => Input::File(Arc::clone(path)),
                AssetOrigin::Url(url) => Input::Url(Arc::clone(url)),
            };
            (id, revision, input)
        }));
        for (id, revision, input) in todo.drain(..) {
            if let Some((decoded, held)) = self.decoded.get(&id)
                && *decoded >= revision
                && let Some(payload) = held.upgrade()
            {
                store.provide_asset(id, *decoded, payload);
                continue;
            }
            let pending = |at: Option<&ResourceRevision>| at.is_some_and(|r| *r >= revision);
            // Known not to decode: the table learns it, nothing is retried.
            if let Some(&failed) = self.failed.get(&id)
                && failed >= revision
            {
                store.fail_asset(id, failed);
                continue;
            }
            if pending(self.decoding.get(&id)) {
                continue;
            }
            match &input {
                Input::Url(url) => {
                    self.fetch(id, revision, url, waker());
                    continue;
                }
                Input::Bytes(bytes) if browser_decodes(bytes) => {
                    self.start(id, revision, browser_decode(bytes.clone()), waker());
                    continue;
                }
                _ => {}
            }
            // A headless build decodes here, every decode on the frame that
            // asks for it, as it lays its text out: its frames step a fixed
            // clock, and a decode finishing between them would land on no
            // frame in particular.
            if self.worker.is_none() && !self.inline {
                self.worker = (!cfg!(feature = "headless") && crate::footprint::workers() > 0)
                    .then(|| Worker::spawn(waker()))
                    .flatten();
                self.inline = self.worker.is_none();
            }
            if self.inline
                && !cfg!(feature = "headless")
                && slice.is_none_or(|slice| inline >= slice)
            {
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
                    let start = viso_platform::Instant::now();
                    let result = input.decode(sampled);
                    inline += start.elapsed();
                    let done = Done {
                        id,
                        revision,
                        result,
                    };
                    let (id, revision, payload) = self.settle(done);
                    give(store, id, revision, payload);
                }
            }
        }
        self.scratch = todo;
        if let Some((backend, renderer)) = gpu {
            self.upload(store, backend, renderer);
        }
        Served { deferred, inline }
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
        self.models.clear();
        self.models.extend(
            store
                .assets()
                .unresident_models()
                .map(|(id, revision, model)| (id, revision, Arc::clone(model))),
        );
        for (id, revision, asset) in self.models.drain(..) {
            let model = renderer.create_scene_model(backend, &asset);
            self.counters.uploads += 1;
            self.counters.upload_bytes += model.bytes();
            if let Err(unused) = store.attach_model(id, revision, model) {
                renderer.release_scene_model(backend, &unused);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_gpu::{CpuRaster, RawWindowHandle};
    use viso_ui::{AssetDraw, AssetFit, BoxStyle, BuildCx, Content, LeafStyle, NodeId, Sizing};

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
                            size: Sizing::fixed(40.0, 40.0),
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
            let _ = assets.serve(
                store,
                Some((&mut gpu.backend, &mut gpu.renderer)),
                &LoopWaker::inert,
                Some(Duration::MAX),
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
        let _ = assets.serve(
            &mut later,
            Some((&mut gpu.backend, &mut gpu.renderer)),
            &LoopWaker::inert,
            Some(Duration::MAX),
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
        let Ok(AssetPayload::Raster(image)) = decode(&bytes, SampledFormats::all(), None) else {
            panic!("a raster");
        };
        assert_eq!(image.format, viso_gpu::TextureFormat::Rgb565Unorm);
        assert_eq!(image.size(), [4.0, 2.0], "lays out at its natural size");
        let texels = image.texels.as_static().expect("borrowed");
        assert_eq!(texels.as_ptr(), converted()[32..].as_ptr());

        let Ok(AssetPayload::Raster(fallback)) = decode(&bytes, SampledFormats::empty(), None)
        else {
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
                        size: Sizing::fixed(40.0, 40.0),
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
        assert_eq!(
            drawn(&store, nodes[2]).0,
            AssetDraw::Failed,
            "the missing file"
        );
        assert_eq!(assets.counters().decode_failures, 1);
        // A window showing it later learns it at once, nothing retried.
        let mut later = NodeStore::new();
        let leaf = {
            let mut cx = BuildCx::new(&mut later);
            let leaf = cx.leaf(LeafStyle {
                size: Sizing::fixed(40.0, 40.0),
                style: BoxStyle::NONE,
            });
            cx.asset(leaf, &missing, AssetFit::Contain, None);
            leaf.id()
        };
        serve(&mut assets, &mut later, &mut gpu);
        assert_eq!(drawn(&later, leaf).0, AssetDraw::Failed);
        assert_eq!(assets.counters().decode_failures, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_model_a_scene_asks_for_decodes_with_its_buffer_and_is_resident_until_let_go_of() {
        let dir = std::env::temp_dir().join(format!("viso-assets-model-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // One triangle whose positions lie in a buffer beside the document.
        let positions: Vec<u8> = [-1.0f32, -1.0, 0.0, 1.0, -1.0, 0.0, 0.0, 1.0, 0.0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        std::fs::write(dir.join("tri.bin"), &positions).unwrap();
        let gltf = br#"{"asset":{"version":"2.0"},"scenes":[{"nodes":[0]}],"nodes":[{"mesh":0}],
"meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
"accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3"}],
"bufferViews":[{"buffer":0,"byteLength":36}],"buffers":[{"byteLength":36,"uri":"tri.bin"}]}"#;
        std::fs::write(dir.join("tri.gltf"), gltf).unwrap();
        let asset = viso_ui::Asset::file(dir.join("tri.gltf"));
        assert_eq!(asset.kind, viso_render::asset::AssetKind::Model);

        let mut store = NodeStore::new();
        let node = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle {
                size: Sizing::fixed(40.0, 40.0),
                style: BoxStyle::NONE,
            })
            .id()
        };
        let slot = store.set_scene(node).unwrap();
        assert!(store.scene_model(slot, &asset).is_none(), "not read yet");
        assert_eq!(store.assets().users(asset.id), 1);
        assert!(store.scene_model(slot, &asset).is_none());
        assert_eq!(store.assets().users(asset.id), 1, "held once");

        let (mut assets, mut gpu) = (Assets::default(), Gpu::new());
        serve(&mut assets, &mut store, &mut gpu);
        let model = store.scene_model(slot, &asset).expect("resident");
        assert_eq!(model.rig().order, [0]);
        assert_eq!(model.bytes(), 3 * (12 + 12 + 8 + 16) + 3 * 4);
        assert!(
            store.assets().payload(asset.id).is_none(),
            "the decoded geometry is let go of"
        );
        assert!(store.scenes_mut().frame_mut(slot).is_some());

        // The node leaving the scene lets go of the model, which a sweep then
        // retires for the renderer to release.
        store.set_asset(node, ResourceId::of("other.png"), AssetFit::Contain, None);
        assert_eq!(store.assets().users(asset.id), 0);
        store.assets_mut().evict_unshown(|_| {});
        let retired: Vec<_> = store.assets_mut().take_retired_models().collect();
        assert_eq!(retired.len(), 1);
        assert!(Arc::ptr_eq(&retired[0], &model));
        for model in &retired {
            gpu.renderer.release_scene_model(&mut gpu.backend, model);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Pending once, then ready: a fetch that answers on a later wake.
    struct Later(bool);

    impl Future for Later {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if std::mem::replace(&mut self.0, true) {
                Poll::Ready(())
            } else {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }

    /// A store showing each of `assets` in a leaf of its own.
    fn showing(assets: &[&viso_ui::Asset]) -> (NodeStore, Vec<NodeId>) {
        let mut store = NodeStore::new();
        let mut nodes = Vec::new();
        {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(Default::default(), |cx| {
                for asset in assets {
                    let leaf = cx.leaf(LeafStyle {
                        size: Sizing::fixed(40.0, 40.0),
                        style: BoxStyle::NONE,
                    });
                    cx.asset(leaf, asset, AssetFit::Contain, None);
                    nodes.push(leaf.id());
                }
            });
        }
        (store, nodes)
    }

    #[test]
    fn an_image_from_a_url_is_fetched_through_the_loader_once_and_decoded() {
        let asked = Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = asked.clone();
        let loader = ImageLoader::new(move |url| {
            seen.borrow_mut().push(url.to_owned());
            let found = url.ends_with(".svg?size=2");
            async move {
                Later(false).await;
                if found { Ok(SVG) } else { Err("404") }
            }
        });
        let picture = viso_ui::Asset::url("https://example.org/a.svg?size=2");
        assert_eq!(picture.kind, viso_render::asset::AssetKind::Vector);
        let missing = viso_ui::Asset::url("https://example.org/gone.png");
        let (mut store, nodes) = showing(&[&picture, &missing, &picture]);
        let (mut assets, mut gpu) = (Assets::with_loader(Some(loader)), Gpu::new());
        serve(&mut assets, &mut store, &mut gpu);
        let (draw, size) = drawn(&store, nodes[0]);
        assert!(matches!(draw, AssetDraw::Vector(_)), "{draw:?}");
        assert_eq!(size, [10.0, 20.0]);
        assert_eq!(drawn(&store, nodes[1]).0, AssetDraw::Failed);
        let mut asked = asked.borrow().clone();
        asked.sort();
        assert_eq!(
            asked,
            [
                "https://example.org/a.svg?size=2",
                "https://example.org/gone.png"
            ],
            "each URL fetched once"
        );
    }

    #[test]
    fn an_image_from_a_url_fails_without_a_loader() {
        let picture = viso_ui::Asset::url("https://example.org/a.png");
        let (mut store, nodes) = showing(&[&picture]);
        let (mut assets, mut gpu) = (Assets::default(), Gpu::new());
        serve(&mut assets, &mut store, &mut gpu);
        assert_eq!(drawn(&store, nodes[0]).0, AssetDraw::Failed);
    }
}
