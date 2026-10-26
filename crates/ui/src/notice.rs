//! Notices: what a node reports about itself once a frame has laid out, rather
//! than from an input sample — a viewport's scroll, a virtual list's visible
//! rows, the end of an animation it ran, the asset it shows loading, the
//! video it plays moving on.
//!
//! A node that has a handler for one is watched: after the frame's layout,
//! [`deliver_notices`] reads each watched node's state and, when it moved since
//! the last delivery, calls the handler with it. A change of several fields in
//! one frame — a scroll that also grew the content — is one notice, and a
//! frame that moved nothing calls no handler. What a handler writes is the
//! next frame's transaction.
//!
//! A viewport's scroll is reported when it changes, not at mount; a list's
//! visible rows from the first layout that shows the list. An animation's end
//! reaches its own handler first, then its node's watcher, in the order the
//! animations ended.

use crate::animation::LookValue;
use crate::asset::AssetDraw;
use crate::component::NodeStore;
use crate::content::Content;
use crate::layout::{Axis, Vec2};
use crate::node::NodeId;
use crate::state::StateStore;
use crate::video::VideoStatus;
use crate::virtual_list::VirtualLists;

/// What a node reports.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Notice {
    /// A scroll viewport's offset, its own box and its content's extent.
    Scroll {
        offset: Vec2,
        viewport: Vec2,
        content: Vec2,
    },
    /// The rows of a virtual list its viewport shows, first and last
    /// inclusive, out of `count`.
    VisibleRange {
        first: usize,
        last: usize,
        count: usize,
    },
    /// An animation the node ran ended ([`NodeStore::animate`]).
    AnimationEnd { target: LookValue, finished: bool },
    /// The asset the node shows drew, at its natural size: once it loads,
    /// and again when a new one — or a new revision of it — loads at another
    /// size.
    AssetLoaded { size: Vec2 },
    /// The asset the node shows could not be read or did not decode.
    AssetFailed,
    /// What the player of the video the node shows reports, when it changed:
    /// ready, a new position, playing or paused, waiting, ended.
    Video(VideoStatus),
    /// The video the node shows played to its end and stopped there.
    VideoEnded,
    /// The video the node shows stopped playing, or plays again, since the
    /// watcher last heard.
    VideoPlaying(bool),
    /// The node — or a node holding it — went full screen, or came back,
    /// since the watcher last heard ([`NodeStore::set_fullscreen`]).
    Fullscreen(bool),
    /// The node's laid-out size: at its first layout, and whenever it
    /// changes.
    Size(Vec2),
}

/// Which notices a watched node reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NoticeKinds(u8);

impl NoticeKinds {
    pub const SCROLL: Self = Self(1);
    pub const VISIBLE_RANGE: Self = Self(2);
    pub const ANIMATION_END: Self = Self(4);
    pub const ASSET: Self = Self(8);
    pub const VIDEO: Self = Self(16);
    pub const FULLSCREEN: Self = Self(32);
    pub const SIZE: Self = Self(64);

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl core::ops::BitOr for NoticeKinds {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// A watched node's handler: called with each notice, and the state cells it
/// writes.
pub type NoticeHandler = Box<dyn FnMut(&Notice, &mut StateStore)>;

/// One watched node.
struct Watch {
    /// The node the watch belongs to, which it goes with.
    node: NodeId,
    /// The node it reports on: `node` itself, or one it hangs on.
    subject: NodeId,
    kinds: NoticeKinds,
    /// The scroll last seen, `None` before the first layout.
    scroll: Option<(Vec2, Vec2, Vec2)>,
    /// The visible rows last delivered.
    range: Option<(usize, usize, usize)>,
    /// The asset notice last delivered.
    asset: Option<Notice>,
    /// The video status last delivered.
    video: Option<VideoStatus>,
    /// Whether the subject was full screen when last looked at.
    fullscreen: Option<bool>,
    /// The size last delivered.
    size: Option<Vec2>,
    /// Taken out while it runs.
    handler: Option<NoticeHandler>,
}

/// The watched nodes of a store. Cold: a node with no notice handler is not
/// here, and a frame visits only these.
#[derive(Default)]
pub(crate) struct Notices {
    watches: Vec<Watch>,
}

impl Notices {
    pub(crate) fn clear(&mut self) {
        self.watches.clear();
    }
}

impl NodeStore {
    /// Watches node `id` for the notices `kinds` names, calling `handler` with
    /// each, in place of any handler it had; empty `kinds` stops watching it.
    pub fn watch(&mut self, id: NodeId, kinds: NoticeKinds, handler: NoticeHandler) {
        self.watch_on(id, id, kinds, handler);
    }

    /// Watches node `subject` on behalf of node `id`, as
    /// [`watch`](Self::watch) does `id` itself: the watch is `id`'s, goes
    /// when either is freed, and leaves any watch of `subject`'s own alone —
    /// the parts a control builds inside a node hear that node this way.
    pub fn watch_on(
        &mut self,
        id: NodeId,
        subject: NodeId,
        kinds: NoticeKinds,
        handler: NoticeHandler,
    ) {
        let watches = &mut self.notices_mut().watches;
        watches.retain(|watch| watch.node != id);
        if kinds.is_empty() {
            return;
        }
        watches.push(Watch {
            node: id,
            subject,
            kinds,
            scroll: None,
            range: None,
            asset: None,
            video: None,
            fullscreen: None,
            size: None,
            handler: Some(handler),
        });
    }

    /// Stops watching node `id`.
    pub fn unwatch(&mut self, id: NodeId) {
        self.notices_mut().watches.retain(|watch| watch.node != id);
    }

    /// Whether any node is watched.
    pub fn has_watches(&self) -> bool {
        !self.notices().watches.is_empty()
    }
}

/// After a frame's layout: calls each watched node's handler with what moved
/// since its last delivery, and forgets the nodes freed since. Returns how
/// many notices it delivered.
pub fn deliver_notices(
    store: &mut NodeStore,
    lists: &VirtualLists,
    states: &mut StateStore,
) -> u32 {
    let mut delivered = 0;
    if store.is_transitioning() {
        for (node, end, on_end) in store.take_animation_ends() {
            if let Some(on_end) = on_end {
                on_end(&end, states);
            }
            let notice = Notice::AnimationEnd {
                target: end.target,
                finished: end.finished,
            };
            if store.arena().is_live(node)
                && tell(store, node, NoticeKinds::ANIMATION_END, &notice, states)
            {
                delivered += 1;
            }
        }
    }
    if !store.has_watches() {
        return delivered;
    }
    let mut at = 0;
    while at < store.notices().watches.len() {
        let Watch {
            node,
            subject,
            kinds,
            ..
        } = store.notices().watches[at];
        if !store.arena().is_live(node) || !store.arena().is_live(subject) {
            store.notices_mut().watches.swap_remove(at);
            continue;
        }
        let mut due: [Option<Notice>; 8] = [None; 8];
        // The subject's state, as the node's own is read for a watch of it.
        let node = subject;
        if kinds.contains(NoticeKinds::SCROLL) && store.is_scroll(node) {
            let now = (
                store.scroll(node),
                store.bounds_size(node),
                store.content(node),
            );
            let watch = &mut store.notices_mut().watches[at];
            if watch.scroll.is_some_and(|seen| seen != now) {
                due[0] = Some(Notice::Scroll {
                    offset: now.0,
                    viewport: now.1,
                    content: now.2,
                });
            }
            watch.scroll = Some(now);
        }
        if kinds.contains(NoticeKinds::VISIBLE_RANGE)
            && let Some(list) = lists.get(node)
            && let Some(axis) = store.scroll_axis(node)
        {
            let extent = store.bounds_main(node, axis);
            if extent > 0.0 {
                let now = list.visible(store.scroll_distance(node, axis), extent);
                let watch = &mut store.notices_mut().watches[at];
                if watch.range != Some(now) {
                    due[1] = Some(Notice::VisibleRange {
                        first: now.0,
                        last: now.1,
                        count: now.2,
                    });
                    watch.range = Some(now);
                }
            }
        }
        if kinds.contains(NoticeKinds::ASSET) {
            let now = match store.content_payload(node) {
                Some(Content::Asset { draw, natural, .. }) => match draw {
                    AssetDraw::Nothing => None,
                    AssetDraw::Failed => Some(Notice::AssetFailed),
                    AssetDraw::Raster(_) | AssetDraw::Vector(_) => {
                        Some(Notice::AssetLoaded { size: *natural })
                    }
                },
                _ => None,
            };
            let watch = &mut store.notices_mut().watches[at];
            // Loading again — a new asset — is delivered once it settles.
            if now.is_none() {
                watch.asset = None;
            } else if watch.asset != now {
                due[2] = now;
                watch.asset = now;
            }
        }
        if kinds.contains(NoticeKinds::VIDEO)
            && let Some(now) = store.video_status(node)
        {
            let watch = &mut store.notices_mut().watches[at];
            if watch.video != Some(now) {
                due[3] = Some(Notice::Video(now));
                if now.ended && !watch.video.is_some_and(|seen| seen.ended) {
                    due[4] = Some(Notice::VideoEnded);
                }
                if let Some(seen) = watch.video
                    && seen.playing != now.playing
                {
                    due[5] = Some(Notice::VideoPlaying(now.playing));
                }
                watch.video = Some(now);
            }
        }
        if kinds.contains(NoticeKinds::FULLSCREEN) {
            let now = store.fullscreen().is_some_and(|f| store.within(node, f));
            let watch = &mut store.notices_mut().watches[at];
            if watch.fullscreen.is_some_and(|seen| seen != now) {
                due[6] = Some(Notice::Fullscreen(now));
            }
            watch.fullscreen = Some(now);
        }
        if kinds.contains(NoticeKinds::SIZE) {
            let now = store.bounds_size(node);
            let watch = &mut store.notices_mut().watches[at];
            if watch.size != Some(now) {
                due[7] = Some(Notice::Size(now));
                watch.size = Some(now);
            }
        }
        let node = store.notices().watches[at].node;
        if due.iter().any(Option::is_some)
            && let Some(mut handler) = store.notices_mut().watches[at].handler.take()
        {
            for notice in due.iter().flatten() {
                handler(notice, states);
                delivered += 1;
            }
            if let Some(watch) = store
                .notices_mut()
                .watches
                .iter_mut()
                .find(|watch| watch.node == node && watch.handler.is_none())
            {
                watch.handler = Some(handler);
            }
        }
        at += 1;
    }
    delivered
}

/// Calls node `node`'s watcher with `notice` when it watches for `kind`.
/// Returns whether it did.
fn tell(
    store: &mut NodeStore,
    node: NodeId,
    kind: NoticeKinds,
    notice: &Notice,
    states: &mut StateStore,
) -> bool {
    let Some(watch) = store
        .notices_mut()
        .watches
        .iter_mut()
        .find(|watch| watch.node == node && watch.kinds.contains(kind))
    else {
        return false;
    };
    let Some(mut handler) = watch.handler.take() else {
        return false;
    };
    handler(notice, states);
    if let Some(watch) = store
        .notices_mut()
        .watches
        .iter_mut()
        .find(|watch| watch.node == node && watch.handler.is_none())
    {
        watch.handler = Some(handler);
    }
    true
}

impl NodeStore {
    /// A node's laid-out box size.
    fn bounds_size(&self, id: NodeId) -> Vec2 {
        Vec2 {
            x: self.bounds_main(id, Axis::Row),
            y: self.bounds_main(id, Axis::Column),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use crate::component::{BuildCx, LeafStyle, ScrollStyle};
    use crate::layout::{Length, Sizing};
    use viso_render::Rect;

    #[test]
    fn a_scroll_is_noticed_once_per_frame_that_moves_it_and_not_at_mount() {
        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        let viewport = cx
            .scroll(
                ScrollStyle {
                    size: Sizing {
                        width: Length::Fixed(100.0),
                        height: Length::Fixed(50.0),
                    },
                    ..Default::default()
                },
                |cx| {
                    cx.leaf(LeafStyle {
                        size: Sizing {
                            width: Length::Fixed(100.0),
                            height: Length::Fixed(400.0),
                        },
                        ..Default::default()
                    });
                },
            )
            .id();
        let root = cx.root().expect("a root");
        let seen = Rc::new(RefCell::new(Vec::new()));
        let log = Rc::clone(&seen);
        store.watch(
            viewport,
            NoticeKinds::SCROLL,
            Box::new(move |notice, _| log.borrow_mut().push(*notice)),
        );
        let mut states = StateStore::new();
        let lists = VirtualLists::new();
        let mut frame = |store: &mut NodeStore| {
            store.layout(
                root,
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 100.0,
                    h: 50.0,
                },
                &mut Vec::new(),
            );
            deliver_notices(store, &lists, &mut states)
        };
        assert_eq!(frame(&mut store), 0, "not at mount");
        store.scroll_by(viewport, Vec2 { x: 0.0, y: 30.0 });
        store.scroll_by(viewport, Vec2 { x: 0.0, y: 10.0 });
        assert_eq!(frame(&mut store), 1, "one notice for the frame");
        assert_eq!(frame(&mut store), 0, "a still frame notices nothing");
        assert_eq!(
            seen.borrow().as_slice(),
            [Notice::Scroll {
                offset: Vec2 { x: 0.0, y: 40.0 },
                viewport: Vec2 { x: 100.0, y: 50.0 },
                content: Vec2 { x: 100.0, y: 400.0 },
            }]
        );
        store.unwatch(viewport);
        store.scroll_by(viewport, Vec2 { x: 0.0, y: 10.0 });
        assert_eq!(frame(&mut store), 0);
    }

    #[test]
    fn a_size_is_noticed_at_first_layout_and_when_it_changes() {
        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        let node = cx
            .leaf(LeafStyle {
                size: Sizing::fill(),
                ..Default::default()
            })
            .id();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let log = Rc::clone(&seen);
        store.watch(
            node,
            NoticeKinds::SIZE,
            Box::new(move |notice, _| log.borrow_mut().push(*notice)),
        );
        let mut states = StateStore::new();
        let lists = VirtualLists::new();
        let mut frame = |store: &mut NodeStore, w: f32| {
            store.layout(node, Rect::new(0.0, 0.0, w, 50.0), &mut Vec::new());
            deliver_notices(store, &lists, &mut states)
        };
        assert_eq!(frame(&mut store, 100.0), 1);
        assert_eq!(frame(&mut store, 100.0), 0);
        assert_eq!(frame(&mut store, 80.0), 1);
        assert_eq!(
            seen.borrow().as_slice(),
            [
                Notice::Size(Vec2::new(100.0, 50.0)),
                Notice::Size(Vec2::new(80.0, 50.0))
            ]
        );
    }
}
