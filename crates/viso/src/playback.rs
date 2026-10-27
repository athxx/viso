//! Animated images playing in place: a GIF, APNG, animated WebP or AVIF
//! sequence an asset node shows plays in the texture its first frame was
//! uploaded into.
//!
//! The asset worker decodes an animated image's first frame like any still
//! and hands back its encoded bytes, which the app keeps while a table shows
//! it. A window whose table holds its texture opens a player: a decoder on
//! the playback thread, which decodes one frame ahead, so the frame is ready
//! by the time it is due. Each frame step writes the frame that came due into
//! the texture and repaints the nodes showing it; the loop sleeps until the
//! next is due. Frames show for the time the file gives them, and the
//! animation plays as many times as it says.
//!
//! A player advances only while a node showing the image lets it play and
//! shows on screen — inside the window, unclipped, not hidden — and the
//! environment does not ask for reduced motion; otherwise it holds its
//! frame, and what was left of the frame's time when it stopped is what is
//! left when it plays again. An image no table holds closes its player.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use viso_gpu::GpuBackend;
use viso_image::{Animation, Loops};
use viso_platform::{Instant, LoopWaker};
use viso_render::Rect;
use viso_render::asset::{ResourceId, ResourceRevision};
use viso_ui::{AssetBytes, NodeStore, TextureId};

/// What the app asks the playback thread.
enum Order {
    /// Open `bytes` as player `key`'s animation and step past its first frame,
    /// which the texture already holds.
    Open {
        key: u64,
        bytes: AssetBytes,
    },
    /// Decode player `key`'s next frame into `buffer`.
    Next {
        key: u64,
        buffer: Vec<u8>,
    },
    Close {
        key: u64,
    },
}

/// What the playback thread answers.
enum Reply {
    /// Player `key`'s animation opened; its first frame shows for `delay`.
    Opened { key: u64, delay: Duration },
    /// Player `key`'s next frame, premultiplied RGBA8, and how long it shows.
    Frame {
        key: u64,
        pixels: Vec<u8>,
        delay: Duration,
    },
    /// Player `key`'s animation played as many times as it says, or could
    /// not be decoded further: it holds its last frame.
    Ended { key: u64 },
}

impl Reply {
    fn key(&self) -> u64 {
        match self {
            Reply::Opened { key, .. } | Reply::Frame { key, .. } | Reply::Ended { key } => *key,
        }
    }
}

/// One open animation and the plays it finished.
struct Track {
    animation: Animation,
    plays: u32,
}

/// The open animations, by player.
#[derive(Default)]
struct Decoders {
    tracks: HashMap<u64, Track>,
}

impl Decoders {
    fn handle(&mut self, order: Order) -> Option<Reply> {
        match order {
            Order::Open { key, bytes } => {
                let animation = Animation::open(&bytes).ok().flatten();
                let Some(mut animation) = animation else {
                    return Some(Reply::Ended { key });
                };
                let first = animation.next_frame().ok().flatten().map(|f| f.delay);
                let Some(delay) = first else {
                    return Some(Reply::Ended { key });
                };
                self.tracks.insert(
                    key,
                    Track {
                        animation,
                        plays: 0,
                    },
                );
                Some(Reply::Opened { key, delay })
            }
            Order::Next { key, mut buffer } => {
                let ended = Reply::Ended { key };
                let Some(track) = self.tracks.get_mut(&key) else {
                    return Some(ended);
                };
                for _ in 0..2 {
                    match track.animation.next_frame() {
                        Ok(Some(frame)) => {
                            buffer.clear();
                            buffer.extend_from_slice(frame.pixels);
                            return Some(Reply::Frame {
                                key,
                                pixels: buffer,
                                delay: frame.delay,
                            });
                        }
                        Ok(None) => {
                            track.plays += 1;
                            let again = match track.animation.loops() {
                                Loops::Forever => true,
                                Loops::Times(n) => track.plays < n.get(),
                            };
                            if !again || track.animation.rewind().is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                self.tracks.remove(&key);
                Some(ended)
            }
            Order::Close { key } => {
                self.tracks.remove(&key);
                None
            }
        }
    }
}

/// The playback thread.
struct Worker {
    orders: Sender<Order>,
    replies: Receiver<Reply>,
}

impl Worker {
    /// A thread that wakes the loop with `waker` after each reply; `None` when
    /// none can be spawned.
    fn spawn(waker: LoopWaker) -> Option<Worker> {
        let (orders, inbox) = mpsc::channel::<Order>();
        let (outbox, replies) = mpsc::channel();
        thread::Builder::new()
            .name("viso-playback".into())
            .spawn(move || {
                let mut decoders = Decoders::default();
                for order in inbox {
                    if let Some(reply) = decoders.handle(order) {
                        if outbox.send(reply).is_err() {
                            break;
                        }
                        waker.wake();
                    }
                }
            })
            .ok()?;
        Some(Worker { orders, replies })
    }
}

/// The app's animations: the encoded bytes of each animated image, the
/// playback thread, and the replies its windows have yet to take.
#[derive(Default)]
pub(crate) struct Playback {
    /// The bytes of each animated image, and the revision they are of.
    sources: HashMap<ResourceId, (ResourceRevision, AssetBytes)>,
    worker: Option<Worker>,
    /// Whether no thread could be spawned: frames decode in the frame step.
    inline: Option<Decoders>,
    replies: Vec<Reply>,
    next_key: u64,
    /// Frames written into textures, counted since the app started.
    frames: u64,
}

impl Playback {
    /// Records that resource `id` at `revision` is an animation encoded as
    /// `bytes`.
    pub(crate) fn animated(
        &mut self,
        id: ResourceId,
        revision: ResourceRevision,
        bytes: AssetBytes,
    ) {
        let newer = self.sources.get(&id).is_none_or(|(r, _)| revision >= *r);
        if newer {
            self.sources.insert(id, (revision, bytes));
        }
    }

    /// Forgets the animations `known` says no table knows.
    pub(crate) fn forget(&mut self, known: impl Fn(ResourceId) -> bool) {
        self.sources.retain(|&id, _| known(id));
    }

    /// Frames written into textures since the app started.
    pub(crate) fn frames(&self) -> u64 {
        self.frames
    }

    /// Takes the playback thread's replies; `true` when any arrived.
    pub(crate) fn receive(&mut self) -> bool {
        let before = self.replies.len();
        while let Some(worker) = &self.worker {
            match worker.replies.try_recv() {
                Ok(reply) => self.replies.push(reply),
                Err(TryRecvError::Empty) => break,
                // The thread died: decode in the frame step from now on.
                Err(TryRecvError::Disconnected) => {
                    self.worker = None;
                    self.inline = Some(Decoders::default());
                }
            }
        }
        self.replies.len() > before
    }

    fn send(&mut self, order: Order, waker: &dyn Fn() -> LoopWaker) {
        if self.worker.is_none() && self.inline.is_none() {
            self.worker = (crate::footprint::workers() > 0)
                .then(|| Worker::spawn(waker()))
                .flatten();
            if self.worker.is_none() {
                self.inline = Some(Decoders::default());
            }
        }
        let order = match &self.worker {
            Some(worker) => match worker.orders.send(order) {
                Ok(()) => return,
                Err(mpsc::SendError(order)) => {
                    self.worker = None;
                    self.inline = Some(Decoders::default());
                    order
                }
            },
            None => order,
        };
        if let Some(reply) = self.inline.get_or_insert_default().handle(order) {
            self.replies.push(reply);
        }
    }
}

/// One animation a window plays.
struct Player {
    resource: ResourceId,
    /// The revision and texture its frames are written into.
    revision: ResourceRevision,
    texture: TextureId,
    size: (u32, u32),
    key: u64,
    /// When the frame showing gives way to the next, while it plays.
    due: Option<Instant>,
    /// What is left of the frame's time while it does not play.
    left: Duration,
    /// The next frame, decoded, and how long it shows.
    ready: Option<(Vec<u8>, Duration)>,
    /// Whether the thread owes it a reply.
    asked: bool,
    /// Whether its animation opened, its first frame's time known.
    opened: bool,
    /// Whether it played at the last step.
    playing: bool,
    ended: bool,
    /// A frame's buffer, kept for the next.
    spare: Vec<u8>,
}

/// The animations a window plays.
#[derive(Default)]
pub(crate) struct Players {
    players: Vec<Player>,
}

impl Players {
    /// Whether the window plays no animation.
    pub(crate) fn is_empty(&self) -> bool {
        self.players.is_empty()
    }

    /// Plays the animations of `store` resident in `backend` at `now`, on
    /// screen in `viewport` (world space) unless `still`: the frames that
    /// came due are written and their nodes repainted. Returns when the next
    /// is due, `None` when none plays or the one due waits on the thread
    /// (whose reply wakes the loop).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn step<B: GpuBackend>(
        &mut self,
        store: &mut NodeStore,
        backend: &mut B,
        playback: &mut Playback,
        now: Instant,
        viewport: Rect,
        still: bool,
        waker: &dyn Fn() -> LoopWaker,
    ) -> Option<Instant> {
        if self.players.is_empty() && playback.sources.is_empty() {
            return None;
        }
        // A texture replaced or let go of closes its player.
        let table = store.assets();
        let mut i = 0;
        while i < self.players.len() {
            let p = &self.players[i];
            if table.texture(p.resource) != Some((p.revision, p.texture)) {
                let key = p.key;
                self.players.swap_remove(i);
                playback.send(Order::Close { key }, waker);
            } else {
                i += 1;
            }
        }
        // An animation resident with no player opens one.
        let mut opened = Vec::new();
        for (&id, (revision, bytes)) in &playback.sources {
            if let Some((r, texture)) = table.texture(id)
                && r == *revision
                && let Some(size) = table.texture_size(id)
                && !self.players.iter().any(|p| p.resource == id)
            {
                opened.push((id, r, texture, size, bytes.clone()));
            }
        }
        for (resource, revision, texture, size, bytes) in opened {
            let key = playback.next_key;
            playback.next_key += 1;
            self.players.push(Player {
                resource,
                revision,
                texture,
                size,
                key,
                due: None,
                left: Duration::ZERO,
                ready: None,
                asked: true,
                opened: false,
                playing: false,
                ended: false,
                spare: Vec::new(),
            });
            playback.send(Order::Open { key, bytes }, waker);
        }
        self.take(playback);
        self.play(store, backend, playback, now, viewport, still, waker);
        // Decoded where it plays, what was just asked for is at hand.
        if self.take(playback) {
            self.play(store, backend, playback, now, viewport, still, waker);
        }
        self.players
            .iter()
            .filter(|p| p.playing && p.ready.is_some())
            .filter_map(|p| p.due)
            .min()
    }

    /// Takes the replies for this window's players; `true` when any came.
    fn take(&mut self, playback: &mut Playback) -> bool {
        let mut took = false;
        let mut at = 0;
        while at < playback.replies.len() {
            let key = playback.replies[at].key();
            let Some(player) = self.players.iter_mut().find(|p| p.key == key) else {
                at += 1;
                continue;
            };
            took = true;
            player.asked = false;
            match playback.replies.swap_remove(at) {
                Reply::Opened { delay, .. } => {
                    player.left = delay;
                    player.opened = true;
                }
                Reply::Frame { pixels, delay, .. } => player.ready = Some((pixels, delay)),
                Reply::Ended { .. } => player.ended = true,
            }
        }
        took
    }

    /// Advances each player that plays, writing the frames due at `now`
    /// and asking for the next.
    #[allow(clippy::too_many_arguments)]
    fn play<B: GpuBackend>(
        &mut self,
        store: &mut NodeStore,
        backend: &mut B,
        playback: &mut Playback,
        now: Instant,
        viewport: Rect,
        still: bool,
        waker: &dyn Fn() -> LoopWaker,
    ) {
        for p in &mut self.players {
            p.playing = !p.ended
                && p.opened
                && !still
                && store
                    .assets()
                    .nodes(p.resource)
                    .iter()
                    .any(|&n| store.plays_asset(n) && store.on_screen(n, viewport));
            if !p.playing {
                if let Some(due) = p.due.take() {
                    p.left = due.saturating_duration_since(now);
                }
                continue;
            }
            let due = *p.due.get_or_insert(now + p.left);
            if now >= due
                && let Some((pixels, delay)) = p.ready.take()
            {
                let (w, h) = p.size;
                backend.write_texture(p.texture, 0, 0, w, h, &pixels);
                playback.frames += 1;
                store.repaint_asset(p.resource);
                p.spare = pixels;
                // On time the next follows this one's schedule; late, it
                // counts from now.
                let after = due + delay;
                p.due = Some(if after > now { after } else { now + delay });
            }
            if !p.asked && p.ready.is_none() {
                p.asked = true;
                let buffer = std::mem::take(&mut p.spare);
                playback.send(Order::Next { key: p.key, buffer }, waker);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::Assets;
    use viso_gpu::{CpuRaster, RawWindowHandle};
    use viso_render::Renderer;
    use viso_ui::{Asset, AssetFit, BoxStyle, BuildCx, LeafStyle, NodeId, Sizing};

    const GIF: &[u8] = include_bytes!("../../image/tests/fixtures/anim.gif");
    const APNG: &[u8] = include_bytes!("../../image/tests/fixtures/anim.png");

    fn open(decoders: &mut Decoders, bytes: &'static [u8]) -> Duration {
        match decoders.handle(Order::Open {
            key: 1,
            bytes: AssetBytes::Static(bytes),
        }) {
            Some(Reply::Opened { delay, .. }) => delay,
            _ => panic!("does not open"),
        }
    }

    /// The frames after the first until it ends, as their delays.
    fn rest(decoders: &mut Decoders) -> Vec<Duration> {
        let mut delays = Vec::new();
        for _ in 0..64 {
            match decoders.handle(Order::Next {
                key: 1,
                buffer: Vec::new(),
            }) {
                Some(Reply::Frame { pixels, delay, .. }) => {
                    assert_eq!(pixels.len(), 16 * 16 * 4);
                    delays.push(delay);
                }
                Some(Reply::Ended { .. }) => return delays,
                _ => panic!("no reply"),
            }
        }
        delays
    }

    #[test]
    fn a_track_plays_as_many_times_as_its_file_says() {
        let mut decoders = Decoders::default();
        // Twice through three frames: five after the first, then the end.
        assert_eq!(open(&mut decoders, APNG), Duration::from_millis(100));
        assert_eq!(rest(&mut decoders).len(), 5);
        assert!(decoders.tracks.is_empty(), "an ended track is closed");
        // Forever: it never ends.
        open(&mut decoders, GIF);
        assert_eq!(rest(&mut decoders).len(), 64);
        decoders.handle(Order::Close { key: 1 });
        assert!(decoders.tracks.is_empty());
    }

    const SCREEN: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 100.0,
        h: 100.0,
    };

    /// A window showing the GIF on one 16 × 16 leaf at its origin, resident
    /// in a raster device, its frames decoded where it plays.
    struct Window {
        store: NodeStore,
        leaf: NodeId,
        gif: ResourceId,
        assets: Assets,
        backend: CpuRaster,
        players: Players,
    }

    impl Window {
        fn new() -> Window {
            let mut store = NodeStore::new();
            let gif = Asset::bytes(GIF);
            let leaf = {
                let mut cx = BuildCx::new(&mut store);
                let handle = cx.leaf(LeafStyle {
                    size: Sizing::fixed(16.0, 16.0),
                    style: BoxStyle::NONE,
                });
                cx.asset(handle, &gif, AssetFit::Fill, None);
                handle.id()
            };
            let mut scratch = Vec::new();
            store.layout(leaf, SCREEN, &mut scratch);
            let mut backend = CpuRaster::new();
            let surface = backend.create_surface(RawWindowHandle::Headless, 64, 64);
            let format = backend.surface_format(surface);
            let mut renderer = Renderer::new(&mut backend, format);
            let mut assets = Assets::default();
            assets.playback.inline = Some(Decoders::default());
            for _ in 0..2000 {
                assets.provide(std::iter::once(&mut store));
                let _ = assets.serve(
                    &mut store,
                    Some((&mut backend, &mut renderer)),
                    &LoopWaker::inert,
                    Some(Duration::MAX),
                );
                if store.assets().texture(gif.id).is_some() {
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
            Window {
                store,
                leaf,
                gif: gif.id,
                assets,
                backend,
                players: Players::default(),
            }
        }

        fn step(&mut self, at: Instant, viewport: Rect, still: bool) -> Option<Instant> {
            self.players.step(
                &mut self.store,
                &mut self.backend,
                &mut self.assets.playback,
                at,
                viewport,
                still,
                &LoopWaker::inert,
            )
        }

        /// The texel at 5, 5 of the GIF's texture, RGBA.
        fn texel(&self) -> [u8; 4] {
            let (_, texture) = self.store.assets().texture(self.gif).expect("resident");
            let bgra = self.backend.read_texture(texture);
            let i = (5 * 16 + 5) * 4;
            [bgra[i + 2], bgra[i + 1], bgra[i], bgra[i + 3]]
        }
    }

    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];

    fn near(got: [u8; 4], want: [u8; 4]) -> bool {
        got.iter().zip(want).all(|(&g, w)| g.abs_diff(w) <= 4)
    }

    #[test]
    fn a_resident_animation_writes_each_frame_into_its_texture_when_it_is_due() {
        let mut w = Window::new();
        assert!(near(w.texel(), RED), "the first frame, decoded as a still");
        let t0 = Instant::now();
        // Opened, the first frame's time starts; the next is decoded ahead.
        let due = w.step(t0, SCREEN, false).expect("the next frame is due");
        assert_eq!(due, t0 + Duration::from_millis(100));
        assert!(near(w.texel(), RED), "not due yet");
        w.store.clear_dirty();
        assert_eq!(
            w.step(t0 + Duration::from_millis(50), SCREEN, false),
            Some(due)
        );
        let next = w.step(due, SCREEN, false).expect("and the one after");
        assert!(near(w.texel(), GREEN), "{:?}", w.texel());
        assert!(w.store.dirty(w.leaf).contains(viso_ui::DirtyClass::PAINT));
        assert_eq!(next, due + Duration::from_millis(100));
        assert_eq!(w.assets.playback.frames(), 1);
    }

    #[test]
    fn an_animation_off_screen_held_still_or_under_reduced_motion_holds_its_frame() {
        let mut w = Window::new();
        let t0 = Instant::now();
        w.step(t0, SCREEN, false);
        let away = Rect { x: 200.0, ..SCREEN };
        let later = t0 + Duration::from_secs(5);
        assert_eq!(w.step(later, away, false), None, "off screen it sleeps");
        assert_eq!(w.step(later, SCREEN, true), None, "reduced motion");
        w.store.set_asset_still(w.leaf, true);
        assert_eq!(w.step(later, SCREEN, false), None, "held still");
        assert!(near(w.texel(), RED));
        assert_eq!(w.assets.playback.frames(), 0);
        // Back on screen it shows what was left of its frame, then moves on.
        w.store.set_asset_still(w.leaf, false);
        let back = later + Duration::from_secs(1);
        assert_eq!(
            w.step(back, SCREEN, false),
            Some(back + Duration::from_millis(100))
        );
    }
}
