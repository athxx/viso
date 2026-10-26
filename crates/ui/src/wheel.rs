//! Wheels: what a node that takes the wheel hears of it.
//!
//! A node that takes the wheel ([`NodeStore::set_takes_wheel`]) hears every
//! wheel or trackpad scroll over it — a notch, the fingers' scroll and the
//! system's coast after it — as a [`WheelSample`] on its own chain, before
//! any viewport above it scrolls. A viewport between the node under the
//! pointer and the one that takes the wheel scrolls as usual: what lies
//! inside it is its own.
//!
//! The router records the sample under dispatch, so a handler reads it
//! through [`NodeStore::wheel_sample`].

use crate::binding::BindingTable;
use crate::component::NodeStore;
use crate::hit_test::HitTestTree;
use crate::input::{Modifiers, PointerId, ScrollEvent, ScrollPhase, pointer_dispatch_chain};
use crate::layout::Vec2;
use crate::node::NodeId;
use crate::state::StateStore;

/// A wheel's sample under dispatch, in window coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WheelSample {
    /// The pointer.
    pub at: Vec2,
    /// How far the content would scroll: positive down and right reveals
    /// what lies further down and right.
    pub delta: Vec2,
    pub modifiers: Modifiers,
    /// Where the sample stands in the trackpad gesture that made it.
    pub phase: ScrollPhase,
}

/// The nodes that take the wheel, and the sample under dispatch. Cold: a
/// store none of whose nodes takes it routes nothing here.
#[derive(Debug, Default)]
pub(crate) struct Wheels {
    takers: Vec<NodeId>,
    sample: Option<WheelSample>,
}

impl Wheels {
    pub(crate) fn clear(&mut self) {
        self.takers.clear();
        self.sample = None;
    }
}

impl NodeStore {
    /// Makes node `id` hear the wheel over it, or no longer.
    pub fn set_takes_wheel(&mut self, id: NodeId, takes: bool) {
        let mut takers = std::mem::take(&mut self.wheels.takers);
        takers.retain(|&n| n != id && self.arena().is_live(n));
        if takes && self.arena().is_live(id) {
            takers.push(id);
        }
        self.wheels.takers = takers;
    }

    /// Whether node `id` hears the wheel over it.
    pub fn takes_wheel(&self, id: NodeId) -> bool {
        self.wheels.takers.contains(&id)
    }

    /// The wheel sample under dispatch, `None` for any other dispatch.
    pub fn wheel_sample(&self) -> Option<WheelSample> {
        self.wheels.sample
    }
}

/// Routes `ev` to the nearest node at or above the one under it that takes
/// the wheel, unless a scroll viewport comes first. Returns whether a node
/// took it; one that did leaves nothing for the viewports.
pub fn route_wheel(
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &BindingTable,
    root: NodeId,
    ev: ScrollEvent,
    chain: &mut Vec<NodeId>,
) -> bool {
    if store.wheels.takers.is_empty() {
        return false;
    }
    let Some(target) = HitTestTree::hit(store, root, ev.x, ev.y) else {
        return false;
    };
    let mut node = Some(target);
    let taker = loop {
        let Some(n) = node else {
            return false;
        };
        if store.takes_wheel(n) {
            break n;
        }
        if store.is_scroll(n) {
            return false;
        }
        node = store.parent(n);
    };
    if store.inert(taker) {
        return false;
    }
    store.wheels.sample = Some(WheelSample {
        at: Vec2::new(ev.x, ev.y),
        delta: Vec2::new(ev.delta_x, ev.delta_y),
        modifiers: ev.modifiers,
        phase: ev.phase,
    });
    pointer_dispatch_chain(store, states, bindings, taker, PointerId::MOUSE, chain);
    store.wheels.sample = None;
    true
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use crate::component::{BuildCx, LeafStyle, ScrollStyle, StackStyle};
    use crate::layout::Sizing;
    use viso_render::Rect;

    fn wheel(x: f32, y: f32, dy: f32) -> ScrollEvent {
        ScrollEvent {
            x,
            y,
            delta_x: 0.0,
            delta_y: dy,
            modifiers: Modifiers::default(),
            phase: ScrollPhase::None,
        }
    }

    #[test]
    fn a_node_that_takes_the_wheel_hears_it_before_a_viewport_above() {
        let mut store = NodeStore::new();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let (root, inner) = {
            let mut cx = BuildCx::new(&mut store);
            let mut inner = None;
            let root = cx.scroll(
                ScrollStyle {
                    size: Sizing::fixed(200.0, 200.0),
                    ..ScrollStyle::default()
                },
                |cx| {
                    let map = cx.stack(
                        StackStyle {
                            size: Sizing::fixed(200.0, 600.0),
                            ..StackStyle::default()
                        },
                        |cx| {
                            inner = Some(cx.leaf(LeafStyle {
                                size: Sizing::fixed(50.0, 50.0),
                                ..LeafStyle::default()
                            }));
                        },
                    );
                    cx.takes_wheel(map, true);
                    let seen = Rc::clone(&seen);
                    cx.on_pointer(map, move |ev| {
                        if let Some(w) = ev.wheel() {
                            seen.borrow_mut().push((w.at, w.delta));
                        }
                    });
                },
            );
            (root.id(), inner.unwrap().id())
        };
        store.layout(root, Rect::new(0.0, 0.0, 200.0, 200.0), &mut Vec::new());
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        let mut chain = Vec::new();
        assert!(route_wheel(
            &mut store,
            &mut states,
            &bindings,
            root,
            wheel(10.0, 10.0, 30.0),
            &mut chain
        ));
        assert_eq!(
            *seen.borrow(),
            [(Vec2::new(10.0, 10.0), Vec2::new(0.0, 30.0))]
        );
        assert_eq!(store.wheel_sample(), None, "only under dispatch");
        assert!(store.takes_wheel(store.parent(inner).unwrap()));
        // Outside every taker the viewports have it.
        store.set_takes_wheel(store.parent(inner).unwrap(), false);
        assert!(!route_wheel(
            &mut store,
            &mut states,
            &bindings,
            root,
            wheel(10.0, 10.0, 30.0),
            &mut chain
        ));
    }

    #[test]
    fn a_viewport_inside_the_taker_keeps_its_own_wheel() {
        let mut store = NodeStore::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            let root = cx.stack(
                StackStyle {
                    size: Sizing::fixed(200.0, 200.0),
                    ..StackStyle::default()
                },
                |cx| {
                    cx.scroll(
                        ScrollStyle {
                            size: Sizing::fixed(100.0, 100.0),
                            ..ScrollStyle::default()
                        },
                        |cx| {
                            cx.leaf(LeafStyle {
                                size: Sizing::fixed(100.0, 400.0),
                                ..LeafStyle::default()
                            });
                        },
                    );
                },
            );
            cx.takes_wheel(root, true);
            root.id()
        };
        store.layout(root, Rect::new(0.0, 0.0, 200.0, 200.0), &mut Vec::new());
        let mut states = StateStore::new();
        let bindings = BindingTable::new();
        assert!(!route_wheel(
            &mut store,
            &mut states,
            &bindings,
            root,
            wheel(10.0, 10.0, 30.0),
            &mut Vec::new()
        ));
        assert!(route_wheel(
            &mut store,
            &mut states,
            &bindings,
            root,
            wheel(150.0, 150.0, 30.0),
            &mut Vec::new()
        ));
    }
}
