//! Video controls — the [`Video`] widget.
//!
//! `Video` is one leaf node that plays a video through the platform's
//! player — from a file, a URL (HLS included) or bytes — and shows its
//! pictures fitted into its box by its [`AssetFit`], at the video's natural
//! size unless its box is set. It shows nothing until its first picture
//! arrives; a [`poster`](Video::poster) may stand in until then and an
//! [`error`](Video::error) view once it fails.
//!
//! What it plays is declared: [`playing`](Video::playing),
//! [`looping`](Video::looping), [`muted`](Video::muted),
//! [`volume`](Video::volume), [`rate`](Video::rate). A build asks for them
//! again only when they changed since the last, so a pause from a handler
//! or the video's end stands until the app asks otherwise. A
//! [`VideoHandle`] steers it from handlers — play, pause, seek — and reads
//! where it is; [`on_progress`](Video::on_progress) and
//! [`on_ended`](Video::on_ended) report it.
//!
//! Its semantics are [`Role::Image`] named by its [`label`](Video::label);
//! a video without one is decoration, a mute [`Role::Group`].

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use viso_ui::{
    Alignment2D, AssetFit, BoxStyle, BuildCx, Component, EventCx, Handle, LeafStyle, Length,
    NodeId, Notice, NoticeKinds, Playback, Rgba, Role, Semantics, Size, StackStyle, StateStore,
    VideoSource, VideoStatus,
};

use crate::image::ImageLoad;

/// A video control. Construct one with [`video`].
///
/// ```
/// use viso_ui::{AssetFit, BuildCx, Component, NodeStore, VideoSource};
/// use viso_widgets::video;
///
/// let mut store = NodeStore::new();
/// let mut cx = BuildCx::new(&mut store);
/// video(VideoSource::file("intro.mp4"))
///     .playing(true)
///     .looping(true)
///     .muted(true)
///     .fit(AssetFit::Cover)
///     .build(&mut cx);
/// ```
pub struct Video {
    source: VideoSource,
    fit: AssetFit,
    tint: Option<Rgba>,
    size: Size,
    playback: Playback,
    /// The text alternative; `None` for decoration.
    label: Option<String>,
    /// Shown until the first picture.
    poster: Option<Box<dyn Component>>,
    /// Shown once it fails.
    error: Option<Box<dyn Component>>,
    on_load: Option<OnLoad>,
    on_progress: Option<OnProgress>,
    on_ended: Option<OnEnded>,
    slot: Option<VideoHandleSlot>,
}

/// A [`Video::on_load`] handler, shared by the builds of the video.
type OnLoad = Rc<RefCell<dyn FnMut(ImageLoad, &mut StateStore)>>;

/// A [`Video::on_progress`] handler.
type OnProgress = Rc<RefCell<dyn FnMut(VideoStatus, &mut StateStore)>>;

/// A [`Video::on_ended`] handler.
type OnEnded = Rc<RefCell<dyn FnMut(&mut StateStore)>>;

/// Where a built [`Video`] leaves its [`VideoHandle`].
pub type VideoHandleSlot = Rc<RefCell<Option<VideoHandle>>>;

/// A handle an application captures to steer a built video from its own
/// handlers and read where it is. Cheap to copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoHandle(NodeId);

impl VideoHandle {
    /// The video's node.
    pub fn node(self) -> NodeId {
        self.0
    }

    /// Play from where it is; from the start once it ended.
    pub fn play(self, ev: &mut EventCx<'_>) {
        ev.play_video(self.0, true);
    }

    /// Hold it where it is.
    pub fn pause(self, ev: &mut EventCx<'_>) {
        ev.play_video(self.0, false);
    }

    /// Play it if paused, pause it if playing.
    pub fn toggle(self, ev: &mut EventCx<'_>) {
        let playing = ev.playback(self.0).is_some_and(|p| p.playing);
        ev.play_video(self.0, !playing);
    }

    /// Move it to `time`; the picture there shows even while it is paused.
    pub fn seek(self, ev: &mut EventCx<'_>, time: Duration) {
        ev.seek_video(self.0, time);
    }

    /// Ask it to play as `playback` says.
    pub fn set_playback(self, ev: &mut EventCx<'_>, playback: Playback) {
        ev.set_playback(self.0, playback);
    }

    /// What its player reports; `None` where the handler cannot see the
    /// tree.
    pub fn status(self, ev: &EventCx<'_>) -> Option<VideoStatus> {
        ev.video_status(self.0)
    }
}

/// Construct a [`Video`] playing `source` — a [`VideoSource`], or an
/// [`Asset`](viso_ui::Asset) such as `asset!("intro.mp4")` — paused on its
/// first picture, at its natural size, contained in its box.
pub fn video(source: impl Into<VideoSource>) -> Video {
    Video {
        source: source.into(),
        fit: AssetFit::Contain,
        tint: None,
        size: Size {
            width: Length::Fit,
            height: Length::Fit,
        },
        playback: Playback::default(),
        label: None,
        poster: None,
        error: None,
        on_load: None,
        on_progress: None,
        on_ended: None,
        slot: None,
    }
}

impl Video {
    /// Set how the picture fills the box (defaults to [`AssetFit::Contain`]).
    pub fn fit(mut self, fit: AssetFit) -> Self {
        self.fit = fit;
        self
    }

    /// Set the tint the picture is modulated by.
    pub fn tint(mut self, tint: Rgba) -> Self {
        self.tint = Some(tint);
        self
    }

    /// Set the video's own size request within its parent (defaults to
    /// fitting its natural size).
    pub fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }

    /// Set whether it plays (defaults to paused). Playing, it ends paused at
    /// its end unless it loops.
    pub fn playing(mut self, playing: bool) -> Self {
        self.playback.playing = playing;
        self
    }

    /// Set whether it starts over at its end.
    pub fn looping(mut self, looping: bool) -> Self {
        self.playback.looping = looping;
        self
    }

    /// Set whether its sound is silenced.
    pub fn muted(mut self, muted: bool) -> Self {
        self.playback.muted = muted;
        self
    }

    /// Set its sound's volume, from 0 to 1 (defaults to 1).
    pub fn volume(mut self, volume: f32) -> Self {
        self.playback.volume = volume.clamp(0.0, 1.0);
        self
    }

    /// Set how many times its speed it plays at (defaults to 1).
    pub fn rate(mut self, rate: f32) -> Self {
        self.playback.rate = rate;
        self
    }

    /// Set the text alternative, which names the video to assistive
    /// technology; a video without one is decoration.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Show `poster` over the box until the first picture arrives.
    pub fn poster(mut self, poster: impl Component + 'static) -> Self {
        self.poster = Some(Box::new(poster));
        self
    }

    /// Show `error` over the box once the video could not be opened or
    /// played.
    pub fn error(mut self, error: impl Component + 'static) -> Self {
        self.error = Some(Box::new(error));
        self
    }

    /// Call `handler` once the first picture shows — with the video's
    /// natural size — or the video fails.
    pub fn on_load(mut self, handler: impl FnMut(ImageLoad, &mut StateStore) + 'static) -> Self {
        self.on_load = Some(Rc::new(RefCell::new(handler)));
        self
    }

    /// Call `handler` with what the player reports each time it changes:
    /// ready, playing or paused, waiting, a few times a second where it is.
    pub fn on_progress(
        mut self,
        handler: impl FnMut(VideoStatus, &mut StateStore) + 'static,
    ) -> Self {
        self.on_progress = Some(Rc::new(RefCell::new(handler)));
        self
    }

    /// Call `handler` each time it plays to its end and stops there.
    pub fn on_ended(mut self, handler: impl FnMut(&mut StateStore) + 'static) -> Self {
        self.on_ended = Some(Rc::new(RefCell::new(handler)));
        self
    }

    /// Leave the built video's [`VideoHandle`] in `slot`.
    pub fn handle(mut self, slot: &VideoHandleSlot) -> Self {
        self.slot = Some(Rc::clone(slot));
        self
    }

    /// The leaf showing the pictures, sized `size`.
    fn picture(&self, cx: &mut BuildCx<'_>, size: Size) -> Handle {
        let handle = cx.leaf(LeafStyle {
            size,
            style: BoxStyle::NONE,
        });
        cx.video(handle, &self.source, self.fit, self.tint);
        cx.playback(handle, self.playback);
        let mut kinds = NoticeKinds::default();
        if self.on_load.is_some() {
            kinds = kinds | NoticeKinds::ASSET;
        }
        if self.on_progress.is_some() || self.on_ended.is_some() {
            kinds = kinds | NoticeKinds::VIDEO;
        }
        if !kinds.is_empty() {
            let on_load = self.on_load.clone();
            let on_progress = self.on_progress.clone();
            let on_ended = self.on_ended.clone();
            let report = move |notice: &Notice, states: &mut StateStore| match *notice {
                Notice::AssetLoaded { size } => {
                    if let Some(on_load) = &on_load {
                        (on_load.borrow_mut())(ImageLoad::Loaded { size }, states);
                    }
                }
                Notice::AssetFailed => {
                    if let Some(on_load) = &on_load {
                        (on_load.borrow_mut())(ImageLoad::Failed, states);
                    }
                }
                Notice::Video(status) => {
                    if let Some(on_progress) = &on_progress {
                        (on_progress.borrow_mut())(status, states);
                    }
                }
                Notice::VideoEnded => {
                    if let Some(on_ended) = &on_ended {
                        (on_ended.borrow_mut())(states);
                    }
                }
                _ => {}
            };
            cx.watch(handle, kinds, Box::new(report));
        }
        if let Some(slot) = &self.slot {
            *slot.borrow_mut() = Some(VideoHandle(handle.id()));
        }
        handle
    }
}

impl Component for Video {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let semantics = match &self.label {
            Some(label) => Semantics::role(Role::Image).with_label(label.clone()),
            None => Semantics::role(Role::Group),
        };
        if self.poster.is_none() && self.error.is_none() {
            let handle = self.picture(cx, self.size);
            cx.semantics(handle, semantics);
            return;
        }
        // The picture under its stand-ins, one stack sized as the video: an
        // axis that fits its content fits the picture, any other fills it.
        let inner = |l: Length| match l {
            Length::Fit => Length::Fit,
            _ => Size::fill().width,
        };
        let size = Size {
            width: inner(self.size.width),
            height: inner(self.size.height),
        };
        let (mut picture, mut loading, mut failed) = (None, None, None);
        let layer = StackStyle {
            align: Alignment2D::Center,
            size: Size::fill(),
            ..StackStyle::default()
        };
        let outer = cx.stack(
            StackStyle {
                align: Alignment2D::Center,
                size: self.size,
                ..StackStyle::default()
            },
            |cx| {
                picture = Some(self.picture(cx, size));
                for (view, at) in [(&self.poster, &mut loading), (&self.error, &mut failed)] {
                    if let Some(view) = view {
                        *at = Some(cx.stack(layer, |cx| view.build(cx)));
                    }
                }
            },
        );
        if let Some(picture) = picture {
            cx.asset_fallbacks(picture, loading, failed);
        }
        cx.semantics(outer, semantics);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_ui::{AssetDraw, BindingTable, Content, NodeStore, TextureId};

    fn built(widget: Video) -> (NodeStore, NodeId) {
        let mut store = NodeStore::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            widget.build(&mut cx);
            cx.root().unwrap()
        };
        (store, root)
    }

    #[test]
    fn a_video_is_one_leaf_showing_its_source_as_it_is_asked_to_play() {
        let slot = VideoHandleSlot::default();
        let widget = video(VideoSource::url("https://x/a.m3u8"))
            .playing(true)
            .muted(true)
            .volume(3.0)
            .fit(AssetFit::Cover)
            .label("Intro")
            .handle(&slot);
        let (store, root) = built(widget);
        assert_eq!(store.arena().children(root).count(), 0);
        let entry = store.videos().get(root).unwrap();
        assert_eq!(entry.source, VideoSource::url("https://x/a.m3u8"));
        assert_eq!(
            entry.playback,
            Playback {
                playing: true,
                muted: true,
                ..Playback::default()
            }
        );
        assert!(matches!(
            store.content_payload(root),
            Some(Content::Asset {
                fit: AssetFit::Cover,
                draw: AssetDraw::Nothing,
                ..
            })
        ));
        assert_eq!(store.semantics(root).unwrap().role, Role::Image);
        assert_eq!(slot.borrow().unwrap().node(), root);
    }

    #[test]
    fn a_build_asking_the_same_again_leaves_a_pause_from_a_handler_alone() {
        let source = VideoSource::file("a.mp4");
        let slot = VideoHandleSlot::default();
        let mut store = NodeStore::new();
        let build = |store: &mut NodeStore, playing: bool| {
            let mut cx = BuildCx::new(store);
            video(source.clone())
                .playing(playing)
                .handle(&slot)
                .build(&mut cx);
        };
        build(&mut store, true);
        let handle = slot.borrow().unwrap();
        let node = handle.node();

        // A handler pauses it.
        let (mut states, bindings) = (StateStore::new(), BindingTable::new());
        let mut ev = EventCx::__new(&mut states, &bindings);
        handle.pause(&mut ev);
        handle.seek(&mut ev, Duration::from_secs(2));
        for request in ev.__take_store_requests() {
            request.__apply(&mut store);
        }
        assert!(!store.playback(node).unwrap().playing);
        assert_eq!(
            store.videos().get(node).unwrap().seek,
            Some(Duration::from_secs(2))
        );

        // The same declaration again does not undo it; another does.
        store.declare_playback(
            node,
            Playback {
                playing: true,
                ..Playback::default()
            },
        );
        assert!(!store.playback(node).unwrap().playing);
        store.declare_playback(node, Playback::default());
        store.declare_playback(
            node,
            Playback {
                playing: true,
                ..Playback::default()
            },
        );
        assert!(store.playback(node).unwrap().playing);
    }

    #[test]
    fn a_poster_shows_until_the_first_picture_and_reports_follow_the_player() {
        let heard = Rc::new(RefCell::new(Vec::new()));
        let (progress, ended, loaded) = (Rc::clone(&heard), Rc::clone(&heard), Rc::clone(&heard));
        let poster = video(VideoSource::file("never.mp4"));
        let widget = video(VideoSource::file("a.mp4"))
            .size(Size::fixed(40.0, 30.0))
            .poster(poster)
            .on_progress(move |s, _| progress.borrow_mut().push(format!("at {:?}", s.position)))
            .on_ended(move |_| ended.borrow_mut().push("ended".into()))
            .on_load(move |l, _| loaded.borrow_mut().push(format!("{l:?}")));
        let (mut store, outer) = built(widget);
        let picture = store.arena().children(outer).next().unwrap();
        let resource = store.videos().get(picture).unwrap().resource;
        let poster = store.arena().children(outer).nth(1).unwrap();
        assert!(!store.hidden(poster));

        store.size_video(resource, 8, 6);
        assert_eq!(
            store.show_video_frame(resource, TextureId::new(3), (8, 6)),
            None
        );
        assert!(store.hidden(poster), "the first picture hides the poster");

        let status = &mut store.videos_mut().get_mut(picture).unwrap().status;
        status.position = Duration::from_secs(1);
        status.ended = true;
        let (lists, mut states) = (viso_ui::VirtualLists::new(), StateStore::new());
        viso_ui::deliver_notices(&mut store, &lists, &mut states);
        assert_eq!(
            *heard.borrow(),
            ["Loaded { size: Vec2 { x: 8.0, y: 6.0 } }", "at 1s", "ended"]
        );
    }
}
