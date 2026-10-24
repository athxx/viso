//! A window's videos: one platform player a video node, steered by what the
//! app asks of the node and reporting back what it does, its pictures shown
//! as a texture of the window's renderer.
//!
//! A player opens when its node first shows its source and closes when the
//! node lets it go. While it plays and a node shows it on screen, the window
//! beats each frame and takes each new picture: the first, and any of another
//! size, becomes a texture of its own; the rest go into it, marking only
//! `PAINT`. A picture the GPU already holds is sampled where it lies — the
//! texture adopts it, no texel copied — where the backend can; otherwise its
//! texels are written into the texture. Off screen it plays on unread. A
//! player loading, or playing where nothing beats for it, is polled on a
//! timer instead; one paused is polled briefly after it is ready or sought,
//! until the picture it then shows comes.

use std::time::Duration;

use viso_gpu::{GpuBackend, TextureDesc, TextureFormat, TextureId};
use viso_media::{Frame, VideoError, VideoEvent, VideoPlayer};
use viso_render::Renderer;
use viso_runtime::Instant;
use viso_ui::{NodeId, NodeStore, Playback, Rect, VideoSource, resource::ResourceId};

/// How often a player is polled while nothing beats for it.
const POLL: Duration = Duration::from_millis(100);

/// How often a paused player is asked for the picture it was ready with or
/// sought to, and for how long.
const AWAITED: Duration = Duration::from_millis(16);
const AWAITED_FOR: Duration = Duration::from_secs(2);

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
        // Android's players are Java's, reached through the activity.
        #[cfg(target_os = "android")]
        viso_platform::backend::android::with_activity(|env, activity| {
            // SAFETY: this thread's environment and a live reference to the
            // activity, both valid for the closure.
            unsafe { viso_media::attach_android(env.cast(), activity.cast()) }
        });
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
    /// Since when a picture is due though it is paused: the first, or one
    /// sought to, which a player may hand over a little later.
    awaits: Option<Instant>,
    /// The backend took none of its pictures where they lie: the rest are
    /// converted or copied without asking again.
    refused: bool,
    /// The backend converted none of its pictures' planes: the rest are
    /// copied without asking again.
    copied: bool,
    /// The texture its pictures are shown as.
    texture: Option<Shown>,
}

/// The texture a video's pictures are shown as.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Shown {
    texture: TextureId,
    size: (u32, u32),
    /// Whether the backend writes it — sampling the pictures where they
    /// lie, or converting their planes — rather than it holding copies.
    adopted: bool,
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
                awaits: None,
                refused: false,
                copied: false,
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
                        status.facing = info.facing.map(facing);
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
            status.muted = wish.muted;
            status.volume = wish.volume;
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
            // A report changed is heard on the next pass, which runs at once.
            if status != entry.status {
                step.due = earliest(step.due, now);
            }
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
                store.mirror_video(p.node);
            }
            let shown = store.on_screen(p.node, viewport);
            if !p.ready {
                step.due = earliest(step.due, now + POLL);
                continue;
            }
            if size.is_some() || sought.is_some() {
                p.awaits = Some(now);
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
                if !status.playing
                    && let Some(since) = p.awaits
                {
                    if now.saturating_duration_since(since) < AWAITED_FOR {
                        step.due = earliest(step.due, now + AWAITED);
                    } else {
                        p.awaits = None;
                    }
                }
                continue;
            };
            p.awaits = None;
            let size = (frame.width, frame.height);
            let shown_as = p.texture.filter(|t| t.size == size);
            // In place where the backend adopts it: the same texture pointed
            // at the new picture, or a new one over it.
            if !p.refused
                && let Some(image) = frame.native()
            {
                if let Some(t) = shown_as.filter(|t| t.adopted)
                    && backend.adopt_external(t.texture, &image)
                {
                    store.repaint_asset(p.resource);
                    self.frames += 1;
                    continue;
                }
                if let Some(texture) = backend.import_external(&image) {
                    if let Some(old) = store.show_video_frame(p.resource, texture, size) {
                        renderer.release_texture(*backend, old);
                    }
                    p.texture = Some(Shown {
                        texture,
                        size,
                        adopted: true,
                    });
                    self.frames += 1;
                    continue;
                }
                p.refused = true;
            }
            // Planes at hand, converted on the GPU where the backend does:
            // into the texture it converted the last into, or a new one.
            if !p.copied
                && let Some(planes) = frame.planes()
            {
                let into = shown_as.filter(|t| t.adopted).map(|t| t.texture);
                if let Some(texture) = backend.write_nv12(into, &planes) {
                    if Some(texture) == into {
                        store.repaint_asset(p.resource);
                    } else {
                        if let Some(old) = store.show_video_frame(p.resource, texture, size) {
                            renderer.release_texture(*backend, old);
                        }
                        p.texture = Some(Shown {
                            texture,
                            size,
                            adopted: true,
                        });
                    }
                    self.frames += 1;
                    continue;
                }
                p.copied = true;
            }
            if !frame.copy_rgba(&mut self.scratch) {
                continue;
            }
            match shown_as.filter(|t| !t.adopted) {
                Some(t) => {
                    backend.write_texture(t.texture, 0, 0, size.0, size.1, &self.scratch);
                    store.repaint_asset(p.resource);
                }
                None => {
                    // A video's texels are sRGB-encoded: sampling decodes them.
                    let texture = backend.create_texture(&TextureDesc {
                        width: size.0,
                        height: size.1,
                        format: TextureFormat::Rgba8UnormSrgb,
                        render_target: false,
                        label: "video",
                    });
                    backend.write_texture(texture, 0, 0, size.0, size.1, &self.scratch);
                    if let Some(old) = store.show_video_frame(p.resource, texture, size) {
                        renderer.release_texture(*backend, old);
                    }
                    p.texture = Some(Shown {
                        texture,
                        size,
                        adopted: false,
                    });
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
        VideoSource::Camera(camera) => viso_media::VideoSource::Camera(viso_media::CameraRequest {
            camera: match &camera.choice {
                viso_ui::CameraChoice::Default => viso_media::CameraChoice::Default,
                viso_ui::CameraChoice::Facing(f) => viso_media::CameraChoice::Facing(match f {
                    viso_ui::CameraFacing::Front => viso_media::CameraFacing::Front,
                    viso_ui::CameraFacing::Back => viso_media::CameraFacing::Back,
                    viso_ui::CameraFacing::External => viso_media::CameraFacing::External,
                }),
                viso_ui::CameraChoice::Id(id) => viso_media::CameraChoice::Id(id.to_string()),
            },
            width: camera.width,
            height: camera.height,
            frame_rate: camera.frame_rate,
        }),
    }
}

/// The cameras the platform has, for [`Camera::id`](viso_ui::Camera::id)
/// to choose among; none where it has no camera, or captures from none.
pub fn cameras() -> Vec<viso_ui::CameraInfo> {
    viso_media::cameras()
        .into_iter()
        .map(|info| viso_ui::CameraInfo {
            id: info.id.into(),
            name: info.name,
            facing: facing(info.facing),
        })
        .collect()
}

/// A camera's facing as a node reports it.
fn facing(facing: viso_media::CameraFacing) -> viso_ui::CameraFacing {
    match facing {
        viso_media::CameraFacing::Front => viso_ui::CameraFacing::Front,
        viso_media::CameraFacing::Back => viso_ui::CameraFacing::Back,
        viso_media::CameraFacing::External => viso_ui::CameraFacing::External,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    use viso_gpu::{CpuRaster, RawWindowHandle};
    use viso_media::VideoInfo;
    use viso_ui::{AssetDraw, AssetFit, BoxStyle, BuildCx, Content, LeafStyle, Size};

    /// What a scripted player does: the events it reports and the pictures
    /// it hands over, in order, and what was asked of it.
    #[derive(Default)]
    struct Script {
        events: VecDeque<VideoEvent>,
        /// Each picture: its size and one RGB colour.
        frames: VecDeque<(u32, u32, [u8; 3])>,
        /// Pictures in GPU memory, handed over before any of `frames`.
        external: VecDeque<viso_gpu::ExternalImage>,
        /// Grey pictures in NV12 planes, full range: their size and luma,
        /// handed over before any of `frames`.
        planes: VecDeque<(u32, u32, u8)>,
        playing: bool,
        position: Duration,
        looping: bool,
        muted: bool,
        seeks: Vec<Duration>,
        dropped: bool,
        /// What it was opened on.
        source: String,
    }

    thread_local! {
        /// The scripts of the players opened on this thread, in order.
        static OPENED: RefCell<Vec<Rc<RefCell<Script>>>> = const { RefCell::new(Vec::new()) };
    }

    struct Fake {
        script: Rc<RefCell<Script>>,
        picture: Vec<u8>,
        chroma: Vec<u8>,
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
            let script = Rc::new(RefCell::new(Script {
                source: format!("{source:?}"),
                ..Script::default()
            }));
            OPENED.with(|o| o.borrow_mut().push(Rc::clone(&script)));
            Ok(Fake {
                script,
                picture: Vec::new(),
                chroma: Vec::new(),
            })
        }
        fn poll(&mut self) -> Option<VideoEvent> {
            self.script.borrow_mut().events.pop_front()
        }
        fn frame(&mut self) -> Option<Frame<'_>> {
            if let Some(image) = self.script.borrow_mut().external.pop_front() {
                let (w, h) = image.size();
                return Some(Frame::external(w, h, image, Duration::ZERO));
            }
            if let Some((w, h, luma)) = self.script.borrow_mut().planes.pop_front() {
                self.picture = vec![luma; (w * h) as usize];
                self.chroma = vec![128; (w * h / 2) as usize];
                let picture = viso_media::Nv12 {
                    width: w,
                    height: h,
                    y: &self.picture,
                    y_stride: w as usize,
                    uv: &self.chroma,
                    uv_stride: w as usize,
                    matrix: viso_media::YuvMatrix::Bt709,
                    full_range: true,
                    left_chroma: true,
                };
                return Some(Frame::nv12(picture, Duration::ZERO));
            }
            let (w, h, [r, g, b]) = self.script.borrow_mut().frames.pop_front()?;
            self.picture = [b, g, r, 255].repeat((w * h) as usize);
            Some(Frame::bgra(
                w,
                h,
                &self.picture,
                w as usize * 4,
                Duration::ZERO,
            ))
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
            facing: None,
        })
    }

    #[test]
    fn a_camera_opens_as_asked_and_shows_mirrored_once_it_faces_the_user() {
        let camera = viso_ui::Camera::facing(viso_ui::CameraFacing::Front).size(640, 480);
        let mut win = Window::new(VideoSource::Camera(camera));
        win.step(SCREEN);
        let script = opened(0);
        let request = viso_media::CameraRequest {
            camera: viso_media::CameraChoice::Facing(viso_media::CameraFacing::Front),
            width: 640,
            height: 480,
            frame_rate: 30,
        };
        assert_eq!(
            script.borrow().source,
            format!("{:?}", viso_media::VideoSource::Camera(request))
        );
        let mirrored = |win: &Window| match win.store.content_payload(win.leaf) {
            Some(Content::Asset { mirrored, .. }) => *mirrored,
            other => panic!("{other:?}"),
        };
        assert!(!mirrored(&win));
        script
            .borrow_mut()
            .events
            .push_back(VideoEvent::Ready(VideoInfo {
                width: 8,
                height: 6,
                duration: None,
                frame_rate: Some(30.0),
                audio: false,
                seekable: false,
                facing: Some(viso_media::CameraFacing::Front),
            }));
        script.borrow_mut().frames.push_back((8, 6, [255, 0, 0]));
        win.step(SCREEN);
        let status = win.store.video_status(win.leaf).unwrap();
        assert_eq!(status.facing, Some(viso_ui::CameraFacing::Front));
        assert_eq!(status.duration, None);
        assert!(mirrored(&win));
        assert_eq!(win.texel(), [255, 0, 0]);
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
        assert_eq!(step.due, Some(win.now), "what it reported heard at once");
        assert!(!step.beating);
        assert_eq!(
            win.step(SCREEN),
            Step::default(),
            "then a paused video asks for nothing"
        );
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
    fn planes_a_backend_does_not_convert_are_converted_and_copied() {
        let mut win = Window::new(VideoSource::file("clip.mp4"));
        win.step(SCREEN);
        let script = opened(0);
        script.borrow_mut().events.push_back(ready(8, 6));
        script.borrow_mut().planes.push_back((8, 6, 200));
        win.step(SCREEN);
        assert_eq!(win.texel(), [200; 3]);
        let before = win.shown().0;
        win.store.set_video_playing(win.leaf, true);
        script.borrow_mut().planes.push_back((8, 6, 40));
        win.step(SCREEN);
        assert_eq!(win.texel(), [40; 3]);
        assert_eq!(win.shown().0, before, "the same texture");
        assert_eq!(win.videos.frames, 2);
    }

    /// A `side` × `side` BGRA IOSurface every texel of which is `bgra`.
    #[cfg(target_vendor = "apple")]
    fn surface(
        side: u32,
        bgra: [u8; 4],
    ) -> objc2_core_foundation::CFRetained<objc2_io_surface::IOSurfaceRef> {
        use objc2_core_foundation::{CFDictionary, CFNumber, CFString, CFType};
        use objc2_io_surface::{
            IOSurfaceLockOptions, IOSurfaceRef, kIOSurfaceBytesPerElement, kIOSurfaceHeight,
            kIOSurfacePixelFormat, kIOSurfaceWidth,
        };
        let (w, h, bytes, format) = (
            CFNumber::new_i32(side as i32),
            CFNumber::new_i32(side as i32),
            CFNumber::new_i32(4),
            CFNumber::new_i32(i32::from_be_bytes(*b"BGRA")),
        );
        // SAFETY: the keys are IOSurface's own statics.
        let keys: [&CFString; 4] = unsafe {
            [
                kIOSurfaceWidth,
                kIOSurfaceHeight,
                kIOSurfaceBytesPerElement,
                kIOSurfacePixelFormat,
            ]
        };
        let values: [&CFType; 4] = [&w, &h, &bytes, &format];
        let properties = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
        // SAFETY: the properties are CFStrings to CFNumbers, as
        // IOSurfaceCreate takes them; the surface is locked while its rows
        // are written, each `bytes_per_row` apart for its height.
        unsafe {
            let surface = IOSurfaceRef::new(properties.as_ref()).expect("an IOSurface");
            surface.lock(IOSurfaceLockOptions::empty(), std::ptr::null_mut());
            let base = surface.base_address().as_ptr().cast::<u8>();
            let stride = surface.bytes_per_row();
            for y in 0..side as usize {
                for x in 0..side as usize {
                    std::ptr::copy_nonoverlapping(bgra.as_ptr(), base.add(y * stride + x * 4), 4);
                }
            }
            surface.unlock(IOSurfaceLockOptions::empty(), std::ptr::null_mut());
            surface
        }
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_picture_in_gpu_memory_is_adopted_into_one_texture_no_texel_copied() {
        use viso_gpu::{ExternalImage, MetalBackend, TextureFormat};
        use viso_ui::DirtyClass;
        OPENED.with(|o| o.borrow_mut().clear());
        let mut store = NodeStore::new();
        let leaf = {
            let mut cx = BuildCx::new(&mut store);
            let handle = cx.leaf(LeafStyle {
                size: Size::fixed(40.0, 30.0),
                style: BoxStyle::NONE,
            });
            cx.video(
                handle,
                &VideoSource::file("clip.mp4"),
                AssetFit::Contain,
                None,
            );
            handle.id()
        };
        store.layout(leaf, SCREEN, &mut Vec::new());
        let mut backend = MetalBackend::new();
        let mut renderer = Renderer::new(&mut backend, TextureFormat::Bgra8UnormSrgb);
        let mut videos = Videos::<Fake>::default();
        let mut now = Instant::now();
        let mut step = |store: &mut NodeStore, backend: &mut MetalBackend| {
            now += Duration::from_millis(16);
            videos.step(store, Some((backend, &mut renderer)), now, SCREEN)
        };
        let image = |surface: &objc2_io_surface::IOSurfaceRef| ExternalImage::IoSurface {
            surface: std::ptr::from_ref(surface).cast_mut().cast(),
            width: 4,
            height: 4,
        };
        let shown = |store: &NodeStore| match store.content_payload(leaf) {
            Some(Content::Asset {
                draw: AssetDraw::Raster(texture),
                ..
            }) => *texture,
            other => panic!("{other:?}"),
        };

        step(&mut store, &mut backend);
        let script = opened(0);
        let (red, blue) = (surface(4, [0, 0, 255, 255]), surface(4, [255, 0, 0, 255]));
        script.borrow_mut().events.push_back(ready(4, 4));
        script.borrow_mut().external.push_back(image(&red));
        step(&mut store, &mut backend);
        let texture = shown(&store);
        assert_eq!(backend.read_texture(texture), [0, 0, 255, 255].repeat(16));
        assert_eq!(red.use_count(), 1, "the texture samples it");

        // The next picture: the same texture, pointed at it; only a repaint.
        store.clear_dirty();
        script.borrow_mut().external.push_back(image(&blue));
        step(&mut store, &mut backend);
        assert_eq!(shown(&store), texture);
        assert_eq!(backend.read_texture(texture), [255, 0, 0, 255].repeat(16));
        assert!(store.dirty(leaf).contains(DirtyClass::PAINT));
        assert!(!store.dirty(leaf).contains(DirtyClass::LAYOUT));
        assert_eq!(store.assets().texture_bytes(), 4 * 4 * 4);
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
    fn a_paused_player_is_polled_for_the_picture_it_was_ready_with_or_sought_to() {
        let mut win = Window::new(VideoSource::file("clip.mp4"));
        win.step(SCREEN);
        let script = opened(0);
        // Ready before its first picture: asked again soon, not on the slow
        // timer.
        script.borrow_mut().events.push_back(ready(8, 6));
        assert_eq!(win.step(SCREEN).due, Some(win.now));
        let step = win.step(SCREEN);
        assert_eq!(step.due, Some(win.now + AWAITED));
        script.borrow_mut().frames.push_back((8, 6, [255, 0, 0]));
        let step = win.step(SCREEN);
        assert_eq!(step, Step::default(), "the picture came");
        assert_eq!(win.texel(), [255, 0, 0]);

        // Sought while paused: asked again until the sought picture comes.
        win.store.seek_video(win.leaf, Duration::from_secs(1));
        assert_eq!(win.step(SCREEN).due, Some(win.now), "its new position");
        let step = win.step(SCREEN);
        assert_eq!(step.due, Some(win.now + AWAITED));
        script.borrow_mut().frames.push_back((8, 6, [0, 0, 255]));
        let step = win.step(SCREEN);
        assert_eq!(step, Step::default());
        assert_eq!(win.texel(), [0, 0, 255]);

        // One that never comes is given up.
        win.store.seek_video(win.leaf, Duration::from_secs(2));
        win.step(SCREEN);
        win.now += AWAITED_FOR;
        assert_eq!(win.step(SCREEN), Step::default());
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
