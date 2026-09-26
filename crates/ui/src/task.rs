//! UI tasks: futures a handler spawns, owned by the handler's node (ADR 0032).
//!
//! A handler spawns through [`EventCx::spawn`](crate::EventCx::spawn) or
//! [`EventCx::spawn_then`](crate::EventCx::spawn_then). The spawn is queued on
//! the context and handed to the node store under the node whose handler ran,
//! so freeing that node's subtree drops the task. A finished task yields an
//! optional [`Continuation`] that the driver runs with an
//! [`UpdateCx`](crate::context::UpdateCx) at the next frame boundary: state is
//! written only there, never across an `.await`.

use std::future::Future;
use std::pin::Pin;

pub use viso_runtime::TaskId;

use crate::context::UpdateCx;

/// The state-writing tail of a finished task, run at a frame boundary.
pub type Continuation = Box<dyn FnOnce(&mut UpdateCx<'_>)>;

/// A task's future as the store holds it.
pub(crate) type TaskFuture = Pin<Box<dyn Future<Output = Option<Continuation>>>>;

/// The spawns and cancellations one dispatch recorded, in the order the
/// handler made them.
#[derive(Default)]
pub struct TaskOps {
    pub(crate) spawns: Vec<(TaskId, TaskFuture)>,
    pub(crate) cancels: Vec<TaskId>,
}

impl TaskOps {
    /// Whether the dispatch spawned and cancelled nothing.
    pub fn is_empty(&self) -> bool {
        self.spawns.is_empty() && self.cancels.is_empty()
    }
}

/// Wrap a unit future as a task that finishes with no continuation.
pub(crate) fn detached(future: impl Future<Output = ()> + 'static) -> TaskFuture {
    Box::pin(async move {
        future.await;
        None
    })
}

/// Wrap `future` as a task whose output `then` writes back.
pub(crate) fn then<T: 'static>(
    future: impl Future<Output = T> + 'static,
    then: impl FnOnce(&mut UpdateCx<'_>, T) + 'static,
) -> TaskFuture {
    Box::pin(async move {
        let output = future.await;
        Some(Box::new(move |cx: &mut UpdateCx<'_>| then(cx, output)) as Continuation)
    })
}
