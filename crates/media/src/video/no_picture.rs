//! The picture of a player whose frames are rows at hand: there is never a
//! platform buffer to name.

use std::marker::PhantomData;

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
