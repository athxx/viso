//! A window's videos: one platform player a video node, steered by what the
//! app asks of the node and reporting back what it does, its pictures
//! written into a texture of the window's renderer.
//!
//! A player opens when its node first shows its source and closes when the
//! node lets it go. While it plays and a node shows it on screen, the window
//! beats each frame and takes each new picture: the first, and any of another
//! size, becomes a texture of its own; the rest are written into it, marking
//! only `PAINT`. Off screen it plays on unread. A player loading, or playing
//! where nothing beats for it, is polled on a timer instead.

use std::time::Duration;

use viso_gpu::{GpuBackend, TextureDesc, TextureFormat, TextureId};
use viso_media::{Frame, VideoError, VideoEvent, VideoPlayer};
use viso_render::Renderer;
use viso_runtime::Instant;
use viso_ui::{NodeId, NodeStore, Playback, Rect, VideoSource, resource::ResourceId};

/// How often a player is polled while nothing beats for it.
const POLL: Duration = Duration::from_millis(100);

/// How far a playing video moves between reports of where it is.
const REPORTED: Duration = Duration::from_millis(250);

/// What a window's videos play through: the platform's player, or a
/// scripted one in a test.
pub(crate) trait Play: Sized {
    fn open(source: &viso_media::VideoSource) -> Result<Self, VideoError>;
    fn poll(&mut self) -> Option<VideoEvent>;
    fn frame(&mut self) -> Option<Frame<'_>>;
    fn play(&mut self);
    fn pause(&mut self);
    fn seek(&mut self, time: Duration);
    fn set_rate(&mut self, rate: f32);
    fn set_volume(&mut self, volume: f32);
    fn set_muted(&mut self, muted: bool);
    fn set_looping(&mut self, looping: bool);
    fn position(&self) -> Duration;
    fn is_playing(&self) -> bool;
}

impl Play for VideoPlayer {
    fn open(source: &viso_media::VideoSource) -> Result<Self, VideoError> {
        VideoPlayer::open(source)
    }
    fn poll(&mut self) -> Option<VideoEvent> {
        VideoPlayer::poll(self)
    }
    fn frame(&mut self) -> Option<Frame<'_>> {
        VideoPlayer::frame(self)
    }
    fn play(&mut self) {
        VideoPlayer::play(self);
    }
    fn pause(&mut self) {
        VideoPlayer::pause(self);
    }
    fn seek(&mut self, time: Duration) {
        VideoPlayer::seek(self, time);
    }
    fn set_rate(&mut self, rate: f32) {
        VideoPlayer::set_rate(self, rate);
    }
    fn set_volume(&mut self, volume: f32) {
        VideoPlayer::set_volume(self, volume);
    }
    fn set_muted(&mut self, muted: bool) {
        VideoPlayer::set_muted(self, muted);
    }
    fn set_looping(&mut self, looping: bool) {
        VideoPlayer::set_looping(self, looping);
    }
    fn position(&self) -> Duration {
        VideoPlayer::position(self)
    }
    fn is_playing(&self) -> bool {
        VideoPlayer::is_playing(self)
    }
}

/// The players of one window.
pub(crate) struct Videos<P = VideoPlayer> {
    players: Vec<Played<P>>,
    /// Reused for a picture's texels.
    scratch: Vec<u8>,
    /// Pictures written into textures, for the counters.
    pub(crate) frames: u64,
}

impl<P> Default for Videos<P> {
    fn default() -> Self {
        Videos {
            players: Vec::new(),
            scratch: Vec::new(),
            frames: 0,
        }
    }
}

/// One video node's player.
struct Played<P> {
    node: NodeId,
    resource: ResourceId,
    /// `None` once it failed.
    player: Option<P>,
    /// What was last asked of the player.
    asked: Playback,
    ready: bool,
    /// The texture its pictures are written into, and its size.
    texture: Option<(TextureId, (u32, u32))>,
}

/// What a step leaves for the loop.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Step {
    /// Whether a video plays where a node shows it: the window beats each
    /// frame.
    pub(crate) beating: bool,
    /// When a player is next polled with nothing beating.
    pub(crate) due: Option<Instant>,
}

impl<P: Play> Videos<P> {
    pub(crate) fn is_empty(&self) -> bool {
        self.players.is_empty()
    }

    /// Opens a player for each new video node and closes those of nodes
    /// gone, carries each node's requests to its player, reports what it
    /// did, and writes the pictures due now that a node shows within
    /// `viewport` into `gpu`'s textures.
    pub(crate) fn step<B: GpuBackend>(
        &mut self,
        store: &mut NodeStore,
        mut gpu: Option<(&mut B, &mut Renderer)>,
        now: Instant,
        viewport: Rect,
    ) -> Step {
        if self.players.is_empty() && store.videos().is_empty() {
            return Step::default();
        }
        // Players whose node let go of them.
        let mut at = 0;
        while at < self.players.len() {
            let p = &self.players[at];
            let live = store
                .videos()
                .get(p.node)
                .is_some_and(|v| v.resource == p.resource);
            if live {
                at += 1;
                continue;
            }
            let gone = self.players.swap_remove(at);
            let texture = store.forget_video(gone.resource);
            if let (Some(texture), Some((backend, renderer))) = (texture, gpu.as_mut()) {
                renderer.release_texture(*backend, texture);
            }
        }
        // Nodes new to their source.
        let mut opened = Vec::new();
        for entry in store.videos().iter() {
            if self.players.iter().any(|p| p.resource == entry.resource) {
                continue;
            }
            let player = match P::open(&media_source(&entry.source)) {
                Ok(player) => Some(player),
                Err(error) => {
                    eprintln!("[viso] video: {error}");
                    None
                }
            };
            opened.push((entry.node, entry.resource, player));
        }
        for (node, resource, player) in opened {
            if player.is_none() {
                store.fail_video(resource);
            }
            self.players.push(Played {
                node,
                resource,
                player,
                asked: Playback {
                    // Opened paused, at full volume and its own speed.
                    playing: false,
                    ..Playback::default()
                },
                ready: false,
                texture: None,
            });
        }
        let mut step = Step::default();
        for p in &mut self.players {
            let Some(entry) = store.videos_mut().get_mut(p.node) else {
                continue;
            };
            let Some(player) = &mut p.player else {
                continue;
            };
            // What the app asks, carried to the player.
            let wish = entry.playback;
            if wish.looping != p.asked.looping {
                player.set_looping(wish.looping);
            }
            if wish.muted != p.asked.muted {
                player.set_muted(wish.muted);
            }
            if wish.volume != p.asked.volume {
                player.set_volume(wish.volume);
            }
            if wish.rate != p.asked.rate {
                player.set_rate(wish.rate);
            }
            if wish.playing != p.asked.playing {
                if wish.playing {
                    player.play();
                } else {
                    player.pause();
                }
            }
            p.asked = wish;
            let sought = entry.seek.take();
            if let Some(time) = sought {
                player.seek(time);
            }
            // What the player did.
            let mut status = entry.status;
            if sought.is_some() || wish.playing {
                status.ended = false;
            }
            let mut failed = false;
            let mut size = None;
            while let Some(event) = player.poll() {
                match event {
                    VideoEvent::Ready(info) => {
                        p.ready = true;
                        status.ready = true;
                        status.duration = info.duration;
                        size = Some((info.width, info.height));
                    }
                    VideoEvent::Ended => {
                        status.ended = true;
                        entry.playback.playing = false;
                        p.asked.playing = false;
                    }
                    VideoEvent::Waiting(waiting) => status.waiting = waiting,
                    VideoEvent::Failed(error) => {
                        eprintln!("[viso] video: {error}");
                        failed = true;
                        break;
                    }
                }
            }
            status.playing = player.is_playing();
            if !status.playing {
                status.waiting = false;
            }
            let position = player.position();
            // A playing video reports where it is a few times a second; a
            // seek or any other change at once.
            let moved = position.abs_diff(entry.status.position) >= REPORTED
                || sought.is_some()
                || !status.playing;
            status.position = if moved {
                position
            } else {
                entry.status.position
            };
            entry.status = status;
            if failed {
                p.player = None;
                if let Some(texture) = store.fail_video(p.resource)
                    && let Some((backend, renderer)) = gpu.as_mut()
                {
                    renderer.release_texture(*backend, texture);
                }
                p.texture = None;
                continue;
            }
            if let Some((w, h)) = size {
                store.size_video(p.resource, w, h);
            }
            let shown = store.on_screen(p.node, viewport);
            if !p.ready {
                step.due = earliest(step.due, now + POLL);
                continue;
            }
            if status.playing {
                if shown {
                    step.beating = true;
                } else {
                    step.due = earliest(step.due, now + POLL);
                }
            }
            // A picture to show: the first wherever it lies, the next where
            // a node shows it.
            let Some((backend, renderer)) = gpu.as_mut() else {
                continue;
            };
            if !shown && p.texture.is_some() {
                continue;
            }
            let Some(player) = &mut p.player else {
                continue;
            };
            let Some(frame) = player.frame() else {
                continue;
            };
            frame.copy_rgba(&mut self.scratch);
            let texels = (frame.width, frame.height);
            match p.texture {
                Some((texture, at)) if at == texels => {
                    backend.write_texture(texture, 0, 0, texels.0, texels.1, &self.scratch);
                    store.repaint_asset(p.resource);
                }
                _ => {
                    // A video's texels are sRGB-encoded: sampling decodes them.
                    let texture = backend.create_texture(&TextureDesc {
                        width: texels.0,
                        height: texels.1,
                        format: TextureFormat::Rgba8UnormSrgb,
                        render_target: false,
                        label: "video",
                    });
                    backend.write_texture(texture, 0, 0, texels.0, texels.1, &self.scratch);
                    if let Some(old) = store.show_video_frame(p.resource, texture, texels) {
                        renderer.release_texture(*backend, old);
                    }
                    p.texture = Some((texture, texels));
                }
            }
            self.frames += 1;
        }
        step
    }
}

/// The earlier of `due` and `at`.
fn earliest(due: Option<Instant>, at: Instant) -> Option<Instant> {
    Some(due.map_or(at, |due| due.min(at)))
}

/// A node's source as the player opens it.
fn media_source(source: &VideoSource) -> viso_media::VideoSource {
    match source {
        VideoSource::File(path) => viso_media::VideoSource::File(path.to_path_buf()),
        VideoSource::Url(url) => viso_media::VideoSource::Url(url.to_string()),
        VideoSource::Bytes(viso_ui::AssetBytes::Static(bytes)) => {
            viso_media::VideoSource::Static(bytes)
        }
        VideoSource::Bytes(viso_ui::AssetBytes::Shared(bytes)) => {
            viso_media::VideoSource::Bytes(std::sync::Arc::clone(bytes))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    use viso_gpu::{CpuRaster, RawWindowHandle};
    use viso_media::{PixelLayout, VideoInfo};
    use viso_ui::{AssetDraw, AssetFit, BoxStyle, BuildCx, Content, LeafStyle, Size};

    /// What a scripted player does: the events it reports and the pictures
    /// it hands over, in order, and what was asked of it.
    #[derive(Default)]
    struct Script {
        events: VecDeque<VideoEvent>,
        /// Each picture: its size and one RGB colour.
        frames: VecDeque<(u32, u32, [u8; 3])>,
        playing: bool,
        position: Duration,
        looping: bool,
        muted: bool,
        seeks: Vec<Duration>,
        dropped: bool,
    }

    thread_local! {
        /// The scripts of the players opened on this thread, in order.
        static OPENED: RefCell<Vec<Rc<RefCell<Script>>>> = const { RefCell::new(Vec::new()) };
    }

    struct Fake {
        script: Rc<RefCell<Script>>,
        picture: Vec<u8>,
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            self.script.borrow_mut().dropped = true;
        }
    }

    impl Play for Fake {
        fn open(source: &viso_media::VideoSource) -> Result<Self, VideoError> {
            if matches!(source, viso_media::VideoSource::Url(url) if url == "bad:") {
                return Err(VideoError::Open("bad".into()));
            }
            let script = Rc::new(RefCell::new(Script::default()));
            OPENED.with(|o| o.borrow_mut().push(Rc::clone(&script)));
            Ok(Fake {
                script,
                picture: Vec::new(),
            })
        }
        fn poll(&mut self) -> Option<VideoEvent> {
            self.script.borrow_mut().events.pop_front()
        }
        fn frame(&mut self) -> Option<Frame<'_>> {
            let (w, h, [r, g, b]) = self.script.borrow_mut().frames.pop_front()?;
            self.picture = [b, g, r, 255].repeat((w * h) as usize);
            Some(Frame {
                width: w,
                height: h,
                layout: PixelLayout::Bgra8,
                rows: &self.picture,
                stride: w as usize * 4,
                time: Duration::ZERO,
            })
        }
        fn play(&mut self) {
            self.script.borrow_mut().playing = true;
        }
        fn pause(&mut self) {
            self.script.borrow_mut().playing = false;
        }
        fn seek(&mut self, time: Duration) {
            let mut s = self.script.borrow_mut();
            s.seeks.push(time);
            s.position = time;
        }
        fn set_rate(&mut self, _: f32) {}
        fn set_volume(&mut self, _: f32) {}
        fn set_muted(&mut self, muted: bool) {
            self.script.borrow_mut().muted = muted;
        }
        fn set_looping(&mut self, looping: bool) {
            self.script.borrow_mut().looping = looping;
        }
        fn position(&self) -> Duration {
            self.script.borrow().position
        }
        fn is_playing(&self) -> bool {
            self.script.borrow().playing
        }
    }

    fn opened(i: usize) -> Rc<RefCell<Script>> {
        OPENED.with(|o| Rc::clone(&o.borrow()[i]))
    }

    const SCREEN: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 100.0,
        h: 100.0,
    };

    /// A window with one 40 × 30 video leaf at its origin, on a raster
    /// device.
    struct Window {
        store: NodeStore,
        leaf: NodeId,
        backend: CpuRaster,
        renderer: Renderer,
        videos: Videos<Fake>,
        now: Instant,
    }

    impl Window {
        fn new(source: VideoSource) -> Window {
            OPENED.with(|o| o.borrow_mut().clear());
            let mut store = NodeStore::new();
            let leaf = {
                let mut cx = BuildCx::new(&mut store);
                let handle = cx.leaf(LeafStyle {
                    size: Size::fixed(40.0, 30.0),
                    style: BoxStyle::NONE,
                });
                cx.video(handle, &source, AssetFit::Contain, None);
                handle.id()
            };
            let mut scratch = Vec::new();
            store.layout(leaf, SCREEN, &mut scratch);
            let mut backend = CpuRaster::new();
            let surface = backend.create_surface(RawWindowHandle::Headless, 64, 64);
            let format = backend.surface_format(surface);
            let renderer = Renderer::new(&mut backend, format);
            Window {
                store,
                leaf,
                backend,
                renderer,
                videos: Videos::default(),
                now: Instant::now(),
            }
        }

        fn step(&mut self, viewport: Rect) -> Step {
            self.now += Duration::from_millis(16);
            let gpu = Some((&mut self.backend, &mut self.renderer));
            self.videos.step(&mut self.store, gpu, self.now, viewport)
        }

        fn shown(&self) -> (AssetDraw, [f32; 2]) {
            match self.store.content_payload(self.leaf) {
                Some(Content::Asset { draw, natural, .. }) => {
                    (draw.clone(), [natural.x, natural.y])
                }
                other => panic!("{other:?}"),
            }
        }

        /// The RGB of the shown picture's first texel.
        fn texel(&mut self) -> [u8; 3] {
            let AssetDraw::Raster(texture) = self.shown().0 else {
                panic!("no picture");
            };
            let bgra = self.backend.read_texture(texture);
            [bgra[2], bgra[1], bgra[0]]
        }
    }

    fn ready(w: u32, h: u32) -> VideoEvent {
        VideoEvent::Ready(VideoInfo {
            width: w,
            height: h,
            duration: Some(Duration::from_secs(3)),
            frame_rate: Some(30.0),
            audio: true,
            seekable: true,
        })
    }

    #[test]
    fn a_video_node_opens_a_player_and_shows_its_pictures_in_one_texture() {
        let mut win = Window::new(VideoSource::file("clip.mp4"));
        let step = win.step(SCREEN);
        assert_eq!(step.due, Some(win.now + POLL), "polled while it loads");
        assert!(!step.beating);
        let script = opened(0);
        assert_eq!(win.shown(), (AssetDraw::Nothing, [0.0; 2]));

        // Ready, paused on its first picture.
        script.borrow_mut().events.push_back(ready(8, 6));
        script.borrow_mut().frames.push_back((8, 6, [255, 0, 0]));
        let step = win.step(SCREEN);
        assert_eq!(step, Step::default(), "a paused video asks for nothing");
        let (draw, natural) = win.shown();
        assert!(matches!(draw, AssetDraw::Raster(_)));
        assert_eq!(natural, [8.0, 6.0]);
        assert_eq!(win.texel(), [255, 0, 0]);
        let status = win.store.video_status(win.leaf).unwrap();
        assert!(status.ready && !status.playing);
        assert_eq!(status.duration, Some(Duration::from_secs(3)));

        // Asked to play, muted and looping: it beats, and each picture is
        // written into the same texture.
        win.store.set_playback(
            win.leaf,
            Playback {
                playing: true,
                looping: true,
                muted: true,
                ..Playback::default()
            },
        );
        win.store.clear_dirty();
        script.borrow_mut().frames.push_back((8, 6, [0, 255, 0]));
        let step = win.step(SCREEN);
        assert!(step.beating);
        {
            let s = script.borrow();
            assert!(s.playing && s.looping && s.muted);
        }
        let before = win.shown().0;
        assert_eq!(win.texel(), [0, 255, 0]);
        assert_eq!(win.shown().0, before, "the same texture");
        assert!(
            win.store
                .dirty(win.leaf)
                .contains(viso_ui::DirtyClass::PAINT),
            "repainted"
        );
        assert!(
            !win.store
                .dirty(win.leaf)
                .contains(viso_ui::DirtyClass::LAYOUT),
            "not laid out again"
        );
        assert_eq!(win.videos.frames, 2);

        // Off screen it plays on unread, polled.
        script.borrow_mut().frames.push_back((8, 6, [0, 0, 255]));
        let away = Rect { x: 500.0, ..SCREEN };
        let step = win.step(away);
        assert!(!step.beating);
        assert_eq!(step.due, Some(win.now + POLL));
        assert_eq!(win.texel(), [0, 255, 0], "not read off screen");

        // A picture of another size takes a texture of its own.
        win.step(SCREEN);
        assert_eq!(win.texel(), [0, 0, 255]);
        script.borrow_mut().frames.push_back((16, 12, [9, 9, 9]));
        win.step(SCREEN);
        assert_ne!(win.shown().0, before);
        assert_eq!(win.texel(), [9, 9, 9]);
    }

    #[test]
    fn it_reports_where_it_is_ends_paused_and_a_seek_reaches_its_player() {
        let mut win = Window::new(VideoSource::file("clip.mp4"));
        win.step(SCREEN);
        let script = opened(0);
        script.borrow_mut().events.push_back(ready(8, 6));
        win.store.set_video_playing(win.leaf, true);
        win.step(SCREEN);
        // A playing video reports its position a few times a second.
        script.borrow_mut().position = Duration::from_millis(100);
        win.step(SCREEN);
        let status = win.store.video_status(win.leaf).unwrap();
        assert!(status.playing);
        assert_eq!(status.position, Duration::ZERO, "not yet a report's worth");
        script.borrow_mut().position = Duration::from_millis(300);
        win.step(SCREEN);
        assert_eq!(
            win.store.video_status(win.leaf).unwrap().position,
            Duration::from_millis(300)
        );

        // It ends: paused, the request with it.
        {
            let mut s = script.borrow_mut();
            s.events.push_back(VideoEvent::Ended);
            s.playing = false;
            s.position = Duration::from_secs(3);
        }
        win.step(SCREEN);
        let status = win.store.video_status(win.leaf).unwrap();
        assert!(status.ended && !status.playing);
        assert_eq!(status.position, Duration::from_secs(3));
        assert!(!win.store.playback(win.leaf).unwrap().playing);

        // A seek goes to the player and is reported at once.
        win.store.seek_video(win.leaf, Duration::from_secs(1));
        win.step(SCREEN);
        assert_eq!(script.borrow().seeks, [Duration::from_secs(1)]);
        let status = win.store.video_status(win.leaf).unwrap();
        assert!(!status.ended);
        assert_eq!(status.position, Duration::from_secs(1));
        assert_eq!(win.store.videos().get(win.leaf).unwrap().seek, None);
    }

    #[test]
    fn a_player_closes_with_its_node_and_one_that_fails_shows_the_error() {
        let mut win = Window::new(VideoSource::file("clip.mp4"));
        win.step(SCREEN);
        let script = opened(0);
        script.borrow_mut().events.push_back(ready(8, 6));
        script.borrow_mut().frames.push_back((8, 6, [1, 2, 3]));
        win.step(SCREEN);
        assert!(matches!(win.shown().0, AssetDraw::Raster(_)));

        // Another source: the old player closes, a new one opens.
        win.store
            .set_video(win.leaf, &VideoSource::url("bad:"), AssetFit::Contain, None);
        win.step(SCREEN);
        assert!(script.borrow().dropped);
        assert!(win.videos.players.len() == 1);
        assert_eq!(win.shown().0, AssetDraw::Failed, "it could not open");

        // One that fails while it plays shows its error too.
        win.store.set_video(
            win.leaf,
            &VideoSource::file("b.mp4"),
            AssetFit::Contain,
            None,
        );
        win.step(SCREEN);
        let b = opened(1);
        b.borrow_mut().events.push_back(ready(4, 4));
        b.borrow_mut().frames.push_back((4, 4, [5, 5, 5]));
        win.step(SCREEN);
        assert!(matches!(win.shown().0, AssetDraw::Raster(_)));
        b.borrow_mut()
            .events
            .push_back(VideoEvent::Failed(VideoError::Play("gone".into())));
        win.step(SCREEN);
        assert!(b.borrow().dropped);
        assert_eq!(win.shown().0, AssetDraw::Failed);
        assert_eq!(win.store.assets().texture_bytes(), 0);
    }
}
