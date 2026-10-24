//! A target with no player: nothing opens.

use std::marker::PhantomData;
use std::time::Duration;

use super::{Frame, VideoError, VideoEvent, VideoSource};

pub(crate) enum Player {}

/// A platform buffer's picture, of which there is none.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Picture<'a> {
    never: Never,
    at: PhantomData<&'a ()>,
}

#[derive(Debug, Clone, Copy)]
enum Never {}

impl Picture<'_> {
    pub(crate) fn external(&self, _: u32, _: u32) -> Option<viso_handle::ExternalImage> {
        let _ = self.at;
        match self.never {}
    }

    pub(crate) fn read(&self, _: impl FnOnce(&[u8], usize)) -> bool {
        match self.never {}
    }
}

impl Player {
    pub(crate) fn open(_: &VideoSource) -> Result<Player, VideoError> {
        Err(VideoError::Unsupported)
    }

    pub(crate) fn poll(&mut self) -> Option<VideoEvent> {
        match *self {}
    }

    pub(crate) fn frame(&mut self) -> Option<Frame<'_>> {
        match *self {}
    }

    pub(crate) fn play(&mut self) {
        match *self {}
    }

    pub(crate) fn pause(&mut self) {
        match *self {}
    }

    pub(crate) fn seek(&mut self, _: Duration) {
        match *self {}
    }

    pub(crate) fn set_rate(&mut self, _: f32) {
        match *self {}
    }

    pub(crate) fn set_volume(&mut self, _: f32) {
        match *self {}
    }

    pub(crate) fn set_muted(&mut self, _: bool) {
        match *self {}
    }

    pub(crate) fn set_looping(&mut self, _: bool) {
        match *self {}
    }

    pub(crate) fn position(&self) -> Duration {
        match *self {}
    }

    pub(crate) fn is_playing(&self) -> bool {
        match *self {}
    }
}
