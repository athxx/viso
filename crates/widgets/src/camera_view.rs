//! A camera's live picture — the [`CameraView`] widget.
//!
//! `CameraView` shows what a camera captures as it captures it: the camera
//! its [`Camera`] names, at about the size and rate it asks, covering its
//! box. It captures while it plays — from its first build, unless
//! [`playing`](CameraView::playing) says otherwise — and lets the camera go
//! while paused, so a preview put away holds no device. A camera facing the
//! user shows mirrored, as a mirror would show them, unless
//! [`mirror`](CameraView::mirror) says otherwise. A
//! [`placeholder`](CameraView::placeholder) stands in until the first
//! picture, and an [`error`](CameraView::error) view once the camera cannot
//! be opened: there is none, the user refused it, or no extension answers
//! for the cameras (`viso_media::provide_cameras`).
//!
//! It is a [`Video`] of the camera: one leaf, steered by the same
//! [`VideoHandle`], its [`on_progress`](CameraView::on_progress) reporting
//! which way the camera faces once it is ready.

use viso_ui::{AssetFit, BuildCx, Camera, Component, Rgba, Sizing, StateStore, VideoStatus};

use crate::image::ImageLoad;
use crate::video::{Video, VideoHandleSlot, video};

/// A camera's live picture. Construct one with [`camera_view`].
///
/// ```
/// use viso_ui::{BuildCx, Camera, CameraFacing, Component, NodeStore};
/// use viso_widgets::camera_view;
///
/// let mut store = NodeStore::new();
/// let mut cx = BuildCx::new(&mut store);
/// camera_view(Camera::facing(CameraFacing::Front))
///     .label("Preview")
///     .build(&mut cx);
/// ```
pub struct CameraView {
    video: Video,
}

/// Construct a [`CameraView`] showing what `camera` captures, capturing,
/// covering its box at the picture's natural size.
pub fn camera_view(camera: impl Into<Camera>) -> CameraView {
    CameraView {
        video: video(camera.into())
            .playing(true)
            .muted(true)
            .fit(AssetFit::Cover),
    }
}

impl CameraView {
    /// Set how the picture fills the box (defaults to [`AssetFit::Cover`]).
    pub fn fit(mut self, fit: AssetFit) -> Self {
        self.video = self.video.fit(fit);
        self
    }

    /// Set the tint the picture is modulated by.
    pub fn tint(mut self, tint: Rgba) -> Self {
        self.video = self.video.tint(tint);
        self
    }

    /// Set its own size request within its parent (defaults to fitting the
    /// picture's natural size).
    pub fn size(mut self, size: Sizing) -> Self {
        self.video = self.video.size(size);
        self
    }

    /// Set whether it captures (defaults to capturing); paused, it shows the
    /// last picture and lets the camera go.
    pub fn playing(mut self, playing: bool) -> Self {
        self.video = self.video.playing(playing);
        self
    }

    /// Set whether the picture shows mirrored (defaults to mirroring a
    /// camera facing the user).
    pub fn mirror(mut self, mirror: bool) -> Self {
        self.video = self.video.mirror(mirror);
        self
    }

    /// Set the text alternative, which names the picture to assistive
    /// technology; one without is decoration.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.video = self.video.label(label);
        self
    }

    /// Show `placeholder` over the box until the first picture arrives.
    pub fn placeholder(mut self, placeholder: impl Component + 'static) -> Self {
        self.video = self.video.poster(placeholder);
        self
    }

    /// Show `error` over the box once the camera could not be opened.
    pub fn error(mut self, error: impl Component + 'static) -> Self {
        self.video = self.video.error(error);
        self
    }

    /// Call `handler` once the first picture shows — with its natural size
    /// — or the camera fails.
    pub fn on_load(mut self, handler: impl FnMut(ImageLoad, &mut StateStore) + 'static) -> Self {
        self.video = self.video.on_load(handler);
        self
    }

    /// Call `handler` with what the camera reports each time it changes:
    /// ready, which way it faces, capturing or paused.
    pub fn on_progress(
        mut self,
        handler: impl FnMut(VideoStatus, &mut StateStore) + 'static,
    ) -> Self {
        self.video = self.video.on_progress(handler);
        self
    }

    /// Leave the built view's [`VideoHandle`](crate::video::VideoHandle) in
    /// `slot`.
    pub fn handle(mut self, slot: &VideoHandleSlot) -> Self {
        self.video = self.video.handle(slot);
        self
    }
}

impl Component for CameraView {
    fn build(&self, cx: &mut BuildCx<'_>) {
        self.video.build(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_ui::{CameraFacing, Content, NodeStore, VideoSource};

    #[test]
    fn a_camera_view_is_a_video_of_its_camera_capturing_and_covering_its_box() {
        let mut store = NodeStore::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            camera_view(Camera::facing(CameraFacing::Back).size(640, 480)).build(&mut cx);
            cx.root().unwrap()
        };
        let entry = store.videos().get(root).unwrap();
        assert_eq!(
            entry.source,
            VideoSource::Camera(Camera::facing(CameraFacing::Back).size(640, 480))
        );
        assert!(entry.playback.playing && entry.playback.muted);
        assert_eq!(entry.mirror, None, "mirrored by its facing");
        assert!(matches!(
            store.content_payload(root),
            Some(Content::Asset {
                fit: AssetFit::Cover,
                ..
            })
        ));
    }

    #[test]
    fn a_camera_view_asked_to_mirror_does_whichever_way_it_faces() {
        let mut store = NodeStore::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            camera_view(Camera::default())
                .mirror(true)
                .playing(false)
                .build(&mut cx);
            cx.root().unwrap()
        };
        let entry = store.videos().get(root).unwrap();
        assert_eq!(entry.mirror, Some(true));
        assert!(!entry.playback.playing);
        assert!(matches!(
            store.content_payload(root),
            Some(Content::Asset { mirrored: true, .. })
        ));
    }
}
