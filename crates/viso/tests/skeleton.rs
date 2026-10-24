//! A placeholder driven through the facade: while its cell reads loading the
//! skeleton shows, announced as a progress indicator, its blocks shimmering
//! on one clock; once the cell reads loaded the content shows instead, fading
//! in, and the clock stops.

use std::time::Duration;

use viso::render::Rect;
use viso::ui::{
    Axis, BindingTable, BoxStyle, BuildCx, Component, EffectStore, FlexStyle, LeafStyle, NodeId,
    NodeStore, Role, SemanticProjector, ShimmerLook, Size, StateId, StateStore, StateValue,
    TextEdits, VirtualLists, run_structure_hooks,
};
use viso::widgets::{placeholder, skeleton_circle, skeleton_rect, skeleton_text};

const SURFACE: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 400.0,
    h: 300.0,
};

struct Mounted {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    root: NodeId,
    loading: StateId,
}

impl Mounted {
    fn new() -> Self {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();
        let (root, loading) = {
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut states,
                &mut bindings,
                &mut lists,
                &mut text_edits,
                &mut projectors,
            );
            let loading = cx.state(StateValue::Bool(true));
            let root = cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    size: Size::fixed(300.0, 200.0),
                    ..FlexStyle::default()
                },
                |cx| {
                    placeholder(loading)
                        .label("Loading profile")
                        .skeleton(|cx| {
                            cx.flex(
                                FlexStyle {
                                    axis: Axis::Row,
                                    gap: 12.0,
                                    ..FlexStyle::default()
                                },
                                |cx| {
                                    skeleton_circle(40.0).build(cx);
                                    skeleton_text(3).build(cx);
                                },
                            );
                            skeleton_rect(Size::fixed(120.0, 60.0))
                                .look(ShimmerLook::Pulse)
                                .build(cx);
                        })
                        .content(|cx| {
                            cx.leaf(LeafStyle {
                                size: Size::fixed(300.0, 80.0),
                                style: BoxStyle::NONE,
                            });
                        })
                        .build(cx);
                },
            );
            (root.id(), loading)
        };
        store.layout(root, SURFACE, &mut Vec::new());
        Self {
            store,
            states,
            bindings,
            root,
            loading,
        }
    }

    /// The skeleton part and the content part.
    fn parts(&self) -> (NodeId, NodeId) {
        let arena = self.store.arena();
        let control = arena.first_child(self.root).unwrap();
        let mut kids = arena.children(control);
        (kids.next().unwrap(), kids.next().unwrap())
    }

    /// The shimmer leaves under `id`, depth first.
    fn shimmers(&self, id: NodeId, out: &mut Vec<NodeId>) {
        if self.store.shimmer(id).is_some() {
            out.push(id);
        }
        for child in self.store.arena().children(id) {
            self.shimmers(child, out);
        }
    }

    fn set_loading(&mut self, loading: bool) {
        self.states.set(self.loading, StateValue::Bool(loading));
        let mut changed = Vec::new();
        self.states.take_pending(&mut changed);
        self.store
            .flush_state_transactions(&changed, &self.bindings);
        run_structure_hooks(
            &mut self.store,
            &mut self.states,
            &mut self.bindings,
            &mut EffectStore::new(),
            &mut VirtualLists::new(),
            &changed,
        );
        self.store.layout(self.root, SURFACE, &mut Vec::new());
    }
}

#[test]
fn while_loading_the_skeleton_shows_as_a_progress_indicator() {
    let form = Mounted::new();
    let (skeleton, content) = form.parts();
    assert!(!form.store.hidden(skeleton) && form.store.hidden(content));
    let semantics = form.store.semantics(skeleton).unwrap();
    assert_eq!(
        (semantics.role, semantics.label.as_deref()),
        (Role::ProgressIndicator, Some("Loading profile"))
    );
    let mut blocks = Vec::new();
    form.shimmers(skeleton, &mut blocks);
    assert_eq!(blocks.len(), 5, "a circle, three lines, a rectangle");
    let store = &form.store;
    let circle = store.bounds(blocks[0]);
    assert_eq!((circle.w, circle.h), (40.0, 40.0));
    assert_eq!(store.shimmer(blocks[0]).unwrap().radius, 20.0);
    let (first, last) = (store.bounds(blocks[1]), store.bounds(blocks[3]));
    assert_eq!(first.h, 12.0);
    assert!(
        (last.w - first.w * 0.6).abs() < 0.5,
        "the last line is short: {last:?} under {first:?}"
    );
    assert_eq!(store.shimmer(blocks[4]).unwrap().look, ShimmerLook::Pulse);
    assert!(blocks.iter().all(|&b| store.semantics(b).is_none()));
    assert!(store.is_shimmering());
}

#[test]
fn the_blocks_shimmer_on_one_clock() {
    let mut form = Mounted::new();
    let (skeleton, _) = form.parts();
    let mut blocks = Vec::new();
    form.shimmers(skeleton, &mut blocks);
    form.store.tick_shimmers(Duration::from_millis(80));
    let times: Vec<f32> = blocks
        .iter()
        .map(|&b| form.store.shimmer(b).unwrap().time)
        .collect();
    assert!(times.iter().all(|&t| (t - 0.08).abs() < 1e-6), "{times:?}");
}

#[test]
fn once_loaded_the_content_fades_in_and_the_clock_stops() {
    let mut form = Mounted::new();
    let (skeleton, content) = form.parts();
    form.set_loading(false);
    assert!(form.store.hidden(skeleton) && !form.store.hidden(content));
    assert!(
        !form.store.is_shimmering(),
        "no block shows: no frames beat"
    );
    form.store.tick_transitions(Duration::from_millis(16));
    let fading = form.store.opacity(content);
    assert!(
        fading > 0.0 && fading < 1.0,
        "the content fades in: {fading}"
    );
    form.store.tick_transitions(Duration::from_millis(300));
    assert_eq!(form.store.opacity(content), 1.0);
    form.set_loading(true);
    assert!(!form.store.hidden(skeleton) && form.store.hidden(content));
    assert!(form.store.is_shimmering());
}
