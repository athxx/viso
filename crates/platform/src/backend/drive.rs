//! The pump's app and handler held as pointers, for OS callbacks that deliver
//! events while the pump has handed the thread away: a modal resize loop
//! inside one pump turn, or a UIKit/browser loop that calls back after `run`
//! returned or from a `run` that never returns.

use core::ptr::NonNull;

use crate::PlatformApp;
use crate::handler::AppHandler;

/// The app and handler `run` was lent, with the borrows' lifetimes erased.
/// The backend upholds them: it stores a `Drive` only while it holds no other
/// borrow of either, and drops it before the borrows end.
#[derive(Clone, Copy)]
pub(crate) struct Drive {
    app: NonNull<dyn PlatformApp>,
    handler: NonNull<dyn AppHandler>,
}

impl Drive {
    pub(crate) fn new(app: &mut dyn PlatformApp, handler: &mut dyn AppHandler) -> Self {
        // SAFETY: only the trait objects' lifetime bounds change, which leaves
        // the fat pointers' layout as it was.
        unsafe {
            Self {
                app: NonNull::new_unchecked(core::mem::transmute::<
                    *mut (dyn PlatformApp + '_),
                    *mut (dyn PlatformApp + 'static),
                >(app)),
                handler: NonNull::new_unchecked(core::mem::transmute::<
                    *mut (dyn AppHandler + '_),
                    *mut (dyn AppHandler + 'static),
                >(handler)),
            }
        }
    }

    /// The app and handler, borrowed again.
    ///
    /// # Safety
    ///
    /// The borrows [`Drive::new`] took must still be live, and nothing else
    /// may reach the app or the handler until the returned borrows end.
    pub(crate) unsafe fn get<'a>(mut self) -> (&'a mut dyn PlatformApp, &'a mut dyn AppHandler) {
        // SAFETY: per the function contract; the two point at distinct
        // objects, so the borrows do not overlap.
        unsafe { (self.app.as_mut(), self.handler.as_mut()) }
    }
}
