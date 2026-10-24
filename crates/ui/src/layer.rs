//! Layers: nodes laid out over the whole surface, out of their parent's flow,
//! as a modal, a sheet or a toast is.
//!
//! A layer takes no room in its parent: the parent measures and places its
//! other children as if it were not there. The layer itself is laid out
//! against the surface the store was last laid out in, its own size requests
//! and margins resolved there as a root's are, and it paints in the top
//! layer over everything else. It still hangs where it was authored, so it
//! keeps its owner, its handlers and its place in the semantics tree, and
//! the transforms of its ancestors still move it. The table is cold: a store
//! with no layer pays one emptiness check where layout reads a node.
//!
//! A dialog layer traps the focus while it shows: it takes the focus into
//! itself and confines Tab to its subtree, and gives back the focus and scope
//! it took over when it lets go, so dialogs nest.
//!
//! A light-dismiss layer — a menu, a list, a popover — goes when a press
//! lands outside it: outside its own box, the boxes of the layers shown under
//! it (a submenu's), and its anchor's — unless it sits against a point of its
//! anchor, as a context menu does. The window delivers the dismissal to
//! the layer's key handler ([`EventCx::dismissed`]) before it routes the
//! press, which goes on to whatever it landed on.
//!
//! [`EventCx::dismissed`]: crate::context::EventCx::dismissed

use viso_render::Rect;

use crate::component::NodeStore;
use crate::input::{blur_node, focus_quietly};
use crate::layout::{self, Basis, Vec2};
use crate::node::NodeId;

/// The layers, and the one laid out now.
#[derive(Debug, Default)]
pub(crate) struct Layers {
    nodes: Vec<NodeId>,
    /// The layer whose own layout runs, which layout does not skip.
    laying: Option<u32>,
    /// The surface the layers were last laid out in.
    surface: Option<Rect>,
    /// The focus traps the layers hold, the innermost last.
    traps: Vec<Trap>,
    /// The layers a press outside dismisses.
    light: Vec<NodeId>,
}

/// A focus trap: the layer holding it, and the focus and scope it took over.
#[derive(Debug, Clone, Copy)]
struct Trap {
    layer: NodeId,
    focus: Option<NodeId>,
    scope: Option<NodeId>,
}

impl Layers {
    pub(crate) fn clear(&mut self) {
        self.nodes.clear();
        self.laying = None;
        self.surface = None;
        self.traps.clear();
        self.light.clear();
    }

    /// Forgets the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        self.nodes.retain(|n| n.index() != index);
        self.traps.retain(|t| t.layer.index() != index);
        self.light.retain(|n| n.index() != index);
    }

    #[inline]
    fn holds(&self, index: u32) -> bool {
        self.nodes.iter().any(|n| n.index() == index)
    }
}

impl NodeStore {
    /// Makes node `id` a layer, laid out over the whole surface and painted
    /// in the top layer, or puts it back in its parent's flow.
    pub fn set_layer(&mut self, id: NodeId, layer: bool) {
        if !self.arena().is_live(id) || self.is_layer(id) == layer {
            return;
        }
        if layer {
            self.layers.nodes.push(id);
        } else {
            self.layers.forget(id.index());
        }
        self.set_overlay(id, layer);
        // The parent re-places its children without it, or with it again.
        self.shown_changed(id);
    }

    /// Whether node `id` is a layer.
    pub fn is_layer(&self, id: NodeId) -> bool {
        self.arena().is_live(id) && self.layers.holds(id.index())
    }

    /// Whether layout skips the slot `index`: a layer other than the one
    /// laid out now.
    #[inline]
    pub(crate) fn skips_layer(&self, index: u32) -> bool {
        !self.layers.nodes.is_empty()
            && self.layers.laying != Some(index)
            && self.layers.holds(index)
    }

    /// Whether `id` is redone alone: a layer, which moves no sibling.
    pub(crate) fn redone_alone(&self, id: NodeId) -> bool {
        self.redone_alone_at(id.node_index())
    }

    /// [`redone_alone`](Self::redone_alone) by slot.
    #[inline]
    pub(crate) fn redone_alone_at(&self, index: crate::node::NodeIndex) -> bool {
        !self.layers.nodes.is_empty() && self.layers.holds(index.get())
    }

    /// Lays out every layer under `root` over `surface`, outer layers first.
    pub(crate) fn lay_layers(&mut self, root: NodeId, surface: Rect, scratch: &mut Vec<u32>) {
        self.layers.surface = Some(surface);
        if self.layers.nodes.is_empty() {
            return;
        }
        let mut layers = core::mem::take(&mut self.layers.nodes);
        layers.retain(|&l| self.arena().is_live(l));
        layers.sort_by_key(|&l| self.depth_of(l));
        for &layer in &layers {
            if self.is_under(layer, root) {
                self.lay_layer(layer, surface, scratch);
            }
        }
        self.layers.nodes = layers;
    }

    /// Lays out the layer `layer` over `surface`.
    pub(crate) fn lay_layer(&mut self, layer: NodeId, surface: Rect, scratch: &mut Vec<u32>) {
        let index = layer.index();
        let outer = self.layers.laying.replace(index);
        scratch.clear();
        layout::measure(self, index, scratch);
        let anchored = self.anchored_box(index, surface);
        let bounds = match anchored {
            Some((_, laid)) => laid,
            None if self.covers_surface(index) => surface,
            None => layout::root_box(self, index, surface),
        };
        let within = Vec2 {
            x: surface.w,
            y: surface.h,
        };
        scratch.clear();
        layout::layout_on(self, index, bounds, Basis::DEFINITE, within, scratch);
        // A layer put over its anchor by one of its nodes places that node
        // only now, the first time.
        let anchored = match anchored {
            Some((_, laid)) => match self.anchored_box(index, surface) {
                Some(again) if again.1 != laid => {
                    scratch.clear();
                    layout::layout_on(self, index, again.1, Basis::DEFINITE, within, scratch);
                    Some(again)
                }
                again => again,
            },
            None => None,
        };
        self.layers.laying = outer;
        let was = self.reach_in_parent(index as usize);
        self.rejoin_subtree(layer);
        self.lift_reach(layer, was);
        if let Some((placed, _)) = anchored {
            self.placed_anchored(layer, placed);
        }
    }

    /// Traps the focus in node `id`, or lets it go. Trapping takes the focus
    /// to the first focusable node under it, quietly, else to `id` itself,
    /// else off the node outside that holds it, and confines Tab to the
    /// subtree; letting
    /// go gives back the scope and, if the focus is still inside or nowhere,
    /// the focus it took over.
    pub fn trap_focus(&mut self, id: NodeId, trap: bool) {
        let held = self.layers.traps.iter().position(|t| t.layer == id);
        match (trap, held) {
            (true, None) => {
                if !self.arena().is_live(id) {
                    return;
                }
                self.layers.traps.push(Trap {
                    layer: id,
                    focus: self.focused(),
                    scope: self.focus_scope(),
                });
                self.set_focus_scope(Some(id));
                let inside = self.focused().is_some_and(|f| self.is_under(f, id));
                if !inside {
                    match self
                        .first_focusable(id)
                        .or_else(|| self.focusable(id).then_some(id))
                    {
                        Some(node) => {
                            focus_quietly(self, node);
                        }
                        None => {
                            if let Some(focused) = self.focused() {
                                blur_node(self, focused);
                            }
                        }
                    }
                }
            }
            (false, Some(at)) => {
                let trap = self.layers.traps.remove(at);
                // A trap taken inside this one now gives back what this one
                // took over, not a node of the layer let go.
                if let Some(&inner) = self.layers.traps.get(at) {
                    let focus = inner
                        .focus
                        .filter(|&f| self.arena().is_live(f) && !self.is_under(f, id))
                        .or(trap.focus);
                    self.layers.traps[at] = Trap {
                        focus,
                        scope: trap.scope,
                        ..inner
                    };
                    return;
                }
                let scope = trap.scope.filter(|&n| self.arena().is_live(n));
                self.set_focus_scope(scope);
                let stray = self
                    .focused()
                    .is_none_or(|f| self.is_under(f, id) || !self.arena().is_live(f));
                if stray {
                    match trap.focus {
                        Some(node) if focus_quietly(self, node) => {}
                        _ => {
                            if let Some(focused) = self.focused() {
                                blur_node(self, focused);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Makes layer `id` go when a press lands outside it, or stay.
    pub fn set_light_dismiss(&mut self, id: NodeId, light: bool) {
        let at = self.layers.light.iter().position(|&n| n == id);
        match (light, at) {
            (true, None) if self.arena().is_live(id) => self.layers.light.push(id),
            (false, Some(at)) => {
                self.layers.light.swap_remove(at);
            }
            _ => {}
        }
    }

    /// The shown light-dismiss layers a press at window point `(x, y)` lands
    /// outside of, outermost first.
    pub fn dismissed_by_press(&self, x: f32, y: f32) -> Vec<NodeId> {
        if self.layers.light.is_empty() {
            return Vec::new();
        }
        let contains = |id: NodeId| {
            let r = self.world(id);
            r.x <= x && x < r.x + r.w && r.y <= y && y < r.y + r.h
        };
        let shown = |id: NodeId| self.arena().is_live(id) && !self.hidden(id);
        let mut out: Vec<NodeId> = self
            .layers
            .light
            .iter()
            .copied()
            .filter(|&layer| shown(layer))
            .filter(|&layer| {
                let inside = contains(layer)
                    || self.anchor_of(layer).is_some_and(|(anchor, _)| {
                        self.arena().is_live(anchor)
                            && !self.anchored_at_point(layer)
                            && contains(anchor)
                    })
                    || self.layers.nodes.iter().any(|&other| {
                        other != layer
                            && shown(other)
                            && self.is_under(other, layer)
                            && contains(other)
                    });
                !inside
            })
            .collect();
        out.sort_by_key(|&l| self.depth_of(l));
        out
    }

    /// The shown light-dismiss layer Escape dismisses when nothing took it:
    /// the innermost.
    pub fn dismissed_by_escape(&self) -> Option<NodeId> {
        self.layers
            .light
            .iter()
            .copied()
            .filter(|&l| self.arena().is_live(l) && !self.hidden(l))
            .max_by_key(|&l| self.depth_of(l))
    }

    /// Whether node `id` traps the focus.
    pub fn traps_focus(&self, id: NodeId) -> bool {
        self.layers.traps.iter().any(|t| t.layer == id)
    }

    /// The first focusable, enabled, shown node under `root`, in tree order.
    fn first_focusable(&self, root: NodeId) -> Option<NodeId> {
        let mut stack: Vec<NodeId> = self.arena().children(root).collect();
        stack.reverse();
        while let Some(node) = stack.pop() {
            if !self.enabled(node) || self.hidden(node) {
                continue;
            }
            if self.focusable(node) {
                return Some(node);
            }
            let start = stack.len();
            stack.extend(self.arena().children(node));
            stack[start..].reverse();
        }
        None
    }

    /// The surface the layers were last laid out in.
    pub(crate) fn layer_surface(&self) -> Option<Rect> {
        self.layers.surface
    }

    /// Whether `id` lies at or below `root`.
    pub(crate) fn is_under(&self, id: NodeId, root: NodeId) -> bool {
        let stop = root.node_index();
        let mut at = id.node_index();
        while !at.is_none() {
            if at == stop {
                return true;
            }
            at = self.arena().parent_of(at);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use viso_render::Rect;

    use crate::component::{BuildCx, FlexStyle, LeafStyle, NodeStore};
    use crate::layout::{Axis, Size};

    const SURFACE: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 400.0,
        h: 300.0,
    };

    #[test]
    fn a_layer_covers_the_surface_and_takes_no_room_in_its_parent() {
        let mut store = NodeStore::new();
        let (mut panel, mut layer, mut after, mut inner) = (None, None, None, None);
        let root = {
            let mut cx = BuildCx::new(&mut store);
            let column = FlexStyle {
                axis: Axis::Column,
                ..FlexStyle::default()
            };
            cx.flex(column, |cx| {
                let small = FlexStyle {
                    axis: Axis::Column,
                    size: Size::fixed(100.0, 50.0),
                    ..FlexStyle::default()
                };
                panel = Some(
                    cx.flex(small, |cx| {
                        let cover = FlexStyle {
                            axis: Axis::Column,
                            size: Size::fill(),
                            ..FlexStyle::default()
                        };
                        layer = Some(
                            cx.flex(cover, |cx| {
                                let leaf = LeafStyle {
                                    size: Size::fixed(40.0, 30.0),
                                    ..LeafStyle::default()
                                };
                                inner = Some(cx.leaf(leaf).id());
                            })
                            .id(),
                        );
                    })
                    .id(),
                );
                let leaf = LeafStyle {
                    size: Size::fixed(10.0, 20.0),
                    ..LeafStyle::default()
                };
                after = Some(cx.leaf(leaf).id());
            })
            .id()
        };
        let (layer, after, inner) = (layer.unwrap(), after.unwrap(), inner.unwrap());
        store.set_layer(layer, true);
        assert!(store.is_layer(layer) && store.is_overlay(layer));
        store.layout(root, SURFACE, &mut Vec::new());
        assert_eq!(store.bounds(layer), SURFACE, "over the whole surface");
        assert_eq!(store.bounds(inner).h, 30.0);
        assert_eq!(store.bounds(after).y, 50.0, "the panel kept its own size");

        // A change inside the layer, and the layer's own, relayout alone.
        store.clear_dirty();
        let bigger = Size::fixed(40.0, 60.0);
        store.set_fixed_size(inner, bigger);
        let (mut scratch, mut roots) = (Vec::new(), Vec::new());
        store.relayout_dirty(root, SURFACE, &mut scratch, &mut roots);
        assert_eq!(store.bounds(inner).h, 60.0);
        assert_eq!(store.bounds(layer), SURFACE);

        // A new surface moves the layer with it.
        let wide = Rect {
            w: 800.0,
            ..SURFACE
        };
        store.relayout_dirty(root, wide, &mut scratch, &mut roots);
        assert_eq!(store.bounds(layer), wide);

        store.set_layer(layer, false);
        store.layout(root, SURFACE, &mut Vec::new());
        assert_eq!(store.bounds(layer).w, 100.0, "back in the panel's flow");
        let _ = panel;
    }

    #[test]
    fn nested_traps_hand_the_focus_back_in_turn() {
        let mut store = NodeStore::new();
        let ids = core::cell::RefCell::new(Vec::new());
        let leaf = |cx: &mut BuildCx<'_>| {
            let id = cx.leaf(LeafStyle::default()).id();
            ids.borrow_mut().push(id);
        };
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                leaf(cx);
                let outer = cx.flex(FlexStyle::default(), |cx| {
                    leaf(cx);
                    let inner = cx.flex(FlexStyle::default(), leaf).id();
                    ids.borrow_mut().push(inner);
                });
                ids.borrow_mut().push(outer.id());
            })
            .id()
        };
        let ids = ids.into_inner();
        let (button, field, inner_field, inner, outer) = (ids[0], ids[1], ids[2], ids[3], ids[4]);
        for id in [button, field, inner_field] {
            store.set_focusable(id, true);
        }
        store.set_focused(Some(button));
        store.trap_focus(outer, true);
        assert_eq!(store.focused(), Some(field));
        store.trap_focus(inner, true);
        assert_eq!(
            (store.focused(), store.focus_scope()),
            (Some(inner_field), Some(inner))
        );
        store.trap_focus(inner, false);
        assert_eq!(
            (store.focused(), store.focus_scope()),
            (Some(field), Some(outer))
        );

        // The outer let go first: the inner, closed after, gives back what
        // the outer took over.
        store.trap_focus(inner, true);
        store.trap_focus(outer, false);
        assert!(store.traps_focus(inner) && !store.traps_focus(outer));
        store.trap_focus(inner, false);
        assert_eq!((store.focused(), store.focus_scope()), (Some(button), None));
        let _ = root;
    }
}
