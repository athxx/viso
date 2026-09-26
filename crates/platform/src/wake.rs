//! A cross-thread handle that wakes the platform event loop.

use std::fmt;
use std::sync::Arc;

/// Wakes the platform event loop from any thread.
///
/// Each [`wake`](Self::wake) eventually delivers a [`RawEvent::Wakeup`] on
/// the loop thread; wakes that arrive before the loop runs may coalesce into
/// one. The waker carries no payload: whoever woke the loop keeps its own
/// record of why.
///
/// [`RawEvent::Wakeup`]: crate::RawEvent::Wakeup
#[derive(Clone)]
pub struct LoopWaker {
    kick: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl LoopWaker {
    /// A waker that runs `kick` to unblock the loop.
    pub fn new(kick: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            kick: Some(Arc::new(kick)),
        }
    }

    /// A waker that does nothing, for a loop that never blocks.
    pub fn inert() -> Self {
        Self { kick: None }
    }

    /// Wake the loop.
    pub fn wake(&self) {
        if let Some(kick) = &self.kick {
            kick();
        }
    }
}

impl fmt::Debug for LoopWaker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoopWaker")
            .field("inert", &self.kick.is_none())
            .finish()
    }
}
