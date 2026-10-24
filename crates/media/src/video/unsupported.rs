//! A target with no player: nothing opens.

use std::time::Duration;

use super::{Frame, VideoError, VideoEvent, VideoSource};

pub(crate) enum Player {}

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
