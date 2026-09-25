//! The one-shot answer every asynchronous service call returns.
//!
//! A service call hands back a [`Reply`] at once and keeps the paired
//! [`Completer`]; the OS answers later, from whichever thread its callback
//! runs on. The reply is a [`Future`] for async code and can also be polled
//! without an executor through [`Reply::try_take`]. A completer dropped
//! without answering (a dialog torn down with its window, say) yields
//! [`ServiceError::Cancelled`].

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};

/// Why a service call produced no result.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ServiceError {
    /// This OS, or this build of it, has no such capability.
    Unsupported,
    /// The user or the system refused the permission the call needs.
    Denied,
    /// The user dismissed the request, or it was abandoned before answering.
    Cancelled,
    /// The OS reported an error.
    Failed(String),
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => f.write_str("the service is not supported here"),
            Self::Denied => f.write_str("permission denied"),
            Self::Cancelled => f.write_str("cancelled"),
            Self::Failed(reason) => write!(f, "service failed: {reason}"),
        }
    }
}

impl std::error::Error for ServiceError {}

/// A service call's outcome.
pub type ServiceResult<T> = Result<T, ServiceError>;

enum Slot<T> {
    Waiting(Option<Waker>),
    Done(ServiceResult<T>),
    Taken,
}

/// The pending answer to one service call.
#[must_use = "a reply does nothing unless awaited or polled"]
pub struct Reply<T> {
    slot: Arc<Mutex<Slot<T>>>,
}

/// The sending half of a [`Reply`]; completes it exactly once.
pub struct Completer<T> {
    slot: Option<Arc<Mutex<Slot<T>>>>,
}

/// A pending reply and the completer that answers it.
pub fn reply<T>() -> (Completer<T>, Reply<T>) {
    let slot = Arc::new(Mutex::new(Slot::Waiting(None)));
    (
        Completer {
            slot: Some(slot.clone()),
        },
        Reply { slot },
    )
}

fn lock<T>(slot: &Mutex<Slot<T>>) -> MutexGuard<'_, Slot<T>> {
    slot.lock().unwrap_or_else(|e| e.into_inner())
}

impl<T> Reply<T> {
    /// A reply that is already answered.
    pub fn ready(result: ServiceResult<T>) -> Self {
        Self {
            slot: Arc::new(Mutex::new(Slot::Done(result))),
        }
    }

    /// A reply already failed with `error`.
    pub fn err(error: ServiceError) -> Self {
        Self::ready(Err(error))
    }

    /// The answer, if it has arrived and was not taken yet.
    pub fn try_take(&mut self) -> Option<ServiceResult<T>> {
        let mut slot = lock(&self.slot);
        match std::mem::replace(&mut *slot, Slot::Taken) {
            Slot::Done(result) => Some(result),
            waiting @ Slot::Waiting(_) => {
                *slot = waiting;
                None
            }
            Slot::Taken => None,
        }
    }
}

impl<T> Future for Reply<T> {
    type Output = ServiceResult<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut slot = lock(&self.slot);
        match std::mem::replace(&mut *slot, Slot::Taken) {
            Slot::Done(result) => Poll::Ready(result),
            Slot::Waiting(_) => {
                *slot = Slot::Waiting(Some(cx.waker().clone()));
                Poll::Pending
            }
            // Polled again after yielding its answer.
            Slot::Taken => Poll::Ready(Err(ServiceError::Cancelled)),
        }
    }
}

impl<T> Completer<T> {
    /// Answer the reply and wake whoever awaits it.
    pub fn complete(mut self, result: ServiceResult<T>) {
        self.fill(result);
    }

    fn fill(&mut self, result: ServiceResult<T>) {
        let Some(slot) = self.slot.take() else { return };
        let waker = {
            let mut slot = lock(&slot);
            match std::mem::replace(&mut *slot, Slot::Done(result)) {
                Slot::Waiting(waker) => waker,
                _ => None,
            }
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

/// A completer shared with a callback the OS may call by reference, from
/// any thread, and more than once: the first call completes it.
#[cfg(any(target_vendor = "apple", target_os = "windows"))]
pub(crate) struct OnceCompleter<T>(Mutex<Option<Completer<T>>>);

#[cfg(any(target_vendor = "apple", target_os = "windows"))]
impl<T> OnceCompleter<T> {
    pub(crate) fn new(completer: Completer<T>) -> Self {
        Self(Mutex::new(Some(completer)))
    }

    pub(crate) fn complete(&self, result: ServiceResult<T>) {
        let completer = self.0.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(completer) = completer {
            completer.complete(result);
        }
    }
}

impl<T> Drop for Completer<T> {
    fn drop(&mut self) {
        self.fill(Err(ServiceError::Cancelled));
    }
}

impl<T> fmt::Debug for Reply<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = match *lock(&self.slot) {
            Slot::Waiting(_) => "waiting",
            Slot::Done(_) => "done",
            Slot::Taken => "taken",
        };
        f.debug_struct("Reply").field("state", &state).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Wake;

    struct Count(AtomicUsize);

    impl Wake for Count {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn a_reply_completed_from_another_thread_wakes_its_task() {
        let (completer, mut reply) = reply::<u32>();
        let count = Arc::new(Count(AtomicUsize::new(0)));
        let waker = Waker::from(count.clone());
        let mut cx = Context::from_waker(&waker);
        assert!(Pin::new(&mut reply).poll(&mut cx).is_pending());
        std::thread::spawn(move || completer.complete(Ok(7)))
            .join()
            .unwrap();
        assert_eq!(count.0.load(Ordering::SeqCst), 1);
        assert_eq!(Pin::new(&mut reply).poll(&mut cx), Poll::Ready(Ok(7)));
        assert_eq!(
            Pin::new(&mut reply).poll(&mut cx),
            Poll::Ready(Err(ServiceError::Cancelled))
        );
    }

    #[test]
    fn a_dropped_completer_cancels() {
        let (completer, mut reply) = reply::<()>();
        assert_eq!(reply.try_take(), None);
        drop(completer);
        assert_eq!(reply.try_take(), Some(Err(ServiceError::Cancelled)));
        assert_eq!(reply.try_take(), None);
    }

    #[test]
    fn a_ready_reply_answers_at_once() {
        let mut reply = Reply::ready(Ok("done"));
        assert_eq!(reply.try_take(), Some(Ok("done")));
    }
}
