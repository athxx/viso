//! Native views: platform views — a browser, a map, a player — shown over a
//! node's box.
//!
//! A plugin gives a node a [`NativeView`] ([`BuildCx::native_view`]). After
//! each frame's layout the window tells the view where the node is, when that
//! changed: its box in the window, the part of it the clips above leave, and
//! whether it shows at all ([`NativePlace`]). The view goes with the node: a
//! freed node drops it, which takes the platform view down. A platform view
//! draws over everything the window draws in its box, and takes the input
//! there itself.
//!
//! What the platform view reports goes through a [`NativeSender`]
//! ([`BuildCx::native_events`]): any thread sends, and the node's handler
//! hears every report at the next frame boundary, with the state store to
//! write. Once the node is freed a send reports `false` and keeps nothing.
//!
//! [`BuildCx::native_view`]: crate::BuildCx::native_view
//! [`BuildCx::native_events`]: crate::BuildCx::native_events

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use viso_runtime::RawWindowHandle;

use crate::Rect;
use crate::component::NodeStore;
use crate::context::UpdateCx;
use crate::node::NodeId;
use crate::state::StateStore;
use crate::task::{Continuation, TaskFuture};

/// A platform view shown over a node's box.
pub trait NativeView: 'static {
    /// Show the view as `place` says: called on the window's thread after a
    /// frame's layout, first once the node is laid out, then each time its
    /// box, its clip or whether it shows changed.
    fn place(&mut self, place: &NativePlace);
}

/// Where a node's native view goes on its window.
#[derive(Debug, Clone, Copy)]
pub struct NativePlace {
    /// The window, to put the platform view in.
    pub window: RawWindowHandle,
    /// The node's box in the window's logical points, from its top-left.
    pub frame: Rect,
    /// The part of `frame` the clips above it and the window leave; empty
    /// when it shows nowhere.
    pub clip: Rect,
    /// Device pixels per logical point.
    pub scale: f32,
    /// Whether it shows: the node and its ancestors are neither hidden nor
    /// transparent, and `clip` is not empty.
    pub shown: bool,
}

impl NativePlace {
    fn same(&self, other: &NativePlace) -> bool {
        self.frame == other.frame
            && self.clip == other.clip
            && self.scale == other.scale
            && self.shown == other.shown
    }
}

/// A node's native view and where it was last placed.
pub(crate) struct NativeSlot {
    pub(crate) node: NodeId,
    view: Box<dyn NativeView>,
    placed: Option<NativePlace>,
}

/// The native views of a store, few: a plain list.
#[derive(Default)]
pub(crate) struct NativeViews {
    pub(crate) slots: Vec<NativeSlot>,
}

impl NativeViews {
    pub(crate) fn set(&mut self, node: NodeId, view: Box<dyn NativeView>) {
        self.slots.retain(|s| s.node != node);
        self.slots.push(NativeSlot {
            node,
            view,
            placed: None,
        });
    }

    /// Drop the view of `node`, freed.
    pub(crate) fn free(&mut self, node: NodeId) {
        if !self.slots.is_empty() {
            self.slots.retain(|s| s.node != node);
        }
    }
}

impl NodeStore {
    /// Whether any node has a native view.
    #[inline]
    pub fn has_native_views(&self) -> bool {
        !self.native_views.slots.is_empty()
    }

    /// Tell each native view where its node is on `window`, `viewport` big
    /// in logical points at `scale`, when that changed since it was last
    /// told. The facade calls it after each frame's layout.
    pub fn place_native_views(&mut self, window: RawWindowHandle, viewport: Rect, scale: f32) {
        let mut slots = std::mem::take(&mut self.native_views.slots);
        for slot in &mut slots {
            let place = self.native_place(slot.node, window, viewport, scale);
            if slot.placed.is_none_or(|p| !p.same(&place)) {
                slot.view.place(&place);
                slot.placed = Some(place);
            }
        }
        // A view placed may not add one; keep any the calls added all the same.
        slots.append(&mut self.native_views.slots);
        self.native_views.slots = slots;
    }

    fn native_place(
        &self,
        node: NodeId,
        window: RawWindowHandle,
        viewport: Rect,
        scale: f32,
    ) -> NativePlace {
        let frame = self.world(node);
        let mut clip = frame.intersect(viewport);
        let mut at = self.arena().parent(node);
        while let Some(up) = at {
            if self.clips_children(up) {
                clip = clip.intersect(self.world(up));
            }
            at = self.arena().parent(up);
        }
        let shown = self.shows(node) && !clip.is_empty();
        NativePlace {
            window,
            frame,
            clip: if shown {
                clip
            } else {
                Rect::new(0.0, 0.0, 0.0, 0.0)
            },
            scale,
            shown,
        }
    }
}

/// Sends a node's handler what a platform view reports, from any thread.
pub struct NativeSender<T> {
    inbox: Arc<Inbox<T>>,
}

impl<T> Clone for NativeSender<T> {
    fn clone(&self) -> Self {
        Self {
            inbox: Arc::clone(&self.inbox),
        }
    }
}

impl<T> std::fmt::Debug for NativeSender<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeSender")
            .field("open", &!self.inbox.closed.load(Ordering::Acquire))
            .finish()
    }
}

impl<T> NativeSender<T> {
    /// Hand `item` to the node's handler at the next frame boundary; `false`,
    /// dropping it, once the node is gone.
    pub fn send(&self, item: T) -> bool {
        if self.inbox.closed.load(Ordering::Acquire) {
            return false;
        }
        let waker = {
            let mut queue = self.inbox.queue.lock().unwrap_or_else(|e| e.into_inner());
            queue.items.push(item);
            queue.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
        true
    }

    /// Whether the node still hears what is sent.
    pub fn is_open(&self) -> bool {
        !self.inbox.closed.load(Ordering::Acquire)
    }
}

struct Inbox<T> {
    queue: Mutex<Queue<T>>,
    closed: AtomicBool,
}

struct Queue<T> {
    items: Vec<T>,
    waker: Option<Waker>,
}

/// The receiving end, moved from each task to its continuation and on to
/// the next task: dropped only when the chain ends with the node, which
/// closes the inbox.
struct Receiver<T> {
    inbox: Arc<Inbox<T>>,
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.inbox.closed.store(true, Ordering::Release);
        let mut queue = self.inbox.queue.lock().unwrap_or_else(|e| e.into_inner());
        queue.items.clear();
        queue.waker = None;
    }
}

/// Waits until the inbox holds something, then takes it all.
struct Next<T> {
    receiver: Option<Receiver<T>>,
}

impl<T> Future for Next<T> {
    type Output = (Receiver<T>, Vec<T>);

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        let Some(receiver) = &this.receiver else {
            return Poll::Pending;
        };
        let items = {
            let mut queue = receiver
                .inbox
                .queue
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if queue.items.is_empty() {
                queue.waker = Some(cx.waker().clone());
                return Poll::Pending;
            }
            std::mem::take(&mut queue.items)
        };
        Poll::Ready((this.receiver.take().unwrap(), items))
    }
}

impl<T> Unpin for Next<T> {}

/// A node's handler of reports.
pub(crate) type OnNative<T> = Rc<RefCell<dyn FnMut(T, &mut StateStore)>>;

/// A sender to `on`, and the first task of the chain that hands it each
/// report, to run owned by `owner`.
pub(crate) fn channel<T: 'static>(owner: NodeId, on: OnNative<T>) -> (NativeSender<T>, TaskFuture) {
    let inbox = Arc::new(Inbox {
        queue: Mutex::new(Queue {
            items: Vec::new(),
            waker: None,
        }),
        closed: AtomicBool::new(false),
    });
    let sender = NativeSender {
        inbox: Arc::clone(&inbox),
    };
    (sender, receive(owner, Receiver { inbox }, on))
}

fn receive<T: 'static>(owner: NodeId, receiver: Receiver<T>, on: OnNative<T>) -> TaskFuture {
    Box::pin(async move {
        let (receiver, items) = Next {
            receiver: Some(receiver),
        }
        .await;
        let then: Continuation = Box::new(move |cx: &mut UpdateCx<'_>| {
            let (states, nodes) = cx.stores();
            for item in items {
                (on.borrow_mut())(item, states);
            }
            nodes.spawn_task(owner, receive(owner, receiver, on));
        });
        Some(then)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_place_compares_by_where_not_by_window() {
        let at = |x: f32| NativePlace {
            window: RawWindowHandle::Headless,
            frame: Rect::new(x, 0.0, 10.0, 10.0),
            clip: Rect::new(x, 0.0, 10.0, 10.0),
            scale: 2.0,
            shown: true,
        };
        assert!(at(1.0).same(&at(1.0)));
        assert!(!at(1.0).same(&at(2.0)));
    }

    #[test]
    fn a_sender_closes_with_its_receiver() {
        let on: OnNative<u32> = Rc::new(RefCell::new(|_: u32, _: &mut StateStore| {}));
        let (sender, task) = channel(crate::node::NodeArena::new().alloc(), on);
        assert!(sender.send(1));
        drop(task);
        assert!(!sender.is_open());
        assert!(!sender.send(2));
    }
}
